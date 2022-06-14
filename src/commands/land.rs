/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use async_compat::CompatExt;
use indoc::formatdoc;
use std::{io::Write, time::Duration};

use crate::{
    error::{Error, Result, ResultExt},
    github::{PullRequestState, PullRequestUpdate, ReviewStatus},
    message::build_github_body_for_merging,
    output::{output, write_commit_title},
    utils::run_command,
};

#[derive(Debug, clap::Parser)]
pub struct LandOptions {
    /// Merge a Pull Request that was created or updated with spr diff
    /// --cherry-pick
    #[clap(long)]
    cherry_pick: bool,
}

pub async fn land(
    opts: LandOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    git.check_no_uncommitted_changes()?;
    let mut prepared_commits = git.get_prepared_commits(config)?;

    let based_on_unlanded_commits = prepared_commits.len() > 1;

    if based_on_unlanded_commits && !opts.cherry_pick {
        return Err(Error::new(formatdoc!(
            "Cannot land a commit whose parent is not on {master}. To land \
             this commit, rebase it so that it is a direct child of {master}.
             Alternatively, if you used the `--cherry-pick` option with `spr \
             diff`, then you can pass it to `spr land`, too.",
            master = &config.master_ref.branch_name(),
        )));
    }

    let prepared_commit = match prepared_commits.last_mut() {
        Some(c) => c,
        None => {
            output("👋", "Branch is empty - nothing to do. Good bye!")?;
            return Ok(());
        }
    };

    write_commit_title(prepared_commit)?;

    let pull_request_number =
        if let Some(number) = prepared_commit.pull_request_number {
            output("#️⃣ ", &format!("Pull Request #{}", number))?;
            number
        } else {
            return Err(Error::new(
                "This commit does not refer to a Pull Request.",
            ));
        };

    // Load Pull Request information
    let pull_request = gh.get_pull_request(pull_request_number).await??;

    if pull_request.state != PullRequestState::Open {
        return Err(Error::new(formatdoc!(
            "This Pull Request is already closed!",
        )));
    }

    if config.require_approval
        && pull_request.review_status != Some(ReviewStatus::Approved)
    {
        return Err(Error::new(
            "This Pull Request has not been approved on GitHub.",
        ));
    }

    output("🛫", "Getting started...")?;

    // Fetch current master from GitHub.
    run_command(
        async_process::Command::new("git")
            .arg("fetch")
            .arg("--no-write-fetch-head")
            .arg("--")
            .arg(&config.remote_name)
            .arg(config.master_ref.on_github()),
    )
    .await
    .reword("git fetch failed".to_string())?;

    let current_master = git.resolve_reference(config.master_ref.local())?;
    let base_is_master = pull_request.base.is_master_branch();
    let index = git.cherrypick(prepared_commit.oid, current_master)?;

    if index.has_conflicts() {
        return Err(Error::new(formatdoc!(
            "This commit cannot be applied on top of the '{master}' branch.
             Please rebase this commit on top of current \
             '{remote}/{master}'.{unlanded}",
            master = &config.master_ref.branch_name(),
            remote = &config.remote_name,
            unlanded = if based_on_unlanded_commits {
                " You may also have to land commits that this commit depends on first."
            } else {
                ""
            },
        )));
    }

    // This is the tree we are getting from cherrypicking the local commit
    // on the selected base (master or stacked-on Pull Request).
    let our_tree_oid = git.write_index(index)?;

    // Now let's predict what merging the PR into the master branch would
    // produce.
    let merge_index = git.repo().merge_commits(
        &git.repo().find_commit(current_master)?,
        &git.repo().find_commit(pull_request.head_oid)?,
        None,
    )?;

    let merge_matches_cherrypick = if merge_index.has_conflicts() {
        false
    } else {
        let merge_tree_oid = git.write_index(merge_index)?;
        merge_tree_oid == our_tree_oid
    };

    if !merge_matches_cherrypick {
        return Err(Error::new(formatdoc!(
            "This commit has been updated and/or rebased since the pull \
             request was last updated. Please run `spr diff` to update the \
             pull request and then try `spr land` again!"
        )));
    }

    if !base_is_master {
        // The base of the Pull Request on GitHub is not set to master. This
        // means the Pull Request uses a base branch. We tested above that
        // merging the Pull Request branch into the master branch produces the
        // intended result (the same as cherry-picking the local commit onto
        // master), so what we want to do is actually merge the Pull Request as
        // it is into master. Hence, we change the base to the master branch.

        gh.update_pull_request(
            pull_request_number,
            PullRequestUpdate {
                base: Some(config.master_ref.on_github().to_string()),
                ..Default::default()
            },
        )
        .await?;
    }

    // Check whether GitHub says this PR is mergeable. This happens in a
    // retry-loop because recent changes to the Pull Request can mean that
    // GitHub has not finished the mergeability check yet.
    let mut attempts = 0;
    let result = loop {
        attempts += 1;

        let mergeability = gh
            .get_pull_request_mergeability(pull_request_number)
            .await?;

        if mergeability.head_oid != pull_request.head_oid {
            break Err(Error::new(formatdoc!(
                "The Pull Request seems to have been updated externally.
                     Please try again!"
            )));
        }

        if mergeability.base.is_master_branch()
            && mergeability.mergeable.is_some()
        {
            if mergeability.mergeable != Some(true) {
                break Err(Error::new(formatdoc!(
                    "GitHub concluded the Pull Request is not mergeable at \
                    this point. Please rebase your changes and try again!"
                )));
            }

            if let Some(merge_commit) = mergeability.merge_commit {
                git.fetch_commits_from_remote(
                    &[merge_commit],
                    &config.remote_name,
                )
                .await?;

                if git.get_tree_oid_for_commit(merge_commit)? != our_tree_oid {
                    return Err(Error::new(formatdoc!(
                    "This commit has been updated and/or rebased since the pull
                     request was last updated. Please run `spr diff` to update the pull
                     request and then try `spr land` again!"
                )));
                }
            };

            break Ok(());
        }

        if attempts >= 10 {
            // After ten failed attempts we give up.
            break Err(Error::new(
                "GitHub Pull Request did not update. Please try again!",
            ));
        }

        // Wait one second before retrying
        async_io::Timer::after(Duration::from_secs(1)).await;
    };

    let result = match result {
        Ok(()) => {
            // We have checked that merging the Pull Request branch into the master
            // branch produces the intended result, and that's independent of whether we
            // used a base branch with this Pull Request or not. We have made sure the
            // target of the Pull Request is set to the master branch. So let GitHub do
            // the merge now!
            octocrab::instance()
                .pulls(&config.owner, &config.repo)
                .merge(pull_request_number)
                .method(octocrab::params::pulls::MergeMethod::Squash)
                .title(pull_request.title)
                .message(build_github_body_for_merging(&pull_request.sections))
                .sha(format!("{}", pull_request.head_oid))
                .send()
                .compat()
                .await
                .convert()
                .and_then(|merge| {
                    if merge.merged {
                        Ok(merge)
                    } else {
                        Err(Error::new(formatdoc!(
                            "GitHub Pull Request merge failed: {}",
                            merge.message.unwrap_or_default()
                        )))
                    }
                })
        }
        Err(err) => Err(err),
    };

    let merge = match result {
        Ok(merge) => merge,
        Err(mut error) => {
            output("❌", "GitHub Pull Request merge failed")?;

            // If we changed the target branch of the Pull Request earlier, then
            // undo this change now.
            if !base_is_master {
                let result = gh
                    .update_pull_request(
                        pull_request_number,
                        PullRequestUpdate {
                            base: Some(
                                pull_request.base.on_github().to_string(),
                            ),
                            ..Default::default()
                        },
                    )
                    .await;
                if let Err(e) = result {
                    error.push(format!("{}", e));
                }
            }

            return Err(error);
        }
    };

    output("🛬", "Landed!")?;

    let mut remove_old_branch_child_process =
        async_process::Command::new("git")
            .arg("push")
            .arg("--no-verify")
            .arg("--delete")
            .arg("--")
            .arg(&config.remote_name)
            .arg(pull_request.head.on_github())
            .stdout(async_process::Stdio::null())
            .stderr(async_process::Stdio::null())
            .spawn()?;

    let remove_old_base_branch_child_process = if base_is_master {
        None
    } else {
        Some(
            async_process::Command::new("git")
                .arg("push")
                .arg("--no-verify")
                .arg("--delete")
                .arg("--")
                .arg(&config.remote_name)
                .arg(pull_request.base.on_github())
                .stdout(async_process::Stdio::null())
                .stderr(async_process::Stdio::null())
                .spawn()?,
        )
    };

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
                .arg(config.master_ref.on_github())
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
        git.rebase_commits(
            &mut prepared_commits[..],
            git2::Oid::from_str(&sha)?,
        )
        .context(
            "The automatic rebase failed - please rebase manually!".to_string(),
        )?;
    }

    // Wait for the "git push" to delete the old Pull Request branch to finish,
    // but ignore the result. GitHub may be configured to delete the branch
    // automatically, in which case it's gone already and this command fails.
    remove_old_branch_child_process.status().await?;
    if let Some(mut proc) = remove_old_base_branch_child_process {
        proc.status().await?;
    }

    Ok(())
}
