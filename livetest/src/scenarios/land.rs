//! Landing Pull Requests

use color_eyre::eyre::Result;

use super::{Ctx, Scenario, check};

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
    check(pr.merged, || format!("#{number} wasn't merged"))?;
    let landed = ctx.fetch(&ctx.run.target_branch)?;
    check(ctx.tree(&landed)? == local_tree, || {
        "The target branch doesn't have the tree of the local commit".into()
    })?;
    // The local branch is on the landed commit now
    check(ctx.rev_parse("HEAD")? == landed, || {
        "The local branch wasn't moved to the landed commit".into()
    })?;
    Ok((number, landed))
}

fn land_squash(ctx: &Ctx) -> Result<()> {
    let (number, landed) = create_and_land(ctx, "land")?;

    // One commit, with the Pull Request's title, and trailers that Git
    // recognises as such
    let parents = ctx.env.git(&["log", "-1", "--format=%P", &landed])?;
    check(parents.split_whitespace().count() == 1, || {
        format!("Expected a squash commit, got parents {parents}")
    })?;
    let message = ctx.message(&landed)?;
    check(message.starts_with("Test land"), || {
        format!("Unexpected message of the landed commit:\n{message}")
    })?;
    let trailers = ctx.env.git(&[
        "log",
        "-1",
        "--format=%(trailers:key=Pull-request,valueonly)",
        &landed,
    ])?;
    check(trailers.ends_with(&format!("/pull/{number}")), || {
        format!(
            "Git doesn't see a Pull-request trailer in the landed commit:\n\
             {message}"
        )
    })?;
    Ok(())
}

fn land_merge(ctx: &Ctx) -> Result<()> {
    ctx.env.configure(&[("mergeMethod", "merge")])?;
    let (number, landed) = create_and_land(ctx, "land-merge")?;

    // A merge commit, with the Pull Request's head as its second parent
    let pr = ctx.api.pull_request(number)?;
    let parents = ctx.env.git(&["log", "-1", "--format=%P", &landed])?;
    let parents: Vec<&str> = parents.split_whitespace().collect();
    check(parents.len() == 2 && parents[1] == pr.head_sha, || {
        format!(
            "Expected a merge commit of the Pull Request's head {}, got \
             parents {parents:?}",
            pr.head_sha
        )
    })?;
    Ok(())
}
