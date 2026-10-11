//! Stacked Pull Requests, in the three stacking modes

use color_eyre::eyre::Result;

use super::{Ctx, Scenario, check, check_eq};
use crate::log::{short, step};

pub const BASE_BRANCHES: Scenario = Scenario {
    name: "stack-base-branches",
    description: "a stack with synthetic base branches (the default)",
    run: base_branches,
    only_on_github_com: None,
};

pub const CHAIN: Scenario = Scenario {
    name: "stack-chain",
    description: "a stack of chained Pull Requests (spr.stackingMode chain)",
    run: chain,
    only_on_github_com: None,
};

pub const GITHUB_STACK: Scenario = Scenario {
    name: "stack-github",
    description: "a stack on GitHub (github-stack), landed as a whole",
    run: github_stack,
    only_on_github_com: Some(
        "GitHub Enterprise has no stacked pull requests (yet)",
    ),
};

/// Two commits, each with a Pull Request (`spr diff --all`). Returns the
/// numbers of the Pull Requests, bottom first.
fn create_stack(ctx: &Ctx, name: &str) -> Result<(u64, u64)> {
    ctx.commit(
        &[(&format!("{name}-1.txt"), "one\n")],
        &format!("{name}: one"),
    )?;
    ctx.commit(
        &[(&format!("{name}-2.txt"), "two\n")],
        &format!("{name}: two"),
    )?;
    ctx.env.spr_ok(&["diff", "--all"])?;
    let bottom = ctx.pr_number("HEAD~")?;
    let top = ctx.pr_number("HEAD")?;
    ctx.check_pr_matches(bottom, "HEAD~")?;
    ctx.check_pr_matches(top, "HEAD")?;
    Ok((bottom, top))
}

/// Amend the commit below HEAD (and rebase HEAD onto it)
fn amend_below_head(ctx: &Ctx, files: &[(&str, &str)]) -> Result<()> {
    let top = ctx.rev_parse("HEAD")?;
    ctx.env.git(&["reset", "--quiet", "--hard", "HEAD~"])?;
    ctx.amend(files)?;
    ctx.env
        .git(&["cherry-pick", "--allow-empty", "--quiet", &top])?;
    step(format!(
        "rebased the top commit {} onto it: {}",
        short(&top),
        short(&ctx.rev_parse("HEAD")?)
    ));
    Ok(())
}

/// Amend the bottom commit, update both Pull Requests, and check they match
/// their commits, without their branches being rewritten. Returns the new
/// heads.
fn update_bottom(
    ctx: &Ctx,
    name: &str,
    bottom: u64,
    top: u64,
) -> Result<(String, String)> {
    let (old_bottom, _) = ctx.check_pr_matches(bottom, "HEAD~")?;
    let (old_top, _) = ctx.check_pr_matches(top, "HEAD")?;
    amend_below_head(ctx, &[(&format!("{name}-1.txt"), "one, amended\n")])?;
    ctx.env
        .spr_ok(&["diff", "--all", "-m", "Amended the bottom commit"])?;
    let (new_bottom, _) = ctx.check_pr_matches(bottom, "HEAD~")?;
    let (new_top, _) = ctx.check_pr_matches(top, "HEAD")?;
    check(
        format!(
            "PR #{bottom} ({} → {}) and PR #{top} ({} → {}) were updated \
             without rewriting their branches",
            short(&old_bottom),
            short(&new_bottom),
            short(&old_top),
            short(&new_top)
        ),
        ctx.is_ancestor(&old_bottom, &new_bottom)?
            && ctx.is_ancestor(&old_top, &new_top)?,
    )?;
    Ok((new_bottom, new_top))
}

fn base_branches(ctx: &Ctx) -> Result<()> {
    let name = "stack-base-branches";
    let (bottom, top) = create_stack(ctx, name)?;

    // The top Pull Request is based on a base branch spr created
    let bottom_pr = ctx.api.pull_request(bottom)?;
    let top_pr = ctx.api.pull_request(top)?;
    check_eq(
        format!("PR #{bottom} targets {}", ctx.run.target_branch),
        &bottom_pr.base_ref,
        &ctx.run.target_branch,
    )?;
    let base = top_pr.base_ref.clone();
    check(
        format!("PR #{top} targets a synthetic base branch: {base}"),
        base.starts_with(&ctx.run.test_prefix(name))
            && base.contains(&format!("{}.", ctx.run.target_branch))
            && base != bottom_pr.head_ref,
    )?;

    update_bottom(ctx, name, bottom, top)?;

    // Closing both deletes the base branch, too
    ctx.env.spr_ok(&["close", "--all"])?;
    for number in [bottom, top] {
        check(
            format!("PR #{number} is closed"),
            !ctx.api.pull_request(number)?.is_open(),
        )?;
    }
    check(
        format!("the base branch {base} is deleted"),
        !ctx.branch_exists(&base)?,
    )
}

fn chain(ctx: &Ctx) -> Result<()> {
    let name = "stack-chain";
    ctx.env.configure(&[("stackingMode", "chain")])?;
    let (bottom, top) = create_stack(ctx, name)?;

    // The top Pull Request is based on the bottom one's branch
    let bottom_pr = ctx.api.pull_request(bottom)?;
    let top_pr = ctx.api.pull_request(top)?;
    check_eq(
        format!("PR #{top} targets PR #{bottom}'s branch"),
        &top_pr.base_ref,
        &bottom_pr.head_ref,
    )?;

    // After updating, the top Pull Request contains the bottom one's head
    let (new_bottom, new_top) = update_bottom(ctx, name, bottom, top)?;
    check(
        format!(
            "PR #{top}'s head {} contains PR #{bottom}'s head {}",
            short(&new_top),
            short(&new_bottom)
        ),
        ctx.is_ancestor(&new_bottom, &new_top)?,
    )?;

    ctx.env.spr_ok(&["close", "--all"])?;
    Ok(())
}

fn github_stack(ctx: &Ctx) -> Result<()> {
    let name = "stack-github";
    ctx.env.configure(&[("stackingMode", "github-stack")])?;
    let (bottom, top) = create_stack(ctx, name)?;

    // Both are in one stack on GitHub, bottom first
    let stack = ctx.api.stack_of_pull_request(top)?;
    let numbers: Vec<u64> = stack
        .as_ref()
        .and_then(|stack| stack["pull_requests"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|pr| pr["number"].as_u64())
        .collect();
    check_eq(
        format!(
            "PRs #{bottom} and #{top} are a stack on GitHub (#{})",
            stack
                .as_ref()
                .and_then(|stack| stack["number"].as_u64())
                .unwrap_or_default()
        ),
        numbers,
        vec![bottom, top],
    )?;

    update_bottom(ctx, name, bottom, top)?;

    // spr land on the top commit lands the whole stack
    let local_tree = ctx.tree("HEAD")?;
    ctx.env.spr_ok(&["land"])?;
    for number in [bottom, top] {
        check(
            format!("PR #{number} is merged"),
            ctx.api.pull_request(number)?.merged,
        )?;
    }
    let landed = ctx.fetch(&ctx.run.target_branch)?;
    check(
        format!(
            "the target branch (now at {}) has the tree of the local commits",
            short(&landed)
        ),
        ctx.tree(&landed)? == local_tree,
    )?;
    check(
        format!(
            "the local branch is on the landed commits {}",
            short(&landed)
        ),
        ctx.rev_parse("HEAD")? == landed,
    )
}
