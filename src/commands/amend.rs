use color_eyre::eyre::{Result, eyre};

use crate::{
    forge::Forge,
    git::PreparedCommit,
    output::{output, write_commit_title},
};

#[derive(Debug, clap::Parser)]
pub struct AmendOptions {
    /// Amend all commits in branch, not just HEAD
    #[clap(long, short = 'a')]
    all: bool,
}

pub async fn amend(
    opts: AmendOptions,
    git: &crate::git::Git,
    gh: &mut impl Forge,
    config: &crate::config::Config,
) -> Result<()> {
    let mut pc = gh.get_prepared_commits()?;

    let len = pc.len();
    if len == 0 {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return Ok(());
    }

    // The slice of prepared commits we want to operate on.
    let slice = if opts.all {
        &mut pc[..]
    } else {
        &mut pc[len - 1..]
    };

    // Request the Pull Request information for each commit (well, those that
    // declare to have Pull Requests). This list is in reverse order, so that
    // below we can pop from the vector as we iterate.
    let mut pull_requests: Vec<_> = slice
        .iter()
        .rev()
        .map(|pc: &PreparedCommit| {
            pc.pull_request_number.map(|number| {
                tokio::task::spawn_local(gh.clone().get_pull_request(number))
            })
        })
        .collect();

    let mut result = Ok(());

    // Errors must not return from this function directly (e.g. with `?`),
    // but end the loop, so that the messages of the commits updated so far
    // still get written below. (The commits above those keep their messages.)
    let mut updated = 0;
    for commit in slice.iter_mut() {
        if let Err(error) = write_commit_title(commit) {
            result = Err(error);
            break;
        }
        if let Some(pull_request) = pull_requests.pop().flatten() {
            match pull_request.await {
                Ok(Ok(pull_request)) => commit
                    .message
                    .update_from_pull_request(&pull_request.message),
                Ok(Err(error)) => {
                    result = Err(error);
                    break;
                }
                Err(error) => {
                    result = Err(error.into());
                    break;
                }
            }
        }
        if commit.message.validate(config).is_err() && result.is_ok() {
            result = Err(eyre!("amend failed"));
        }
        updated += 1;
    }
    git.rewrite_commit_messages(slice, Some(updated))?;

    result
}
