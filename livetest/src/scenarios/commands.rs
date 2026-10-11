//! The other commands: spr patch, spr amend, spr list, and dry runs

use std::time::{Duration, Instant};

use color_eyre::eyre::Result;

use super::{Ctx, Scenario, check, check_eq, check_output};
use crate::{
    api::wait_for,
    log::{short, step},
};

/// How long GitHub's search may take to find a new Pull Request
const SEARCH_DELAY: Duration = Duration::from_secs(120);

pub const PATCH: Scenario = Scenario {
    name: "patch",
    description: "spr patch checks out a Pull Request (with --spr-id)",
    run: patch,
    only_on_github_com: None,
};

pub const AMEND: Scenario = Scenario {
    name: "amend",
    description: "spr amend takes the title from GitHub; spr list shows it",
    run: amend,
    only_on_github_com: None,
};

pub const DRY_RUN: Scenario = Scenario {
    name: "dry-run",
    description: "spr diff --dry-run creates nothing",
    run: dry_run,
    only_on_github_com: None,
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
    check_eq(
        format!("branch {branch} is checked out"),
        ctx.env.git(&["branch", "--show-current"])?,
        branch.clone(),
    )?;
    let patched = ctx.rev_parse("HEAD")?;
    check(
        format!(
            "the patched commit {} has the tree of PR #{number}'s head {}",
            short(&patched),
            short(&head)
        ),
        ctx.tree(&patched)? == ctx.tree(&head)?,
    )?;
    check_eq(
        format!("spr recorded {} as PR #{number}'s head", short(&head)),
        ctx.expected_head("HEAD")?.as_deref(),
        Some(head.as_str()),
    )?;

    // Updating the Pull Request from there works (nothing to do)
    let output = ctx.env.spr_ok(&["diff", "-m", "From the patched branch"])?;
    check_output(&output, "No update necessary")
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
    // Pull Requests. (Not logging every attempt.)
    step("$ spr list (until GitHub's search finds the new PR)");
    let start = Instant::now();
    wait_for("spr list to show the Pull Request", SEARCH_DELAY, || {
        let output = ctx.env.run({
            let mut command = ctx.env.spr_command();
            command.arg("list");
            command
        })?;
        Ok((output.success && output.stdout.contains("Amend test"))
            .then_some(()))
    })?;
    step(format!(
        "✓ spr list shows PR #{number} (after {}s)",
        start.elapsed().as_secs()
    ));

    // Somebody changes the title on GitHub; spr amend takes it, keeping
    // the local trailers
    ctx.api
        .set_pull_request_title(number, "Amend test, retitled")?;
    step(format!(
        "somebody changes PR #{number}'s title to \"Amend test, retitled\""
    ));
    ctx.env.spr_ok(&["amend"])?;
    let message = ctx.message("HEAD")?;
    check_eq(
        "the local commit has the new title",
        message.lines().next().unwrap_or_default(),
        "Amend test, retitled",
    )?;
    check(
        "the local commit kept its Spr-Id and Signed-off-by trailers",
        ctx.trailer("HEAD", "Spr-Id")? == spr_id
            && ctx.trailer("HEAD", "Signed-off-by")?.is_some(),
    )
}

fn dry_run(ctx: &Ctx) -> Result<()> {
    let before = ctx.commit(&[("dry-run.txt", "dry\n")], "Dry run test")?;
    let output = ctx.env.spr_ok(&["diff", "--dry-run"])?;
    check_output(&output, "Would create")?;

    // Nothing changed: no Pull Request, no branch, same local commit
    check_eq(
        format!("the local commit is still {}", short(&before)),
        ctx.rev_parse("HEAD")?,
        before.clone(),
    )?;
    let prefix = ctx.run.test_prefix(DRY_RUN.name);
    let branches = ctx.api.branches_with_prefix(&prefix)?;
    check_eq(
        format!("no branches were created under {prefix}"),
        branches,
        Vec::<String>::new(),
    )
}
