//! Stacked Pull Requests, in the three stacking modes

use color_eyre::eyre::Result;

use super::{Ctx, Scenario, check};

pub const BASE_BRANCHES: Scenario = Scenario {
    name: "stack-base-branches",
    description: "a stack with synthetic base branches (the default)",
    run: base_branches,
};

pub const CHAIN: Scenario = Scenario {
    name: "stack-chain",
    description: "a stack of chained Pull Requests (spr.stackingMode chain)",
    run: chain,
};

pub const GITHUB_STACK: Scenario = Scenario {
    name: "stack-github",
    description: "a stack on GitHub (github-stack), landed as a whole",
    run: github_stack,
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
    ctx.env.git(&["cherry-pick", "--allow-empty", &top])?;
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
        ctx.is_ancestor(&old_bottom, &new_bottom)?
            && ctx.is_ancestor(&old_top, &new_top)?,
        || "A Pull Request branch was rewritten".into(),
    )?;
    Ok((new_bottom, new_top))
}

fn base_branches(ctx: &Ctx) -> Result<()> {
    let name = "stack-base-branches";
    let (bottom, top) = create_stack(ctx, name)?;

    // The top Pull Request is based on a base branch spr created
    let bottom_pr = ctx.api.pull_request(bottom)?;
    let top_pr = ctx.api.pull_request(top)?;
    check(bottom_pr.base_ref == ctx.run.target_branch, || {
        format!("#{bottom} is based on {}", bottom_pr.base_ref)
    })?;
    let base = top_pr.base_ref.clone();
    check(
        base.starts_with(&ctx.run.test_prefix(name))
            && base.contains(&format!("{}.", ctx.run.target_branch))
            && base != bottom_pr.head_ref,
        || format!("#{top} isn't based on a synthetic base branch: {base}"),
    )?;

    update_bottom(ctx, name, bottom, top)?;

    // Closing both deletes the base branch, too
    ctx.env.spr_ok(&["close", "--all"])?;
    for number in [bottom, top] {
        check(!ctx.api.pull_request(number)?.is_open(), || {
            format!("#{number} is still open")
        })?;
    }
    check(!ctx.branch_exists(&base)?, || {
        format!("The base branch {base} still exists")
    })?;
    Ok(())
}

fn chain(ctx: &Ctx) -> Result<()> {
    let name = "stack-chain";
    ctx.env.configure(&[("stackingMode", "chain")])?;
    let (bottom, top) = create_stack(ctx, name)?;

    // The top Pull Request is based on the bottom one's branch
    let bottom_pr = ctx.api.pull_request(bottom)?;
    let top_pr = ctx.api.pull_request(top)?;
    check(top_pr.base_ref == bottom_pr.head_ref, || {
        format!("#{top} is based on {}", top_pr.base_ref)
    })?;

    // After updating, the top Pull Request contains the bottom one's head
    let (new_bottom, new_top) = update_bottom(ctx, name, bottom, top)?;
    check(ctx.is_ancestor(&new_bottom, &new_top)?, || {
        format!("#{top} doesn't contain the head of #{bottom}")
    })?;

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
    check(numbers == [bottom, top], || {
        format!("Expected a stack of #{bottom} and #{top}, got {stack:?}")
    })?;

    update_bottom(ctx, name, bottom, top)?;

    // spr land on the top commit lands the whole stack
    let local_tree = ctx.tree("HEAD")?;
    ctx.env.spr_ok(&["land"])?;
    for number in [bottom, top] {
        check(ctx.api.pull_request(number)?.merged, || {
            format!("#{number} wasn't merged")
        })?;
    }
    let landed = ctx.fetch(&ctx.run.target_branch)?;
    check(ctx.tree(&landed)? == local_tree, || {
        "The target branch doesn't have the tree of the local commits".into()
    })?;
    check(ctx.rev_parse("HEAD")? == landed, || {
        "The local branch wasn't moved to the landed commits".into()
    })?;
    Ok(())
}
