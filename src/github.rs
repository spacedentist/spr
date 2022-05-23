use graphql_client::{GraphQLQuery, Response};
use serde::Deserialize;

use crate::{
    async_memoizer::AsyncMemoizer,
    error::{Error, Result, ResultExt},
    future::Future,
    message::{
        build_github_body, parse_message, MessageSection, MessageSectionsMap,
    },
    utils::normalise_ref,
};
use async_compat::CompatExt;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct GitHub {
    pub config: crate::config::Config,

    pull_request_cache: std::rc::Rc<AsyncMemoizer<u64, Result<PullRequest>>>,
    user_cache: std::rc::Rc<AsyncMemoizer<String, Result<UserWithName>>>,
    reviewers_cache: std::rc::Rc<AsyncMemoizer<(), Result<ReviewersMap>>>,
}

type ReviewersMap = HashMap<String, Option<String>>;

#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub state: PullRequestState,
    pub title: String,
    pub body: Option<String>,
    pub sections: MessageSectionsMap,
    pub base: String,
    pub head: String,
    pub base_oid: git2::Oid,
    pub head_oid: git2::Oid,
    pub merge_commit: Option<git2::Oid>,
    pub reviewers: HashMap<String, ReviewStatus>,
    pub review_status: Option<ReviewStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewStatus {
    Requested,
    Approved,
    Rejected,
}

#[derive(serde::Serialize, Default, Debug)]
pub struct PullRequestUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

impl PullRequestUpdate {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.body.is_none() && self.base.is_none()
    }
}

#[derive(serde::Serialize, Default, Debug)]
pub struct PullRequestRequestReviewers {
    pub reviewers: Vec<String>,
    pub team_reviewers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestState {
    Open,
    Closed,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct UserWithName {
    pub login: String,
    pub name: Option<String>,
    #[serde(default)]
    pub is_collaborator: bool,
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/pullrequest_query.graphql",
    response_derives = "Debug"
)]
pub struct PullRequestQuery;
type GitObjectID = String;

impl GitHub {
    pub fn new(
        config: crate::config::Config,
        git: &crate::git::Git,
        graphql_client: reqwest::Client,
    ) -> Self {
        let pull_request_cache = std::rc::Rc::new(AsyncMemoizer::new({
            let config = config.clone();
            let git = git.clone();
            move |number| {
                GitHub::get_pull_request_impl(
                    number,
                    config.clone(),
                    git.clone(),
                    graphql_client.clone(),
                )
            }
        }));

        let user_cache =
            std::rc::Rc::new(AsyncMemoizer::new(GitHub::get_github_user_impl));

        let reviewers_cache = std::rc::Rc::new(AsyncMemoizer::new({
            let config = config.clone();
            let user_cache = user_cache.clone();

            move |_| {
                let user_cache = user_cache.clone();
                GitHub::get_reviewers_impl(config.clone(), move |login| {
                    user_cache.get(login)
                })
            }
        }));

        Self {
            config,
            pull_request_cache,
            user_cache,
            reviewers_cache,
        }
    }

    pub fn get_github_user(
        &self,
        login: String,
    ) -> Future<Result<UserWithName>> {
        self.user_cache.get(login)
    }
    async fn get_github_user_impl(login: String) -> Result<UserWithName> {
        octocrab::instance()
            .get::<UserWithName, _, _>(format!("users/{}", login), None::<&()>)
            .compat()
            .await
            .map_err(Error::from)
    }

    pub fn get_pull_request(&self, number: u64) -> Future<Result<PullRequest>> {
        self.pull_request_cache.get(number)
    }
    async fn get_pull_request_impl(
        number: u64,
        config: crate::config::Config,
        git: crate::git::Git,
        graphql_client: reqwest::Client,
    ) -> Result<PullRequest> {
        let variables = pull_request_query::Variables {
            name: config.repo.clone(),
            owner: config.owner.clone(),
            number: number as i64,
        };
        let request_body = PullRequestQuery::build_query(variables);
        let res = graphql_client
            .post("https://api.github.com/graphql")
            .json(&request_body)
            .send()
            .compat()
            .await?;
        let response_body: Response<pull_request_query::ResponseData> =
            res.json().await?;

        if let Some(errors) = response_body.errors {
            let error =
                Err(Error::new(format!("fetching PR #{number} failed")));
            return errors
                .into_iter()
                .fold(error, |err, e| err.context(e.to_string()));
        }

        let pr = response_body
            .data
            .ok_or_else(|| Error::new("failed to fetch PR"))?
            .organization
            .ok_or_else(|| Error::new("failed to find organization"))?
            .repository
            .ok_or_else(|| Error::new("failed to find repository"))?
            .pull_request
            .ok_or_else(|| Error::new("failed to find PR"))?;

        let head_oid = git2::Oid::from_str(&pr.head_ref_oid)?;
        let base_oid = git2::Oid::from_str(&pr.base_ref_oid)?;
        git.fetch_commits_from_remote(
            &[head_oid, base_oid],
            &config.remote_name,
        )
        .await?;

        let mut sections = parse_message(&pr.body, MessageSection::Summary);

        let title = pr.title.trim().to_string();
        sections.insert(
            MessageSection::Title,
            if title.is_empty() {
                String::from("(untitled)")
            } else {
                title
            },
        );

        sections.insert(
            MessageSection::PullRequest,
            config.pull_request_url(number),
        );

        let reviewers: HashMap<String, ReviewStatus> = pr
            .latest_opinionated_reviews
            .iter()
            .flat_map(|all_reviews| &all_reviews.nodes)
            .flatten()
            .flatten()
            .flat_map(|review| {
                let user_name = review.author.as_ref()?.login.clone();
                let status = match review.state {
                    pull_request_query::PullRequestReviewState::APPROVED => ReviewStatus::Approved,
                    pull_request_query::PullRequestReviewState::CHANGES_REQUESTED => ReviewStatus::Rejected,
                    _ => ReviewStatus::Requested,
                };
                Some((user_name, status))
            })
            .collect();

        let review_status = match pr.review_decision {
            Some(pull_request_query::PullRequestReviewDecision::APPROVED) => Some(ReviewStatus::Approved),
            Some(pull_request_query::PullRequestReviewDecision::CHANGES_REQUESTED) => Some(ReviewStatus::Rejected),
            Some(pull_request_query::PullRequestReviewDecision::REVIEW_REQUIRED) => Some(ReviewStatus::Requested),
            _ => None,
        };

        let requested_reviewers: Vec<String> = pr.review_requests
            .iter()
            .flat_map(|x| &x.nodes)
            .flatten()
            .flatten()
            .flat_map(|x| &x.requested_reviewer)
            .flat_map(|reviewer| {
              type UserType = pull_request_query::PullRequestQueryOrganizationRepositoryPullRequestReviewRequestsNodesRequestedReviewer;
              match reviewer {
                UserType::User(user) => Some(user.login.clone()),
                UserType::Team(team) => Some(format!("#{}", team.slug)),
                _ => None,
              }
            })
            .chain(reviewers.keys().cloned())
            .collect::<HashSet<String>>() // de-duplicate
            .into_iter()
            .collect();

        sections.insert(
            MessageSection::Reviewers,
            requested_reviewers.iter().fold(String::new(), |out, slug| {
                if out.is_empty() {
                    slug.to_string()
                } else {
                    format!("{}, {}", out, slug)
                }
            }),
        );

        if review_status == Some(ReviewStatus::Approved) {
            sections.insert(
                MessageSection::ReviewedBy,
                reviewers
                    .iter()
                    .filter_map(|(k, v)| {
                        if v == &ReviewStatus::Approved {
                            Some(k)
                        } else {
                            None
                        }
                    })
                    .fold(String::new(), |out, slug| {
                        if out.is_empty() {
                            slug.to_string()
                        } else {
                            format!("{}, {}", out, slug)
                        }
                    }),
            );
        }

        Ok::<_, Error>(PullRequest {
            number: pr.number as u64,
            state: match pr.state {
                pull_request_query::PullRequestState::OPEN => {
                    PullRequestState::Open
                }
                _ => PullRequestState::Closed,
            },
            title: pr.title,
            body: Some(pr.body),
            sections,
            base: normalise_ref(pr.base_ref_name).into(),
            head: normalise_ref(pr.head_ref_name).into(),
            base_oid,
            head_oid,
            reviewers,
            review_status,
            merge_commit: pr
                .merge_commit
                .and_then(|sha| git2::Oid::from_str(&sha.oid).ok()),
        })
    }

    pub async fn create_pull_request(
        &self,
        message: &MessageSectionsMap,
        base_ref_name: String,
        head_ref_name: String,
        draft: bool,
    ) -> Result<u64> {
        let number = octocrab::instance()
            .pulls(self.config.owner.clone(), self.config.repo.clone())
            .create(
                message
                    .get(&MessageSection::Title)
                    .unwrap_or(&String::new()),
                head_ref_name,
                base_ref_name,
            )
            .body(build_github_body(message))
            .draft(Some(draft))
            .send()
            .compat()
            .await?
            .number;

        Ok(number)
    }

    pub async fn update_pull_request(
        &self,
        number: u64,
        updates: PullRequestUpdate,
    ) -> Result<()> {
        octocrab::instance()
            .patch::<octocrab::models::pulls::PullRequest, _, _>(
                format!(
                    "repos/{}/{}/pulls/{}",
                    self.config.owner, self.config.repo, number
                ),
                Some(&updates),
            )
            .compat()
            .await?;

        Ok(())
    }

    pub async fn request_reviewers(
        &self,
        number: u64,
        reviewers: PullRequestRequestReviewers,
    ) -> Result<()> {
        #[derive(Deserialize)]
        struct Ignore {}
        let _: Ignore = octocrab::instance()
            .post(
                format!(
                    "repos/{}/{}/pulls/{}/requested_reviewers",
                    self.config.owner, self.config.repo, number
                ),
                Some(&reviewers),
            )
            .compat()
            .await?;

        Ok(())
    }

    pub fn get_reviewers(
        &self,
    ) -> Future<Result<HashMap<String, Option<String>>>> {
        self.reviewers_cache.get(())
    }
    async fn get_reviewers_impl(
        config: crate::config::Config,
        get_github_user: impl Fn(String) -> Future<Result<UserWithName>>,
    ) -> Result<HashMap<String, Option<String>>> {
        let (users, teams): (
            Vec<UserWithName>,
            octocrab::Page<octocrab::models::teams::RequestedTeam>,
        ) = futures_lite::future::try_zip(
            async {
                let users = octocrab::instance()
                    .get::<Vec<octocrab::models::User>, _, _>(
                        format!(
                            "repos/{}/{}/collaborators",
                            &config.owner, &config.repo
                        ),
                        None::<&()>,
                    )
                    .compat()
                    .await?;

                let user_names = futures::future::join_all(
                    users.into_iter().map(|u| get_github_user(u.login)),
                )
                .await
                .into_iter()
                .map(|fr| fr?)
                .collect::<Result<Vec<_>>>()?;

                Ok::<_, Error>(user_names)
            },
            async {
                Ok(octocrab::instance()
                    .teams(&config.owner)
                    .list()
                    .send()
                    .compat()
                    .await
                    .ok()
                    .unwrap_or_default())
            },
        )
        .await?;

        let mut map = HashMap::new();

        for user in users {
            map.insert(user.login, user.name);
        }

        for team in teams {
            map.insert(format!("#{}", team.slug), team.description);
        }

        Ok::<_, Error>(map)
    }
}
