//! Creating, updating and closing a Pull Request

use color_eyre::eyre::Result;

use super::{Ctx, Scenario, check};

pub const SCENARIO: Scenario = Scenario {
    name: "basic",
    description: "create, update and close a Pull Request",
    run,
};

fn run(ctx: &Ctx) -> Result<()> {
    // Create
    ctx.commit(
        &[("basic.txt", "one\n")],
        "Basic test\n\nThe description of the test commit.",
    )?;
    ctx.env.spr_ok(&["diff"])?;

    let number = ctx.pr_number("HEAD")?;
    let (first_head, pr) = ctx.check_pr_matches(number, "HEAD")?;
    check(pr.is_open(), || format!("#{number} isn't open"))?;
    check(pr.title == "Basic test", || {
        format!("#{number} has the title {:?}", pr.title)
    })?;
    check(pr.base_ref == ctx.run.target_branch, || {
        format!("#{number} is based on {}", pr.base_ref)
    })?;

    // Update: a new commit on top of the Pull Request branch (no force-push)
    ctx.amend(&[("basic.txt", "one\ntwo\n")])?;
    ctx.env.spr_ok(&["diff", "-m", "Second version"])?;
    let (second_head, _) = ctx.check_pr_matches(number, "HEAD")?;
    check(second_head != first_head, || {
        "The head didn't change".into()
    })?;
    check(ctx.is_ancestor(&first_head, &second_head)?, || {
        "The Pull Request branch was rewritten".into()
    })?;

    // Close: Pull Request closed, branch deleted, trailer removed
    ctx.env.spr_ok(&["close"])?;
    let pr = ctx.api.pull_request(number)?;
    check(!pr.is_open(), || format!("#{number} is still open"))?;
    check(!ctx.branch_exists(&pr.head_ref)?, || {
        format!("Branch {} still exists", pr.head_ref)
    })?;
    check(ctx.trailer("HEAD", "Pull-request")?.is_none(), || {
        "The local commit still has a Pull-request trailer".into()
    })?;
    Ok(())
}
