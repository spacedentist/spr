/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use async_compat::CompatExt;
use indoc::formatdoc;
use std::io::Write;

use crate::{
    error::{Error, Result, ResultExt},
    github::{PullRequestState, PullRequestUpdate, ReviewStatus},
    message::build_github_body_for_merging,
    output::{output, write_commit_title},
    utils::{get_branch_name_from_ref_name, run_command},
};

#[derive(Debug, clap::Parser)]
pub struct LandOptions {}

pub async fn land(
    _opts: LandOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    git.check_no_uncommitted_changes()?;
    let mut prepared_commits = git.get_prepared_commits(config)?;

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

    if pull_request.review_status != Some(ReviewStatus::Approved) {
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
            .arg(&config.master_ref),
    )
    .await
    .reword("git fetch failed".to_string())?;

    let current_master = git.resolve_reference(&config.remote_master_ref)?;

    let base_is_master = get_branch_name_from_ref_name(&pull_request.base).ok()
        == Some(&config.master_branch);

    let index = git.cherrypick(prepared_commit.oid, current_master)?;

    if index.has_conflicts() {
        return Err(Error::new(formatdoc!(
            "This commit cannot be applied on top of the '{master}' branch.
             Please rebase this commit on top of current '{remote}/{master}'. \
             You may also have to land commits that this commit depends on \
             first.",
            master = &config.master_branch,
            remote = &config.remote_name
        )));
    }

    // This is the tree we are getting from cherrypicking the local commit
    // on the selected base (master or stacked-on Pull Request).
    let our_tree_oid = git.write_index(index)?;

    // With that we construct the final version of the Pull Request, which will
    // get landed. The contents of this commit (the tree) is what we produced by
    // cherry-picking the local commit onto current master. The parents are the
    // previous commit in the Pull Request branch and current master. Because
    // current master is a parent of this commit, squash-merging this commit
    // will give us essentially the cherry-picked commit on master, which is
    // what we want.
    let final_commit = git.create_derived_commit(
        prepared_commit.oid,
        "[𝘀𝗽𝗿] 𝘭𝘢𝘯𝘥𝘦𝘥 𝘷𝘦𝘳𝘴𝘪𝘰𝘯\n\n[skip ci]",
        our_tree_oid,
        &[pull_request.head_oid, current_master],
    )?;

    // Update the Pull Request branch with the final commit
    run_command(
        async_process::Command::new("git")
            .arg("push")
            .arg("--no-verify")
            .arg("--")
            .arg(&config.remote_name)
            .arg(format!(
                "{}:refs/heads/{}",
                final_commit,
                get_branch_name_from_ref_name(&pull_request.head)?
            )),
    )
    .await
    .reword("git push failed".to_string())?;

    // Update the Pull Request: set the base branch (that we are going to
    // squash-merge into) to master. Depending on the contents of the Pull
    // Request, the base branch may already be set to master, but we make this
    // update unconditionally, because it has the positive side effect of making
    // the actual merge below not fail. That's right, when the base was master
    // already and I didn't make this call, the merge below would fail with
    // "GitHub: Head branch was modified. Review and try the merge again". It
    // may be a timing problem when the merge is happening *immediately* after
    // updating the branch. This API call here also returns the updated Pull
    // Request data, which might help settle the situation. With this API call
    // in place, I have not seen the error once.
    //
    // PS: @jozef-mokry mentioned in code review that this is a known timing
    // problem:
    // https://github.community/t/merging-via-rest-api-returns-405-base-branch-was-modified-review-and-try-the-merge-again/13787
    // TODO: implement retry loop around the merge instead
    gh.update_pull_request(
        pull_request_number,
        PullRequestUpdate {
            base: Some(config.master_ref.clone()),
            ..Default::default()
        },
    )
    .await?;

    // We have the oven-ready to-be-merged commit (which is a direct child of
    // current, or *very* recent master) on top of the head branch. Let's merge!
    // Master may have been updated just now, so that our final version is not
    // based on current master but an ancestor of that. That's fine, unless
    // these new changes to master conflict with this Pull Request, in which
    // case this merge will fail. Fair enough, we need the user to rebase on
    // current master to deal with the conflicts.
    let merge_result = octocrab::instance()
        .pulls(&config.owner, &config.repo)
        .merge(pull_request_number)
        .method(octocrab::params::pulls::MergeMethod::Squash)
        .title(pull_request.title)
        .message(build_github_body_for_merging(&pull_request.sections))
        .sha(format!("{}", final_commit))
        .send()
        .compat()
        .await;
    let success = match merge_result {
        Ok(ref merge) => merge.merged,
        Err(_) => false,
    };

    if success {
        output("🛬", "Landed!")?;
    } else {
        output("❌", "GitHub Pull Request merge failed")?;

        // This is the error we'll report
        let mut error = Err(match merge_result {
            Err(err) => err.into(),
            Ok(merge) => {
                merge.message.map(Error::new).unwrap_or_else(Error::empty)
            }
        });

        // Let's try to undo the last bits, so the user can retry cleanly.
        // First: set the Pull Request head to what it was (if it wasn't master)
        if !base_is_master {
            let result = gh
                .update_pull_request(
                    pull_request_number,
                    PullRequestUpdate {
                        base: Some(pull_request.base.clone()),
                        ..Default::default()
                    },
                )
                .await;
            if let Err(e) = result {
                error = error.context(format!("{}", e));
            }
        }

        // Second: force-push the Pull Request branch to remove the final commit
        // again
        let git_push_failed = run_command(
            async_process::Command::new("git")
                .arg("push")
                .arg("--no-verify")
                .arg("--force")
                .arg("--")
                .arg(&config.remote_name)
                .arg(format!(
                    "{}:refs/heads/{}",
                    pull_request.head_oid,
                    get_branch_name_from_ref_name(&pull_request.head)?
                )),
        )
        .await
        .is_err();
        if git_push_failed {
            error = error.context("git push failed".to_string());
        }

        return error;
    }

    // We know merge_result is Ok (we return above if it isn't)
    let merge = merge_result.unwrap();

    let mut remove_old_branch_child_process =
        async_process::Command::new("git")
            .arg("push")
            .arg("--no-verify")
            .arg("--delete")
            .arg("--")
            .arg(&config.remote_name)
            .arg(&pull_request.head)
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
                .arg(&pull_request.base)
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
