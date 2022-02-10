use octocrab::models::IssueState;
use serde::Deserialize;

use crate::{
    error::{Error, Result},
    executor::spawn,
    future::{Future, SharedFuture},
    message::{
        build_github_body, parse_message, MessageSection, MessageSectionsMap,
    },
    utils::normalise_ref,
};
use async_compat::CompatExt;
use std::collections::HashMap;

#[derive(Clone)]
pub struct GitHub {
    pub config: crate::config::Config,
    inner: std::rc::Rc<async_lock::Mutex<Inner>>,
}
struct Inner {
    pull_request_cache: HashMap<u64, SharedFuture<Result<PullRequest>>>,
    logged_in_user_cache: Option<SharedFuture<Result<UserWithName>>>,
    user_cache: HashMap<String, SharedFuture<Result<UserWithName>>>,
    reviewers_cache: Option<SharedFuture<Result<ReviewersMap>>>,
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
    pub head_oid: git2::Oid,
    pub mergeable: Option<bool>,
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

impl Inner {
    pub fn new() -> Self {
        Self {
            pull_request_cache: Default::default(),
            logged_in_user_cache: None,
            user_cache: Default::default(),
            reviewers_cache: Default::default(),
        }
    }
}

impl GitHub {
    pub fn new(config: crate::config::Config) -> Self {
        Self {
            config,
            inner: std::rc::Rc::new(async_lock::Mutex::new(Inner::new())),
        }
    }

    pub fn get_logged_in_git_hub_user(&self) -> Future<Result<UserWithName>> {
        let (p, f) = Future::new_promise();
        let inner = self.inner.clone();

        spawn(async move {
            let mut inner = inner.lock().await;
            let shared = inner
                .logged_in_user_cache
                .get_or_insert_with(|| {
                    Future::new(async {
                        octocrab::instance()
                            .get::<UserWithName, _, _>("user", None::<&()>)
                            .compat()
                            .await
                            .map_err(Error::from)
                    })
                    .shared()
                })
                .clone();

            drop(inner);
            if let Ok(result) = shared.await {
                p.set(result).ok();
            }
        })
        .detach();

        f
    }

    pub fn get_github_user(
        &self,
        login: String,
    ) -> Future<Result<UserWithName>> {
        let (p, f) = Future::new_promise();
        let inner = self.inner.clone();

        spawn(async move {
            let mut inner = inner.lock().await;
            let login = login;

            let shared = inner
                .user_cache
                .entry(login.clone())
                .or_insert_with(|| {
                    Future::new(async move {
                        octocrab::instance()
                            .get::<UserWithName, _, _>(
                                format!("users/{}", login),
                                None::<&()>,
                            )
                            .compat()
                            .await
                            .map_err(Error::from)
                    })
                    .shared()
                })
                .clone();

            drop(inner);
            if let Ok(result) = shared.await {
                p.set(result).ok();
            }
        })
        .detach();

        f
    }

    pub fn get_pull_request(
        &self,
        number: u64,
        git: &crate::git::Git,
    ) -> Future<Result<PullRequest>> {
        let (p, f) = Future::new_promise();
        let inner = self.inner.clone();
        let config = self.config.clone();
        let git = git.clone();

        spawn(async move {
            let mut inner = inner.lock().await;

            let shared = inner
                .pull_request_cache
                .entry(number)
                .or_insert_with(|| {
                    Future::new(async move {
                        #[derive(Deserialize)]
                        struct User {
                            login: String,
                        }
                        #[derive(Deserialize)]
                        struct PullRequestReview {
                            user: User,
                            state: String,
                        }

                        let reviewers_future = spawn({
                            let route = format!(
                                "repos/{owner}/{repo}/pulls/{number}/reviews",
                                owner = &config.owner,
                                repo = &config.repo
                            );
                            async {
                                octocrab::instance()
                                    .get::<Vec<PullRequestReview>, _, _>(
                                        route,
                                        None::<&()>,
                                    )
                                    .compat()
                                    .await
                            }
                        });

                        let pr = octocrab::instance()
                            .pulls(config.owner.clone(), config.repo.clone())
                            .get(number)
                            .compat()
                            .await?;

                        let head_oid = git2::Oid::from_str(&pr.head.sha[..])?;
                        git.fetch_commit_from_remote(
                            head_oid,
                            config.remote_name.clone(),
                        )
                        .await?;

                        let mut sections = parse_message(
                            pr.body.as_ref().map(|s| &s[..]).unwrap_or(""),
                            MessageSection::Summary,
                        );

                        sections.insert(
                            MessageSection::Title,
                            pr.title
                                .as_ref()
                                .map(|s| &s[..])
                                .unwrap_or("(untitled)")
                                .trim()
                                .to_string(),
                        );

                        sections.insert(
                            MessageSection::PullRequest,
                            config.pull_request_url(number),
                        );

                        let mut reviewers =
                            HashMap::<String, ReviewStatus>::new();
                        let mut review_status: Option<ReviewStatus> = None;

                        if let Some(requested_reviewers) =
                            pr.requested_reviewers
                        {
                            for reviewer in requested_reviewers {
                                reviewers.insert(
                                    reviewer.login,
                                    ReviewStatus::Requested,
                                );
                            }
                        }
                        if let Some(requested_teams) = pr.requested_teams {
                            for team in requested_teams {
                                reviewers.insert(
                                    format!("#{}", team.slug),
                                    ReviewStatus::Requested,
                                );
                            }
                        }

                        if let Ok(reviewers_list) = reviewers_future.await {
                            for reviewer in reviewers_list {
                                if reviewers.contains_key(&reviewer.user.login)
                                {
                                    continue;
                                }
                                let state = match &reviewer.state[..] {
                                    "APPROVED" => Some(ReviewStatus::Approved),
                                    "CHANGES_REQUESTED" => {
                                        Some(ReviewStatus::Rejected)
                                    }
                                    _ => None,
                                };
                                if let Some(state) = state {
                                    reviewers.insert(
                                        reviewer.user.login,
                                        state.clone(),
                                    );
                                    review_status = Some(state);
                                }
                            }
                        }

                        sections.insert(
                            MessageSection::Reviewers,
                            reviewers.keys().fold(
                                String::new(),
                                |out, slug| {
                                    if out.is_empty() {
                                        slug.to_string()
                                    } else {
                                        format!("{}, {}", out, slug)
                                    }
                                },
                            ),
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
                            number: pr.number,
                            state: if pr.state == Some(IssueState::Open) {
                                PullRequestState::Open
                            } else {
                                PullRequestState::Closed
                            },
                            title: pr.title.unwrap_or_default(),
                            body: pr.body,
                            sections,
                            base: normalise_ref(pr.base.ref_field).into(),
                            head: normalise_ref(pr.head.ref_field).into(),
                            head_oid,
                            reviewers,
                            review_status,
                            mergeable: pr.mergeable,
                            merge_commit: pr
                                .merge_commit_sha
                                .map(|sha| git2::Oid::from_str(&sha).ok())
                                .flatten(),
                        })
                    })
                    .shared()
                })
                .clone();
            drop(inner);
            if let Ok(result) = shared.await {
                p.set(result).ok();
            }
        })
        .detach();

        f
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
        let (p, f) = Future::new_promise();
        let inner = self.inner.clone();
        let github = self.clone();

        spawn(async move {
            let mut inner = inner.lock().await;

            let shared = inner
                .reviewers_cache
                .get_or_insert_with(|| {
                    Future::new(async move {
                        let (users, teams): (
                            Vec<UserWithName>,
                            octocrab::Page<
                                octocrab::models::teams::RequestedTeam,
                            >,
                        ) = futures_lite::future::try_zip(
                            async {
                                let users = octocrab::instance()
                                    .get::<Vec<octocrab::models::User>, _, _>(
                                        format!(
                                            "repos/{}/{}/collaborators",
                                            &github.config.owner,
                                            &github.config.repo
                                        ),
                                        None::<&()>,
                                    )
                                    .compat()
                                    .await?;

                                let user_names = futures::future::join_all(
                                    users.into_iter().map(|u| {
                                        github.get_github_user(u.login)
                                    }),
                                )
                                .await
                                .into_iter()
                                .map(|fr| fr?)
                                .collect::<Result<Vec<_>>>()?;

                                Ok::<_, Error>(user_names)
                            },
                            async {
                                Ok(octocrab::instance()
                                    .teams(&github.config.owner)
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
                            map.insert(
                                format!("#{}", team.slug),
                                team.description,
                            );
                        }

                        Ok::<_, Error>(map)
                    })
                    .shared()
                })
                .clone();

            drop(inner);
            if let Ok(result) = shared.await {
                p.set(result).ok();
            }
        })
        .detach();

        f
    }
}
