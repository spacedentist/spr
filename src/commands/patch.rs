/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    error::Result,
    message::{build_commit_message, MessageSection},
    output::output,
};

#[derive(Debug, clap::Parser)]
pub struct PatchOptions {
    /// Pull Request number
    pull_request: u64,

    /// Name of the branch to be created. Defaults to `PR-<number>`
    #[clap(long)]
    branch_name: Option<String>,

    /// If given, create new branch but do not check out
    #[clap(long)]
    no_checkout: bool,
}

pub async fn patch(
    opts: PatchOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    let pr = gh.get_pull_request(opts.pull_request).await??;
    output(
        "#️⃣ ",
        &format!(
            "Pull Request #{}: {}",
            pr.number,
            pr.sections
                .get(&MessageSection::Title)
                .map(|s| &s[..])
                .unwrap_or("(no title)")
        ),
    )?;

    let branch_name = if let Some(name) = opts.branch_name {
        name
    } else {
        git.get_pr_patch_branch_name(pr.number)?
    };

    let patch_branch_oid = if let Some(oid) = pr.merge_commit {
        output("❗", "Pull Request has been merged")?;

        oid
    } else {
        // Current oid of the master branch
        let current_master_oid =
            git.resolve_reference(&config.remote_master_ref)?;

        // The parent commit to base the new PR branch on shall be the master
        // commit this PR is based on
        let mut pr_master_oid =
            git.repo().merge_base(pr.head_oid, current_master_oid)?;

        // The PR may be against master or some base branch. `pr.base_oid`
        // indicates what the PR base is, but might point to the latest commit
        // of the target (i.e. base) branch, and especially if the target branch
        // is master, might be different from the commit the PR is actually
        // based on. But the merge base of the given `pr.base_oid` and the PR
        // head is the right commit.
        let pr_base_oid = git.repo().merge_base(pr.head_oid, pr.base_oid)?;

        if pr_base_oid != pr_master_oid {
            // So the commit the PR is based on is not the same as the master
            // commit it's based on. This means there must be a base branch that
            // contains additional commits. We want to squash those changes into
            // one commit that we then title "Base of Pull Reqeust #x".
            // Oh, one more thing. The base commit might not be on master, but
            // if it, for whatever reason, contains the same tree as the master
            // base, the base commit we construct here would turn out to be
            // empty. No point in creating an empty commit, so let's first check
            // whether base tree and master tree are different.
            let pr_base_tree = git.get_tree_oid_for_commit(pr.base_oid)?;
            let master_tree = git.get_tree_oid_for_commit(pr_master_oid)?;

            if pr_base_tree != master_tree {
                // The base of this PR is not on master. We need to create two
                // commits on the new branch we are making. First, a commit that
                // represents the base of the PR. And then second, the commit
                // that represents the contents of the PR.

                pr_master_oid = git.create_derived_commit(
                    pr_base_oid,
                    &format!("[𝘀𝗽𝗿] Base of Pull Request #{}", pr.number),
                    pr_base_tree,
                    &[pr_master_oid],
                )?;
            }
        }

        // Create the main commit for the patch branch. This is based on a
        // master commit, or, if the PR can't be based on master directly, on
        // the commit we created above to prepare the base of this commit.
        git.create_derived_commit(
            pr.head_oid,
            &build_commit_message(&pr.sections),
            git.get_tree_oid_for_commit(pr.head_oid)?,
            &[pr_master_oid],
        )?
    };

    let patch_branch_commit = git.repo().find_commit(patch_branch_oid)?;

    // Create the new branch, now that we know the commit it shall point to
    git.repo()
        .branch(&branch_name, &patch_branch_commit, true)?;

    output("🌱", &format!("Created new branch: {}", &branch_name))?;

    if !opts.no_checkout {
        // Check out the new branch
        git.repo()
            .checkout_tree(patch_branch_commit.as_object(), None)?;
        git.repo()
            .set_head(&format!("refs/heads/{}", branch_name))?;
        output("✅", "Checked out")?;
    }

    Ok(())
}
