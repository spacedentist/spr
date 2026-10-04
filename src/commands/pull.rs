use color_eyre::eyre::{Result, bail, eyre};
use git2::Oid;
use indoc::formatdoc;

use crate::{
    git::PreparedCommit,
    github::PullRequestState,
    output::{output, write_commit_title},
    pr_commits::merge_trees,
    remote_changes::{PullOutcome, pull_changes},
};

#[derive(Debug, clap::Parser)]
pub struct PullOptions {
    /// Pull changes into all commits of the branch, not just the HEAD
    /// commit
    #[clap(long, short = 'a')]
    all: bool,

    /// Pull changes even if the Pull Request has merge commits or was
    /// rewritten (force-pushed) since spr last updated it
    #[clap(long)]
    force: bool,

    /// Show what would be done, without changing anything
    #[clap(long)]
    dry_run: bool,
}

pub async fn pull(
    opts: PullOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    if !opts.dry_run {
        git.check_no_uncommitted_changes()?;
    }

    let prepared_commits = gh.get_prepared_commits()?;
    let Some(last) = prepared_commits.last() else {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return Ok(());
    };
    let old_head = last.oid;
    let master_base_oid = prepared_commits[0].parent_oid;
    let first_index = if opts.all {
        0
    } else {
        prepared_commits.len() - 1
    };

    // Go through the commits from the bottom, working out the new commits.
    // Nothing is changed until the end, and only if everything worked.
    let mut parent_oid = master_base_oid;
    let mut rewrites = Vec::new();
    // The expected heads to record: Spr-Id, Pull Request head, and the one
    // recorded before (for undoing)
    let mut records: Vec<(String, Oid, Oid)> = Vec::new();

    for (index, prepared_commit) in prepared_commits.iter().enumerate() {
        let mut tree = git.get_tree_oid_for_commit(prepared_commit.oid)?;

        if index >= first_index {
            write_commit_title(prepared_commit)?;

            // Rebase onto the amended parent commit, if there is one
            if parent_oid != prepared_commit.parent_oid {
                let Some(rebased) = merge_trees(
                    git,
                    git.get_tree_oid_for_commit(prepared_commit.parent_oid)?,
                    git.get_tree_oid_for_commit(parent_oid)?,
                    tree,
                )?
                else {
                    return stop(
                        &opts,
                        "Rebasing this commit onto the changes pulled into \
                         the commits below has conflicts. Nothing was \
                         changed. You can look at the Pull Requests with \
                         `spr patch`, or overwrite the changes on them with \
                         `spr diff --force`."
                            .to_string(),
                    );
                };
                tree = rebased;
            }

            let context = Context {
                opts: &opts,
                git,
                config,
                master_base_oid,
                is_head: index == prepared_commits.len() - 1,
            };
            match pull_commit(&context, gh, prepared_commit, tree).await? {
                Step::Skip => (),
                Step::Stop => return Ok(()),
                Step::Record {
                    spr_id,
                    head,
                    expected_head,
                    new_tree,
                } => {
                    records.push((spr_id, head, expected_head));
                    if let Some(new_tree) = new_tree {
                        tree = new_tree;
                    }
                }
            }
        }

        // Create the new commit, if it changed
        if tree != git.get_tree_oid_for_commit(prepared_commit.oid)?
            || parent_oid != prepared_commit.parent_oid
        {
            let new_oid =
                amend_commit(git, prepared_commit.oid, tree, parent_oid)?;
            rewrites.push((prepared_commit.oid, new_oid));
            parent_oid = new_oid;
        } else {
            parent_oid = prepared_commit.oid;
        }
    }

    if opts.dry_run {
        return Ok(());
    }

    if parent_oid != old_head {
        git.replace_head(old_head, parent_oid, &rewrites, "spr pull")?;
    }
    for (spr_id, head, _) in &records {
        git.set_expected_head(spr_id, *head)?;
    }

    if parent_oid != old_head {
        output("✅", "Pulled the changes into your local branch")?;
        // Commands to undo the pull, one per line, so they can be copied
        output("↩️", "To undo:")?;
        let term = console::Term::stdout();
        term.write_line(&format!("      git reset --keep {old_head}"))?;
        for (spr_id, _, expected_head) in &records {
            term.write_line(&format!(
                "      git update-ref refs/spr/{spr_id}/head {expected_head}"
            ))?;
        }
    }

    Ok(())
}

/// What to do with a commit after looking at its Pull Request
enum Step {
    /// Leave the commit as it is (`--all`: not tracked)
    Skip,
    /// Stop (in a dry run, where we'd fail otherwise)
    Stop,
    /// Record the Pull Request's head as the commit's expected head (and
    /// give the commit the new tree, if there is one)
    Record {
        spr_id: String,
        head: Oid,
        expected_head: Oid,
        new_tree: Option<Oid>,
    },
}

/// What `pull_commit` needs to know
struct Context<'a> {
    opts: &'a PullOptions,
    git: &'a crate::git::Git,
    config: &'a crate::config::Config,
    /// The master commit the local branch is based on
    master_base_oid: Oid,
    /// Whether the commit is the HEAD commit
    is_head: bool,
}

/// Pull the changes of the Pull Request of a commit, which (after rebasing
/// it on pulled changes below) has the tree `tree`
async fn pull_commit(
    context: &Context<'_>,
    gh: &mut crate::github::GitHub,
    prepared_commit: &PreparedCommit,
    tree: Oid,
) -> Result<Step> {
    let Context {
        opts,
        git,
        config,
        master_base_oid,
        is_head,
    } = *context;
    // A commit spr can't pull changes into: with `--all`, skip it,
    // otherwise fail with the explanation
    let not_tracked = |reason: &str, explanation: &str| -> Result<Step> {
        if opts.all {
            output("⏭️", &format!("{reason} - skipping"))?;
            return Ok(Step::Skip);
        }
        bail!("{reason}. {explanation}");
    };

    let Some(number) = prepared_commit.pull_request_number else {
        return not_tracked(
            "This commit has no Pull Request",
            "There is nothing to pull.",
        );
    };
    let Some(spr_id) = prepared_commit.message.spr_id() else {
        return not_tracked(
            "This commit has no Spr-Id",
            "Without it, spr can't tell what changed on its Pull Request. \
             (See `spr diff --spr-id`.)",
        );
    };
    let Some(expected_head) = git.get_expected_head(spr_id)? else {
        return not_tracked(
            "spr has no record of this commit's Pull Request yet",
            "So it can't tell what changed on it. The next `spr diff` makes \
             the record.",
        );
    };

    let pull_request = gh.clone().get_pull_request(number).await?;
    if pull_request.state != PullRequestState::Open {
        if opts.all {
            output(
                "⏭️",
                &format!("Pull Request #{number} is closed - skipping"),
            )?;
            return Ok(Step::Skip);
        }
        bail!("Pull Request #{number} is closed.");
    }

    let head = pull_request.head_oid;
    let outcome = pull_changes(
        git,
        tree,
        master_base_oid,
        head,
        expected_head,
        opts.force,
        &format!(
            "Changes pushed to Pull Request #{number}\n\n\
             Created by spr pull, for cherry-picking into the local commit."
        ),
    )?;

    let record = |new_tree| {
        Ok(Step::Record {
            spr_id: spr_id.to_string(),
            head,
            expected_head,
            new_tree,
        })
    };

    match outcome {
        PullOutcome::NothingToPull => {
            output("✅", "Nothing to pull")?;
            record(None)
        }
        PullOutcome::Merged { tree: new_tree } => {
            output(
                if opts.dry_run { "🔍" } else { "⬇️" },
                &format!(
                    "{} changes from Pull Request #{number}:",
                    if opts.dry_run {
                        "Would pull"
                    } else {
                        "Pulling"
                    }
                ),
            )?;
            write_diffstat(git, tree, new_tree)?;
            record(Some(new_tree))
        }
        PullOutcome::Conflict { changes } => {
            let message = if is_head {
                formatdoc!(
                    "The changes pushed to Pull Request #{number} conflict \
                     with your local commit. Nothing was changed.
                     To apply them, run:
                       git cherry-pick --no-commit {changes}
                     resolve the conflicts, and then run:
                       git commit --amend --no-edit
                       spr diff --force
                     (To give up while resolving: git reset --merge)"
                )
            } else {
                formatdoc!(
                    "The changes pushed to Pull Request #{number} conflict \
                     with your local commit {short_id}. Nothing was changed.
                     To apply them to that commit, use `git rebase -i` and \
                     add the line `fixup {changes}` after it. Then update \
                     the Pull Request with `spr diff --force`.",
                    short_id = prepared_commit.short_id,
                )
            };
            stop(opts, message).map(|()| Step::Stop)
        }
        PullOutcome::NotLinear => {
            let master = config.master_ref.branch_name();
            let master_tip = gh.remote().fetch_branch(master)?;
            let pr_master = git.repo().merge_base(head, master_tip)?;
            let message = if !git.is_ancestor(pr_master, master_base_oid)? {
                format!(
                    "Pull Request #{number} is based on `{master}` at \
                     {pr_master}, your local commit on an older one. Rebase \
                     your local branch onto it first (e.g. `git rebase \
                     {pr_master}`), then run `spr pull --force`."
                )
            } else {
                format!(
                    "Pull Request #{number} has merge commits or was \
                     rewritten (force-pushed) since spr last updated it, so \
                     spr can't be sure which changes were made by somebody \
                     else. To pull them anyway, run `spr pull --force` (and \
                     check the result). Or look at the Pull Request with \
                     `spr patch {number}`, or overwrite it with \
                     `spr diff --force`."
                )
            };
            stop(opts, message).map(|()| Step::Stop)
        }
    }
}

/// Stop: fail with the given message, or in a dry run, report that we'd
/// fail
fn stop(opts: &PullOptions, message: String) -> Result<()> {
    if opts.dry_run {
        output("🔍", &format!("Would stop: {message}"))?;
        return Ok(());
    }
    Err(eyre!(message))
}

/// A new version of the commit `oid`, with the given tree and parent (same
/// message and author)
fn amend_commit(
    git: &crate::git::Git,
    oid: Oid,
    tree: Oid,
    parent: Oid,
) -> Result<Oid> {
    let repo = git.repo();
    let commit = repo.find_commit(oid)?;
    let committer = repo.signature().unwrap_or_else(|_| commit.committer());
    Ok(repo.commit(
        None,
        &commit.author(),
        &committer,
        &String::from_utf8_lossy(commit.message_bytes()),
        &repo.find_tree(tree)?,
        &[&repo.find_commit(parent)?],
    )?)
}

/// Print the diffstat of the changes between two trees
fn write_diffstat(git: &crate::git::Git, from: Oid, to: Oid) -> Result<()> {
    let repo = git.repo();
    let diff = repo.diff_tree_to_tree(
        Some(&repo.find_tree(from)?),
        Some(&repo.find_tree(to)?),
        None,
    )?;
    let stats = diff.stats()?.to_buf(git2::DiffStatsFormat::FULL, 72)?;
    let term = console::Term::stdout();
    for line in String::from_utf8_lossy(&stats).lines() {
        term.write_line(&format!("     {line}"))?;
    }
    Ok(())
}
