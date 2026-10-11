//! The automated live test scenarios, and what they need to run spr and
//! check the results

mod basic;
mod commands;
mod land;
mod remote_changes;
mod stacks;
mod targets;

use std::time::Duration;

use color_eyre::eyre::{Result, bail, eyre};

use crate::{
    api::{Api, PullRequest, wait_for},
    env::TestEnv,
    log::{short, step, title},
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
    vec![
        basic::SCENARIO,
        basic::OLD_CONFIG,
        land::SQUASH,
        land::MERGE,
        remote_changes::REMOTE_CHANGES,
        remote_changes::PULL,
        remote_changes::PULL_CONFLICT,
        remote_changes::UPDATE_BRANCH,
        stacks::BASE_BRANCHES,
        stacks::CHAIN,
        stacks::GITHUB_STACK,
        targets::TARGET,
        targets::CHERRY_PICK,
        commands::PATCH,
        commands::AMEND,
        commands::DRY_RUN,
    ]
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
        let commit = self.rev_parse("HEAD")?;
        step(format!("commit {} {:?}", short(&commit), title(message)));
        Ok(commit)
    }

    /// Amend the HEAD commit with the files. Returns the new commit.
    pub fn amend(&self, files: &[(&str, &str)]) -> Result<String> {
        let before = self.rev_parse("HEAD")?;
        self.write(files)?;
        self.env.git(&["add", "--all"])?;
        self.env
            .git(&["commit", "--quiet", "--amend", "--no-edit"])?;
        let commit = self.rev_parse("HEAD")?;
        step(format!("amend {} → {}", short(&before), short(&commit)));
        Ok(commit)
    }

    /// Somebody else changes a file on a branch on GitHub (like in its web
    /// interface). Returns the new commit.
    pub fn somebody_edits(
        &self,
        branch: &str,
        path: &str,
        content: &str,
    ) -> Result<String> {
        let commit = self.api.put_file(
            branch,
            path,
            content,
            "Change by somebody else",
        )?;
        step(format!(
            "somebody else changes {path} on {branch}: commit {}",
            short(&commit)
        ));
        Ok(commit)
    }

    /// Somebody presses "Update branch" on the Pull Request on GitHub.
    /// Returns its new head, once GitHub has updated it.
    pub fn update_branch(&self, number: u64) -> Result<String> {
        let old_head = self.api.pull_request(number)?.head_sha;
        self.api.update_branch(number)?;
        let new_head = wait_for(
            &format!("Pull Request #{number} to get a new head"),
            GITHUB_DELAY,
            || {
                let head = self.api.pull_request(number)?.head_sha;
                Ok((head != old_head).then_some(head))
            },
        )?;
        step(format!(
            "somebody presses \"Update branch\" on PR #{number}: head {} → {}",
            short(&old_head),
            short(&new_head)
        ));
        Ok(new_head)
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
        let number = url
            .rsplit('/')
            .next()
            .and_then(|number| number.parse().ok())
            .ok_or_else(|| eyre!("Unexpected Pull-request trailer: {url}"))?;
        step(format!(
            "commit {} has PR #{number}",
            short(&self.rev_parse(rev)?)
        ));
        Ok(number)
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
        let output = self.env.run(self.env.git_command(&[
            "merge-base",
            "--is-ancestor",
            ancestor,
            rev,
        ]))?;
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
        let local = self.rev_parse(rev)?;
        check(
            format!(
                "PR #{number} (head {}) has the tree of the local commit {}",
                short(&head),
                short(&local)
            ),
            self.tree(&head)? == self.tree(&local)?,
        )?;
        Ok((head, pr))
    }

    /// The expected head spr recorded for the commit's Pull Request
    /// (`refs/spr/<Spr-Id>/head`), if any
    pub fn expected_head(&self, rev: &str) -> Result<Option<String>> {
        let Some(id) = self.trailer(rev, "Spr-Id")? else {
            return Ok(None);
        };
        Ok(self
            .env
            .git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/spr/{id}/head"),
            ])
            .ok())
    }

    /// Whether a branch exists on GitHub
    pub fn branch_exists(&self, branch: &str) -> Result<bool> {
        Ok(self.api.branch_sha(branch)?.is_some())
    }
}

/// Check that something holds, described positively (e.g. "PR #12 is
/// open"): print it, or fail with it
pub fn check(description: impl Into<String>, condition: bool) -> Result<()> {
    let description = description.into();
    if condition {
        step(format!("✓ {description}"));
        Ok(())
    } else {
        bail!("Expected: {description}")
    }
}

/// Check that a value is the expected one, described positively: print the
/// description, or fail with it and the actual value
pub fn check_eq<T: PartialEq + std::fmt::Debug>(
    description: impl Into<String>,
    actual: T,
    expected: T,
) -> Result<()> {
    let description = description.into();
    if actual == expected {
        step(format!("✓ {description}"));
        Ok(())
    } else {
        bail!(
            "Expected: {description}\nbut got {actual:?} instead of \
             {expected:?}"
        )
    }
}

/// Check that the output of a command contains the text
pub fn check_output(output: &str, text: &str) -> Result<()> {
    if output.contains(text) {
        step(format!("✓ the output says {text:?}"));
        Ok(())
    } else {
        bail!("Expected: the output says {text:?}\nThe output was:\n{output}")
    }
}
