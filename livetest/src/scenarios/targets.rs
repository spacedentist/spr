//! Pull Requests for other targets than the default one (`--target`, the
//! `Spr-Target` trailer)

use color_eyre::eyre::Result;

use super::{Ctx, Scenario, check, check_eq, check_output};
use crate::{log::step, util::TARGET_BRANCH_ROOT};

pub const TARGET: Scenario = Scenario {
    name: "target",
    description: "a stack for another branch (--target, Spr-Target), and \
                  landing into it",
    run: target,
};

pub const CHERRY_PICK: Scenario = Scenario {
    name: "cherry-pick-targets",
    description: "--cherry-pick commits for different targets in one run",
    run: cherry_pick_targets,
};

/// Create another target branch for the test (e.g. a release branch), at
/// the head of the run's target branch. Returns its name.
fn create_other_target(ctx: &Ctx, name: &str) -> Result<String> {
    let branch = format!("{TARGET_BRANCH_ROOT}{}/{name}", ctx.run.id);
    let Some(sha) = ctx.api.branch_sha(&ctx.run.target_branch)? else {
        color_eyre::eyre::bail!("The run's target branch doesn't exist");
    };
    ctx.api.create_branch(&branch, &sha)?;
    step(format!("created the branch {branch}"));
    Ok(branch)
}

/// The full notation of a target branch in the test repository
fn full_target(ctx: &Ctx, branch: &str) -> String {
    format!("github:{}/{}:{branch}", ctx.api.owner, ctx.api.repo)
}

fn target(ctx: &Ctx) -> Result<()> {
    // A release branch, and the default target branch moves on without it
    let release = create_other_target(ctx, "target-release")?;
    let main_head = ctx.somebody_edits(
        &ctx.run.target_branch,
        "target-main-only.txt",
        "only on main\n",
    )?;
    let release_head = ctx.fetch(&release)?;
    ctx.env
        .git(&["switch", "--quiet", "-c", "fix", &release_head])?;

    // A Pull Request for the release branch, with the branch name alone
    let a = ctx.commit(&[("target-a.txt", "a\n")], "Fix for release")?;
    ctx.env.spr_ok(&["--target", &release, "diff"])?;
    check_eq(
        "the commit got a Spr-Target trailer with the full target",
        ctx.trailer("HEAD", "Spr-Target")?,
        Some(full_target(ctx, &release)),
    )?;
    let number_a = ctx.pr_number("HEAD")?;
    let (_, pr) = ctx.check_pr_matches(number_a, "HEAD")?;
    check_eq(
        format!("PR #{number_a} targets {release}"),
        &pr.base_ref,
        &release,
    )?;
    check(
        format!("the commit {a} was only rewritten for its trailers"),
        ctx.tree("HEAD")? == ctx.tree(&a)?,
    )?;

    // A commit on top without the trailer is for the default target, but
    // the commit below isn't: the stack is inconsistent
    ctx.commit(&[("target-b.txt", "b\n")], "Second fix for release")?;
    let output = ctx.env.spr_fails(&["diff"])?;
    check_output(&output, "another target than the HEAD commit")?;

    // With --target, it gets the trailer and a Pull Request stacked on the
    // first one
    ctx.env.spr_ok(&["--target", &release, "diff"])?;
    check_eq(
        "the second commit got a Spr-Target trailer",
        ctx.trailer("HEAD", "Spr-Target")?,
        Some(full_target(ctx, &release)),
    )?;
    let number_b = ctx.pr_number("HEAD")?;
    let (_, pr) = ctx.check_pr_matches(number_b, "HEAD")?;
    let base_branch_prefix =
        format!("{}{release}.", ctx.run.test_prefix("target"));
    check(
        format!(
            "PR #{number_b} targets a base branch for {release} ({})",
            pr.base_ref
        ),
        pr.base_ref.starts_with(&base_branch_prefix),
    )?;

    // Without --target, spr knows the target from the trailer
    ctx.amend(&[("target-b.txt", "b\nb2\n")])?;
    ctx.env.spr_ok(&["diff", "-m", "Second version"])?;
    ctx.check_pr_matches(number_b, "HEAD")?;
    ctx.env.spr_ok(&["diff", "--all", "-m", "Update all"])?;

    // --target must not contradict the trailer
    let output =
        ctx.env
            .spr_fails(&["--target", &ctx.run.target_branch, "diff"])?;
    check_output(&output, "not github:")?;

    // Land the first one: into the release branch, without the trailer
    ctx.env
        .git(&["switch", "--quiet", "-c", "land", "HEAD~1"])?;
    let local_tree = ctx.tree("HEAD")?;
    ctx.env.spr_ok(&["land"])?;
    let pr = ctx.api.pull_request(number_a)?;
    check(format!("PR #{number_a} is merged"), pr.merged)?;
    let landed = ctx.fetch(&release)?;
    check(
        format!("{release} has the tree of the local commit"),
        ctx.tree(&landed)? == local_tree,
    )?;
    check(
        "the landed commit has no Spr-Target trailer",
        !ctx.message(&landed)?.contains("Spr-Target"),
    )?;
    check_eq(
        format!("{} didn't change", ctx.run.target_branch),
        ctx.fetch(&ctx.run.target_branch)?,
        main_head,
    )
}

fn cherry_pick_targets(ctx: &Ctx) -> Result<()> {
    // A release branch that has a change the default target branch doesn't
    let release = create_other_target(ctx, "cherry-pick-release")?;
    ctx.somebody_edits(&release, "release-only.txt", "only on release\n")?;

    // One commit for the default target, and one for the release branch
    // (the trailer written by hand, with the branch name alone)
    ctx.commit(&[("cherry-main.txt", "main\n")], "Change for main")?;
    ctx.commit(
        &[("cherry-release.txt", "release\n")],
        &format!("Change for release\n\nSpr-Target: {release}"),
    )?;

    let output = ctx.env.spr_fails(&["diff", "--cherry-pick", "--all"])?;
    check_output(&output, "--cherry-pick can't be used with --all")?;

    ctx.env
        .spr_ok(&["diff", "--cherry-pick", "-r", "HEAD~2..HEAD"])?;

    let number_main = ctx.pr_number("HEAD~1")?;
    let pr = ctx.api.pull_request(number_main)?;
    check_eq(
        format!("PR #{number_main} targets {}", ctx.run.target_branch),
        &pr.base_ref,
        &ctx.run.target_branch,
    )?;

    let number_release = ctx.pr_number("HEAD")?;
    let pr = ctx.api.pull_request(number_release)?;
    check_eq(
        format!("PR #{number_release} targets {release}"),
        &pr.base_ref,
        &release,
    )?;
    // Cherry-picked onto the commit of the release branch the local branch
    // is based on (as for the default target), so without the change on
    // the release branch, and without the commit below
    let head = ctx.fetch(&pr.head_ref)?;
    let files = ctx.env.git(&["ls-tree", "--name-only", &head])?;
    let files: Vec<&str> = files.lines().collect();
    check(
        format!(
            "PR #{number_release} has the commit cherry-picked onto the \
             merge base with {release} (cherry-release.txt, but neither \
             cherry-main.txt nor release-only.txt)"
        ),
        files.contains(&"cherry-release.txt")
            && !files.contains(&"cherry-main.txt")
            && !files.contains(&"release-only.txt"),
    )?;

    // Land it into the release branch, although the commit below is for
    // another target
    ctx.env.spr_ok(&["land", "--cherry-pick"])?;
    let pr = ctx.api.pull_request(number_release)?;
    check(format!("PR #{number_release} is merged"), pr.merged)?;
    let landed = ctx.fetch(&release)?;
    let files = ctx.env.git(&["ls-tree", "--name-only", &landed])?;
    let files: Vec<&str> = files.lines().collect();
    check(
        format!(
            "{release} has cherry-release.txt and release-only.txt, but not \
             cherry-main.txt"
        ),
        files.contains(&"cherry-release.txt")
            && files.contains(&"release-only.txt")
            && !files.contains(&"cherry-main.txt"),
    )
}
