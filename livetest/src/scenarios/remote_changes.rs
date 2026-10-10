//! Changes others push to a Pull Request: with a `Spr-Id`, `spr diff` stops
//! instead of reverting them, `--force` overwrites them, and `spr pull`
//! applies them to the local commit.

use color_eyre::eyre::{Result, eyre};

use super::{Ctx, Scenario, check, check_eq, check_output};
use crate::log::{short, step};

pub const REMOTE_CHANGES: Scenario = Scenario {
    name: "remote-changes",
    description: "spr diff stops on changes others pushed; --force overwrites",
    run: remote_changes,
};

pub const PULL: Scenario = Scenario {
    name: "pull",
    description: "spr pull applies changes others pushed",
    run: pull,
};

pub const PULL_CONFLICT: Scenario = Scenario {
    name: "pull-conflict",
    description: "spr pull with conflicts: the cherry-pick route it explains",
    run: pull_conflict,
};

pub const UPDATE_BRANCH: Scenario = Scenario {
    name: "update-branch",
    description: "after GitHub's \"Update branch\": rebase first",
    run: update_branch,
};

/// A file with a few lines, to change them independently
const LINES: &str = "a\nb\nc\nd\ne\nf\ng\n";

/// `LINES` with line `n` (0-based) replaced
fn edit(content: &str, n: usize, line: &str) -> String {
    let mut lines: Vec<&str> = content.lines().collect();
    lines[n] = line;
    lines.join("\n") + "\n"
}

/// Check that spr recorded the given head as the expected head of the
/// Pull Request of the HEAD commit
fn check_recorded(ctx: &Ctx, number: u64, head: &str) -> Result<()> {
    check_eq(
        format!("spr recorded {} as PR #{number}'s head", short(head)),
        ctx.expected_head("HEAD")?.as_deref(),
        Some(head),
    )
}

/// Commit the file, and create a Pull Request with a Spr-Id. Returns the
/// Pull Request's number, branch and head.
fn create(ctx: &Ctx, path: &str, title: &str) -> Result<(u64, String, String)> {
    ctx.commit(&[(path, LINES)], title)?;
    ctx.env.spr_ok(&["diff", "--spr-id"])?;
    let number = ctx.pr_number("HEAD")?;
    let (head, pr) = ctx.check_pr_matches(number, "HEAD")?;
    check_recorded(ctx, number, &head)?;
    Ok((number, pr.head_ref, head))
}

fn remote_changes(ctx: &Ctx) -> Result<()> {
    let (number, branch, _) = create(ctx, "remote.txt", "Remote changes test")?;

    let theirs =
        ctx.somebody_edits(&branch, "theirs.txt", "by somebody else\n")?;
    ctx.amend(&[("remote.txt", &edit(LINES, 0, "A"))])?;

    // spr diff stops, and leaves the Pull Request alone
    let output = ctx.env.spr_fails(&["diff", "-m", "Local change"])?;
    check_output(&output, "has changes that aren't in your local commit")?;
    check_output(&output, "spr pull")?;
    check_eq(
        format!(
            "PR #{number} still has their change ({}) as its head",
            short(&theirs)
        ),
        ctx.fetch(&branch)?,
        theirs.clone(),
    )?;

    let output = ctx.env.spr_ok(&["diff", "--dry-run"])?;
    check_output(&output, "Would stop")?;

    // --force overwrites their changes, with a new commit on top
    ctx.env
        .spr_ok(&["diff", "--force", "-m", "Overwrite their changes"])?;
    let (head, _) = ctx.check_pr_matches(number, "HEAD")?;
    check(
        format!(
            "the new head {} is on top of their change {} (no force-push)",
            short(&head),
            short(&theirs)
        ),
        ctx.is_ancestor(&theirs, &head)?,
    )?;
    check_recorded(ctx, number, &head)
}

fn pull(ctx: &Ctx) -> Result<()> {
    let (number, branch, _) = create(ctx, "pull.txt", "Pull test")?;

    // Somebody fixes line 2 on GitHub; locally, line 6 changed (not pushed)
    let theirs =
        ctx.somebody_edits(&branch, "pull.txt", &edit(LINES, 1, "B (theirs)"))?;
    let local = edit(LINES, 5, "F (local)");
    ctx.amend(&[("pull.txt", &local)])?;

    let output = ctx.env.spr_ok(&["pull"])?;
    check_output(&output, "Pulling changes from Pull Request")?;
    let content = std::fs::read_to_string(ctx.env.repo_dir.join("pull.txt"))?;
    check_eq(
        "pull.txt has their change and the local one",
        content,
        edit(&local, 1, "B (theirs)"),
    )?;
    check_recorded(ctx, number, &theirs)?;

    // Now spr diff goes ahead
    ctx.env.spr_ok(&["diff", "-m", "Local change"])?;
    ctx.check_pr_matches(number, "HEAD")?;

    let output = ctx.env.spr_ok(&["pull"])?;
    check_output(&output, "Nothing to pull")
}

fn pull_conflict(ctx: &Ctx) -> Result<()> {
    let (number, branch, _) =
        create(ctx, "conflict.txt", "Pull conflict test")?;

    ctx.somebody_edits(&branch, "conflict.txt", &edit(LINES, 0, "A (theirs)"))?;
    let before = ctx.amend(&[("conflict.txt", &edit(LINES, 0, "A (ours)"))])?;

    // spr pull changes nothing, and explains how to apply their changes
    let output = ctx.env.spr_fails(&["pull"])?;
    check_output(&output, "git cherry-pick --no-commit")?;
    check_eq(
        format!("the local commit is still {}", short(&before)),
        ctx.rev_parse("HEAD")?,
        before.clone(),
    )?;

    // Follow the instructions: cherry-pick (conflicts), resolve, amend,
    // spr diff --force
    let changes = output
        .split("git cherry-pick --no-commit ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or_else(|| eyre!("No commit to cherry-pick in:\n{output}"))?;
    step(format!("$ git cherry-pick --no-commit {}", short(changes)));
    let cherry_pick = ctx.env.run(ctx.env.git_command(&[
        "cherry-pick",
        "--no-commit",
        changes,
    ]))?;
    check("the cherry-pick has a conflict", !cherry_pick.success)?;
    ctx.write(&[("conflict.txt", &edit(LINES, 0, "A (resolved)"))])?;
    ctx.env.git(&["add", "conflict.txt"])?;
    ctx.env
        .git(&["commit", "--quiet", "--amend", "--no-edit"])?;
    step(format!(
        "resolved the conflict, amended: {}",
        short(&ctx.rev_parse("HEAD")?)
    ));
    ctx.env.spr_ok(&["diff", "--force", "-m", "Resolved"])?;
    ctx.check_pr_matches(number, "HEAD")?;

    let output = ctx.env.spr_ok(&["pull"])?;
    check_output(&output, "Nothing to pull")
}

fn update_branch(ctx: &Ctx) -> Result<()> {
    let (number, _, _) = create(ctx, "update.txt", "Update branch test")?;

    // The target branch moves on, and somebody presses "Update branch"
    ctx.somebody_edits(&ctx.run.target_branch, "newer.txt", "newer\n")?;
    ctx.update_branch(number)?;

    // spr diff and spr pull ask to rebase first
    let output = ctx.env.spr_fails(&["diff", "-m", "Update"])?;
    check_output(&output, "is based on a newer")?;
    let output = ctx.env.spr_fails(&["pull"])?;
    check_output(&output, "Rebase your local branch onto it first")?;

    // After rebasing, spr diff goes ahead
    ctx.env
        .git(&["fetch", "--quiet", "origin", &ctx.run.target_branch])?;
    ctx.env.git(&["rebase", "--quiet", "FETCH_HEAD"])?;
    step(format!(
        "rebased onto {}: {}",
        ctx.run.target_branch,
        short(&ctx.rev_parse("HEAD")?)
    ));
    ctx.env.spr_ok(&["diff", "-m", "Rebased"])?;
    ctx.check_pr_matches(number, "HEAD")?;
    Ok(())
}
