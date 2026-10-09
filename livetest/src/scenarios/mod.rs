//! The automated live test scenarios, and what they need to run spr and
//! check the results

mod basic;
mod land;

use std::time::Duration;

use color_eyre::eyre::{Result, bail, eyre};

use crate::{
    api::{Api, PullRequest, wait_for},
    env::TestEnv,
    run::Run,
};

/// A live test
pub struct Scenario {
    pub name: &'static str,
    pub description: &'static str,
    pub run: fn(&Ctx) -> Result<()>,
}

/// All scenarios, in the order they run
pub fn all() -> Vec<Scenario> {
    vec![basic::SCENARIO, land::SQUASH, land::MERGE]
}

/// How long to wait for GitHub to reflect a change
const GITHUB_DELAY: Duration = Duration::from_secs(30);

/// What a scenario works with: a controlled environment with a clone of the
/// test repository on a local branch based on the run's target branch, set
/// up for spr, and the GitHub API
pub struct Ctx<'a> {
    pub env: &'a TestEnv,
    pub api: &'a Api,
    pub run: &'a Run,
}

impl Ctx<'_> {
    /// Write the files (path, content) into the working tree
    pub fn write(&self, files: &[(&str, &str)]) -> Result<()> {
        for (path, content) in files {
            let path = self.env.repo_dir.join(path);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, content)?;
        }
        Ok(())
    }

    /// Commit the files with the message. Returns the new commit.
    pub fn commit(
        &self,
        files: &[(&str, &str)],
        message: &str,
    ) -> Result<String> {
        self.write(files)?;
        self.env.git(&["add", "--all"])?;
        self.env.git(&["commit", "--quiet", "-m", message])?;
        self.env.git(&["rev-parse", "HEAD"])
    }

    /// Amend the HEAD commit with the files. Returns the new commit.
    pub fn amend(&self, files: &[(&str, &str)]) -> Result<String> {
        self.write(files)?;
        self.env.git(&["add", "--all"])?;
        self.env
            .git(&["commit", "--quiet", "--amend", "--no-edit"])?;
        self.env.git(&["rev-parse", "HEAD"])
    }

    pub fn rev_parse(&self, rev: &str) -> Result<String> {
        self.env.git(&["rev-parse", rev])
    }

    /// The commit message of a commit
    pub fn message(&self, rev: &str) -> Result<String> {
        self.env.git(&["log", "-1", "--format=%B", rev])
    }

    /// The value of a trailer of a commit, if it has one
    pub fn trailer(&self, rev: &str, key: &str) -> Result<Option<String>> {
        let message = self.message(rev)?;
        let prefix = format!("{key}: ");
        Ok(message
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .map(String::from))
    }

    /// The number of the Pull Request of a commit (`Pull-request` trailer)
    pub fn pr_number(&self, rev: &str) -> Result<u64> {
        let url = self
            .trailer(rev, "Pull-request")?
            .ok_or_else(|| eyre!("Commit {rev} has no Pull-request trailer"))?;
        url.rsplit('/')
            .next()
            .and_then(|number| number.parse().ok())
            .ok_or_else(|| eyre!("Unexpected Pull-request trailer: {url}"))
    }

    /// The Pull Request, once GitHub reports `head` as its head (it takes a
    /// moment to catch up after a push)
    pub fn pull_request_with_head(
        &self,
        number: u64,
        head: &str,
    ) -> Result<PullRequest> {
        wait_for(
            &format!("Pull Request #{number} to have head {head}"),
            GITHUB_DELAY,
            || {
                let pr = self.api.pull_request(number)?;
                Ok((pr.head_sha == head).then_some(pr))
            },
        )
    }

    /// The commit a branch on GitHub points to (fetched into the clone)
    pub fn fetch(&self, branch: &str) -> Result<String> {
        self.env.git(&["fetch", "--quiet", "origin", branch])?;
        self.env.git(&["rev-parse", "FETCH_HEAD"])
    }

    /// The tree of a commit
    pub fn tree(&self, rev: &str) -> Result<String> {
        self.env.git(&["rev-parse", &format!("{rev}^{{tree}}")])
    }

    pub fn is_ancestor(&self, ancestor: &str, rev: &str) -> Result<bool> {
        let output = self.env.run({
            let mut command = self.env.git_command(&[
                "merge-base",
                "--is-ancestor",
                ancestor,
                rev,
            ]);
            command.current_dir(&self.env.repo_dir);
            command
        })?;
        Ok(output.success)
    }

    /// Check that the Pull Request's head on GitHub has the tree of the local
    /// commit. Returns the head, and the Pull Request.
    pub fn check_pr_matches(
        &self,
        number: u64,
        rev: &str,
    ) -> Result<(String, PullRequest)> {
        let pr = self.api.pull_request(number)?;
        let head = self.fetch(&pr.head_ref)?;
        let pr = self.pull_request_with_head(number, &head)?;
        if self.tree(&head)? != self.tree(rev)? {
            bail!(
                "Pull Request #{number} (head {head}) doesn't have the tree \
                 of the local commit {rev}"
            );
        }
        Ok((head, pr))
    }

    /// Whether a branch exists on GitHub
    pub fn branch_exists(&self, branch: &str) -> Result<bool> {
        Ok(self.api.branch_sha(branch)?.is_some())
    }
}

/// Fail with the message unless the condition holds
pub fn check(condition: bool, message: impl FnOnce() -> String) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(eyre!(message()))
    }
}
