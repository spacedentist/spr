use color_eyre::eyre::{Result, bail};

use crate::{
    config::StackingMode,
    git::PreparedCommit,
    git_remote::PushSpec,
    github::{PullRequestState, PullRequestUpdate},
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
    config: &crate::config::Config,
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
        result = close_impl(gh, config, prepared_commit).await;
    }

    // This updates the commit message in the local Git repository (if it was
    // changed by the implementation)
    git.rewrite_commit_messages(prepared_commits.as_mut_slice(), None)?;

    result
}

async fn close_impl(
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
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

    // In the github-stack stacking mode, remove the Pull Request's stack on
    // GitHub first, as the stack won't be valid anymore without it. The next
    // `spr diff` creates a new stack of the remaining Pull Requests.
    if config.stacking_mode == StackingMode::GitHubStack
        && let Some(stack) =
            gh.find_pull_request_stack(pull_request_number).await?
    {
        gh.unstack_pull_request_stack(stack.number).await?;
        output("📚", &format!("Dissolved stack #{}", stack.number))?;
    }

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

    // Pull Requests stacked on this one (in chain stacking mode) are based on
    // its branch, which we are going to delete below. Base them on what this
    // Pull Request was based on.
    let retargeted = gh
        .retarget_pull_requests(&pull_request.head, &pull_request.base)
        .await?;
    for number in retargeted {
        output(
            "🎯",
            &format!(
                "Changed the base of Pull Request #{} to {}",
                number,
                pull_request.base.branch_name()
            ),
        )?;
    }

    // Remove trailers from commit that are not relevant after closing.
    prepared_commit.message.remove_trailer("Pull-request");
    prepared_commit.message.remove_trailer("Reviewed-by");

    let mut push_specs = vec![PushSpec {
        oid: None,
        remote_ref: pull_request.head.on_github(),
    }];

    // Delete the base branch too, if spr created it for this Pull Request. If
    // the Pull Request was stacked on another Pull Request, its base branch
    // is the branch of that other Pull Request, which we must not delete.
    if config.is_spr_base_branch(&pull_request.base) {
        push_specs.push(PushSpec {
            oid: None,
            remote_ref: pull_request.base.on_github(),
        });
    }

    gh.remote().push_to_remote(&push_specs)?;

    Ok(())
}
