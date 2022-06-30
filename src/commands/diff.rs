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
    utils::{parse_name_list, remove_all_parens, run_command},
};
use git2::Oid;
use indoc::{formatdoc, indoc};

#[derive(Debug, clap::Parser)]
pub struct DiffOptions {
    /// Create/update pull requests for the whole branch, not just the HEAD commit
    #[clap(long)]
    all: bool,

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

    let mut result = Ok(());

    // Look up the commits on the local branch
    let mut prepared_commits = git.get_prepared_commits(config)?;

    // The parent of the first commit in the list is the commit on master that
    // the local branch is based on
    let master_base_oid = if let Some(first_commit) = prepared_commits.get(0) {
        first_commit.parent_oid
    } else {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return result;
    };

    if !opts.all {
        // Remove all prepared commits from the vector but the last. So, if
        // `--all` is not given, we only operate on the HEAD commit.
        prepared_commits.drain(0..prepared_commits.len() - 1);
    }

    // Fetch Pull Request information from GitHub for all commits in parallel
    {
        let futures: Vec<_> = prepared_commits
            .iter()
            .filter_map(|prepared_commit| prepared_commit.pull_request_number)
            .map(|number| gh.get_pull_request(number))
            .collect();
        for future in futures {
            let _ = future.await?;
        }
    }

    let mut message_on_prompt = "".to_string();

    for prepared_commit in prepared_commits.iter_mut() {
        if result.is_err() {
            break;
        }

        write_commit_title(prepared_commit)?;

        // The further implementation of the diff command is in a separate function.
        // This makes it easier to run the code to update the local commit message
        // with all the changes that the implementation makes at the end, even if
        // the implementation encounters an error or exits early.
        result = diff_impl(
            &opts,
            &mut message_on_prompt,
            git,
            gh,
            config,
            prepared_commit,
            master_base_oid,
        )
        .await;
    }

    // This updates the commit message in the local Git repository (if it was
    // changed by the implementation)
    add_error(
        &mut result,
        git.rewrite_commit_messages(prepared_commits.as_mut_slice(), None),
    );

    result
}

async fn diff_impl(
    opts: &DiffOptions,
    message_on_prompt: &mut String,
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
                master = config.master_ref.branch_name(),
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
        validate_commit_message(message, &config)?;
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

    let pull_request_branch = match &pull_request {
        Some(pr) => pr.head.clone(),
        None => config.new_github_branch(
            &config.get_new_branch_name(&git.get_all_ref_names()?, title),
        ),
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
                git.resolve_reference(config.master_ref.local())?;
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

    // Check if there is a base branch on GitHub already. That's the case when
    // there is an existing Pull Request, and its base is not the master branch.
    let base_branch = if let Some(ref pr) = pull_request {
        if pr.base.is_master_branch() {
            None
        } else {
            Some(pr.base.clone())
        }
    } else {
        None
    };

    // We are going to construct `pr_base_parent: Option<Oid>`.
    // The value will be the commit we have to merge into the new Pull Request
    // commit to reflect changes in the parent of the local commit (by rebasing
    // or changing commits between master and this one, although technically
    // that's also rebasing).
    // If it's `None`, then we will not merge anything into the new Pull Request
    // commit.
    // If we are updating an existing PR, then there are three cases here:
    // (1) the parent tree of this commit is unchanged and we do not need to
    //     merge in master, which means that the local commit was amended, but
    //     not rebased. We don't need to merge anything into the Pull Request
    //     branch.
    // (2) the parent tree has changed, but the parent of the local commit is on
    //     master (or we are cherry-picking) and we are not already using a base
    //     branch: in this case we can merge the master commit we are based on
    //     into the PR branch, without going via a base branch. Thus, we don't
    //     introduce a base branch here and the PR continues to target the
    //     master branch.
    // (3) the parent tree has changed, and we need to use a base branch (either
    //     because one was already created earlier, or we find that we are not
    //     directly based on master now): we need to construct a new commit for
    //     the base branch. That new commit's tree is always that of that local
    //     commit's parent (thus making sure that the difference between base
    //     branch and pull request branch are exactly the changes made by the
    //     local commit, thus the changes we want to have reviewed). The new
    //     commit may have one or two parents. The previous base is always a
    //     parent (that's either the current commit on an existing base branch,
    //     or the previous master commit the PR was based on if there isn't a
    //     base branch already). In addition, if the master commit this commit
    //     is based on has changed, (i.e. the local commit got rebased on newer
    //     master in the meantime) then we have to merge in that master commit,
    //     which will be the second parent.
    // If we are creating a new pull request then `pr_base_tree` (the current
    // base of the PR) was set above to be the tree of the master commit the
    // local commit is based one, whereas `new_base_tree` is the tree of the
    // parent of the local commit. So if the local commit for this new PR is on
    // master, those two are the same (and we want to apply case 1). If the
    // commit is not directly based on master, we have to create this new PR
    // with a base branch, so that is case 3.

    let (pr_base_parent, base_branch) = if pr_base_tree == new_base_tree
        && !needs_merging_master
    {
        // Case 1
        (None, base_branch)
    } else if base_branch.is_none()
        && (directly_based_on_master || opts.cherry_pick)
    {
        // Case 2
        (Some(master_base_oid), None)
    } else {
        // Case 3

        // We are constructing a base branch commit.
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

        let new_base_branch_commit = git.create_derived_commit(
            local_commit.parent_oid,
            &format!(
                "[𝘀𝗽𝗿] {}\n\nCreated using spr {}\n\n[skip ci]",
                if pull_request.is_some() {
                    "changes introduced through rebase".to_string()
                } else {
                    format!(
                        "changes to {} this commit is based on",
                        config.master_ref.branch_name()
                    )
                },
                env!("CARGO_PKG_VERSION"),
            ),
            new_base_tree,
            &parents[..],
        )?;

        // If `base_branch` is `None` (which means a base branch does not exist
        // yet), then make a `GitHubBranch` with a new name for a base branch
        let base_branch = if let Some(base_branch) = base_branch {
            base_branch
        } else {
            config.new_github_branch(
                &config.get_base_branch_name(&git.get_all_ref_names()?, title),
            )
        };

        (Some(new_base_branch_commit), Some(base_branch))
    };

    let mut github_commit_message = opts.message.clone();
    if pull_request.is_some() && github_commit_message.is_none() {
        let input = dialoguer::Input::<String>::new()
            .with_prompt("Message (leave empty to abort)")
            .with_initial_text(message_on_prompt.clone())
            .allow_empty(true)
            .interact_text()?;

        if input.is_empty() {
            return Err(Error::new("Aborted as per user request".to_string()));
        }

        *message_on_prompt = input.clone();
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
        &format!(
            "{}\n\nCreated using spr {}",
            github_commit_message
                .as_ref()
                .map(|s| &s[..])
                .unwrap_or("[𝘀𝗽𝗿] initial version"),
            env!("CARGO_PKG_VERSION"),
        ),
        new_head_tree,
        &pr_commit_parents[..],
    )?;

    let mut cmd = async_process::Command::new("git");
    cmd.arg("push")
        .arg("--atomic")
        .arg("--no-verify")
        .arg("--")
        .arg(&config.remote_name)
        .arg(format!("{}:{}", pr_commit, pull_request_branch.on_github()));

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

        if let Some(base_branch) = base_branch {
            // We are using a base branch.

            if let Some(base_branch_commit) = pr_base_parent {
                // ...and we prepared a new commit for it, so we need to push an
                // update of the base branch.
                cmd.arg(format!(
                    "{}:{}",
                    base_branch_commit,
                    base_branch.on_github()
                ));
            }

            // Push the new commit onto the Pull Request branch (and also the
            // new base commit, if we added that to cmd above).
            run_command(&mut cmd)
                .await
                .reword("git push failed".to_string())?;

            // If the Pull Request's base is not set to the base branch yet,
            // change that now.
            if pull_request.base.branch_name() != base_branch.branch_name() {
                pull_request_updates.base =
                    Some(base_branch.on_github().to_string());
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
        if let (Some(base_branch), Some(base_branch_commit)) =
            (&base_branch, pr_base_parent)
        {
            cmd.arg(format!(
                "{}:{}",
                base_branch_commit,
                base_branch.on_github()
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
                base_branch
                    .as_ref()
                    .unwrap_or(&config.master_ref)
                    .on_github()
                    .to_string(),
                pull_request_branch.on_github().to_string(),
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
