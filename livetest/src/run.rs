//! A test run in the test repository: its branches, and cleaning them up.
//!
//! Everything a run creates is under its branch prefix
//! (`spr/livetest/<run-id>/`) or its target branch
//! (`livetest/<run-id>/main`, used instead of the default branch). Cleanup
//! only ever touches those, never anything else in the repository.

use std::collections::BTreeSet;

use color_eyre::eyre::{Result, bail};

use crate::{
    api::Api,
    util::{
        self, PR_BRANCH_ROOT, TARGET_BRANCH_ROOT, run_id_of_branch, run_prefix,
        target_branch,
    },
};

/// The file that marks a repository as meant for live tests
pub const MARKER: &str = ".spr-livetest";

pub struct Run {
    pub id: String,
    /// Prefix of all Pull Request branches of the run
    pub prefix: String,
    /// The branch the run's Pull Requests target
    pub target_branch: String,
    pub default_branch: String,
}

impl Run {
    /// Start a test run: check the marker, create the target branch
    pub fn start(api: &Api) -> Result<Self> {
        let default_branch = check_marker(api)?;
        let id = new_run_id();
        let run = Run {
            prefix: run_prefix(&id),
            target_branch: target_branch(&id),
            id,
            default_branch,
        };
        let Some(sha) = api.branch_sha(&run.default_branch)? else {
            bail!("The default branch {} doesn't exist", run.default_branch);
        };
        api.create_branch(&run.target_branch, &sha)?;
        Ok(run)
    }

    /// The branch prefix for one test of the run
    pub fn test_prefix(&self, test: &str) -> String {
        format!("{}{test}/", self.prefix)
    }

    /// Remove everything the run created
    pub fn cleanup(&self, api: &Api) -> Result<()> {
        cleanup_runs(api, &BTreeSet::from([self.id.clone()]))
    }
}

/// Check that the repository has the marker on its default branch. Returns
/// the default branch.
pub fn check_marker(api: &Api) -> Result<String> {
    let default_branch = api.default_branch()?;
    if !api.file_exists(&default_branch, MARKER)? {
        bail!(
            "The repository {owner}/{repo} has no file {MARKER} on its \
             default branch ({default_branch}).\n\n\
             The live tests create and close Pull Requests and branches in \
             the repository they run in. To make sure they only run in a \
             repository meant for that, add an empty file {MARKER} to its \
             default branch, e.g. in GitHub's web interface (Add file → \
             Create new file), or in a clone:\n\n  \
             touch {MARKER} && git add {MARKER} && \
             git commit -m 'Allow spr live tests' && git push\n\n\
             Use a dedicated test repository, not a real project.",
            owner = api.owner,
            repo = api.repo,
        );
    }
    Ok(default_branch)
}

fn new_run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    util::run_id(now.as_secs(), now.subsec_nanos() ^ std::process::id())
}

/// The IDs of all runs that left branches behind
pub fn leftover_runs(api: &Api) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for root in [PR_BRANCH_ROOT, TARGET_BRANCH_ROOT] {
        for branch in api.branches_with_prefix(root)? {
            if let Some(id) = run_id_of_branch(&branch) {
                ids.insert(id.to_string());
            }
        }
    }
    Ok(ids)
}

/// Remove everything the given runs created: dissolve stacks, close Pull
/// Requests, delete branches
pub fn cleanup_runs(api: &Api, ids: &BTreeSet<String>) -> Result<()> {
    let belongs_to_run = |branch: &str| {
        run_id_of_branch(branch).is_some_and(|id| ids.contains(id))
    };

    for pr in api.open_pull_requests()? {
        if !belongs_to_run(&pr.head_ref) && !belongs_to_run(&pr.base_ref) {
            continue;
        }
        // GitHub doesn't allow changing stacked Pull Requests
        if let Some(stack) = api.stack_of_pull_request(pr.number)?
            && let Some(number) = stack["number"].as_u64()
        {
            api.unstack(number)?;
        }
        api.close_pull_request(pr.number)?;
    }

    for id in ids {
        for prefix in [run_prefix(id), format!("{TARGET_BRANCH_ROOT}{id}/")] {
            for branch in api.branches_with_prefix(&prefix)? {
                if belongs_to_run(&branch) {
                    api.delete_branch(&branch)?;
                }
            }
        }
    }
    Ok(())
}
