/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    error::{add_error, Error, Result, ResultExt},
    git::PreparedCommit,
    github::{
        PullRequestRequestReviewers, PullRequestState, PullRequestUpdate,
    },
    message::{validate_commit_message, MessageSection},
    output::{output, write_commit_title},
    utils::{
        get_branch_name_from_ref_name, parse_name_list, remove_all_parens,
        run_command,
    },
};
use git2::Oid;
use indoc::{formatdoc, indoc};

#[derive(Debug, clap::Parser)]
pub struct DiffOptions {
    /// Update the pull request title and description on GitHub from the local
    /// commit message
    #[clap(long)]
    update_message: bool,

    /// Submit any new Pull Request as a draft
    #[clap(long)]
    draft: bool,

    /// Message to be used for commits updating existing pull requests (e.g.
    /// 'rebase' or 'review comments')
    #[clap(long, short = 'm')]
    message: Option<String>,

    /// Submit this commit as if it was cherry-picked on master. Do not base it
    /// on any intermediate changes between the master branch and this commit.
    #[clap(long)]
    cherry_pick: bool,
}

pub async fn diff(
    opts: DiffOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    // Abort right here if the local Git repository is not clean
    git.check_no_uncommitted_changes()?;

    // Look up the commits on the local branch
    let mut prepared_commits = git.get_prepared_commits(config)?;

    let mut prepared_commit = match prepared_commits.pop() {
        Some(c) => c,
        None => {
            output("👋", "Branch is empty - nothing to do. Good bye!")?;
            return Ok(());
        }
    };

    // The parent of the first commit in the list is the commit on master that
    // the local branch is based on
    let master_base_oid = prepared_commits
        .get(0)
        .unwrap_or(&prepared_commit)
        .parent_oid;

    drop(prepared_commits);

    write_commit_title(&prepared_commit)?;

    // The further implementation of the diff command is in a separate function.
    // This makes it easier to run the code to update the local commit message
    // with all the changes that the implementation makes at the end, even if
    // the implementation encounters an error or exits early.
    let mut result =
        diff_impl(opts, git, gh, config, &mut prepared_commit, master_base_oid)
            .await;

    // This updates the commit message in the local Git repository (if it was
    // changed by the implementation)
    add_error(
        &mut result,
        git.rewrite_commit_messages(&mut [prepared_commit], None),
    );

    result
}

async fn diff_impl(
    opts: DiffOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
    local_commit: &mut PreparedCommit,
    master_base_oid: Oid,
) -> Result<()> {
    // Parsed commit message of the local commit
    let message = &mut local_commit.message;

    // Check if the local commit is based directly on the master branch.
    let directly_based_on_master = local_commit.parent_oid == master_base_oid;

    // Determine the trees the Pull Request branch and the base branch should
    // have when we're done here.
    let (new_head_tree, new_base_tree) = if !opts.cherry_pick
        || directly_based_on_master
    {
        // Unless the user tells us to --cherry-pick, these should be the trees
        // of the current commit and its parent.
        // If the current commit is directly based on master (i.e.
        // directly_based_on_master is true), then we can do this here even when
        // the user tells us to --cherry-pick, because we would cherry pick the
        // current commit onto its parent, which gives us the same tree as the
        // current commit has, and the master base is the same as this commit's
        // parent.
        let head_tree = git.get_tree_oid_for_commit(local_commit.oid)?;
        let base_tree = git.get_tree_oid_for_commit(local_commit.parent_oid)?;

        (head_tree, base_tree)
    } else {
        // Cherry-pick the current commit onto master
        let index = git.cherrypick(local_commit.oid, master_base_oid)?;

        if index.has_conflicts() {
            return Err(Error::new(formatdoc!(
                "This commit cannot be cherry-picked on {master}.",
                master = &config.master_branch,
            )));
        }

        // This is the tree we are getting from cherrypicking the local commit
        // on master.
        let cherry_pick_tree = git.write_index(index)?;
        let master_tree = git.get_tree_oid_for_commit(master_base_oid)?;

        (cherry_pick_tree, master_tree)
    };

    // If this is a new Pull Request and the commit message has a "Reviewers"
    // section, then start getting a list of eligible reviewers in the
    // background;
    let eligible_reviewers = if local_commit.pull_request_number.is_none()
        && message.contains_key(&MessageSection::Reviewers)
    {
        Some(gh.get_reviewers())
    } else {
        None
    };

    if let Some(number) = local_commit.pull_request_number {
        output(
            "#️⃣ ",
            &format!(
                "Pull Request #{}: {}",
                number,
                config.pull_request_url(number)
            ),
        )?;
    }

    if local_commit.pull_request_number.is_none() || opts.update_message {
        validate_commit_message(message)?;
    }

    // Load Pull Request information
    let pull_request = if let Some(number) = local_commit.pull_request_number {
        let pr = gh.get_pull_request(number).await??;
        if pr.state == PullRequestState::Closed {
            return Err(Error::new(formatdoc!(
                "Pull request is closed. If you want to open a new one, \
                 remove the 'Pull Request' section from the commit message."
            )));
        }
        Some(pr)
    } else {
        None
    };

    if !opts.update_message {
        if let Some(ref pull_request) = pull_request {
            let mut pull_request_updates: PullRequestUpdate =
                Default::default();
            pull_request_updates.update_message(pull_request, message);

            if !pull_request_updates.is_empty() {
                output(
                    "⚠️",
                    indoc!(
                        "The Pull Request's title/message differ from the \
                         local commit's message.
                         Use `spr diff --update-message` to overwrite the \
                         title and message on GitHub with the local message, \
                         or `spr amend` to go the other way (rewrite the local \
                         commit message with what is on GitHub)."
                    ),
                )?;
            }
        }
    }

    // Parse "Reviewers" section, if this is a new Pull Request
    let mut requested_reviewers = PullRequestRequestReviewers::default();

    if let (Some(task), Some(reviewers)) =
        (eligible_reviewers, message.get(&MessageSection::Reviewers))
    {
        let eligible_reviewers = task.await??;

        let reviewers = parse_name_list(reviewers);
        let mut checked_reviewers = Vec::new();

        for reviewer in reviewers {
            if let Some(entry) = eligible_reviewers.get(&reviewer) {
                if let Some(slug) = reviewer.strip_prefix('#') {
                    requested_reviewers.team_reviewers.push(slug.to_string());
                } else {
                    requested_reviewers.reviewers.push(reviewer.clone());
                }

                if let Some(name) = entry {
                    checked_reviewers.push(format!(
                        "{} ({})",
                        reviewer,
                        remove_all_parens(name)
                    ));
                } else {
                    checked_reviewers.push(reviewer);
                }
            } else {
                return Err(Error::new(format!(
                    "Reviewers field contains unknown user/team '{}'",
                    reviewer
                )));
            }
        }

        message.insert(MessageSection::Reviewers, checked_reviewers.join(", "));
    }

    // Get the name of the existing Pull Request branch, or constuct one if
    // there is none yet.

    let title = message
        .get(&MessageSection::Title)
        .map(|t| &t[..])
        .unwrap_or("");

    let pull_request_branch_name = match &pull_request {
        Some(pr) => get_branch_name_from_ref_name(&pr.head)?.to_string(),
        None => config.get_new_branch_name(&git.get_all_ref_names()?, title),
    };

    // Check if there is a base branch on GitHub already. That's the case when
    // there is an existing Pull Request, and its base is not the master branch.
    let base_branch = match &pull_request {
        Some(pr) => {
            let base_branch_name = get_branch_name_from_ref_name(&pr.base).ok();
            let base_is_master =
                base_branch_name == Some(&config.master_branch);

            if base_is_master {
                None
            } else {
                base_branch_name.map(String::from)
            }
        }
        None => None,
    };

    let base_branch_name = match &base_branch {
        Some(br) => br.to_string(),
        None => config.get_base_branch_name(&git.get_all_ref_names()?, title),
    };

    // Get the tree ids of the current head of the Pull Request, as well as the
    // base, and the commit id of the master commit this PR is currently based
    // on.
    // If there is no pre-existing Pull Request, we fill in the equivalent
    // values.
    let (pr_head_oid, pr_head_tree, pr_base_oid, pr_base_tree, pr_master_base) =
        if let Some(pr) = &pull_request {
            let pr_head_tree = git.get_tree_oid_for_commit(pr.head_oid)?;

            let current_master_oid =
                git.resolve_reference(&config.remote_master_ref)?;
            let pr_base_oid =
                git.repo().merge_base(pr.head_oid, pr.base_oid)?;
            let pr_base_tree = git.get_tree_oid_for_commit(pr_base_oid)?;

            let pr_master_base =
                git.repo().merge_base(pr.head_oid, current_master_oid)?;

            (
                pr.head_oid,
                pr_head_tree,
                pr_base_oid,
                pr_base_tree,
                pr_master_base,
            )
        } else {
            let master_base_tree =
                git.get_tree_oid_for_commit(master_base_oid)?;
            (
                master_base_oid,
                master_base_tree,
                master_base_oid,
                master_base_tree,
                master_base_oid,
            )
        };
    let needs_merging_master = pr_master_base != master_base_oid;

    // At this point we can check if we can exit early because no update to the
    // existing Pull Request is necessary
    if let Some(ref pull_request) = pull_request {
        // So there is an existing Pull Request...
        if !needs_merging_master
            && pr_head_tree == new_head_tree
            && pr_base_tree == new_base_tree
        {
            // ...and it does not need a rebase, and the trees of both Pull
            // Request branch and base are all the right ones.
            output("✅", "No update necessary")?;

            if opts.update_message {
                // However, the user requested to update the commit message on
                // GitHub

                let mut pull_request_updates: PullRequestUpdate =
                    Default::default();
                pull_request_updates.update_message(pull_request, message);

                if !pull_request_updates.is_empty() {
                    // ...and there are actual changes to the message
                    gh.update_pull_request(
                        pull_request.number,
                        pull_request_updates,
                    )
                    .await?;
                    output("✍", "Updated commit message on GitHub")?;
                }
            }

            return Ok(());
        }
    }

    // This is the commit we need to merge into the Pull Request branch to
    // reflect changes in the base of this commit.
    let pr_base_parent = if pr_base_tree != new_base_tree
        || (base_branch.is_some() && needs_merging_master)
    {
        // The current base tree of the Pull Request is not what we need. Or, we
        // need to merge in master while already having a base branch. (Although
        // when the latter is the case, the former probably is true, too.)

        // One parent of the new base branch commit will be the current base
        // commit, that could be either the top commit of an existing base
        // branch, or a commit on master.
        let mut parents = vec![pr_base_oid];

        // If we need to rebase on master, make the master commit also a
        // parent (except if the first parent is that same commit, we don't
        // want duplicates in `parents`).
        if needs_merging_master && pr_base_oid != master_base_oid {
            parents.push(master_base_oid);
        }

        Some(git.create_derived_commit(
            local_commit.parent_oid,
            if pull_request.is_some() {
                "[𝘀𝗽𝗿] 𝘤𝘩𝘢𝘯𝘨𝘦𝘴 𝘪𝘯𝘵𝘳𝘰𝘥𝘶𝘤𝘦𝘥 𝘵𝘩𝘳𝘰𝘶𝘨𝘩 𝘳𝘦𝘣𝘢𝘴𝘦\n\n[skip ci]"
            } else {
                "[𝘀𝗽𝗿] 𝘤𝘩𝘢𝘯𝘨𝘦𝘴 𝘵𝘰 𝘮𝘢𝘴𝘵𝘦𝘳 𝘵𝘩𝘪𝘴 𝘤𝘰𝘮𝘮𝘪𝘵 𝘪𝘴 𝘣𝘢𝘴𝘦𝘥 𝘰𝘯\n\n[skip ci]"
            },
            new_base_tree,
            &parents[..],
        )?)
    } else {
        // We are operating without a base branch, i.e. this Pull Request is
        // against the master branch. If the commit was rebased, we have to
        // merge the master commit that we are now based on.
        if needs_merging_master {
            Some(master_base_oid)
        } else {
            None
        }
    };

    let mut github_commit_message = opts.message.clone();
    if pull_request.is_some() && github_commit_message.is_none() {
        let input = dialoguer::Input::<String>::new()
            .with_prompt("Message (leave empty to abort)")
            .allow_empty(true)
            .interact_text()?;

        if input.is_empty() {
            return Err(Error::new("Aborted as per user request".to_string()));
        }

        github_commit_message = Some(input);
    }

    // Construct the new commit for the Pull Request branch. First parent is the
    // current head commit of the Pull Request (we set this to the master base
    // commit earlier if the Pull Request does not yet exist)
    let mut pr_commit_parents = vec![pr_head_oid];

    // If we prepared a commit earlier that needs merging into the Pull Request
    // branch, then that commit is a parent of the new Pull Request commit.
    if let Some(oid) = pr_base_parent {
        // ...unless if that's the same commit as the one we added to
        // pr_commit_parents first.
        if pr_commit_parents.get(0) != Some(&oid) {
            pr_commit_parents.push(oid);
        }
    }

    // Create the new commit
    let pr_commit = git.create_derived_commit(
        local_commit.oid,
        github_commit_message
            .as_ref()
            .map(|s| &s[..])
            .unwrap_or("[𝘀𝗽𝗿] 𝘪𝘯𝘪𝘵𝘪𝘢𝘭 𝘷𝘦𝘳𝘴𝘪𝘰𝘯"),
        new_head_tree,
        &pr_commit_parents[..],
    )?;

    let mut cmd = async_process::Command::new("git");
    cmd.arg("push")
        .arg("--atomic")
        .arg("--no-verify")
        .arg("--")
        .arg(&config.remote_name)
        .arg(format!(
            "{}:refs/heads/{}",
            pr_commit, pull_request_branch_name
        ));

    if let Some(pull_request) = pull_request {
        // We are updating an existing Pull Request

        if needs_merging_master {
            output(
                "⚾",
                &format!(
                    "Commit was rebased - updating Pull Request #{}",
                    pull_request.number
                ),
            )?;
        } else {
            output(
                "🔁",
                &format!(
                    "Commit was changed - updating Pull Request #{}",
                    pull_request.number
                ),
            )?;
        }

        // Things we want to update in the Pull Request on GitHub
        let mut pull_request_updates: PullRequestUpdate = Default::default();

        if opts.update_message {
            pull_request_updates.update_message(&pull_request, message);
        }

        if pr_base_parent.is_some() {
            // We are using a base branch.

            if let Some(base_branch_commit) = pr_base_parent {
                // ...and we prepared a new commit for it, so we need to push an
                // update of the base branch.
                cmd.arg(format!(
                    "{}:refs/heads/{}",
                    base_branch_commit, base_branch_name
                ));
            }

            // Push the new commit onto the Pull Request branch (and also the
            // new base commit, if we added that to cmd above).
            run_command(&mut cmd)
                .await
                .reword("git push failed".to_string())?;

            // If the Pull Request's base is not set to the base branch yet,
            // change that now.
            if get_branch_name_from_ref_name(&pull_request.base)?
                != base_branch_name
            {
                pull_request_updates.base =
                    Some(format!("refs/heads/{}", base_branch_name));
            }
        } else {
            // The Pull Request is against the master branch. In that case we
            // only need to push the update to the Pull Request branch.
            run_command(&mut cmd)
                .await
                .reword("git push failed".to_string())?;
        }

        if !pull_request_updates.is_empty() {
            gh.update_pull_request(pull_request.number, pull_request_updates)
                .await?;
        }
    } else {
        // We are creating a new Pull Request.

        // If there's a base branch, add it to the push
        if pr_base_parent.is_some() {
            cmd.arg(format!(
                "{}:refs/heads/{}",
                pr_base_parent.unwrap(),
                base_branch_name
            ));
        }
        // Push the pull request branch and the base branch if present
        run_command(&mut cmd)
            .await
            .reword("git push failed".to_string())?;

        // Then call GitHub to create the Pull Request.
        let pull_request_number = gh
            .create_pull_request(
                message,
                if pr_base_parent.is_some() {
                    format!("refs/heads/{}", base_branch_name)
                } else {
                    config.master_ref.clone()
                },
                format!("refs/heads/{}", pull_request_branch_name),
                opts.draft,
            )
            .await?;

        let pull_request_url = config.pull_request_url(pull_request_number);

        output(
            "✨",
            &format!(
                "Created new Pull Request #{}: {}",
                pull_request_number, &pull_request_url,
            ),
        )?;

        message.insert(MessageSection::PullRequest, pull_request_url);

        gh.request_reviewers(pull_request_number, requested_reviewers)
            .await?;
    }

    Ok(())
}
