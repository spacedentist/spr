//! The other commands: spr patch, spr amend, spr list, and dry runs

use color_eyre::eyre::Result;

use std::time::Duration;

use super::{Ctx, Scenario, check, check_output};
use crate::api::wait_for;

/// How long GitHub's search may take to find a new Pull Request
const SEARCH_DELAY: Duration = Duration::from_secs(120);

pub const PATCH: Scenario = Scenario {
    name: "patch",
    description: "spr patch checks out a Pull Request (with --spr-id)",
    run: patch,
};

pub const AMEND: Scenario = Scenario {
    name: "amend",
    description: "spr amend takes the title from GitHub; spr list shows it",
    run: amend,
};

pub const DRY_RUN: Scenario = Scenario {
    name: "dry-run",
    description: "spr diff --dry-run creates nothing",
    run: dry_run,
};

fn patch(ctx: &Ctx) -> Result<()> {
    ctx.commit(&[("patch.txt", "patched\n")], "Patch test")?;
    ctx.env.spr_ok(&["diff"])?;
    let number = ctx.pr_number("HEAD")?;
    let (head, _) = ctx.check_pr_matches(number, "HEAD")?;

    // Check it out as somebody else would (on a new branch, with an ID)
    let branch = format!("PR-{number}");
    ctx.env.spr_ok(&[
        "patch",
        &number.to_string(),
        "--spr-id",
        "--branch-name",
        &branch,
    ])?;
    check(
        ctx.env.git(&["branch", "--show-current"])? == branch,
        || format!("Branch {branch} isn't checked out"),
    )?;
    check(ctx.tree("HEAD")? == ctx.tree(&head)?, || {
        "The patched commit doesn't have the Pull Request's tree".into()
    })?;
    check(ctx.expected_head("HEAD")?.as_deref() == Some(&head), || {
        "spr patch didn't record the Pull Request's head".into()
    })?;

    // Updating the Pull Request from there works (nothing to do)
    let output = ctx.env.spr_ok(&["diff", "-m", "From the patched branch"])?;
    check_output(&output, "No update necessary")?;
    Ok(())
}

fn amend(ctx: &Ctx) -> Result<()> {
    ctx.commit(
        &[("amend.txt", "amend\n")],
        "Amend test\n\nSigned-off-by: spr livetest <livetest@example.com>",
    )?;
    ctx.env.spr_ok(&["diff", "--spr-id"])?;
    let number = ctx.pr_number("HEAD")?;
    let spr_id = ctx.trailer("HEAD", "Spr-Id")?;

    // spr list uses GitHub's search, which takes a while to know about new
    // Pull Requests
    wait_for("spr list to show the Pull Request", SEARCH_DELAY, || {
        let output = ctx.env.spr_ok(&["list"])?;
        Ok(output.contains("Amend test").then_some(()))
    })?;

    // Somebody changes the title on GitHub; spr amend takes it, keeping
    // the local trailers
    ctx.api
        .set_pull_request_title(number, "Amend test, retitled")?;
    ctx.env.spr_ok(&["amend"])?;
    let message = ctx.message("HEAD")?;
    check(message.starts_with("Amend test, retitled\n"), || {
        format!("spr amend didn't take the new title:\n{message}")
    })?;
    check(
        ctx.trailer("HEAD", "Spr-Id")? == spr_id
            && ctx.trailer("HEAD", "Signed-off-by")?.is_some(),
        || format!("spr amend dropped local trailers:\n{message}"),
    )?;
    Ok(())
}

fn dry_run(ctx: &Ctx) -> Result<()> {
    let before = ctx.commit(&[("dry-run.txt", "dry\n")], "Dry run test")?;
    let output = ctx.env.spr_ok(&["diff", "--dry-run"])?;
    check_output(&output, "Would create")?;

    // Nothing changed: no Pull Request, no branch, same local commit
    check(ctx.rev_parse("HEAD")? == before, || {
        "The local commit changed".into()
    })?;
    let prefix = ctx.run.test_prefix(DRY_RUN.name);
    let branches = ctx.api.branches_with_prefix(&prefix)?;
    check(branches.is_empty(), || {
        format!("Branches were created: {branches:?}")
    })?;
    Ok(())
}
