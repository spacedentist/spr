use color_eyre::eyre::{Error, Report, Result, WrapErr as _, bail, eyre};
use indoc::formatdoc;
use std::time::Duration;

use crate::{
    config::{MergeMethod, StackingMode},
    git::PreparedCommit,
    git_remote::PushSpec,
    github::{
        PullRequest, PullRequestStack, PullRequestState, PullRequestUpdate,
        ReviewStatus,
    },
    output::{output, write_commit_title},
};

#[derive(Debug, clap::Parser)]
pub struct LandOptions {
    /// Merge a Pull Request that was created or updated with spr diff
    /// --cherry-pick
    #[clap(long)]
    cherry_pick: bool,
}

pub async fn land(
    opts: LandOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    git.check_no_uncommitted_changes()?;
    let mut prepared_commits = gh.get_prepared_commits()?;

    // In the github-stack stacking mode, if the Pull Request of this commit
    // is part of a stack on GitHub, we land it together with all Pull
    // Requests below it.
    if config.stacking_mode == StackingMode::GitHubStack
        && let Some(number) = prepared_commits
            .last()
            .and_then(|pc| pc.pull_request_number)
        && let Some(stack) = gh.find_pull_request_stack(number).await?
    {
        return land_stack(git, gh, config, &mut prepared_commits, stack).await;
    }

    let based_on_unlanded_commits = prepared_commits.len() > 1;

    if based_on_unlanded_commits && !opts.cherry_pick {
        return Err(Error::msg(formatdoc!(
            "Cannot land a commit whose parent is not on {master}. To land \
             this commit, rebase it so that it is a direct child of {master}.
             Alternatively, if you used the `--cherry-pick` option with `spr \
             diff`, then you can pass it to `spr land`, too.",
            master = &config.master_ref.branch_name(),
        )));
    }

    let prepared_commit = match prepared_commits.last_mut() {
        Some(c) => c,
        None => {
            output("👋", "Branch is empty - nothing to do. Good bye!")?;
            return Ok(());
        }
    };

    write_commit_title(prepared_commit)?;

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

    if config.require_approval
        && pull_request.review_status != Some(ReviewStatus::Approved)
    {
        bail!("This Pull Request has not been approved on GitHub.");
    }

    let base_is_master = pull_request.base.is_master_branch();

    if config.merge_method == MergeMethod::Merge
        && config.is_spr_base_branch(&pull_request.base)
    {
        // Merging this Pull Request with a merge commit would bring the
        // commits on the base branch that spr created into the history of the
        // master branch.
        bail!(
            "This Pull Request uses a base branch created by spr, which does \
             not work with merge method '{}'.",
            config.merge_method,
        );
    }

    output("🛫", "Getting started...")?;

    // Fetch current master from GitHub.
    let current_master =
        gh.remote().fetch_branch(config.master_ref.branch_name())?;

    let index = git.cherrypick(prepared_commit.oid, current_master)?;

    if index.has_conflicts() {
        return Err(Error::msg(formatdoc!(
            "This commit cannot be applied on top of the '{master}' branch.
             Please rebase this commit.{unlanded}",
            master = &config.master_ref.branch_name(),
            unlanded = if based_on_unlanded_commits {
                " You may also have to land commits that this commit depends on first."
            } else {
                ""
            },
        )));
    }

    // This is the tree we are getting from cherrypicking the local commit
    // on the selected base (master or stacked-on Pull Request).
    let our_tree_oid = git.write_index(index)?;

    // Now let's predict what merging the PR into the master branch would
    // produce.
    let merge_index = {
        let repo = git.repo();
        let current_master = repo.find_commit(current_master)?;
        let pr_head = repo.find_commit(pull_request.head_oid)?;
        repo.merge_commits(&current_master, &pr_head, None)
    }?;

    let merge_matches_cherrypick = if merge_index.has_conflicts() {
        false
    } else {
        let merge_tree_oid = git.write_index(merge_index)?;
        merge_tree_oid == our_tree_oid
    };

    if !merge_matches_cherrypick {
        return Err(Error::msg(formatdoc!(
            "This commit has been updated and/or rebased since the pull \
             request was last updated. Please run `spr diff` to update the \
             pull request and then try `spr land` again!"
        )));
    }

    // Okay, we are confident now that the PR can be merged and the result of
    // that merge would be a master commit with the same tree as if we
    // cherry-picked the commit onto master.
    let mut pr_head_oid = pull_request.head_oid;

    if !base_is_master {
        // The base of the Pull Request on GitHub is not set to master. This
        // means the Pull Request uses a base branch. We tested above that
        // merging the Pull Request branch into the master branch produces the
        // intended result (the same as cherry-picking the local commit onto
        // master), so what we want to do is actually merge the Pull Request as
        // it is into master. Hence, we change the base to the master branch.
        //
        // Before we do that, there is one more edge case to look out for: if
        // the base branch contains changes that have since been landed on
        // master, then Git might be able to figure out that these changes
        // appear both in the pull request branch (via the merge branch) and in
        // master, but are identical in those two so it is not a merge conflict
        // but can go ahead. The result of this in master if we merge now is
        // correct, but there is one problem: when looking at the Pull Request
        // in GitHub after merging, it will show these change as part of the
        // Pull Request. So when you look at the changed files of the Pull
        // Request, you will see both changes in this commit (great!) and those
        // in the base branch (a previous commit that has already been landed on
        // master - not great!). This is because the changes shown are the ones
        // that happened on this Pull Request branch (now including the base
        // branch) since it branched off master. This can include changes in the
        // base branch that are already on master, but were added to master
        // after the Pull Request branch branched from master.
        // The solution is to merge current master into the Pull Request branch.
        // Doing that now means that the final changes done by this Pull Request
        // are only the changes that are not yet in master. That's what we want.
        // This final merge never introduces any changes to the Pull Request. In
        // fact, the tree that we use for the merge commit is the one we got
        // above from the cherry-picking of this commit on master.

        // The commit on the base branch that the PR branch is currently based on
        let pr_base_oid =
            git.repo().merge_base(pr_head_oid, pull_request.base_oid)?;
        let pr_base_tree = git.get_tree_oid_for_commit(pr_base_oid)?;

        let pr_master_base =
            git.repo().merge_base(pr_base_oid, current_master)?;
        let pr_master_base_tree =
            git.get_tree_oid_for_commit(pr_master_base)?;

        if pr_base_tree != pr_master_base_tree {
            // So the current file contents of the base branch are not the same
            // as those of the master branch commit that the base branch is
            // based on. In other words, the base branch is currently not
            // "empty". Or, the base branch has changes in them. These changes
            // must all have been landed on master in the meantime (after this
            // base branch was branched off) or otherwise we would have aborted
            // this whole operation further above. But in order not to show them
            // as part of this Pull Request after landing, we have to make clear
            // those are changes in master, not in this Pull Request.
            // Here comes the additional merge-in-master commit on the Pull
            // Request branch that achieves that!

            pr_head_oid = git.create_derived_commit(
                pr_head_oid,
                &format!(
                    "[𝘀𝗽𝗿] landed version\n\nCreated using spr {}",
                    env!("CARGO_PKG_VERSION"),
                ),
                our_tree_oid,
                &[pr_head_oid, current_master],
            )?;

            gh.remote()
                .push_to_remote(&[PushSpec {
                    oid: Some(pr_head_oid),
                    remote_ref: pull_request.head.on_github(),
                }])
                .wrap_err("git push failed")?;
        }

        gh.update_pull_request(
            pull_request_number,
            PullRequestUpdate {
                base: Some(config.master_ref.branch_name().to_string()),
                ..Default::default()
            },
        )
        .await?;
    }

    // Check whether GitHub says this PR is mergeable. This happens in a
    // retry-loop because recent changes to the Pull Request can mean that
    // GitHub has not finished the mergeability check yet.
    let mut attempts = 0;
    let result = loop {
        attempts += 1;

        let mergeability = gh
            .get_pull_request_mergeability(pull_request_number)
            .await?;

        if mergeability.head_oid != pr_head_oid {
            break Err(eyre!(
                "The Pull Request seems to have been updated externally. Please try again!"
            ));
        }

        if mergeability.base.is_master_branch()
            && mergeability.mergeable.is_some()
        {
            if mergeability.mergeable != Some(true) {
                break Err(Error::msg(formatdoc!(
                    "GitHub concluded the Pull Request is not mergeable at \
                    this point. Please rebase your changes and try again!"
                )));
            }

            if let Some(merge_commit) = mergeability.merge_commit {
                gh.remote().fetch_from_remote(&[], &[merge_commit])?;

                if git.get_tree_oid_for_commit(merge_commit)? != our_tree_oid {
                    return Err(Error::msg(formatdoc!(
                    "This commit has been updated and/or rebased since the pull
                     request was last updated. Please run `spr diff` to update the pull
                     request and then try `spr land` again!"
                )));
                }
            };

            break Ok(());
        }

        if attempts >= 10 {
            // After ten failed attempts we give up.
            break Err(eyre!(
                "GitHub Pull Request did not update. Please try again!"
            ));
        }

        // Wait one second before retrying
        tokio::time::sleep(Duration::from_secs(1)).await;
    };

    let result = match result {
        Ok(()) => {
            // We have checked that merging the Pull Request branch into the master
            // branch produces the intended result, and that's independent of whether we
            // used a base branch with this Pull Request or not. We have made sure the
            // target of the Pull Request is set to the master branch. So let GitHub do
            // the merge now!
            octocrab::instance()
                .pulls(&config.owner, &config.repo)
                .merge(pull_request_number)
                .method(match config.merge_method {
                    MergeMethod::Squash => {
                        octocrab::params::pulls::MergeMethod::Squash
                    }
                    MergeMethod::Merge => {
                        octocrab::params::pulls::MergeMethod::Merge
                    }
                })
                .title(pull_request.title)
                .message(pull_request.message.to_github_body_for_merging())
                .sha(format!("{}", pr_head_oid))
                .send()
                .await
                .map_err(Report::new)
                .and_then(|merge| {
                    if merge.merged {
                        Ok(merge)
                    } else {
                        Err(eyre!(
                            "GitHub Pull Request merge failed: {}",
                            merge.message.unwrap_or_default()
                        ))
                    }
                })
        }
        Err(err) => Err(err),
    };

    let merge = match result {
        Ok(merge) => merge,
        Err(mut error) => {
            output("❌", "GitHub Pull Request merge failed")?;

            // If we changed the target branch of the Pull Request earlier, then
            // undo this change now.
            if !base_is_master {
                let result = gh
                    .update_pull_request(
                        pull_request_number,
                        PullRequestUpdate {
                            base: Some(
                                pull_request.base.on_github().to_string(),
                            ),
                            ..Default::default()
                        },
                    )
                    .await;
                if let Err(e) = result {
                    error = error.wrap_err(e);
                }
            }

            return Err(error);
        }
    };

    output("🛬", "Landed!")?;

    // Pull Requests stacked on this one (in chain stacking mode) are based on
    // its branch, which we are going to delete below. Base them on master now,
    // which is where their changes have to go next.
    // The Pull Request has landed at this point, so failing here is not an
    // error. But we must not delete the branch then, as GitHub would close
    // the Pull Requests based on it.
    let delete_head_branch = match gh
        .retarget_pull_requests(&pull_request.head, &config.master_ref)
        .await
    {
        Ok(retargeted) => {
            for number in retargeted {
                output(
                    "🎯",
                    &format!(
                        "Changed the base of Pull Request #{} to {}",
                        number,
                        config.master_ref.branch_name()
                    ),
                )?;
            }
            true
        }
        Err(error) => {
            output(
                "⚠️",
                &format!(
                    "Could not change the base of Pull Requests stacked on \
                     this one, so not deleting branch {}: {:#}",
                    pull_request.head.branch_name(),
                    error
                ),
            )?;
            false
        }
    };

    // Rebase us on top of the now-landed commit
    if let Some(sha) = merge.sha {
        let new_parent_oid = git2::Oid::from_str(&sha)?;
        // Try this up to three times, because fetching the very moment after
        // the merge might still not find the new commit.
        for i in 0..3 {
            // Fetch current master and the merge commit from GitHub.
            let result = gh.remote().fetch_from_remote(&[], &[new_parent_oid]);

            if result.is_ok() {
                break;
            } else if i == 2 {
                return result
                    .map(|_| ())
                    .context("git fetch failed".to_string());
            }
        }
        git.rebase_commits(&mut prepared_commits[..], new_parent_oid)
            .context(
                "The automatic rebase failed - please rebase manually!"
                    .to_string(),
            )?;
    }

    let mut push_specs = Vec::new();

    if delete_head_branch {
        push_specs.push(PushSpec {
            oid: None,
            remote_ref: pull_request.head.on_github(),
        });
    }

    // Delete the base branch too, if spr created it for this Pull Request. If
    // the Pull Request was stacked on another Pull Request, its base branch
    // is the branch of that other Pull Request, which we must not delete.
    if config.is_spr_base_branch(&pull_request.base) {
        push_specs.push(PushSpec {
            oid: None,
            remote_ref: pull_request.base.on_github(),
        });
    }

    if !push_specs.is_empty() {
        gh.remote().push_to_remote(&push_specs)?;
    }

    Ok(())
}

/// Land the Pull Request of the current commit, which is part of a stack on
/// GitHub, together with the Pull Requests of all commits below it. GitHub
/// merges all Pull Requests in a stack up to the requested one.
async fn land_stack(
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
    prepared_commits: &mut [PreparedCommit],
    stack: PullRequestStack,
) -> Result<()> {
    // Load the Pull Requests of all commits on the local branch, and check
    // that each of them is in the state we expect.
    let mut pull_requests: Vec<PullRequest> = Vec::new();
    for prepared_commit in prepared_commits.iter() {
        write_commit_title(prepared_commit)?;

        let Some(number) = prepared_commit.pull_request_number else {
            bail!("This commit does not refer to a Pull Request.");
        };
        output("#️⃣ ", &format!("Pull Request #{number}"))?;

        let pull_request = gh.clone().get_pull_request(number).await?;

        if pull_request.state != PullRequestState::Open {
            bail!(
                "This Pull Request is already closed! Rebase your local branch \
                 on {}.",
                config.master_ref.branch_name()
            );
        }

        if config.require_approval
            && pull_request.review_status != Some(ReviewStatus::Approved)
        {
            bail!("This Pull Request has not been approved on GitHub.");
        }

        // The Pull Request must be based on the Pull Request of the commit
        // below (or master, for the first one), and contain exactly what the
        // local commit contains.
        let expected_base = pull_requests
            .last()
            .map(|pr| pr.head.branch_name())
            .unwrap_or(config.master_ref.branch_name());
        if pull_request.base.branch_name() != expected_base
            || git.get_tree_oid_for_commit(pull_request.head_oid)?
                != git.get_tree_oid_for_commit(prepared_commit.oid)?
        {
            bail!(
                "Pull Request #{number} does not reflect the local commit. \
                 Please run `spr diff --all` to update the Pull Requests and \
                 then try `spr land` again!"
            );
        }

        pull_requests.push(pull_request);
    }

    let numbers: Vec<u64> = pull_requests.iter().map(|pr| pr.number).collect();
    if !stack.open_pull_requests().starts_with(&numbers) {
        bail!(
            "The stack on GitHub (#{}) does not match the local branch. Please \
             run `spr diff --all` to update it and then try `spr land` again!",
            stack.number
        );
    }

    let Some(top) = pull_requests.last() else {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return Ok(());
    };

    output("🛫", "Getting started...")?;

    // Check that merging the top Pull Request into the current master branch
    // gives the same result as applying the local commits to it. (Merging
    // the Pull Requests of a stack one by one ends up with the same tree.)
    let current_master =
        gh.remote().fetch_branch(config.master_ref.branch_name())?;
    {
        let repo = git.repo();
        let master_commit = repo.find_commit(current_master)?;
        let local_index = repo.merge_trees(
            &repo.find_commit(prepared_commits[0].parent_oid)?.tree()?,
            &master_commit.tree()?,
            &repo
                .find_commit(prepared_commits[prepared_commits.len() - 1].oid)?
                .tree()?,
            None,
        )?;
        if local_index.has_conflicts() {
            bail!(
                "The local commits cannot be applied on top of the '{}' \
                 branch. Please rebase them.",
                config.master_ref.branch_name()
            );
        }
        let merge_index = repo.merge_commits(
            &master_commit,
            &repo.find_commit(top.head_oid)?,
            None,
        )?;
        if merge_index.has_conflicts()
            || git.write_index(merge_index)? != git.write_index(local_index)?
        {
            bail!(
                "Merging the Pull Requests would not give the same result as \
                 the local commits. Please rebase your local branch on '{}', \
                 run `spr diff --all` and then try `spr land` again!",
                config.master_ref.branch_name()
            );
        }
    }

    // Let GitHub merge the stack up to the top Pull Request
    let request = gh
        .merge_pull_request_async(
            top.number,
            top.head_oid,
            config.merge_method,
            &top.title,
            &top.message.to_github_body_for_merging(),
        )
        .await?;

    let mut status = request;
    let uuid = status.details.uuid.clone();
    for _ in 0..60 {
        if status.status != "pending" {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let Some(ref uuid) = uuid else { break };
        status = gh.get_async_merge_status(top.number, uuid).await?;
    }

    let merge_sha = match status.status.as_str() {
        "merged" => status.details.sha.clone(),
        "enqueued" => {
            output(
                "🚦",
                "The Pull Requests were added to the merge queue. Rebase your \
                 local branch once they have landed.",
            )?;
            return Ok(());
        }
        "pending" => bail!(
            "GitHub has not finished merging the Pull Requests yet. Check on \
             GitHub, and rebase your local branch once they have landed."
        ),
        _ => bail!(
            "GitHub Pull Request merge failed: {}",
            status.details.message.unwrap_or_default()
        ),
    };

    output(
        "🛬",
        &format!(
            "Landed Pull Request{} {}!",
            if numbers.len() > 1 { "s" } else { "" },
            numbers
                .iter()
                .map(|number| format!("#{number}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )?;

    // Pull Requests further up the stack were based on the top Pull Request's
    // branch. GitHub changes their base itself, but make sure that has
    // happened before we delete the branch.
    let delete_branches = match gh
        .retarget_pull_requests(&top.head, &config.master_ref)
        .await
    {
        Ok(_) => true,
        Err(error) => {
            output(
                "⚠️",
                &format!(
                    "Could not change the base of Pull Requests stacked on \
                     the landed ones, so not deleting any branches: {error:#}"
                ),
            )?;
            false
        }
    };

    // Rebase us on top of the now-landed commits
    if let Some(sha) = merge_sha {
        let new_parent_oid = git2::Oid::from_str(&sha)?;
        // Try this up to three times, because fetching the very moment after
        // the merge might still not find the new commit.
        for i in 0..3 {
            let result = gh.remote().fetch_from_remote(&[], &[new_parent_oid]);
            if result.is_ok() {
                break;
            } else if i == 2 {
                return result
                    .map(|_| ())
                    .context("git fetch failed".to_string());
            }
        }
        git.rebase_commits(prepared_commits, new_parent_oid)
            .context(
                "The automatic rebase failed - please rebase manually!"
                    .to_string(),
            )?;
    }

    if delete_branches {
        let push_specs: Vec<_> = pull_requests
            .iter()
            .map(|pr| PushSpec {
                oid: None,
                remote_ref: pr.head.on_github(),
            })
            .collect();
        // GitHub may have deleted the branches already, if the repository is
        // set up to do that after merging.
        if let Err(error) = gh.remote().push_to_remote(&push_specs) {
            output(
                "⚠️",
                &format!("Could not delete Pull Request branches: {error:#}"),
            )?;
        }
    }

    Ok(())
}
