//! The forge implementation for GitHub (github.com), via its REST and
//! GraphQL APIs

use color_eyre::eyre::{Error, Result, WrapErr as _, eyre};
use graphql_client::{GraphQLQuery, Response};
use serde::Deserialize;

use crate::{
    config::{Config, MergeMethod},
    forge::{
        AsyncMergeStatus, Branch, Forge, OpenPullRequest, PullRequest,
        PullRequestMergeability, PullRequestRefs, PullRequestRequestReviewers,
        PullRequestStack, PullRequestState, PullRequestUpdate, ReviewStatus,
        Team, User, review_status_from_reviews,
    },
    git::Git,
    git_remote::GitRemote,
    message::CommitMessage,
};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone)]
pub struct GitHub {
    config: Config,
    git: Git,
    git_remote: GitRemote,
    octocrab: octocrab::Octocrab,
}

/// A Pull Request's description as GitHub returns it, with Unix line endings
/// and without surrounding whitespace, as in a commit message
fn normalize_body(body: &str) -> String {
    body.replace("\r\n", "\n").trim().to_string()
}

/// The version of the GitHub REST API that the stacked pull requests APIs
/// (stacks and asynchronous merging) require.
const STACKS_API_VERSION: &str = "2026-03-10";

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/pullrequest_query.graphql",
    response_derives = "Debug"
)]
pub struct PullRequestQuery;
type GitObjectID = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/pullrequest_mergeability_query.graphql",
    response_derives = "Debug"
)]
pub struct PullRequestMergeabilityQuery;

#[allow(clippy::upper_case_acronyms)]
type URI = String;
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/open_reviews.graphql",
    response_derives = "Debug"
)]
pub struct SearchQuery;

/// Whether an error is GitHub reporting that something doesn't exist
fn is_not_found(error: &octocrab::Error) -> bool {
    matches!(
        error,
        octocrab::Error::GitHub { source, .. }
            if source.status_code == http::StatusCode::NOT_FOUND
    )
}

impl GitHub {
    pub fn new(config: Config, git: Git, auth_token: String) -> Result<Self> {
        let git_remote = GitRemote::new(
            git.repo().clone(),
            format!("{}.git", config.target.repository().web_url()),
            auth_token.clone(),
        );
        let octocrab = octocrab::Octocrab::builder()
            .personal_token(auth_token)
            .build()?;
        Ok(Self {
            config,
            git,
            git_remote,
            octocrab,
        })
    }

    /// Send a request to one of the stacked pull requests APIs, which
    /// require a newer API version than octocrab uses. Returns the parsed
    /// response, or `None` if the response has no body.
    async fn stacks_api_request<R, B>(
        &self,
        method: http::Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Option<R>>
    where
        R: serde::de::DeserializeOwned,
        B: serde::Serialize + ?Sized,
    {
        let octocrab = &self.octocrab;
        let uri = format!(
            "/repos/{}/{}/{}",
            self.config.owner, self.config.repo, path
        );
        let mut request = octocrab.build_request(
            http::request::Builder::new().method(method).uri(uri),
            body,
        )?;
        // `build_request` adds octocrab's default API version header, which
        // we replace.
        request.headers_mut().insert(
            "x-github-api-version",
            http::HeaderValue::from_static(STACKS_API_VERSION),
        );
        let response = octocrab.execute(request).await?;
        let response = octocrab::map_github_error(response).await?;
        let text = octocrab.body_to_string(response).await?;

        if text.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(serde_json::from_str(&text)?))
        }
    }
}

impl Forge for GitHub {
    fn config(&self) -> &Config {
        &self.config
    }

    fn git(&self) -> &Git {
        &self.git
    }

    fn remote(&self) -> &GitRemote {
        &self.git_remote
    }

    async fn get_user(&self, login: &str) -> Result<Option<User>> {
        #[derive(Deserialize)]
        struct UserWithName {
            login: String,
            name: Option<String>,
        }
        match self
            .octocrab
            .get::<UserWithName, _, _>(format!("/users/{login}"), None::<&()>)
            .await
        {
            Ok(user) => Ok(Some(User {
                login: user.login,
                name: user.name,
            })),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn get_team(&self, slug: &str) -> Result<Option<Team>> {
        match self.octocrab.teams(&self.config.owner).get(slug).await {
            Ok(team) => Ok(Some(Team { slug: team.slug })),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn get_pull_request(self, number: u64) -> Result<PullRequest> {
        let GitHub {
            config,
            git_remote,
            octocrab,
            ..
        } = self;

        let variables = pull_request_query::Variables {
            name: config.repo.clone(),
            owner: config.owner.clone(),
            number: number as i64,
        };
        let request_body = PullRequestQuery::build_query(variables);
        let response_body: Response<pull_request_query::ResponseData> =
            octocrab.post("/graphql", Some(&request_body)).await?;

        if let Some(errors) = response_body.errors {
            let error = Err(eyre!("fetching PR #{number} failed"));
            return errors
                .into_iter()
                .fold(error, |err, e| err.context(e.to_string()));
        }

        let pr = response_body
            .data
            .ok_or_else(|| eyre!("failed to fetch PR"))?
            .repository
            .ok_or_else(|| eyre!("failed to find repository"))?
            .pull_request
            .ok_or_else(|| eyre!("failed to find PR"))?;

        let base = config.new_branch_from_ref(&pr.base_ref_name)?;
        let head = config.new_branch_from_ref(&pr.head_ref_name)?;

        let branch_names: Vec<_> =
            [&base, &head].iter().map(|&b| b.branch_name()).collect();

        let [base_oid, head_oid] =
            git_remote.fetch_from_remote(&branch_names, &[])?[0..2]
        else {
            unreachable!();
        };

        let base_oid = base_oid
            .ok_or_else(|| eyre!("{} not found on GitHub", base.ref_name()))?;
        let head_oid = head_oid
            .ok_or_else(|| eyre!("{} not found on GitHub", head.ref_name()))?;

        // Titles and descriptions edited on GitHub can have surrounding
        // whitespace, and descriptions edited in the web interface have
        // Windows line endings. Normalise them, so they compare equal to the
        // local commit message (and `spr amend` doesn't write them into it).
        let pr_title = pr.title.trim().to_string();
        let body = normalize_body(&pr.body);

        let title = if pr_title.is_empty() {
            String::from("(untitled)")
        } else {
            pr_title.clone()
        };

        let mut message = CommitMessage::new(title, body.clone());

        message.set_trailer(
            "Pull-request".to_string(),
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
            // No review decision: the repository doesn't require reviews.
            // Go by the reviews instead.
            _ => review_status_from_reviews(reviewers.values()),
        };

        // Sorted, so the resulting trailers don't change between runs of spr
        // (which would rewrite local commits for no reason).
        let requested_reviewers: BTreeSet<String> = pr.review_requests
            .iter()
            .flat_map(|x| &x.nodes)
            .flatten()
            .flatten()
            .flat_map(|x| &x.requested_reviewer)
            .flat_map(|reviewer| {
              type UserType = pull_request_query::PullRequestQueryRepositoryPullRequestReviewRequestsNodesRequestedReviewer;
              match reviewer {
                UserType::User(user) => Some(user.login.clone()),
                UserType::Team(team) => Some(format!("#{}", team.slug)),
                _ => None,
              }
            })
            .chain(reviewers.keys().cloned())
            .collect();

        if !requested_reviewers.is_empty() {
            message.set_trailer(
                "Reviewers".to_string(),
                requested_reviewers
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }

        if review_status == Some(ReviewStatus::Approved) {
            let mut reviewed_by: Vec<&str> = reviewers
                .iter()
                .filter(|(_, status)| **status == ReviewStatus::Approved)
                .map(|(login, _)| login.as_str())
                .collect();
            reviewed_by.sort();
            let reviewed_by = reviewed_by.join(", ");
            if !reviewed_by.is_empty() {
                message.set_trailer("Reviewed-by".to_string(), reviewed_by);
            }
        }

        Ok::<_, Error>(PullRequest {
            number: pr.number as u64,
            state: match pr.state {
                pull_request_query::PullRequestState::OPEN => {
                    PullRequestState::Open
                }
                _ => PullRequestState::Closed,
            },
            title: pr_title,
            body: Some(body),
            message,
            base,
            head,
            base_oid,
            head_oid,
            reviewers,
            review_status,
            merge_commit: pr
                .merge_commit
                .and_then(|sha| git2::Oid::from_str(&sha.oid).ok()),
        })
    }

    async fn get_pull_request_refs(
        &self,
        number: u64,
    ) -> Result<PullRequestRefs> {
        let pull = self
            .octocrab
            .pulls(&self.config.owner, &self.config.repo)
            .get(number)
            .await?;

        Ok(PullRequestRefs {
            number,
            open: pull.state == Some(octocrab::models::IssueState::Open),
            head: pull.head.ref_field,
            base: pull.base.ref_field,
        })
    }

    async fn get_pull_request_mergeability(
        &self,
        number: u64,
    ) -> Result<PullRequestMergeability> {
        let variables = pull_request_mergeability_query::Variables {
            name: self.config.repo.clone(),
            owner: self.config.owner.clone(),
            number: number as i64,
        };
        let request_body = PullRequestMergeabilityQuery::build_query(variables);
        let response_body: Response<
            pull_request_mergeability_query::ResponseData,
        > = self.octocrab.post("/graphql", Some(&request_body)).await?;

        if let Some(errors) = response_body.errors {
            let error = Err(eyre!("querying PR #{number} mergeability failed"));
            return errors.into_iter().fold(error, |err, e| err.wrap_err(e));
        }

        let pr = response_body
            .data
            .ok_or_else(|| eyre!("failed to fetch PR"))?
            .repository
            .ok_or_else(|| eyre!("failed to find repository"))?
            .pull_request
            .ok_or_else(|| eyre!("failed to find PR"))?;

        Ok::<_, Error>(PullRequestMergeability {
            base: self.config.new_branch_from_ref(&pr.base_ref_name)?,
            head_oid: git2::Oid::from_str(&pr.head_ref_oid)?,
            mergeable: match pr.mergeable {
                pull_request_mergeability_query::MergeableState::CONFLICTING => Some(false),
                pull_request_mergeability_query::MergeableState::MERGEABLE => Some(true),
                pull_request_mergeability_query::MergeableState::UNKNOWN => None,
                _ => None,
            },
            merge_commit: pr
            .merge_commit
            .and_then(|sha| git2::Oid::from_str(&sha.oid).ok()),
        })
    }

    async fn list_open_pull_requests(&self) -> Result<Vec<OpenPullRequest>> {
        let variables = search_query::Variables {
            query: format!(
                "repo:{}/{} is:open is:pr author:@me archived:false",
                self.config.owner, self.config.repo
            ),
        };
        let request_body = SearchQuery::build_query(variables);
        let response_body: Response<search_query::ResponseData> =
            self.octocrab.post("/graphql", Some(&request_body)).await?;

        let nodes = response_body
            .data
            .and_then(|data| data.search.nodes)
            .ok_or_else(|| eyre!("unexpected error"))?;

        Ok(nodes
            .into_iter()
            .flatten()
            .filter_map(|node| match node {
                search_query::SearchQuerySearchNodes::PullRequest(pr) => {
                    Some(pr)
                }
                _ => None,
            })
            .map(|pr| {
                use search_query::{
                    PullRequestReviewDecision as Decision,
                    PullRequestReviewState as State,
                };
                let review_status = match pr.review_decision {
                    Some(Decision::APPROVED) => Some(ReviewStatus::Approved),
                    Some(Decision::CHANGES_REQUESTED) => {
                        Some(ReviewStatus::Rejected)
                    }
                    Some(Decision::REVIEW_REQUIRED) => {
                        Some(ReviewStatus::Requested)
                    }
                    // Without required reviews, GitHub has no review
                    // decision: go by the reviews instead
                    _ => {
                        let reviews: Vec<ReviewStatus> = pr
                            .latest_opinionated_reviews
                            .iter()
                            .flat_map(|reviews| &reviews.nodes)
                            .flatten()
                            .flatten()
                            .map(|review| match review.state {
                                State::APPROVED => ReviewStatus::Approved,
                                State::CHANGES_REQUESTED => {
                                    ReviewStatus::Rejected
                                }
                                _ => ReviewStatus::Requested,
                            })
                            .collect();
                        review_status_from_reviews(&reviews)
                    }
                };
                OpenPullRequest {
                    title: pr.title,
                    url: pr.url,
                    review_status,
                }
            })
            .collect())
    }

    async fn list_pull_requests_with_base(
        &self,
        branch: &Branch,
    ) -> Result<Vec<u64>> {
        let pulls = self
            .octocrab
            .pulls(&self.config.owner, &self.config.repo)
            .list()
            .state(octocrab::params::State::Open)
            .base(branch.branch_name())
            .per_page(100)
            .send()
            .await?;
        let pulls = self.octocrab.all_pages(pulls).await?;
        Ok(pulls.into_iter().map(|pull| pull.number).collect())
    }

    async fn create_pull_request(
        &self,
        message: &CommitMessage,
        base_ref_name: String,
        head_ref_name: String,
        draft: bool,
    ) -> Result<u64> {
        let title = message.title();
        let title = if title.is_empty() {
            "(untitled)"
        } else {
            title
        };

        let number = self
            .octocrab
            .pulls(&self.config.owner, &self.config.repo)
            .create(title, head_ref_name, base_ref_name)
            .body(message.to_github_body())
            .draft(Some(draft))
            .send()
            .await?
            .number;

        Ok(number)
    }

    async fn update_pull_request(
        &self,
        number: u64,
        updates: PullRequestUpdate,
    ) -> Result<()> {
        self.octocrab
            .patch::<octocrab::models::pulls::PullRequest, _, _>(
                format!(
                    "/repos/{}/{}/pulls/{}",
                    self.config.owner, self.config.repo, number
                ),
                Some(&updates),
            )
            .await?;

        Ok(())
    }

    async fn request_reviewers(
        &self,
        number: u64,
        reviewers: PullRequestRequestReviewers,
    ) -> Result<()> {
        #[derive(Deserialize)]
        struct Ignore {}
        let _: Ignore = self
            .octocrab
            .post(
                format!(
                    "/repos/{}/{}/pulls/{}/requested_reviewers",
                    self.config.owner, self.config.repo, number
                ),
                Some(&reviewers),
            )
            .await?;

        Ok(())
    }

    async fn merge_pull_request(
        &self,
        number: u64,
        head_oid: git2::Oid,
        merge_method: MergeMethod,
        commit_title: &str,
        commit_message: &str,
    ) -> Result<Option<git2::Oid>> {
        let merge = self
            .octocrab
            .pulls(&self.config.owner, &self.config.repo)
            .merge(number)
            .method(match merge_method {
                MergeMethod::Squash => {
                    octocrab::params::pulls::MergeMethod::Squash
                }
                MergeMethod::Merge => {
                    octocrab::params::pulls::MergeMethod::Merge
                }
            })
            .title(commit_title)
            .message(commit_message)
            .sha(head_oid.to_string())
            .send()
            .await?;
        if !merge.merged {
            return Err(eyre!(
                "GitHub Pull Request merge failed: {}",
                merge.message.unwrap_or_default()
            ));
        }
        merge
            .sha
            .map(|sha| git2::Oid::from_str(&sha))
            .transpose()
            .map_err(Error::from)
    }

    fn supports_stacks(&self) -> bool {
        true
    }

    async fn find_pull_request_stack(
        &self,
        number: u64,
    ) -> Result<Option<PullRequestStack>> {
        let stacks: Vec<PullRequestStack> = self
            .stacks_api_request(
                http::Method::GET,
                &format!("stacks?pull_request={number}"),
                None::<&()>,
            )
            .await?
            .unwrap_or_default();

        Ok(stacks.into_iter().find(|stack| stack.open))
    }

    async fn create_pull_request_stack(
        &self,
        numbers: &[u64],
    ) -> Result<PullRequestStack> {
        self.stacks_api_request(
            http::Method::POST,
            "stacks",
            Some(&serde_json::json!({ "pull_requests": numbers })),
        )
        .await?
        .ok_or_else(|| eyre!("Creating a Pull Request stack failed"))
    }

    async fn add_to_pull_request_stack(
        &self,
        stack_number: u64,
        numbers: &[u64],
    ) -> Result<()> {
        self.stacks_api_request::<serde_json::Value, _>(
            http::Method::POST,
            &format!("stacks/{stack_number}/add"),
            Some(&serde_json::json!({ "pull_requests": numbers })),
        )
        .await?;
        Ok(())
    }

    async fn unstack_pull_request_stack(
        &self,
        stack_number: u64,
    ) -> Result<()> {
        self.stacks_api_request::<serde_json::Value, _>(
            http::Method::POST,
            &format!("stacks/{stack_number}/unstack"),
            None::<&()>,
        )
        .await?;
        Ok(())
    }

    async fn merge_pull_request_async(
        &self,
        number: u64,
        head_oid: git2::Oid,
        merge_method: MergeMethod,
        commit_title: &str,
        commit_message: &str,
    ) -> Result<AsyncMergeStatus> {
        self.stacks_api_request(
            http::Method::PUT,
            &format!("pulls/{number}/merge-async"),
            Some(&serde_json::json!({
                "sha": head_oid.to_string(),
                "merge_method": merge_method.to_string(),
                "commit_title": commit_title,
                "commit_message": commit_message,
            })),
        )
        .await?
        .ok_or_else(|| {
            eyre!("Requesting merge of Pull Request #{number} failed")
        })
    }

    async fn get_async_merge_status(
        &self,
        number: u64,
        uuid: &str,
    ) -> Result<AsyncMergeStatus> {
        self.stacks_api_request(
            http::Method::GET,
            &format!("pulls/{number}/merge-async/{uuid}"),
            None::<&()>,
        )
        .await?
        .ok_or_else(|| {
            eyre!("Getting merge status of Pull Request #{number} failed")
        })
    }
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_normalize_body() {
        // As edited in GitHub's web interface
        let body = "First paragraph.\r\n\r\n- one\r\n- two\r\n";
        assert_eq!(normalize_body(body), "First paragraph.\n\n- one\n- two");

        // The same as the body of the local commit message
        let message =
            CommitMessage::parse("Title\n\nFirst paragraph.\n\n- one\n- two\n");
        assert_eq!(normalize_body(body), message.body());
    }
}
