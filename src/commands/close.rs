/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::process::Stdio;

use indoc::formatdoc;

use crate::{
    error::{Error, Result},
    github::{PullRequestState, PullRequestUpdate},
    message::MessageSection,
    output::{output, write_commit_title},
};

pub async fn close(
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
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
    let pull_request = gh.get_pull_request(pull_request_number).await?;

    if pull_request.state != PullRequestState::Open {
        return Err(Error::new(formatdoc!(
            "This Pull Request is already closed!",
        )));
    }

    output("📖", "Getting started...")?;

    let base_is_master = pull_request.base.is_master_branch();

    let result = gh
        .update_pull_request(
            pull_request_number,
            PullRequestUpdate {
                state: Some(PullRequestState::Closed),
                ..Default::default()
            },
        )
        .await;

    match result {
        Ok(closed) => closed,
        Err(error) => {
            output("❌", "GitHub Pull Request close failed")?;

            return Err(error);
        }
    };

    output("📕", "Closed!")?;

    // Remove Pull Request section from commit.
    prepared_commit.message.remove(&MessageSection::PullRequest);
    drop(prepared_commit);
    git.rewrite_commit_messages(prepared_commits.as_mut_slice(), None)?;

    let mut remove_old_branch_child_process =
        tokio::process::Command::new("git")
            .arg("push")
            .arg("--no-verify")
            .arg("--delete")
            .arg("--")
            .arg(&config.remote_name)
            .arg(pull_request.head.on_github())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

    let remove_old_base_branch_child_process = if base_is_master {
        None
    } else {
        Some(
            tokio::process::Command::new("git")
                .arg("push")
                .arg("--no-verify")
                .arg("--delete")
                .arg("--")
                .arg(&config.remote_name)
                .arg(pull_request.base.on_github())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        )
    };

    // Wait for the "git push" to delete the old Pull Request branch to finish,
    // but ignore the result.
    // GitHub may be configured to delete the branch automatically,
    // in which case it's gone already and this command fails.
    remove_old_branch_child_process.wait().await?;
    if let Some(mut proc) = remove_old_base_branch_child_process {
        proc.wait().await?;
    }

    Ok(())
}
