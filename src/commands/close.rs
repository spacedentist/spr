/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use color_eyre::eyre::{Result, bail};

use crate::{
    git::PreparedCommit,
    git_remote::PushSpec,
    github::{PullRequestState, PullRequestUpdate},
    message::MessageSection,
    output::{output, write_commit_title},
};

#[derive(Debug, clap::Parser)]
pub struct CloseOptions {
    /// Close Pull Requests for the whole branch, not just the HEAD commit
    #[clap(long, short = 'a')]
    all: bool,
}

pub async fn close(
    opts: CloseOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    _config: &crate::config::Config,
) -> Result<()> {
    let mut result = Ok(());

    let mut prepared_commits = gh.get_prepared_commits()?;

    if prepared_commits.is_empty() {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return result;
    };

    if !opts.all {
        // Remove all prepared commits from the vector but the last. So, if
        // `--all` is not given, we only operate on the HEAD commit.
        prepared_commits.drain(0..prepared_commits.len() - 1);
    }

    for prepared_commit in prepared_commits.iter_mut() {
        if result.is_err() {
            break;
        }

        write_commit_title(prepared_commit)?;

        // The further implementation of the close command is in a separate function.
        // This makes it easier to run the code to update the local commit message
        // with all the changes that the implementation makes at the end, even if
        // the implementation encounters an error or exits early.
        result = close_impl(gh, prepared_commit).await;
    }

    // This updates the commit message in the local Git repository (if it was
    // changed by the implementation)
    git.rewrite_commit_messages(prepared_commits.as_mut_slice(), None)?;

    result
}

async fn close_impl(
    gh: &mut crate::github::GitHub,
    prepared_commit: &mut PreparedCommit,
) -> Result<()> {
    let pull_request_number =
        if let Some(number) = prepared_commit.pull_request_number {
            output("#️⃣ ", &format!("Pull Request #{}", number))?;
            number
        } else {
            bail!("This commit does not refer to a Pull Request.");
        };

    // Load Pull Request information
    let pull_request = gh.clone().get_pull_request(pull_request_number).await?;

    if pull_request.state != PullRequestState::Open {
        bail!("This Pull Request is already closed!");
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
        Ok(()) => (),
        Err(error) => {
            output("❌", "GitHub Pull Request close failed")?;

            return Err(error);
        }
    };

    output("📕", "Closed!")?;

    // Remove sections from commit that are not relevant after closing.
    prepared_commit.message.remove(&MessageSection::PullRequest);
    prepared_commit.message.remove(&MessageSection::ReviewedBy);

    let mut push_specs = vec![PushSpec {
        oid: None,
        remote_ref: pull_request.head.on_github(),
    }];

    if !base_is_master {
        push_specs.push(PushSpec {
            oid: None,
            remote_ref: pull_request.base.on_github(),
        });
    }

    gh.remote().push_to_remote(&push_specs)?;

    Ok(())
}
