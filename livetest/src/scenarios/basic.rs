//! Creating, updating and closing a Pull Request

use color_eyre::eyre::Result;

use super::{Ctx, Scenario, check, check_eq, check_output};
use crate::log::short;

pub const SCENARIO: Scenario = Scenario {
    name: "basic",
    description: "create, update and close a Pull Request",
    run,
    only_on_github_com: None,
};

/// Setups from before `spr.target` and `spr.auth.github.token` keep working
pub const OLD_CONFIG: Scenario = Scenario {
    name: "old-config",
    description: "spr.githubRepository, spr.githubMasterBranch and \
                  spr.githubAuthToken instead of spr.target and \
                  spr.auth.github.token",
    run: run_old_config,
    only_on_github_com: Some(
        "the old options are for repositories on github.com",
    ),
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
    check(format!("PR #{number} is open"), pr.is_open())?;
    check_eq(
        format!("PR #{number} has the commit's title"),
        pr.title.as_str(),
        "Basic test",
    )?;
    check_eq(
        format!("PR #{number} targets {}", ctx.run.target_branch),
        &pr.base_ref,
        &ctx.run.target_branch,
    )?;

    // Update: a new commit on top of the Pull Request branch (no force-push)
    ctx.amend(&[("basic.txt", "one\ntwo\n")])?;
    ctx.env.spr_ok(&["diff", "-m", "Second version"])?;
    let (second_head, _) = ctx.check_pr_matches(number, "HEAD")?;
    check(
        format!(
            "PR #{number}'s head moved on from {} to {}, without rewriting \
             its branch",
            short(&first_head),
            short(&second_head)
        ),
        second_head != first_head
            && ctx.is_ancestor(&first_head, &second_head)?,
    )?;

    // Close: Pull Request closed, branch deleted, trailer removed
    ctx.env.spr_ok(&["close"])?;
    let pr = ctx.api.pull_request(number)?;
    check(format!("PR #{number} is closed"), !pr.is_open())?;
    check(
        format!("its branch {} is deleted", pr.head_ref),
        !ctx.branch_exists(&pr.head_ref)?,
    )?;
    check(
        "the local commit has no Pull-request trailer anymore",
        ctx.trailer("HEAD", "Pull-request")?.is_none(),
    )
}

fn run_old_config(ctx: &Ctx) -> Result<()> {
    ctx.env.git(&["config", "--unset", "spr.target"])?;
    ctx.env.configure(&[
        (
            "githubRepository",
            &format!(
                "{}/{}",
                ctx.api.repository.owner, ctx.api.repository.name
            ),
        ),
        ("githubMasterBranch", &ctx.run.target_branch),
    ])?;
    ctx.env.set_old_token_in_config()?;

    ctx.commit(&[("old-config.txt", "one\n")], "Old config test")?;
    let output = ctx.env.spr_ok(&["diff"])?;
    check_output(&output, "spr.target replaces them")?;
    check_output(&output, "spr.auth.github.token replaces it")?;
    let number = ctx.pr_number("HEAD")?;
    let (_, pr) = ctx.check_pr_matches(number, "HEAD")?;
    check_eq(
        format!("PR #{number} targets {}", ctx.run.target_branch),
        &pr.base_ref,
        &ctx.run.target_branch,
    )?;

    ctx.env.spr_ok(&["close"])?;
    let pr = ctx.api.pull_request(number)?;
    check(format!("PR #{number} is closed"), !pr.is_open())
}
