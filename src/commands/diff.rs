use crate::{
    error::{add_error, Error, Result, ResultExt},
    git::PreparedCommit,
    github::{
        PullRequestRequestReviewers, PullRequestState, PullRequestUpdate,
    },
    message::{build_github_body, validate_commit_message, MessageSection},
    output::{output, write_commit_title},
    utils::{
        get_branch_name_from_ref_name, parse_name_list, remove_all_parens,
        run_command,
    },
};
use git2::Oid;
use indoc::formatdoc;

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
    let pull_request_branch_name = match &pull_request {
        Some(pr) => get_branch_name_from_ref_name(&pr.head)?.to_string(),
        None => config.get_new_branch_name(
            &git.get_all_ref_names()?,
            message
                .get(&MessageSection::Title)
                .map(|t| &t[..])
                .unwrap_or(""),
        ),
    };

    // Check if there is a base branch on GitHub already. That's the case when
    // there is an existing Pull Request, and its base is not the master branch.
    let have_base_branch = match &pull_request {
        Some(pr) => {
            let base_is_master = get_branch_name_from_ref_name(&pr.base).ok()
                == Some(&config.master_branch);

            !base_is_master
        }
        None => false,
    };

    // `current_pr_master_base` is the master commit that the existing Pull
    // Request is currently based on (or `None` if there is no existing Pull
    // Request).
    // `current_base_oid` is what the base of the Pull Request currently is:
    // * if the Pull Request doesn't exist yet, it's the commit on master that
    //   the local commit is based on
    // * if the Pull Request exists but has no base branch, it's the master
    //   commit the existing Pull Request is currently base don
    // * if the Pull Request exists and has a base branch, it's the top of that
    //   base branch
    let (current_base_oid, current_pr_master_base) = match &pull_request {
        Some(pr) => {
            if have_base_branch {
                let master_base = git.find_master_base(
                    pr.head_oid,
                    git.resolve_reference(&config.remote_master_ref)?,
                )?;
                (pr.base_oid, master_base)
            } else {
                let master_base =
                    git.find_master_base(pr.head_oid, pr.base_oid)?;
                (master_base.unwrap_or(pr.base_oid), master_base)
            }
        }
        None => (master_base_oid, None),
    };
    let needs_merging_master = current_pr_master_base != Some(master_base_oid);

    // If there is no base branch (yet), we can try and cherry-pick the commit
    // onto its base on master. If this succeeds, then we do not need to create
    // a base branch, as we can review this commit against master.
    let cherrypicked_tree = if have_base_branch {
        None
    } else {
        let index = git.cherrypick(local_commit.oid, master_base_oid)?;

        if index.has_conflicts() {
            None
        } else {
            // This is the tree we are getting from cherrypicking the local commit
            // on the selected base (master or stacked-on Pull Request).
            Some(git.write_index(index)?)
        }
    };
    let using_base_branch = cherrypicked_tree.is_none();
    // At this point, `have_base_branch` means whether a base branch exists and
    // is used already by the existing Pull Request. This is always false if we
    // are not working with an existing Pull Request. `using_base_branch` on the
    // other hand means if we need a base branch for creating/updating this Pull
    // Request.

    // This is the tree for the commit we want to submit
    let tree_oid = git.get_tree_oid_for_commit(local_commit.oid)?;

    // This is the tree of the parent commit. The Pull Request should show the
    // changes between these two trees.
    let parent_tree_oid =
        git.get_tree_oid_for_commit(local_commit.parent_oid)?;

    // At this point we can check if we can exit early because no update to the
    // existing Pull Request is necessary
    if let Some(ref pull_request) = pull_request {
        // So there is an existing Pull Request...
        if !needs_merging_master {
            // ...and it does not need a rebase...
            // So, the PRs head commit should be:
            // - the same as the local commit if we use a base branch
            // - the same as what we got just now from cherrypicking onto
            //   master, otherwise
            let pr_head_tree_oid =
                git.get_tree_oid_for_commit(pull_request.head_oid)?;
            let expected_tree_oid = cherrypicked_tree.unwrap_or(tree_oid);
            if pr_head_tree_oid == expected_tree_oid {
                // Also, if we use a base branch, the parent of the local commit
                // should have the same tree as the latest base branch commit.
                if !have_base_branch
                    || parent_tree_oid
                        == git.get_tree_oid_for_commit(pull_request.base_oid)?
                {
                    // We don't have a base branch, or if we do, the local
                    // parent commit has the some tree as the base branch on
                    // GitHub.
                    output("✅", "No update necessary")?;
                    return Ok(());
                }
            }
        }
    }

    // This is the commit we need to merge into the Pull Request branch to
    // reflect changes in the base of this commit.
    let pr_base_parent = if using_base_branch {
        // We are operating with a base branch. This might be a new Pull Request
        // that cannot be cherry-picked on master, or it might be an existing
        // one, and in that case the situation could be either that we already
        // created the base branch before, or that this Pull Request was created
        // against master but now has to be turned into one with a base branch.
        // We need to create a new commit for the base branch now, if: we
        // haven't created the base branch before, or we need to merge master
        // into the base branch, or the existing base branch's top commit has a
        // different tree than the parent of the commit that is being diffed.
        if !have_base_branch
            || needs_merging_master
            || git.get_tree_oid_for_commit(current_base_oid)? != parent_tree_oid
        {
            // One parent of the new base branch commit will be the current base
            // commit, that could be either the top commit of an existing base
            // branch, or a commit on master.
            let mut parents = vec![current_base_oid];

            // If we need to rebase on master, make the master commit also a
            // parent (except if the first parent is that same commit, we don't
            // want duplicates in `parents`).
            if needs_merging_master && current_base_oid != master_base_oid {
                parents.push(master_base_oid);
            }

            Some(git.create_derived_commit(
                local_commit.parent_oid,
                Some(if pull_request.is_some() {
                    "[𝘀𝗽𝗿] 𝘤𝘩𝘢𝘯𝘨𝘦𝘴 𝘪𝘯𝘵𝘳𝘰𝘥𝘶𝘤𝘦𝘥 𝘵𝘩𝘳𝘰𝘶𝘨𝘩 𝘳𝘦𝘣𝘢𝘴𝘦"
                } else {
                    "[𝘀𝗽𝗿] 𝘤𝘩𝘢𝘯𝘨𝘦𝘴 𝘵𝘰 𝘮𝘢𝘴𝘵𝘦𝘳 𝘵𝘩𝘪𝘴 𝘤𝘰𝘮𝘮𝘪𝘵 𝘪𝘴 𝘣𝘢𝘴𝘦𝘥 𝘰𝘯"
                }),
                parent_tree_oid,
                &parents[..],
            )?)
        } else {
            None
        }
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
            .interact_text()?;

        if input.is_empty() {
            return Err(Error::new("Aborted as per user request".to_string()));
        }

        github_commit_message = Some(input);
    }

    // Construct the new commit for the Pull Request branch
    let mut pr_commit_parents = Vec::new();

    // If the Pull Request exists already, the head commit is parent of the new
    // commit
    if let Some(pr) = &pull_request {
        pr_commit_parents.push(pr.head_oid);
    }

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
        github_commit_message.as_ref().map(|s| &s[..]),
        cherrypicked_tree.unwrap_or(tree_oid),
        &pr_commit_parents[..],
    )?;

    let mut cmd = async_process::Command::new("git");
    cmd.arg("push")
        .arg("--atomic")
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
            let title = message.get(&MessageSection::Title);
            if title != Some(&pull_request.title) {
                pull_request_updates.title = title.cloned();
            }

            let body = build_github_body(message);
            if pull_request.body.as_ref() != Some(&body) {
                pull_request_updates.body = Some(body);
            }
        }

        if using_base_branch {
            // We are using a base branch.
            let base_branch_name =
                config.get_base_branch_name(pull_request.number);

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
        // First, push the Pull Request branch.
        run_command(&mut cmd)
            .await
            .reword("git push failed".to_string())?;

        // Then call GitHub to create the Pull Request.
        let pull_request_number = gh
            .create_pull_request(
                message,
                config.master_ref.clone(),
                format!("refs/heads/{}", pull_request_branch_name),
                opts.draft,
            )
            .await?;

        if using_base_branch {
            // We are using a base branch.
            let base_branch_name =
                config.get_base_branch_name(pull_request_number);

            // Push the base branch...
            let mut cmd = async_process::Command::new("git");
            cmd.arg("push")
                .arg("--atomic")
                .arg("--")
                .arg(&config.remote_name)
                .arg(format!(
                    "{}:refs/heads/{}",
                    pr_base_parent.unwrap(),
                    base_branch_name
                ));
            run_command(&mut cmd)
                .await
                .reword("git push failed".to_string())?;

            // And update the Pull Request we just created to set the base
            // branch name.
            gh.update_pull_request(
                pull_request_number,
                PullRequestUpdate {
                    base: Some(format!("refs/heads/{}", base_branch_name)),
                    ..Default::default()
                },
            )
            .await?;
        }

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
    // // The parent commit is on master, i.e. this commit branches off directly of
    // // master.
    // let base_is_master = prepared_commit.parent_oid == master_base_oid
    //     || parent_tree_oid == git.get_tree_oid_for_commit(master_base_oid)?;

    // if let Some(pull_request) = pull_request {
    //     // Update existing Pull Request

    //     let current_master_base = git.find_master_base(
    //         pull_request.head_oid,
    //         git.resolve_reference(&config.remote_master_ref)?,
    //     )?;

    //     let pull_request_against_master =
    //         get_branch_name_from_ref_name(&pull_request.base)?
    //             == config.master_branch;

    //     let current_base_oid = if pull_request_against_master {
    //         current_master_base
    //     } else {
    //         Some(pull_request.base_oid)
    //     };

    //     let needs_rebasing_on_master =
    //         current_master_base != Some(master_base_oid);

    //     let mut cmd = async_process::Command::new("git");
    //     cmd.arg("push").arg("--").arg(&config.remote_name);
    //     let second_parent;
    //     let mut base_branch_name = None;

    //     if pull_request_against_master && base_is_master {
    //         second_parent = if needs_rebasing_on_master {
    //             Some(master_base_oid)
    //         } else {
    //             None
    //         };
    //     } else {
    //         second_parent = if needs_rebasing_on_master
    //             || current_base_oid
    //                 .map(|commit_oid| git.get_tree_oid_for_commit(commit_oid))
    //                 .transpose()?
    //                 != Some(parent_tree_oid)
    //         {
    //             let parents = if let Some(current_base_oid) = current_base_oid {
    //                 if needs_rebasing_on_master
    //                     && current_base_oid != master_base_oid
    //                 {
    //                     vec![current_base_oid, master_base_oid]
    //                 } else {
    //                     vec![current_base_oid]
    //                 }
    //             } else {
    //                 vec![master_base_oid]
    //             };

    //             let base_commit = git.create_pull_request_commit(
    //                 prepared_commit.parent_oid,
    //                 Some("[𝘀𝗽𝗿] 𝘤𝘩𝘢𝘯𝘨𝘦𝘴 𝘪𝘯𝘵𝘳𝘰𝘥𝘶𝘤𝘦𝘥 𝘵𝘩𝘳𝘰𝘶𝘨𝘩 𝘳𝘦𝘣𝘢𝘴𝘦"),
    //                 parent_tree_oid,
    //                 &parents[..],
    //             )?;
    //             base_branch_name =
    //                 Some(config.get_base_branch_name(pull_request.number));
    //             cmd.arg(format!(
    //                 "{}:refs/heads/{}",
    //                 base_commit,
    //                 base_branch_name.as_ref().unwrap(),
    //             ));

    //             Some(base_commit)
    //         } else {
    //             None
    //         }
    //     }

    //     let mut parents = vec![pull_request.head_oid];
    //     // if let Some(current_base_oid) = current_base_oid {
    //     //     parents.push(current_base_oid);
    //     // }
    //     if let Some(second_parent) = second_parent {
    //         parents.push(second_parent);
    //     }

    //     let pr_commit = git.create_pull_request_commit(
    //         prepared_commit.oid,
    //         github_commit_message.as_ref().map(|s| &s[..]),
    //         tree_oid,
    //         &parents[..],
    //     )?;

    //     cmd.arg(format!(
    //         "{}:refs/heads/{}",
    //         pr_commit,
    //         get_branch_name_from_ref_name(&pull_request.head)?,
    //     ));

    //     run_command(&mut cmd)
    //         .await
    //         .reword("git push failed".to_string())?;

    //     if let Some(base_branch_name) = base_branch_name {
    //         if get_branch_name_from_ref_name(&pull_request.base)?
    //             != base_branch_name
    //         {
    //             gh.update_pull_request(
    //                 pull_request.number,
    //                 PullRequestUpdate {
    //                     base: Some(format!("refs/heads/{}", base_branch_name)),
    //                     ..Default::default()
    //                 },
    //             )
    //             .await?;
    //         }
    //     }
    // } else {
    //     // Create new Pull Request

    //     let base_commit = if base_is_master {
    //         master_base_oid
    //     } else {
    //         git.create_pull_request_commit(
    //             prepared_commit.parent_oid,
    //             Some("[𝘀𝗽𝗿] 𝘤𝘩𝘢𝘯𝘨𝘦𝘴 𝘵𝘰 𝘮𝘢𝘴𝘵𝘦𝘳 𝘵𝘩𝘪𝘴 𝘤𝘰𝘮𝘮𝘪𝘵 𝘪𝘴 𝘣𝘢𝘴𝘦𝘥 𝘰𝘯"),
    //             parent_tree_oid,
    //             &[master_base_oid],
    //         )?
    //     };

    //     let pr_commit = git.create_pull_request_commit(
    //         prepared_commit.oid,
    //         opts.message.as_ref().map(|s| &s[..]),
    //         tree_oid,
    //         &[base_commit],
    //     )?;

    //     // Construct the name of the new Pull Request branch.
    //     let pr_ref = format!(
    //         "refs/heads/{}",
    //         config.get_new_branch_name(
    //             &git.get_all_ref_names()?,
    //             message
    //                 .get(&MessageSection::Title)
    //                 .map(|t| &t[..])
    //                 .unwrap_or("")
    //         )
    //     );

    //     run_command(
    //         async_process::Command::new("git")
    //             .arg("push")
    //             .arg("--")
    //             .arg(&config.remote_name)
    //             .arg(format!("{}:{}", pr_commit, pr_ref)),
    //     )
    //     .await
    //     .reword("git push failed".to_string())?;

    //     let pull_request_number = gh
    //         .create_pull_request(
    //             message,
    //             config.master_ref.clone(),
    //             pr_ref,
    //             opts.draft,
    //         )
    //         .await?;

    //     if !base_is_master {
    //         let base_github_ref = format!(
    //             "refs/heads/{}",
    //             config.get_base_branch_name(pull_request_number)
    //         );

    //         run_command(
    //             async_process::Command::new("git")
    //                 .arg("push")
    //                 .arg("--")
    //                 .arg(&config.remote_name)
    //                 .arg(format!("{}:{}", base_commit, base_github_ref)),
    //         )
    //         .await
    //         .reword("git push failed".to_string())?;

    //         gh.update_pull_request(
    //             pull_request_number,
    //             PullRequestUpdate {
    //                 base: Some(base_github_ref),
    //                 ..Default::default()
    //             },
    //         )
    //         .await?;
    //     }

    //     output(
    //         "✨",
    //         &format!(
    //             "Created new Pull Request #{}: {}",
    //             pull_request_number,
    //             config.pull_request_url(pull_request_number)
    //         ),
    //     )?;

    //     message.insert(
    //         MessageSection::PullRequest,
    //         config.pull_request_url(pull_request_number),
    //     );

    //     gh.request_reviewers(pull_request_number, requested_reviewers)
    //         .await?;
    // }

    // // If we are stacking, we want to base the GitHub commit for this pull
    // // request on the current state of the stacked-on pull request. If not
    // // stacking, we are basing on the parent of our local branch (the commit in
    // // the remote master lineage from which the branch branches off).
    // let (base_oid, base_github_ref) = match &stacked_on_pull_request {
    //     Some(stacked_on) => (stacked_on.head_oid, stacked_on.head.clone()),
    //     None => (
    //         master_base_oid,
    //         format!("refs/heads/{}", &config.master_branch),
    //     ),
    // };

    // let index = git.cherrypick(prepared_commit.oid, base_oid)?;

    // if index.has_conflicts() {
    //     if let Some(number) = stack_on_number {
    //         return Err(Error::new(formatdoc!(
    //             "
    //             This commit cannot be applied on top of the current version of \
    //             Pull Request #{number}.
    //             You either need to update the other pull request first or you \
    //             have to stack this commit on a different pull request to \
    //             include intermediate commits.
    //         "
    //         )));
    //     } else {
    //         let master_branch = &config.master_branch;
    //         return Err(Error::new(formatdoc!(
    //             "This commit cannot be applied directly on the target branch \
    //              '{master_branch}'.
    //              It probably depends on changes in intermediate commits.
    //              Use the --stack option to declare what Pull Request this one \
    //              should be stacked on."
    //         )));
    //     }
    // }

    // // This is the tree we are getting from cherrypicking the local commit
    // // on the selected base (master or stacked-on Pull Request).
    // let tree_oid = git.write_index(index)?;

    // // Parents of the new Pull Request commit
    // let mut new_pull_request_commit_parents = Vec::<Oid>::new();

    // // Ref name on the GitHub side where we push the new commit (i.e. the Pull
    // // Request branch). This remains null if we don't want to push to GitHub
    // // (that's when a Pull Request exists and is up-to-date)
    // let mut github_ref = None::<String>;

    // if let Some(pr) = &pull_request {
    //     // update existing Pull Request

    //     // Is the tree we get from cherrypicking above different at all from the
    //     // current commit on the Pull Request?
    //     let update_tree = git.get_tree_oid_for_commit(pr.head_oid)? != tree_oid;

    //     // This Pull Request should be based on the commit with oid `base_oid`. Is
    //     // that one already in the direct lineage of the current Pull Request
    //     // commit? (If it isn't we definitely need to update this Pull Request to
    //     // merge in that base.)
    //     let remerge_base = !git.is_based_on(pr.head_oid, base_oid)?;

    //     if update_tree || remerge_base {

    //         // First parent of the new Pull Request commit is always the previous
    //         // Pull Request commit.
    //         new_pull_request_commit_parents.push(pr.head_oid);

    //         // If we need to merge the current base back in, it is the second parent
    //         // of the new commit.
    //         if remerge_base {
    //             new_pull_request_commit_parents.push(base_oid);
    //         }

    //         // The Pull Request branch is taken from `pullRequest`
    //         github_ref = Some(pr.head.clone());

    //         if remerge_base {
    //             output(
    //                 "⚾",
    //                 &format!(
    //                     "Commit was rebased - updating Pull Request #{}",
    //                     pr.number
    //                 ),
    //             )?;
    //         } else {
    //             output(
    //                 "🔁",
    //                 &format!(
    //                     "Commit was changed - updating Pull Request #{}",
    //                     pr.number
    //                 ),
    //             )?;
    //         }
    //     } else {
    //         output("✅", "No update necessary")?;
    //     }
    // } else {
    //     // Create new Pull Request

    //     // Only parent of this first commit on the Pull Request brach is the base.
    //     new_pull_request_commit_parents.push(base_oid);

    //     // Construct the name of the new Pull Request branch.
    //     github_ref = Some(format!(
    //         "refs/heads/{}",
    //         config.get_new_branch_name(
    //             &git.get_all_ref_names()?,
    //             message
    //                 .get(&MessageSection::Title)
    //                 .map(|t| &t[..])
    //                 .unwrap_or("")
    //         )
    //     ));
    // }

    // if let Some(github_ref) = github_ref {
    //     // Create the new commit for this Pull Request.
    //     let new_pr_commit_oid = git.create_pull_request_commit(
    //         prepared_commit.oid,
    //         github_commit_message.as_ref().map(|s| &s[..]),
    //         tree_oid,
    //         &new_pull_request_commit_parents[..],
    //     )?;

    //     // And push it to GitHub.
    //     let git_push = async_process::Command::new("git")
    //         .arg("push")
    //         .arg("--atomic")
    //         .arg("--")
    //         .arg(&config.remote_name)
    //         .arg(format!("{}:{}", new_pr_commit_oid, github_ref))
    //         .stdout(async_process::Stdio::null())
    //         .stderr(async_process::Stdio::piped())
    //         .output()
    //         .await?;

    //     if !git_push.status.success() {
    //         console::Term::stderr().write_all(&git_push.stderr)?;
    //         return Err(Error::new("git push failed"));
    //     }

    //     if pull_request.is_none() {
    //         let pull_request_number = gh
    //             .create_pull_request(
    //                 message,
    //                 base_github_ref.clone(),
    //                 github_ref,
    //                 opts.draft,
    //             )
    //             .await?;

    //         message.insert(
    //             MessageSection::PullRequest,
    //             config.pull_request_url(pull_request_number),
    //         );

    //         gh.request_reviewers(pull_request_number, requested_reviewers)
    //             .await?;
    //     }
    // }

    // if let Some(pull_request) = pull_request {
    //     let mut updates: PullRequestUpdate = Default::default();

    //     if pull_request.base != base_github_ref {
    //         updates.base = Some(base_github_ref);
    //     }

    //     if opts.update_message {
    //         let title = message.get(&MessageSection::Title);
    //         if title != Some(&pull_request.title) {
    //             updates.title = title.cloned();
    //         }
    //     }

    //     let body = if opts.update_message {
    //         build_github_body(message)
    //     } else {
    //         let mut sections = pull_request.sections.clone();

    //         if let Some(stacked_on) = message.get(&MessageSection::StackedOn) {
    //             sections.insert(MessageSection::StackedOn, stacked_on.clone());
    //         } else {
    //             sections.remove(&MessageSection::StackedOn);
    //         }

    //         build_github_body(&sections)
    //     };

    //     if pull_request.body.as_ref() != Some(&body) {
    //         updates.body = Some(body);
    //     }

    //     if updates.base.is_some()
    //         || updates.title.is_some()
    //         || updates.body.is_some()
    //     {
    //         gh.update_pull_request(pull_request.number, updates).await?;
    //     }
    // }
}
