/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use color_eyre::eyre::{Result, eyre};

use crate::{
    message::validate_commit_message,
    output::{output, write_commit_title},
};

#[derive(Debug, clap::Parser)]
pub struct FormatOptions {
    /// format all commits in branch, not just HEAD
    #[clap(long, short = 'a')]
    all: bool,
}

pub async fn format(
    opts: FormatOptions,
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

    let mut failure = false;

    for commit in slice.iter() {
        write_commit_title(commit)?;
        failure = validate_commit_message(&commit.message, config).is_err()
            || failure;
    }
    git.rewrite_commit_messages(slice, None)?;

    if failure {
        Err(eyre!("format failed"))
    } else {
        Ok(())
    }
}
