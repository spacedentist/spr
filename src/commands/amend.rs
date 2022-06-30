/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    error::{Error, Result},
    message::validate_commit_message,
    output::{output, write_commit_title},
};

#[derive(Debug, clap::Parser)]
pub struct AmendOptions {
    /// format all commits in branch, not just HEAD
    #[clap(long)]
    all: bool,
}

pub async fn amend(
    opts: AmendOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    let mut pc = git.get_prepared_commits(config)?;

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
        .map(|pc| {
            pc.pull_request_number
                .map(|number| gh.get_pull_request(number))
        })
        .collect();

    let mut failure = false;

    for commit in slice.iter_mut() {
        write_commit_title(commit)?;
        let pull_request = pull_requests.pop().flatten();
        if let Some(pull_request) = pull_request {
            let pull_request = pull_request.await??;
            commit.message = pull_request.sections;
        }
        failure = validate_commit_message(&commit.message, &config).is_err()
            || failure;
    }
    git.rewrite_commit_messages(slice, None)?;

    if failure {
        Err(Error::empty())
    } else {
        Ok(())
    }
}
