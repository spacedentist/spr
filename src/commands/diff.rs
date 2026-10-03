use std::collections::HashSet;
use std::iter::zip;

use color_eyre::eyre::{Error, Result, WrapErr as _, bail, eyre};

use crate::{
    config::StackingMode,
    git::PreparedCommit,
    git_remote::PushSpec,
    github::{
        GitHub, GitHubBranch, PullRequest, PullRequestRequestReviewers,
        PullRequestStack, PullRequestState, PullRequestUpdate,
    },
    output::{output, write_commit_title},
    pr_commits::{
        self, BaseOutdated, CommitInfo, Conflict, HeadMoved, Plan,
        PullRequestCommits,
    },
    utils::{parse_name_list, remove_all_parens, slugify},
};
use git2::Oid;
use indoc::{formatdoc, indoc};

#[derive(Debug, clap::Parser)]
pub struct DiffOptions {
    /// Create/update pull requests for the whole branch, not just the HEAD commit
    #[clap(long, short = 'a')]
    all: bool,

    /// Update the pull request title and description on GitHub from the local
    /// commit message
    #[clap(long)]
    update_message: bool,

    /// Submit any new Pull Request as a draft
    #[clap(long)]
    draft: bool,

    /// Message to be used for commits updating existing pull requests (e.g.
    /// 'rebase' or 'review comments')
    #[clap(long, short = 'm')]
    message: Option<String>,

    /// Which commits in the branch should be created/updated. This can be a
    /// revspec such as HEAD~4..HEAD~1 or just one commit like HEAD~7.
    #[clap(long, short = 'r')]
    refs: Option<String>,

    /// Submit this commit as if it was cherry-picked on master. Do not base it
    /// on any intermediate changes between the master branch and this commit.
    #[clap(long)]
    cherry_pick: bool,

    /// Show what would be done, without creating commits, pushing, or
    /// changing anything on GitHub or in the local repository
    #[clap(long)]
    dry_run: bool,

    /// Give the local commit an ID (`Spr-Id` trailer), if it doesn't have
    /// one yet. (Always done if `spr.sprIds` is set.)
    #[clap(long)]
    spr_id: bool,

    /// Update Pull Requests even if they have changes that aren't in the
    /// local commit (e.g. pushed by somebody else), overwriting them. Only
    /// checked for commits with an ID (`Spr-Id` trailer).
    #[clap(long)]
    force: bool,
}

fn get_oids(refs: &str, repo: &git2::Repository) -> Result<HashSet<Oid>> {
    // refs might be a single (eg 012345abc or HEAD) or a range (HEAD~4..HEAD~2)
    let revspec = repo.revparse(refs)?;

    let from = revspec
        .from()
        .ok_or_else(|| eyre!("Unexpectedly no from id in range"))?
        .id();
    if revspec.mode().contains(git2::RevparseMode::SINGLE) {
        // simple case, just return the id
        return Ok(HashSet::from([from]));
    }
    let to = revspec
        .to()
        .ok_or_else(|| eyre!("Unexpectedly no to id in range"))?
        .id();

    let mut walk = repo.revwalk()?;
    walk.push(to)?;
    walk.hide(from)?;
    walk.map(|r| Ok(r?)).collect()
}

pub async fn diff(
    opts: DiffOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    // Abort right here if the local Git repository is not clean
    git.check_no_uncommitted_changes()?;

    let mut result = Ok(());

    // Look up the commits on the local branch
    let mut prepared_commits = gh.get_prepared_commits()?;

    // The parent of the first commit in the list is the commit on master that
    // the local branch is based on
    let master_base_oid = if let Some(first_commit) = prepared_commits.first() {
        first_commit.parent_oid
    } else {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return result;
    };

    // If refs is set, we want to track which commits to run `diff` against. The
    // simple approach would be to adjust the prepared_commits Vec (as with
    // opts.all above). This does not work however, as we need to know the
    // entire list (or more specifically the list after the first update) for
    // the rewrite_commit_messages step. This is not a problem for opts.all as
    // it only ever has a single commit to update, and so nothing after it.
    // In chain stacking mode, we need the Pull Request of the parent of each
    // commit we operate on. Remember the one of the parent of the first
    // commit in the list, before we possibly drop commits from the list
    // below. (That's `None` for the first commit on the branch, which is based
    // on master.)
    let mut parent_pull_request_number: Option<u64> = None;
    if opts.refs.is_none() && !opts.all && prepared_commits.len() > 1 {
        parent_pull_request_number =
            prepared_commits[prepared_commits.len() - 2].pull_request_number;
    }

    // In the github-stack stacking mode, we link the Pull Requests of the
    // local branch into a stack on GitHub at the end. Remember the Pull
    // Request numbers of all commits for that, before we possibly drop commits
    // from the list below.
    let mut branch_pull_request_numbers: Vec<Option<u64>> = prepared_commits
        .iter()
        .map(|pc| pc.pull_request_number)
        .collect();
    let first_commit_index = if opts.refs.is_none() && !opts.all {
        prepared_commits.len() - 1
    } else {
        0
    };

    let revs_to_pr = match (opts.refs.as_deref(), opts.all) {
        (Some(refs), false) => Some(get_oids(refs, git.repo())?),
        (Some(_), true) => {
            bail!("Do not use --refs with --all");
        }
        (None, true) => {
            // Operate on all commits
            None
        }
        (None, false) => {
            // Only operate on the HEAD commit.
            prepared_commits.drain(0..prepared_commits.len() - 1);
            None
        }
    };

    #[allow(clippy::needless_collect)]
    let pull_request_tasks: Vec<_> = prepared_commits
        .iter()
        .map(|pc: &PreparedCommit| {
            if revs_to_pr
                .as_ref()
                .map(|revs| revs.contains(&pc.oid))
                .unwrap_or(true)
            {
                // We are going to want to look at this pull request below.
                pc.pull_request_number.map(|number| {
                    tokio::task::spawn_local(
                        gh.clone().get_pull_request(number),
                    )
                })
            } else {
                // We will be skipping this commit below, because we have as set
                // of commit oids to operate on, and this commit is not in
                // there.
                None
            }
        })
        .collect();

    let mut message_on_prompt = "".to_string();

    for (prepared_commit, pull_request_task) in
        zip(prepared_commits.iter_mut(), pull_request_tasks)
    {
        if result.is_err() {
            break;
        }

        // Check whether to skip this commit because we have a hashset of oids
        // to operate on, but it doesn't contain this commit oid
        if revs_to_pr
            .as_ref()
            .map(|revs| !revs.contains(&prepared_commit.oid))
            .unwrap_or(false)
        {
            parent_pull_request_number = prepared_commit.pull_request_number;
            continue;
        }

        if let Err(error) = write_commit_title(prepared_commit) {
            result = Err(error);
            break;
        }

        // Errors must not return from this function directly (e.g. with `?`),
        // but end the loop, so that the local commit messages still get
        // updated below. Otherwise, Pull Requests created for earlier commits
        // would not be recorded in their commit messages.
        let pull_request = if let Some(task) = pull_request_task {
            match task.await {
                Ok(Ok(pull_request)) => Some(pull_request),
                Ok(Err(error)) => {
                    result = Err(error);
                    break;
                }
                Err(error) => {
                    result = Err(error.into());
                    break;
                }
            }
        } else {
            None
        };

        // In chain stacking mode, a commit that is not directly based on
        // master gets a Pull Request that is based on the parent commit's Pull
        // Request.
        let stack_on = if config.stacking_mode.is_chained()
            && !opts.cherry_pick
            && prepared_commit.parent_oid != master_base_oid
        {
            match parent_pull_request_number {
                Some(number) => {
                    match gh.clone().get_pull_request(number).await {
                        Ok(pull_request) => Some(pull_request),
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
                None if opts.dry_run => {
                    // In a dry run, we don't create the parent commit's Pull
                    // Request, so we can't tell much about this one.
                    output(
                        "🔍",
                        "Would create a Pull Request stacked on the new Pull \
                         Request of the parent commit",
                    )?;
                    parent_pull_request_number = None;
                    continue;
                }
                None => {
                    result = Err(eyre!(
                        "The parent commit does not have a Pull Request. Run \
                         `spr diff` on the parent commit first, or use \
                         `spr diff --all`."
                    ));
                    break;
                }
            }
        } else {
            None
        };

        // The further implementation of the diff command is in a separate
        // function. This makes it easier to run the code to update the local
        // commit message with all the changes that the implementation makes at
        // the end, even if the implementation encounters an error or exits
        // early.
        result = diff_impl(
            &opts,
            &mut message_on_prompt,
            git,
            gh,
            config,
            prepared_commit,
            master_base_oid,
            pull_request,
            stack_on,
        )
        .await;

        parent_pull_request_number = prepared_commit.pull_request_number;
    }

    if opts.dry_run {
        return result;
    }

    // This updates the commit message in the local Git repository (if it was
    // changed by the implementation)
    git.rewrite_commit_messages(prepared_commits.as_mut_slice(), None)?;

    if result.is_ok() && config.stacking_mode == StackingMode::GitHubStack {
        for (index, prepared_commit) in prepared_commits.iter().enumerate() {
            branch_pull_request_numbers[first_commit_index + index] =
                prepared_commit.pull_request_number;
        }
        sync_github_stack(gh, config, &branch_pull_request_numbers).await?;
    }

    result
}

/// Whether an error is GitHub reporting that something doesn't exist
fn is_not_found(error: &Error) -> bool {
    matches!(
        error.downcast_ref::<octocrab::Error>(),
        Some(octocrab::Error::GitHub { source, .. })
            if source.status_code == http::StatusCode::NOT_FOUND
    )
}

/// The message of a new commit on the Pull Request branch: the update
/// message the user gave, or, for the first commit of a new Pull Request,
/// the title of the local commit or "[𝘀𝗽𝗿] initial version", depending on
/// the configuration.
fn pull_request_commit_message(
    update_message: Option<&str>,
    title: &str,
    config: &crate::config::Config,
) -> String {
    let message = update_message.unwrap_or(
        if config.use_commit_title_for_initial_commit {
            title
        } else {
            "[𝘀𝗽𝗿] initial version"
        },
    );
    format!(
        "{message}\n\nCreated using spr {}",
        env!("CARGO_PKG_VERSION")
    )
}

/// Report what `spr diff` would do for a commit, for --dry-run
#[allow(clippy::too_many_arguments)]
fn describe_dry_run(
    opts: &DiffOptions,
    config: &crate::config::Config,
    message: &crate::message::CommitMessage,
    pull_request: Option<&PullRequest>,
    pull_request_branch: &GitHubBranch,
    base_branch: Option<&GitHubBranch>,
    plan: &Plan,
    needs_new_base_branch: bool,
    keeps_base: bool,
    stacked_on: Option<u64>,
) -> Result<()> {
    let base_name = base_branch
        .unwrap_or(&config.master_ref)
        .branch_name()
        .to_string();
    let base_description = match stacked_on {
        Some(number) => format!("{base_name} (Pull Request #{number})"),
        None => base_name,
    };

    if let Some(base_branch) = base_branch {
        if needs_new_base_branch {
            output(
                "🔍",
                &format!(
                    "Would create base branch {}",
                    base_branch.branch_name()
                ),
            )?;
        } else if plan.new_base_parents.is_some() {
            output(
                "🔍",
                &format!(
                    "Would add a commit to base branch {}",
                    base_branch.branch_name()
                ),
            )?;
        }
    }

    let Some(pull_request) = pull_request else {
        output(
            "🔍",
            &format!(
                "Would create a Pull Request from branch {} into {}",
                pull_request_branch.branch_name(),
                base_description,
            ),
        )?;
        return Ok(());
    };

    if plan.is_empty() && !needs_new_base_branch {
        output("🔍", "No update necessary")?;
    } else if plan.new_head_parents.is_some() {
        output(
            "🔍",
            &format!(
                "Would add a commit to {}{}",
                pull_request_branch.branch_name(),
                if plan.merges {
                    ", merging in the new base"
                } else {
                    ""
                },
            ),
        )?;
    }

    if !keeps_base {
        output(
            "🔍",
            &format!(
                "Would change the base of Pull Request #{} to {}",
                pull_request.number, base_description
            ),
        )?;
    }

    if opts.update_message {
        let mut updates: PullRequestUpdate = Default::default();
        updates.update_message(pull_request, message);
        if !updates.is_empty() {
            output(
                "🔍",
                &format!(
                    "Would update the title and description of Pull Request \
                     #{}",
                    pull_request.number
                ),
            )?;
        }
    }

    Ok(())
}

/// GitHub refuses to change the base of a Pull Request that is part of a
/// stack. Dissolve the Pull Request's stack, if there is one. (In the
/// github-stack stacking mode, `spr diff` creates a new stack afterwards.)
async fn leave_github_stack(gh: &GitHub, number: u64) -> Result<()> {
    // If the stacks API is not available (e.g. on older GitHub Enterprise
    // Server versions), the Pull Request can't be part of a stack either.
    let Ok(Some(stack)) = gh.find_pull_request_stack(number).await else {
        return Ok(());
    };

    gh.unstack_pull_request_stack(stack.number).await?;
    output(
        "📚",
        &format!(
            "Dissolved stack #{} to change the base of Pull Request #{}",
            stack.number, number
        ),
    )?;

    Ok(())
}

/// Make the chain of Pull Requests of the local branch a stack on GitHub.
///
/// `pull_request_numbers` are the Pull Requests of the commits of the local
/// branch, from bottom to top.
async fn sync_github_stack(
    gh: &GitHub,
    config: &crate::config::Config,
    pull_request_numbers: &[Option<u64>],
) -> Result<()> {
    // Determine the chain of Pull Requests: starting at the bottom of the
    // local branch, each Pull Request must be open and based on the head
    // branch of the one below (the first one on master). The chain ends at
    // the first commit that doesn't fit (e.g. it has no Pull Request, or its
    // Pull Request was created with --cherry-pick).
    let mut chain: Vec<u64> = Vec::new();
    let mut expected_base = config.master_ref.branch_name().to_string();
    for &number in pull_request_numbers {
        let Some(number) = number else { break };
        let refs = gh.get_pull_request_refs(number).await?;
        if !refs.open || refs.base != expected_base {
            break;
        }
        expected_base = refs.head;
        chain.push(number);
    }

    // The stacks on GitHub that these Pull Requests currently belong to
    let mut stacks: Vec<PullRequestStack> = Vec::new();
    for &number in &chain {
        if let Some(stack) = gh.find_pull_request_stack(number).await?
            && !stacks.iter().any(|s| s.number == stack.number)
        {
            stacks.push(stack);
        }
    }

    // If all Pull Requests of the chain that are in a stack are in the same
    // stack, and the order matches, we keep that stack. The local branch may
    // contain only the lower part of the stack (e.g. when running `spr diff`
    // during an interactive rebase), or more Pull Requests than the stack,
    // which we then add on top.
    if let [stack] = &stacks[..] {
        let in_stack = stack.open_pull_requests();
        if in_stack.starts_with(&chain) {
            return Ok(());
        }
        if chain.starts_with(&in_stack) {
            let new = &chain[in_stack.len()..];
            gh.add_to_pull_request_stack(stack.number, new).await?;
            output(
                "📚",
                &format!(
                    "Added {} to stack #{}",
                    format_pull_request_numbers(new),
                    stack.number
                ),
            )?;
            return Ok(());
        }
    }

    // Otherwise, the existing stacks don't reflect the local branch anymore
    // (e.g. commits were reordered or dropped). Dissolve them, and create a
    // new stack.
    for stack in &stacks {
        gh.unstack_pull_request_stack(stack.number).await?;
        output(
            "📚",
            &format!(
                "Dissolved stack #{} as it does not match the local branch \
                 anymore",
                stack.number
            ),
        )?;
    }

    if chain.len() >= 2 {
        let stack = gh.create_pull_request_stack(&chain).await?;
        output(
            "📚",
            &format!(
                "Created stack #{} with {}",
                stack.number,
                format_pull_request_numbers(&chain)
            ),
        )?;
    }

    Ok(())
}

fn format_pull_request_numbers(numbers: &[u64]) -> String {
    let numbers = numbers
        .iter()
        .map(|number| format!("#{number}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("Pull Requests {numbers}")
}

#[allow(clippy::too_many_arguments)]
async fn diff_impl(
    opts: &DiffOptions,
    message_on_prompt: &mut String,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
    local_commit: &mut PreparedCommit,
    master_base_oid: Oid,
    pull_request: Option<PullRequest>,
    stack_on: Option<PullRequest>,
) -> Result<()> {
    // Parsed commit message of the local commit
    let message = &mut local_commit.message;

    // Check if the local commit is based directly on the master branch.
    let directly_based_on_master = local_commit.parent_oid == master_base_oid;

    // Determine the trees the Pull Request branch and the base branch should
    // have when we're done here: the trees of the local commit and its
    // parent. If the user tells us to --cherry-pick, the change of the local
    // commit is applied to the master commit we're based on instead. (If the
    // local commit is directly based on master, that makes no difference.)
    let head_tree = git.get_tree_oid_for_commit(local_commit.oid)?;
    let base_tree = git.get_tree_oid_for_commit(local_commit.parent_oid)?;
    let (new_head_tree, new_base_tree) =
        if opts.cherry_pick && !directly_based_on_master {
            match pr_commits::cherry_pick(
                git,
                master_base_oid,
                head_tree,
                base_tree,
            ) {
                Err(error) if error.downcast_ref::<Conflict>().is_some() => {
                    bail!(
                        "This commit cannot be cherry-picked on {master}.",
                        master = config.master_ref.branch_name(),
                    );
                }
                trees => trees?,
            }
        } else {
            (head_tree, base_tree)
        };

    if let Some(number) = local_commit.pull_request_number {
        output(
            "#️⃣ ",
            &format!(
                "Pull Request #{}: {}",
                number,
                config.pull_request_url(number)
            ),
        )?;
    }

    // Give the local commit an ID, if it doesn't have one yet and the user
    // wants one
    if config.spr_ids || opts.spr_id {
        message.ensure_spr_id();
    }

    if local_commit.pull_request_number.is_none() || opts.update_message {
        message.validate(config)?;
    }

    if let Some(ref pull_request) = pull_request {
        if pull_request.state == PullRequestState::Closed {
            return Err(Error::msg(formatdoc!(
                "Pull request is closed. If you want to open a new one, \
                 remove the 'Pull Request' section from the commit message."
            )));
        }

        if !opts.update_message {
            let mut pull_request_updates: PullRequestUpdate =
                Default::default();
            pull_request_updates.update_message(pull_request, message);

            if !pull_request_updates.is_empty() {
                output(
                    "⚠️",
                    indoc!(
                        "The Pull Request's title/message differ from the \
                         local commit's message.
                         Use `spr diff --update-message` to overwrite the \
                         title and message on GitHub with the local message, \
                         or `spr amend` to go the other way (rewrite the local \
                         commit message with what is on GitHub)."
                    ),
                )?;
            }
        }
    }

    // Parse "Reviewers" trailer, if this is a new Pull Request
    let mut requested_reviewers = PullRequestRequestReviewers::default();

    if local_commit.pull_request_number.is_none()
        && let Some(reviewers) = message.get_trailer("Reviewers")
    {
        let reviewers = parse_name_list(reviewers);
        let mut checked_reviewers = Vec::new();

        for reviewer in reviewers {
            // Teams are indicated with a leading #
            if let Some(slug) = reviewer.strip_prefix('#') {
                match GitHub::get_github_team(
                    (&config.owner).into(),
                    slug.into(),
                )
                .await
                {
                    Ok(team) => {
                        requested_reviewers
                            .team_reviewers
                            .push(team.slug.to_string());
                        checked_reviewers.push(reviewer);
                    }
                    Err(error) if is_not_found(&error) => {
                        bail!(
                            "Reviewers field contains unknown team '{}'",
                            reviewer,
                        );
                    }
                    Err(error) => {
                        return Err(error.wrap_err(format!(
                            "Looking up team '{reviewer}' failed"
                        )));
                    }
                }
            } else {
                match GitHub::get_github_user(reviewer.clone()).await {
                    Ok(user) => {
                        requested_reviewers.reviewers.push(user.login);
                        if let Some(name) = user.name {
                            checked_reviewers.push(format!(
                                "{} ({})",
                                reviewer.clone(),
                                remove_all_parens(&name)
                            ));
                        } else {
                            checked_reviewers.push(reviewer);
                        }
                    }
                    Err(error) if is_not_found(&error) => {
                        bail!(
                            "Reviewers field contains unknown user '{}'",
                            reviewer
                        );
                    }
                    Err(error) => {
                        return Err(error.wrap_err(format!(
                            "Looking up user '{reviewer}' failed"
                        )));
                    }
                }
            }
        }

        message
            .set_trailer("Reviewers".to_string(), checked_reviewers.join(", "));
    }

    if let Some(ref stack_on) = stack_on
        && stack_on.state != PullRequestState::Open
    {
        bail!(
            "The Pull Request of the parent commit (#{}) is closed. Rebase \
             this commit or update the parent commit first.",
            stack_on.number
        );
    }

    // Get the name of the existing Pull Request branch, or constuct one if
    // there is none yet.

    let title = message.title();

    let pull_request_branch = match &pull_request {
        Some(pr) => pr.head.clone(),
        None => {
            config.new_github_branch(&gh.remote().find_unused_branch_name(
                &config.branch_prefix,
                &slugify(title),
            )?)
        }
    };

    // Determine what the Pull Request should be based on: the branch (if it's
    // not the master branch), the commit it's currently based on, and whether
    // we may add commits to that branch.
    // * If we are stacking on the parent commit's Pull Request, it's the head
    //   of that Pull Request. We must not add commits to it.
    // * If the user pointed an existing Pull Request at some other branch, we
    //   leave that as it is, and treat it like a base branch.
    // * If the local commit is directly based on master, or we are
    //   cherry-picking, it's the master commit the local commit is based on.
    //   An existing base branch is not needed anymore then.
    // * If the existing Pull Request uses a base branch created by spr, we
    //   keep using it.
    // * Otherwise, the Pull Request needs a base branch, which we create
    //   below if necessary. It's based on what the Pull Request is currently
    //   based on (a master commit, or the head of another Pull Request it
    //   was stacked on before).
    let is_stacked_base = |branch: &GitHubBranch| {
        !branch.is_master_branch()
            && !config.is_spr_base_branch(branch)
            && branch.branch_name().starts_with(&config.branch_prefix)
    };
    let pr_base =
        |pr: &PullRequest| git.repo().merge_base(pr.head_oid, pr.base_oid);
    let (base_branch, base, may_update_base) =
        if let Some(ref stack_on) = stack_on {
            (Some(stack_on.head.clone()), stack_on.head_oid, false)
        } else {
            match &pull_request {
                Some(pr)
                    if !pr.base.is_master_branch()
                        && !config.is_spr_base_branch(&pr.base)
                        && !is_stacked_base(&pr.base) =>
                {
                    (Some(pr.base.clone()), pr_base(pr)?, true)
                }
                _ if directly_based_on_master || opts.cherry_pick => {
                    (None, master_base_oid, false)
                }
                Some(pr) if config.is_spr_base_branch(&pr.base) => {
                    (Some(pr.base.clone()), pr_base(pr)?, true)
                }
                Some(pr) => (None, pr_base(pr)?, true),
                None => (None, master_base_oid, true),
            }
        };

    // Work out which commits we need to create
    let commits = PullRequestCommits {
        // For a new Pull Request, the head starts off at the base.
        head: pull_request.as_ref().map(|pr| pr.head_oid).unwrap_or(base),
        base,
        target: master_base_oid,
        head_tree: new_head_tree,
        base_tree: new_base_tree,
        may_update_base,
    };
    // If somebody else changed the Pull Request, updating it would revert
    // their changes. Stop, unless the user tells us to --force.
    if let Some(ref pull_request) = pull_request
        && !check_remote_changes(
            opts,
            git,
            gh,
            config,
            message,
            &commits,
            pull_request,
        )?
    {
        return Ok(());
    }

    let plan = match commits.plan(git) {
        Err(error) if error.downcast_ref::<BaseOutdated>().is_some() => {
            if let Some(ref stack_on) = stack_on
                && opts.dry_run
            {
                if opts.all || opts.refs.is_some() {
                    // In a real run, the parent commit's Pull Request would
                    // (probably) have been updated first.
                    output(
                        "🔍",
                        &format!(
                            "Would be updated once the Pull Request of the \
                             parent commit (#{}) is up to date",
                            stack_on.number
                        ),
                    )?;
                } else {
                    output(
                        "🔍",
                        &format!(
                            "Would fail: the Pull Request of the parent \
                             commit (#{}) is not up to date. Run `spr diff` \
                             on the parent commit first, or use \
                             `spr diff --all`.",
                            stack_on.number
                        ),
                    )?;
                }
                return Ok(());
            }
            if let Some(ref stack_on) = stack_on {
                bail!(
                    "The Pull Request of the parent commit (#{}) is not up to \
                     date. Run `spr diff` on the parent commit first, or use \
                     `spr diff --all`.",
                    stack_on.number
                );
            }
            return Err(error);
        }
        plan => plan?,
    };

    // A Pull Request that may get base commits, but doesn't have a base
    // branch yet, needs a new base branch unless it can be based on master.
    let needs_new_base_branch = base_branch.is_none()
        && may_update_base
        && (plan.new_base_parents.is_some()
            || !git.is_ancestor(base, master_base_oid)?);
    let base_branch = if needs_new_base_branch {
        Some(
            config.new_github_branch(&gh.remote().find_unused_branch_name(
                &config.branch_prefix,
                &format!(
                    "{}.{}",
                    config.master_ref.branch_name(),
                    slugify(title)
                ),
            )?),
        )
    } else {
        base_branch
    };

    // Whether the existing Pull Request is already based on the right branch
    let keeps_base =
        pull_request.as_ref().is_none_or(|pr| match &base_branch {
            Some(base_branch) => {
                pr.base.branch_name() == base_branch.branch_name()
            }
            None => pr.base.is_master_branch(),
        });

    if opts.dry_run {
        return describe_dry_run(
            opts,
            config,
            message,
            pull_request.as_ref(),
            &pull_request_branch,
            base_branch.as_ref(),
            &plan,
            needs_new_base_branch,
            keeps_base,
            stack_on.as_ref().map(|pr| pr.number),
        );
    }

    // At this point we can check if we can exit early because no update to the
    // existing Pull Request branch is necessary
    if let Some(ref pull_request) = pull_request
        && plan.is_empty()
        && !needs_new_base_branch
    {
        // ...and it does not need a rebase, and the trees of both Pull
        // Request branch and base are all the right ones.
        output("✅", "No update necessary")?;

        // The Pull Request has the local commit's tree, so whatever happened
        // to its head (e.g. GitHub rebasing it), it's consistent with the
        // local commit.
        record_expected_head(git, message, pull_request.head_oid)?;

        if !keeps_base {
            leave_github_stack(gh, pull_request.number).await?;
        }

        let mut pull_request_updates: PullRequestUpdate = Default::default();

        if opts.update_message {
            // However, the user requested to update the commit message on
            // GitHub
            pull_request_updates.update_message(pull_request, message);
        }

        if !keeps_base {
            // The Pull Request branch already contains what it should be
            // based on, so we only need to change its base.
            pull_request_updates.base = Some(
                base_branch
                    .as_ref()
                    .unwrap_or(&config.master_ref)
                    .branch_name()
                    .to_string(),
            );
        }

        if !pull_request_updates.is_empty() {
            let message_updated = pull_request_updates.title.is_some()
                || pull_request_updates.body.is_some();
            gh.update_pull_request(pull_request.number, pull_request_updates)
                .await?;
            if message_updated {
                output("✍", "Updated commit message on GitHub")?;
            }
        }

        if !keeps_base {
            base_changed(
                gh,
                config,
                &pull_request.base,
                base_branch.as_ref().unwrap_or(&config.master_ref),
                stack_on.as_ref().map(|pr| pr.number),
            )?;
        }

        return Ok(());
    }

    // Ask for a message for the new commit on the Pull Request branch, if
    // we're updating an existing Pull Request.
    let mut github_commit_message = opts.message.clone();
    if pull_request.is_some()
        && plan.new_head_parents.is_some()
        && github_commit_message.is_none()
    {
        let input = {
            let message_on_prompt = message_on_prompt.clone();

            tokio::task::spawn_blocking(move || {
                dialoguer::Input::<String>::new()
                    .with_prompt("Message (leave empty to abort)")
                    .with_initial_text(message_on_prompt)
                    .allow_empty(true)
                    .interact_text()
            })
            .await??
        };

        if input.is_empty() {
            bail!("Aborted as per user request");
        }

        *message_on_prompt = input.clone();
        github_commit_message = Some(input);
    }

    // Create the new commits
    let base_message = format!(
        "[𝘀𝗽𝗿] {}\n\nCreated using spr {}\n\n[skip ci]",
        if pull_request.is_some() {
            "changes introduced through rebase".to_string()
        } else {
            format!(
                "changes to {} this commit is based on",
                config.master_ref.branch_name()
            )
        },
        env!("CARGO_PKG_VERSION"),
    );
    let head_message = pull_request_commit_message(
        github_commit_message.as_deref(),
        title,
        config,
    );
    let new_commits = commits.create(
        git,
        &plan,
        CommitInfo {
            message: &base_message,
            author_from: Some(local_commit.parent_oid),
        },
        CommitInfo {
            message: &head_message,
            author_from: Some(local_commit.oid),
        },
    )?;

    let mut push_specs = Vec::new();

    if pull_request.is_none() || new_commits.head != commits.head {
        push_specs.push(PushSpec {
            oid: Some(new_commits.head),
            remote_ref: pull_request_branch.on_github(),
        });
    }

    // If we created a new commit for the base branch, or need a new base
    // branch, add it to the push
    if let Some(ref base_branch) = base_branch
        && (plan.new_base_parents.is_some() || needs_new_base_branch)
    {
        push_specs.push(PushSpec {
            oid: Some(new_commits.base),
            remote_ref: base_branch.on_github(),
        });
    }

    if let Some(ref pull_request) = pull_request {
        // If the Pull Request's base is going to change, it must not be part
        // of a stack on GitHub. Take care of this before pushing anything, so
        // we don't leave the Pull Request half-updated.
        if !keeps_base {
            leave_github_stack(gh, pull_request.number).await?;
        }

        // Report the update of the Pull Request branch (there may be none,
        // if only the base branch changes)
        if plan.new_head_parents.is_some() {
            let (icon, what) = if plan.merges {
                ("⚾", "rebased")
            } else {
                ("🔁", "changed")
            };
            output(
                icon,
                &format!(
                    "Commit was {what} - updating Pull Request #{}",
                    pull_request.number
                ),
            )?;
        }
    }

    // Push the new commit onto the Pull Request branch (and also the new base
    // commit, if we added that to push_specs above).
    gh.remote()
        .push_to_remote(push_specs.as_slice())
        .context("git push failed".to_string())?;
    record_expected_head(git, message, new_commits.head)?;

    if let Some(pull_request) = pull_request {
        // We are updating an existing Pull Request

        // Things we want to update in the Pull Request on GitHub
        let mut pull_request_updates: PullRequestUpdate = Default::default();

        if opts.update_message {
            pull_request_updates.update_message(&pull_request, message);
        }

        // If the Pull Request is not based on the right branch yet, change
        // that now.
        let new_base = base_branch.as_ref().unwrap_or(&config.master_ref);
        let base_is_changing =
            pull_request.base.branch_name() != new_base.branch_name();
        if base_is_changing {
            pull_request_updates.base =
                Some(new_base.branch_name().to_string());
        }

        if !pull_request_updates.is_empty() {
            gh.update_pull_request(pull_request.number, pull_request_updates)
                .await?;
        }

        if base_is_changing {
            base_changed(
                gh,
                config,
                &pull_request.base,
                new_base,
                stack_on.as_ref().map(|pr| pr.number),
            )?;
        }
    } else {
        // We are creating a new Pull Request.

        // Call GitHub to create the Pull Request.
        let pull_request_number = gh
            .create_pull_request(
                message,
                base_branch
                    .as_ref()
                    .unwrap_or(&config.master_ref)
                    .branch_name()
                    .to_string(),
                pull_request_branch.branch_name().to_string(),
                opts.draft,
            )
            .await?;

        let pull_request_url = config.pull_request_url(pull_request_number);

        output(
            "✨",
            &format!(
                "Created new Pull Request #{}: {}",
                pull_request_number, pull_request_url,
            ),
        )?;

        message.set_trailer("Pull-request".to_string(), pull_request_url);
        local_commit.pull_request_number = Some(pull_request_number);

        let result = gh
            .request_reviewers(pull_request_number, requested_reviewers)
            .await;
        match result {
            Ok(()) => (),
            Err(report) => {
                output("⚠️", "Requesting reviewers failed")?;
                for message in report.chain() {
                    output("  ", &message.to_string())?;
                }
            }
        }
    }

    Ok(())
}

/// Check whether the Pull Request has changes that aren't in the local commit
/// (e.g. pushed by somebody else), by comparing its head with the expected
/// head recorded for the local commit (see `Git::get_expected_head`). Only
/// commits with an ID have such a record.
///
/// Fails if there are such changes, unless the user gave `--force`. Returns
/// whether to go ahead, which in a dry run is false if a real run would fail.
///
/// If the Pull Request is based on a newer master commit than the local
/// commit (e.g. after GitHub's "Update branch"), it fails even with
/// `--force`: overwriting would revert those master changes in the Pull
/// Request. The local commit needs to be rebased first.
fn check_remote_changes(
    opts: &DiffOptions,
    git: &crate::git::Git,
    gh: &crate::github::GitHub,
    config: &crate::config::Config,
    message: &crate::message::CommitMessage,
    commits: &PullRequestCommits,
    pull_request: &PullRequest,
) -> Result<bool> {
    let Some(expected_head) = message
        .spr_id()
        .map(|spr_id| git.get_expected_head(spr_id))
        .transpose()?
        .flatten()
    else {
        return Ok(true);
    };

    match commits.check_expected_head(git, expected_head) {
        Err(error) if error.downcast_ref::<HeadMoved>().is_some() => (),
        result => return result.map(|()| true),
    }

    let number = pull_request.number;

    // Is the Pull Request based on a newer master commit than the local one?
    let master = config.master_ref.branch_name();
    let master_tip = gh.remote().fetch_branch(master)?;
    let pr_master = git.repo().merge_base(commits.head, master_tip)?;
    if !git.is_ancestor(pr_master, commits.target)? {
        let newer_master = format!(
            "Pull Request #{number} is based on a newer `{master}` than your \
             local commit (e.g. somebody used GitHub's \"Update branch\")"
        );
        if opts.dry_run {
            output(
                "🔍",
                &format!(
                    "Would stop: {newer_master}. Rebase your local commit \
                     onto `{master}` first."
                ),
            )?;
            return Ok(false);
        }
        bail!(
            "{newer_master}. Rebase your local commit onto the current \
             `{master}` first, then run `spr diff` again."
        );
    }

    let changes = format!(
        "Pull Request #{number} has changes that aren't in your local commit \
         (e.g. somebody else pushed to it)"
    );
    match (opts.force, opts.dry_run) {
        (true, true) => {
            output(
                "🔍",
                &format!("{changes}. Would overwrite them (--force)."),
            )?;
            Ok(true)
        }
        (true, false) => {
            output("💪", &format!("{changes}. Overwriting them (--force)."))?;
            Ok(true)
        }
        (false, true) => {
            output(
                "🔍",
                &format!(
                    "Would stop: {changes}. Use `spr diff --force` to \
                     overwrite them."
                ),
            )?;
            Ok(false)
        }
        (false, false) => bail!(formatdoc!(
            "{changes}. Updating it would revert them.
             To get them locally, `spr patch {number}` checks out the Pull \
             Request's current state as a new branch. To overwrite them with \
             your local commit, run `spr diff --force`."
        )),
    }
}

/// Record the head of the Pull Request of the local commit with the given
/// message as its expected head, if the local commit has an ID (see
/// `Git::get_expected_head`)
fn record_expected_head(
    git: &crate::git::Git,
    message: &crate::message::CommitMessage,
    head: Oid,
) -> Result<()> {
    if let Some(spr_id) = message.spr_id() {
        git.set_expected_head(spr_id, head)?;
    }
    Ok(())
}

/// Report that the base of a Pull Request was changed, and delete its old base
/// branch if that was a base branch created by spr, as it's not needed
/// anymore.
///
/// This must only be called after the Pull Request's base has been changed:
/// GitHub closes Pull Requests whose base branch gets deleted.
fn base_changed(
    gh: &crate::github::GitHub,
    config: &crate::config::Config,
    old_base: &GitHubBranch,
    new_base: &GitHubBranch,
    stacked_on: Option<u64>,
) -> Result<()> {
    let description = if let Some(number) = stacked_on {
        format!("now stacked on Pull Request #{number}")
    } else if new_base.is_master_branch() {
        format!("now based directly on {}", new_base.branch_name())
    } else {
        format!("now based on branch {}", new_base.branch_name())
    };
    output(
        "🎯",
        &format!(
            "Pull Request is {description} - changed its base branch \
             accordingly"
        ),
    )?;

    if !config.is_spr_base_branch(old_base) {
        // Only delete base branches that spr created for this Pull Request.
        // Any other branch may be in use elsewhere (e.g. it's the branch of
        // another Pull Request).
        return Ok(());
    }

    let result = gh.remote().push_to_remote(&[PushSpec {
        oid: None,
        remote_ref: old_base.on_github(),
    }]);
    if let Err(error) = result {
        // The Pull Request is in the right state already, so this is not an
        // error. The branch is merely left behind.
        output(
            "⚠️",
            &format!(
                "Could not delete the old base branch {}: {}",
                old_base.branch_name(),
                error
            ),
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, MergeMethod, StackingMode};

    fn config(use_commit_title_for_initial_commit: bool) -> Config {
        Config::new(
            "acme".into(),
            "codez".into(),
            "master".into(),
            "spr/foo/".into(),
            "xyz".into(),
            false,
            MergeMethod::Squash,
            StackingMode::BaseBranches,
            use_commit_title_for_initial_commit,
            true,
        )
        .unwrap()
    }

    fn created_using_spr() -> String {
        format!("Created using spr {}", env!("CARGO_PKG_VERSION"))
    }

    #[test]
    fn test_initial_commit_message() {
        assert_eq!(
            pull_request_commit_message(None, "Fix the bug", &config(false)),
            format!("[𝘀𝗽𝗿] initial version\n\n{}", created_using_spr())
        );
    }

    #[test]
    fn test_initial_commit_message_with_commit_title() {
        assert_eq!(
            pull_request_commit_message(None, "Fix the bug", &config(true)),
            format!("Fix the bug\n\n{}", created_using_spr())
        );
    }

    #[test]
    fn test_update_commit_message() {
        for use_commit_title in [false, true] {
            assert_eq!(
                pull_request_commit_message(
                    Some("Address review comments"),
                    "Fix the bug",
                    &config(use_commit_title),
                ),
                format!("Address review comments\n\n{}", created_using_spr())
            );
        }
    }
}
