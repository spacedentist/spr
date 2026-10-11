//! The interface to forges (GitHub, and later others): the operations spr
//! needs from a forge, and the forge-independent types they work with.
//!
//! Commands work with any implementation of [`Forge`]; `main` picks the
//! implementation from the kind of forge of the configured target.

use std::collections::HashMap;

use color_eyre::eyre::{Result, bail};
use serde::Deserialize;

use crate::{
    config::{Config, MergeMethod},
    git::{Git, PreparedCommit},
    git_remote::GitRemote,
    github::GitHubBranch,
    message::CommitMessage,
};

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

/// The review status of a Pull Request according to the latest reviews of
/// its reviewers, for when the forge has no review decision (e.g. GitHub,
/// if the repository doesn't require reviews): rejected if anybody
/// requested changes, approved if anybody approved, otherwise none.
pub fn review_status_from_reviews<'a>(
    reviews: impl IntoIterator<Item = &'a ReviewStatus>,
) -> Option<ReviewStatus> {
    let mut status = None;
    for review in reviews {
        match review {
            ReviewStatus::Rejected => return Some(ReviewStatus::Rejected),
            ReviewStatus::Approved => status = Some(ReviewStatus::Approved),
            ReviewStatus::Requested => {}
        }
    }
    status
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

/// A user on the forge
#[derive(Debug, Clone)]
pub struct User {
    pub login: String,
    pub name: Option<String>,
}

/// A team on the forge, which can be asked for reviews
#[derive(Debug, Clone)]
pub struct Team {
    pub slug: String,
}

/// One of the user's open Pull Requests, as `spr list` shows it
#[derive(Debug, Clone)]
pub struct OpenPullRequest {
    pub title: String,
    pub url: String,
    pub review_status: Option<ReviewStatus>,
}

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

/// The operations spr needs from a forge, for the repository of the
/// configured target
///
/// Implementations are cheap to clone (so that requests can run
/// concurrently). spr runs on a single thread, so the futures don't need to
/// be `Send`.
#[allow(async_fn_in_trait)]
pub trait Forge: Clone + 'static {
    fn config(&self) -> &Config;

    fn git(&self) -> &Git;

    /// The repository on the forge, for fetching and pushing
    fn remote(&self) -> &GitRemote;

    /// The local commits on top of the target branch (fetched from the
    /// forge first)
    fn get_prepared_commits(&self) -> Result<Vec<PreparedCommit>> {
        let master_oid = self
            .remote()
            .fetch_branch(self.config().master_ref.branch_name())?;
        self.git().get_prepared_commits(self.config(), master_oid)
    }

    /// The user with the given login name, or `None` if there's none
    async fn get_user(&self, login: &str) -> Result<Option<User>>;

    /// The team with the given name (in the repository's organisation), or
    /// `None` if there's none
    async fn get_team(&self, slug: &str) -> Result<Option<Team>>;

    /// A Pull Request, with its message as for the local commit, and its
    /// base and head branches fetched. Takes `self` by value (clone it), so
    /// that the future can be spawned.
    async fn get_pull_request(self, number: u64) -> Result<PullRequest>;

    async fn get_pull_request_refs(
        &self,
        number: u64,
    ) -> Result<PullRequestRefs>;

    async fn get_pull_request_mergeability(
        &self,
        number: u64,
    ) -> Result<PullRequestMergeability>;

    /// The authenticated user's open Pull Requests in the repository
    async fn list_open_pull_requests(&self) -> Result<Vec<OpenPullRequest>>;

    /// The numbers of the open Pull Requests whose base is the given branch
    async fn list_pull_requests_with_base(
        &self,
        branch: &GitHubBranch,
    ) -> Result<Vec<u64>>;

    /// Create a Pull Request, returning its number
    async fn create_pull_request(
        &self,
        message: &CommitMessage,
        base_ref_name: String,
        head_ref_name: String,
        draft: bool,
    ) -> Result<u64>;

    async fn update_pull_request(
        &self,
        number: u64,
        updates: PullRequestUpdate,
    ) -> Result<()>;

    async fn request_reviewers(
        &self,
        number: u64,
        reviewers: PullRequestRequestReviewers,
    ) -> Result<()>;

    /// Merge a Pull Request whose head is `head_oid`, with the given commit
    /// title and message. Returns the resulting commit, if the forge tells.
    async fn merge_pull_request(
        &self,
        number: u64,
        head_oid: git2::Oid,
        merge_method: MergeMethod,
        commit_title: &str,
        commit_message: &str,
    ) -> Result<Option<git2::Oid>>;

    /// Change the base of all open Pull Requests that are currently based on
    /// `from_branch` to `to_branch`. Returns the numbers of the Pull Requests
    /// that were changed.
    ///
    /// This must be done before deleting a branch that other Pull Requests
    /// are based on, because forges close Pull Requests whose base branch
    /// gets deleted (or base them on another branch than spr would).
    async fn retarget_pull_requests(
        &self,
        from_branch: &GitHubBranch,
        to_branch: &GitHubBranch,
    ) -> Result<Vec<u64>> {
        let mut numbers = Vec::new();
        for number in self.list_pull_requests_with_base(from_branch).await? {
            let result = self
                .update_pull_request(
                    number,
                    PullRequestUpdate {
                        base: Some(to_branch.branch_name().to_string()),
                        ..Default::default()
                    },
                )
                .await;

            if let Err(error) = result {
                // The forge may change the base of Pull Requests whose base
                // branch gets deleted after merging itself (GitHub does, if
                // the repository is set up to delete branches after
                // merging), so the Pull Request may already have been
                // changed in the meantime. Then the forge refuses our update.
                let current_base = self
                    .get_pull_request_refs(number)
                    .await
                    .map(|refs| refs.base);
                if current_base.ok().as_deref() != Some(to_branch.branch_name())
                {
                    return Err(error.wrap_err(format!(
                        "Changing the base of Pull Request #{number} failed"
                    )));
                }
            }

            numbers.push(number);
        }

        Ok(numbers)
    }

    // Stacks of Pull Requests on the forge (GitHub's stacked pull requests
    // feature). Forges without stacks have none: finding one gives `None`,
    // and everything else fails.

    /// Whether the forge supports stacks of Pull Requests
    fn supports_stacks(&self) -> bool {
        false
    }

    /// The open stack that a Pull Request belongs to
    async fn find_pull_request_stack(
        &self,
        _number: u64,
    ) -> Result<Option<PullRequestStack>> {
        Ok(None)
    }

    /// Create a stack from the given Pull Requests, bottom to top
    async fn create_pull_request_stack(
        &self,
        _numbers: &[u64],
    ) -> Result<PullRequestStack> {
        bail!("This forge doesn't support stacks of Pull Requests")
    }

    /// Add the given Pull Requests on top of a stack
    async fn add_to_pull_request_stack(
        &self,
        _stack_number: u64,
        _numbers: &[u64],
    ) -> Result<()> {
        bail!("This forge doesn't support stacks of Pull Requests")
    }

    /// Remove all Pull Requests that are not merged yet from a stack, which
    /// dissolves it
    async fn unstack_pull_request_stack(
        &self,
        _stack_number: u64,
    ) -> Result<()> {
        bail!("This forge doesn't support stacks of Pull Requests")
    }

    /// Request merging a Pull Request asynchronously. This is required for
    /// Pull Requests in a stack on GitHub, and merges all Pull Requests below
    /// it in the stack, too.
    async fn merge_pull_request_async(
        &self,
        _number: u64,
        _head_oid: git2::Oid,
        _merge_method: MergeMethod,
        _commit_title: &str,
        _commit_message: &str,
    ) -> Result<AsyncMergeStatus> {
        bail!("This forge doesn't support stacks of Pull Requests")
    }

    /// The status of an asynchronous merge request
    async fn get_async_merge_status(
        &self,
        _number: u64,
        _uuid: &str,
    ) -> Result<AsyncMergeStatus> {
        bail!("This forge doesn't support stacks of Pull Requests")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_review_status_from_reviews() {
        use ReviewStatus::*;
        assert_eq!(review_status_from_reviews(&[]), None);
        assert_eq!(review_status_from_reviews(&[Requested]), None);
        assert_eq!(
            review_status_from_reviews(&[Requested, Approved]),
            Some(Approved)
        );
        assert_eq!(
            review_status_from_reviews(&[Approved, Rejected, Approved]),
            Some(Rejected)
        );
    }
}
