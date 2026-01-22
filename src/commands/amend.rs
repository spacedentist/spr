use color_eyre::eyre::{Result, eyre};

use crate::{
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
    gh: &mut crate::github::GitHub,
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

    let mut failure = false;

    for commit in slice.iter_mut() {
        write_commit_title(commit)?;
        let pull_request = pull_requests.pop().flatten();
        if let Some(pull_request) = pull_request {
            let pull_request = pull_request.await??;
            commit.message = pull_request.message;
        }
        failure = commit.message.validate(config).is_err() || failure;
    }
    git.rewrite_commit_messages(slice, None)?;

    if failure {
        Err(eyre!("amend failed"))
    } else {
        Ok(())
    }
}
