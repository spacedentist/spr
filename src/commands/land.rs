use async_compat::CompatExt;
use indoc::formatdoc;
use std::io::Write;

use crate::{
    error::{Error, Result, ResultExt},
    github::{PullRequestState, ReviewStatus},
    message::{build_github_body_for_merging, MessageSection},
    output::{output, write_commit_title},
};

#[derive(Debug, clap::Parser)]
pub struct LandOptions {}

pub async fn land(
    _opts: LandOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    let mut prepared_commits = git.get_prepared_commits(config).await?;

    let prepared_commit = match prepared_commits.last_mut() {
        Some(c) => c,
        None => {
            output("👋", "Branch is empty - nothing to do. Good bye!")?;
            return Ok(());
        }
    };

    write_commit_title(&prepared_commit)?;

    let pull_request_number =
        if let Some(number) = prepared_commit.pull_request_number {
            output("#️⃣ ", &format!("Pull Request #{}", number))?;
            number
        } else {
            return Err(Error::new(
                "This commit does not refer to a Pull Request.",
            ));
        };

    let stack_on_number = prepared_commit
        .message
        .get(&MessageSection::StackedOn)
        .map(|text| config.parse_pull_request_field(text))
        .flatten();

    // Load Pull Request information
    let pull_request = gh.get_pull_request(pull_request_number, git);
    let stacked_on_pull_request = if let Some(number) = stack_on_number {
        Some(gh.get_pull_request(number, git).await??)
    } else {
        None
    };
    let pull_request = pull_request.await??;

    if pull_request.state != PullRequestState::Open {
        return Err(Error::new(formatdoc!(
            "This Pull Request is already closed!",
        )));
    }

    if let Some(stacked_on_pull_request) = stacked_on_pull_request {
        if stacked_on_pull_request.state == PullRequestState::Open {
            return Err(Error::new(formatdoc!(
                "This Pull Request is stacked on Pull Request #{}, \
                 which is still open.",
                stacked_on_pull_request.number
            )));
        }
    }

    if pull_request.review_status != Some(ReviewStatus::Approved) {
        return Err(Error::new(
            "This Pull Request has not been approved on GitHub.",
        ));
    }

    if pull_request.mergeable.is_none() {
        return Err(Error::new(formatdoc!(
            "GitHub has not completed the mergeability check for this \
             Pull Requets. Please try again in a few seconds!"
        )));
    }
    if pull_request.mergeable == Some(false) {
        return Err(Error::new(formatdoc!(
            "GitHub concluded the Pull Request is not mergeable at this \
             point. Please rebase your changes and update them on GitHub \
             using 'spr diff'!"
        )));
    }
    let github_merge_commit = if let Some(c) = pull_request.merge_commit {
        c
    } else {
        return Err(Error::new(formatdoc!(
            "OOPS! GitHub says the Pull Request is mergeable but did not \
             provide a merge_commit_sha field. If retrying in a few \
             seconds does not help, then this is a bug in the spr tool."
        )));
    };

    output("🛫", "Getting started...")?;

    // Fetch current master and the merge commit from GitHub.
    let git_fetch = async_process::Command::new("git")
        .arg("fetch")
        .arg("--no-write-fetch-head")
        .arg("--")
        .arg(&config.remote_name)
        .arg(&config.master_ref)
        .arg(format!("{}", github_merge_commit))
        .stdout(async_process::Stdio::null())
        .stderr(async_process::Stdio::piped())
        .output()
        .await?;

    if !git_fetch.status.success() {
        console::Term::stderr().write_all(&git_fetch.stderr)?;
        return Err(Error::new("git fetch failed"));
    }

    let current_master =
        git.resolve_reference(&config.remote_master_ref).await?;

    let index = git.cherrypick(prepared_commit.oid, current_master).await?;

    if index.has_conflicts() {
        return Err(Error::new(formatdoc!(
            "This commit has local changes, and it cannot be applied on top
             of the '{}' branch. Please rebase and update the Pull Request
             on GitHub using 'spr diff'.",
            config.master_branch
        )));
    }

    // This is the tree we are getting from cherrypicking the local commit
    // on the selected base (master or stacked-on Pull Request).
    let our_tree_oid = git.write_index(index).await?;

    let github_tree_oid =
        git.get_tree_oid_for_commit(github_merge_commit).await?;

    if our_tree_oid != github_tree_oid {
        return Err(Error::new(formatdoc!(
            "This commit has local changes. Please update the Pull Request
             on GitHub using 'spr diff'.",
        )));
    }

    let merge = octocrab::instance()
        .pulls(&config.owner, &config.repo)
        .merge(pull_request_number)
        .method(octocrab::params::pulls::MergeMethod::Squash)
        .title(pull_request.title)
        .message(build_github_body_for_merging(&pull_request.sections))
        .sha(format!("{}", pull_request.head_oid))
        .send()
        .compat()
        .await?;

    if merge.merged {
        output("🛬", "Landed!")?;
    } else {
        output("❌", "GitHub Pull Request merge failed")?;

        return Err(merge.message.map(Error::new).unwrap_or(Error::empty()));
    }

    // Rebase us on top of the now-landed commit
    if let Some(sha) = merge.sha {
        // Try this up to three times, because fetching the very moment after
        // the merge might still not find the new commit.
        for i in 0..3 {
            // Fetch current master and the merge commit from GitHub.
            let git_fetch = async_process::Command::new("git")
                .arg("fetch")
                .arg("--no-write-fetch-head")
                .arg("--")
                .arg(&config.remote_name)
                .arg(&config.master_ref)
                .arg(&sha)
                .stdout(async_process::Stdio::null())
                .stderr(async_process::Stdio::piped())
                .output()
                .await?;
            if git_fetch.status.success() {
                break;
            } else if i == 2 {
                console::Term::stderr().write_all(&git_fetch.stderr)?;
                return Err(Error::new("git fetch failed"));
            }
        }
        drop(prepared_commit);
        git.rebase_commits(
            &mut prepared_commits[..],
            git2::Oid::from_str(&sha)?,
        )
        .await
        .context(format!(
            "The automatic rebase failed - please rebase manually!"
        ))?;
    }

    Ok(())
}
