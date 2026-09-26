use color_eyre::eyre::{Error, Result, WrapErr as _, eyre};
use graphql_client::{GraphQLQuery, Response};
use serde::Deserialize;

use crate::{
    git::PreparedCommit, git_remote::GitRemote, message::CommitMessage,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct GitHub {
    config: crate::config::Config,
    git: crate::git::Git,
    git_remote: crate::git_remote::GitRemote,
}

#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub state: PullRequestState,
    pub title: String,
    pub body: Option<String>,
    pub message: CommitMessage,
    pub base: GitHubBranch,
    pub head: GitHubBranch,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<PullRequestState>,
}

impl PullRequestUpdate {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.body.is_none()
            && self.base.is_none()
            && self.state.is_none()
    }

    pub fn update_message(
        &mut self,
        pull_request: &PullRequest,
        message: &CommitMessage,
    ) {
        let title = message.title();
        if !title.is_empty() && title != pull_request.title {
            self.title = Some(title.to_string());
        }

        let body = message.to_github_body();
        if pull_request.body.as_ref() != Some(&body) {
            self.body = Some(body);
        }
    }
}

#[derive(serde::Serialize, Default, Debug)]
pub struct PullRequestRequestReviewers {
    pub reviewers: Vec<String>,
    pub team_reviewers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
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

/// The version of the GitHub REST API that the stacked pull requests APIs
/// (stacks and asynchronous merging) require.
const STACKS_API_VERSION: &str = "2026-03-10";

/// A stack of Pull Requests on GitHub (stacked pull requests feature)
#[derive(Debug, Deserialize)]
pub struct PullRequestStack {
    pub number: u64,
    pub open: bool,
    /// The Pull Requests in the stack, from bottom to top
    pub pull_requests: Vec<PullRequestStackEntry>,
}

#[derive(Debug, Deserialize)]
pub struct PullRequestStackEntry {
    pub number: u64,
    pub state: String,
}

impl PullRequestStack {
    /// The numbers of the Pull Requests in this stack that are still open,
    /// from bottom to top. (Merged Pull Requests remain part of a stack.)
    pub fn open_pull_requests(&self) -> Vec<u64> {
        self.pull_requests
            .iter()
            .filter(|pr| pr.state == "open")
            .map(|pr| pr.number)
            .collect()
    }
}

/// Status of an asynchronous merge request
#[derive(Debug, Deserialize)]
pub struct AsyncMergeStatus {
    /// One of `pending`, `merged`, `enqueued` and `failed`
    pub status: String,
    pub details: AsyncMergeDetails,
}

#[derive(Debug, Deserialize)]
pub struct AsyncMergeDetails {
    pub message: Option<String>,
    pub uuid: Option<String>,
    /// The resulting commit, once merged
    pub sha: Option<String>,
}

/// The branches and state of a Pull Request, as far as needed for stacking
#[derive(Debug, Clone)]
pub struct PullRequestRefs {
    pub number: u64,
    pub open: bool,
    pub head: String,
    pub base: String,
}

#[derive(Debug, Clone)]
pub struct PullRequestMergeability {
    pub base: GitHubBranch,
    pub head_oid: git2::Oid,
    pub mergeable: Option<bool>,
    pub merge_commit: Option<git2::Oid>,
}

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

impl GitHub {
    pub fn new(
        config: crate::config::Config,
        git: crate::git::Git,
        auth_token: String,
    ) -> Self {
        let git_remote = GitRemote::new(
            git.repo().clone(),
            format!("https://github.com/{}/{}.git", config.owner, config.repo),
            auth_token,
        );
        Self {
            config,
            git,
            git_remote,
        }
    }

    pub fn remote(&self) -> &GitRemote {
        &self.git_remote
    }

    pub fn get_prepared_commits(&self) -> Result<Vec<PreparedCommit>> {
        let master_oid = self
            .git_remote
            .fetch_branch(self.config.master_ref.branch_name())?;
        self.git.get_prepared_commits(&self.config, master_oid)
    }

    pub async fn get_github_user(login: String) -> Result<UserWithName> {
        octocrab::instance()
            .get::<UserWithName, _, _>(format!("/users/{}", login), None::<&()>)
            .await
            .map_err(Error::from)
    }

    pub async fn get_github_team(
        owner: String,
        team: String,
    ) -> Result<octocrab::models::teams::Team> {
        octocrab::instance()
            .teams(owner)
            .get(team)
            .await
            .map_err(Error::from)
    }

    pub async fn get_pull_request(self, number: u64) -> Result<PullRequest> {
        let GitHub {
            config, git_remote, ..
        } = self;

        let variables = pull_request_query::Variables {
            name: config.repo.clone(),
            owner: config.owner.clone(),
            number: number as i64,
        };
        let request_body = PullRequestQuery::build_query(variables);
        let response_body: Response<pull_request_query::ResponseData> =
            octocrab::instance()
                .post("/graphql", Some(&request_body))
                .await?;

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

        let base = config.new_github_branch_from_ref(&pr.base_ref_name)?;
        let head = config.new_github_branch_from_ref(&pr.head_ref_name)?;

        let branch_names: Vec<_> =
            [&base, &head].iter().map(|&b| b.branch_name()).collect();

        let [base_oid, head_oid] =
            git_remote.fetch_from_remote(&branch_names, &[])?[0..2]
        else {
            unreachable!();
        };

        let base_oid = base_oid.ok_or_else(|| {
            eyre!("{} not found on GitHub", &base.ref_on_github)
        })?;
        let head_oid = head_oid.ok_or_else(|| {
            eyre!("{} not found on GitHub", &head.ref_on_github)
        })?;

        let title = pr.title.trim().to_string();
        let title = if title.is_empty() {
            String::from("(untitled)")
        } else {
            title
        };

        let mut message = CommitMessage::new(title, pr.body.clone());

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
            _ => None,
        };

        let requested_reviewers: Vec<String> = pr.review_requests
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
            .collect::<HashSet<String>>() // de-duplicate
            .into_iter()
            .collect();

        let reviewers_str =
            requested_reviewers.iter().fold(String::new(), |out, slug| {
                if out.is_empty() {
                    slug.to_string()
                } else {
                    format!("{}, {}", out, slug)
                }
            });
        if !reviewers_str.is_empty() {
            message.set_trailer("Reviewers".to_string(), reviewers_str);
        }

        if review_status == Some(ReviewStatus::Approved) {
            let reviewed_by = reviewers
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
                });
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
            title: pr.title,
            body: Some(pr.body),
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

    pub async fn create_pull_request(
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

        let number = octocrab::instance()
            .pulls(self.config.owner.clone(), self.config.repo.clone())
            .create(title, head_ref_name, base_ref_name)
            .body(message.to_github_body())
            .draft(Some(draft))
            .send()
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
                    "/repos/{}/{}/pulls/{}",
                    self.config.owner, self.config.repo, number
                ),
                Some(&updates),
            )
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
                    "/repos/{}/{}/pulls/{}/requested_reviewers",
                    self.config.owner, self.config.repo, number
                ),
                Some(&reviewers),
            )
            .await?;

        Ok(())
    }

    /// Change the base of all open Pull Requests that are currently based on
    /// `from_branch` to `to_branch`. Returns the numbers of the Pull Requests
    /// that were changed.
    ///
    /// This must be done before deleting a branch that other Pull Requests
    /// are based on, because GitHub closes Pull Requests whose base branch
    /// gets deleted.
    pub async fn retarget_pull_requests(
        &self,
        from_branch: &GitHubBranch,
        to_branch: &GitHubBranch,
    ) -> Result<Vec<u64>> {
        let pulls = octocrab::instance()
            .pulls(self.config.owner.clone(), self.config.repo.clone())
            .list()
            .state(octocrab::params::State::Open)
            .base(from_branch.branch_name())
            .per_page(100)
            .send()
            .await?;
        let pulls = octocrab::instance().all_pages(pulls).await?;

        let mut numbers = Vec::new();
        for pull in pulls {
            let result = self
                .update_pull_request(
                    pull.number,
                    PullRequestUpdate {
                        base: Some(to_branch.branch_name().to_string()),
                        ..Default::default()
                    },
                )
                .await;

            if let Err(error) = result {
                // GitHub itself changes the base of Pull Requests whose base
                // branch gets deleted after merging (if the repository is set
                // up to delete branches after merging), so the Pull Request
                // may already have been changed in the meantime. Then GitHub
                // refuses our update.
                let current_base = octocrab::instance()
                    .pulls(self.config.owner.clone(), self.config.repo.clone())
                    .get(pull.number)
                    .await
                    .map(|pull| pull.base.ref_field);
                if current_base.ok().as_deref() != Some(to_branch.branch_name())
                {
                    return Err(error.wrap_err(format!(
                        "Changing the base of Pull Request #{} failed",
                        pull.number
                    )));
                }
            }

            numbers.push(pull.number);
        }

        Ok(numbers)
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
        let octocrab = octocrab::instance();
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

    /// Find the open stack on GitHub that a Pull Request belongs to.
    pub async fn find_pull_request_stack(
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

    /// Create a stack on GitHub from the given Pull Requests, bottom to top.
    pub async fn create_pull_request_stack(
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

    /// Add the given Pull Requests on top of a stack on GitHub.
    pub async fn add_to_pull_request_stack(
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

    /// Remove all Pull Requests that are not merged yet from a stack on
    /// GitHub, which dissolves it.
    pub async fn unstack_pull_request_stack(
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

    /// Request merging a Pull Request asynchronously. This is required for
    /// Pull Requests in a stack on GitHub, and merges all Pull Requests below
    /// it in the stack, too.
    pub async fn merge_pull_request_async(
        &self,
        number: u64,
        head_oid: git2::Oid,
        merge_method: crate::config::MergeMethod,
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

    /// Get the status of an asynchronous merge request.
    pub async fn get_async_merge_status(
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

    /// Get the head and base branch names and the state of a Pull Request.
    pub async fn get_pull_request_refs(
        &self,
        number: u64,
    ) -> Result<PullRequestRefs> {
        let pull = octocrab::instance()
            .pulls(self.config.owner.clone(), self.config.repo.clone())
            .get(number)
            .await?;

        Ok(PullRequestRefs {
            number,
            open: pull.state == Some(octocrab::models::IssueState::Open),
            head: pull.head.ref_field,
            base: pull.base.ref_field,
        })
    }

    pub async fn get_pull_request_mergeability(
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
        > = octocrab::instance()
            .post("/graphql", Some(&request_body))
            .await?;

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
            base: self.config.new_github_branch_from_ref(&pr.base_ref_name)?,
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
}

#[derive(Debug, Clone)]
pub struct GitHubBranch {
    ref_on_github: String,
    is_master_branch: bool,
}

impl GitHubBranch {
    pub fn new_from_ref(ghref: &str, master_branch_name: &str) -> Result<Self> {
        let ref_on_github = if ghref.starts_with("refs/heads/") {
            ghref.to_string()
        } else if ghref.starts_with("refs/") {
            return Err(eyre!("Ref '{ghref}' does not refer to a branch"));
        } else {
            format!("refs/heads/{ghref}")
        };

        // The branch name is `ref_on_github` with the `refs/heads/` prefix
        // (length 11) removed
        let branch_name = &ref_on_github[11..];
        let is_master_branch = branch_name == master_branch_name;

        Ok(Self {
            ref_on_github,
            is_master_branch,
        })
    }

    pub fn new_from_branch_name(
        branch_name: &str,
        master_branch_name: &str,
    ) -> Self {
        Self {
            ref_on_github: format!("refs/heads/{branch_name}"),
            is_master_branch: branch_name == master_branch_name,
        }
    }

    pub fn on_github(&self) -> &str {
        &self.ref_on_github
    }

    pub fn is_master_branch(&self) -> bool {
        self.is_master_branch
    }

    pub fn branch_name(&self) -> &str {
        // The branch name is `ref_on_github` with the `refs/heads/` prefix
        // (length 11) removed
        &self.ref_on_github[11..]
    }
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_new_from_ref_with_branch_name() {
        let r = GitHubBranch::new_from_ref("foo", "masterbranch").unwrap();
        assert_eq!(r.on_github(), "refs/heads/foo");
        assert_eq!(r.branch_name(), "foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_new_from_ref_with_master_branch_name() {
        let r =
            GitHubBranch::new_from_ref("masterbranch", "masterbranch").unwrap();
        assert_eq!(r.on_github(), "refs/heads/masterbranch");
        assert_eq!(r.branch_name(), "masterbranch");
        assert!(r.is_master_branch());
    }

    #[test]
    fn test_new_from_ref_with_ref_name() {
        let r = GitHubBranch::new_from_ref("refs/heads/foo", "masterbranch")
            .unwrap();
        assert_eq!(r.on_github(), "refs/heads/foo");
        assert_eq!(r.branch_name(), "foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_new_from_ref_with_master_ref_name() {
        let r = GitHubBranch::new_from_ref(
            "refs/heads/masterbranch",
            "masterbranch",
        )
        .unwrap();
        assert_eq!(r.on_github(), "refs/heads/masterbranch");
        assert_eq!(r.branch_name(), "masterbranch");
        assert!(r.is_master_branch());
    }

    #[test]
    fn test_new_from_branch_name() {
        let r = GitHubBranch::new_from_branch_name("foo", "masterbranch");
        assert_eq!(r.on_github(), "refs/heads/foo");
        assert_eq!(r.branch_name(), "foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_new_from_master_branch_name() {
        let r =
            GitHubBranch::new_from_branch_name("masterbranch", "masterbranch");
        assert_eq!(r.on_github(), "refs/heads/masterbranch");
        assert_eq!(r.branch_name(), "masterbranch");
        assert!(r.is_master_branch());
    }

    #[test]
    fn test_new_from_ref_with_edge_case_ref_name() {
        let r = GitHubBranch::new_from_ref(
            "refs/heads/refs/heads/foo",
            "masterbranch",
        )
        .unwrap();
        assert_eq!(r.on_github(), "refs/heads/refs/heads/foo");
        assert_eq!(r.branch_name(), "refs/heads/foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_new_from_edge_case_branch_name() {
        let r = GitHubBranch::new_from_branch_name(
            "refs/heads/foo",
            "masterbranch",
        );
        assert_eq!(r.on_github(), "refs/heads/refs/heads/foo");
        assert_eq!(r.branch_name(), "refs/heads/foo");
        assert!(!r.is_master_branch());
    }
}
