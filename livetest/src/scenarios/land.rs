//! Landing Pull Requests

use color_eyre::eyre::Result;

use super::{Ctx, Scenario, check, check_eq};
use crate::log::short;

pub const SQUASH: Scenario = Scenario {
    name: "land",
    description: "land a Pull Request (squash-merge)",
    run: land_squash,
};

pub const MERGE: Scenario = Scenario {
    name: "land-merge",
    description: "land a Pull Request with a merge commit (spr.mergeMethod)",
    run: land_merge,
};

/// Create a Pull Request and land it. Returns its number, and the landed
/// commit (the target branch's new head).
fn create_and_land(ctx: &Ctx, name: &str) -> Result<(u64, String)> {
    ctx.commit(
        &[(&format!("{name}.txt"), "landed\n")],
        &format!("Test {name}\n\nTo be landed."),
    )?;
    ctx.env.spr_ok(&["diff"])?;
    let number = ctx.pr_number("HEAD")?;
    let local_tree = ctx.tree("HEAD")?;
    ctx.env.spr_ok(&["land"])?;

    let pr = ctx.api.pull_request(number)?;
    check(format!("PR #{number} is merged"), pr.merged)?;
    let landed = ctx.fetch(&ctx.run.target_branch)?;
    check(
        format!(
            "the target branch (now at {}) has the tree of the local commit",
            short(&landed)
        ),
        ctx.tree(&landed)? == local_tree,
    )?;
    check(
        format!(
            "the local branch is on the landed commit {}",
            short(&landed)
        ),
        ctx.rev_parse("HEAD")? == landed,
    )?;
    Ok((number, landed))
}

fn land_squash(ctx: &Ctx) -> Result<()> {
    let (number, landed) = create_and_land(ctx, "land")?;

    // One commit, with the Pull Request's title, and trailers that Git
    // recognises as such
    let parents = ctx.env.git(&["log", "-1", "--format=%P", &landed])?;
    check_eq(
        "the landed commit is a squash commit (one parent)",
        parents.split_whitespace().count(),
        1,
    )?;
    let message = ctx.message(&landed)?;
    check(
        "the landed commit has the Pull Request's title",
        message.starts_with("Test land"),
    )?;
    let trailers = ctx.env.git(&[
        "log",
        "-1",
        "--format=%(trailers:key=Pull-request,valueonly)",
        &landed,
    ])?;
    check(
        format!(
            "Git sees the Pull-request trailer of the landed commit (PR \
             #{number})"
        ),
        trailers.ends_with(&format!("/pull/{number}")),
    )
}

fn land_merge(ctx: &Ctx) -> Result<()> {
    ctx.env.configure(&[("mergeMethod", "merge")])?;
    let (number, landed) = create_and_land(ctx, "land-merge")?;

    // A merge commit, with the Pull Request's head as its second parent
    let pr = ctx.api.pull_request(number)?;
    let parents = ctx.env.git(&["log", "-1", "--format=%P", &landed])?;
    let parents: Vec<&str> = parents.split_whitespace().collect();
    check_eq(
        format!(
            "the landed commit merges PR #{number}'s head {}",
            short(&pr.head_sha)
        ),
        parents.get(1).copied(),
        Some(pr.head_sha.as_str()),
    )?;
    check_eq("the landed commit has two parents", parents.len(), 2)
}
