/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::collections::HashSet;
use std::iter::zip;

use color_eyre::eyre::{Error, Result, WrapErr as _, bail, eyre};
use log::debug;

use crate::{
    git::PreparedCommit,
    git_remote::PushSpec,
    github::{
        GitHub, PullRequest, PullRequestRequestReviewers, PullRequestState,
        PullRequestUpdate,
    },
    message::{MessageSection, validate_commit_message},
    output::{output, write_commit_title},
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
            let head_oid = prepared_commits.last().unwrap().oid;
            Some(HashSet::from([head_oid]))
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
    let mut selected_indices = HashSet::new();

    for (i, (prepared_commit, pull_request_task)) in
        zip(prepared_commits.iter_mut(), pull_request_tasks.into_iter())
            .enumerate()
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
            continue;
        }

        selected_indices.insert(i);

        let pull_request = if let Some(task) = pull_request_task {
            Some(task.await??)
        } else {
            None
        };

        write_commit_title(prepared_commit)?;

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
        )
        .await;
    }

    // This updates the commit message in the local Git repository (if it was
    // changed by the implementation)
    git.rewrite_commit_messages(prepared_commits.as_mut_slice(), None)?;

    // Create or update dependency comments for each PR
    if config.create_dependency_comments && !opts.cherry_pick {
        debug!("Checking for dependency comments");
        for (i, prepared_commit) in prepared_commits.iter().enumerate() {
            if !selected_indices.contains(&i) {
                continue;
            }

            if let Some(number) = prepared_commit.pull_request_number {
                let marker = "<!-- spr-dependencies -->";
                let existing_comment = gh.find_comment(number, marker).await;

                if let Some(body) = build_dependency_body(
                    i,
                    prepared_commits.as_slice(),
                    existing_comment.as_ref().ok().map(|(_, b)| b.as_str()),
                ) {
                    debug!(
                        "PR #{}: dependency comment body generated:\n{}",
                        number, body
                    );
                    if let Ok((comment_id, old_body)) = existing_comment {
                        if old_body == body {
                            debug!(
                                "PR #{}: existing comment {} is identical, skipping update",
                                number, comment_id
                            );
                        } else {
                            debug!(
                                "PR #{}: updating existing comment {}\nOld body:\n{}",
                                number, comment_id, old_body
                            );
                            gh.update_comment(comment_id, body).await?;
                        }
                    } else {
                        debug!(
                            "PR #{}: creating new dependency comment",
                            number
                        );
                        gh.create_comment(number, body).await?;
                    }
                } else {
                    debug!("PR #{}: no dependencies, skipping comment", number);
                }
            }
        }
    }

    result
}

const LIST_START_MARKER: &str = "<!-- spr-dependencies-list-start -->";
const LIST_END_MARKER: &str = "<!-- spr-dependencies-list-end -->";

fn build_dependency_body(
    i: usize,
    prepared_commits: &[PreparedCommit],
    existing_body: Option<&str>,
) -> Option<String> {
    if prepared_commits[i].is_cherry_pick {
        return None;
    }

    let mut current_stack_prs = Vec::new();
    for pc in prepared_commits[0..i].iter().rev() {
        if let Some(num) = pc.pull_request_number {
            current_stack_prs.push(num);
        }
    }
    current_stack_prs.reverse();

    let mut dependencies = Vec::new();
    let mut seen = HashSet::new();

    if let Some(body) = existing_body {
        let re = lazy_regex::regex!(r"^- #(\d+)");
        for line in body.lines() {
            if let Some(caps) = re.captures(line.trim()) {
                if let Ok(num) = caps[1].parse::<u64>() {
                    if !current_stack_prs.contains(&num)
                        && Some(num) != prepared_commits[i].pull_request_number
                        && !seen.contains(&num)
                    {
                        dependencies.push(format!("- #{}", num));
                        seen.insert(num);
                    }
                }
            }
        }
    }

    for num in current_stack_prs {
        if !seen.contains(&num) {
            dependencies.push(format!("- #{}", num));
            seen.insert(num);
        }
    }

    if dependencies.is_empty() {
        None
    } else {
        let marker = "<!-- spr-dependencies -->";
        let list_content = dependencies.join("\n");
        let list_with_markers = format!(
            "{}\n{}\n{}",
            LIST_START_MARKER, list_content, LIST_END_MARKER
        );

        if let Some(body) = existing_body {
            if let (Some(start), Some(end)) =
                (body.find(LIST_START_MARKER), body.find(LIST_END_MARKER))
            {
                if start < end {
                    let mut new_body = body[..start].to_string();
                    new_body.push_str(&list_with_markers);
                    new_body.push_str(&body[end + LIST_END_MARKER.len()..]);
                    return Some(new_body);
                }
            }
            // If markers not found or invalid, append to the end
            let mut new_body = body.trim_end().to_string();
            new_body.push_str("\n\n");
            new_body.push_str(&list_with_markers);
            Some(new_body)
        } else {
            Some(format!(
                "{}\nThis is a stacked pull request managed by [spr](https://github.com/spacedentist/spr). The following pull requests must be merged before this one:\n\n{}\n",
                marker,
                list_with_markers
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageSection;
    use std::collections::BTreeMap;

    fn make_commit(
        oid_byte: u8,
        pr: Option<u64>,
        title: &str,
    ) -> PreparedCommit {
        let mut message = BTreeMap::new();
        message.insert(MessageSection::Title, title.to_string());
        PreparedCommit {
            oid: git2::Oid::from_bytes(&[oid_byte; 20]).unwrap(),
            short_id: format!("{:x}", oid_byte),
            parent_oid: git2::Oid::from_bytes(&[0; 20]).unwrap(),
            message,
            pull_request_number: pr,
            is_cherry_pick: false,
        }
    }

    #[test]
    fn test_build_dependency_body() {
        let commits = vec![
            make_commit(1, Some(101), "Commit 1"),
            make_commit(2, Some(102), "Commit 2"),
            make_commit(3, Some(103), "Commit 3"),
        ];

        // First commit has no dependencies
        assert_eq!(build_dependency_body(0, &commits, None), None);

        // Second commit depends on first
        let body2 = build_dependency_body(1, &commits, None).unwrap();
        assert!(body2.contains("- #101"));
        assert!(!body2.contains("Commit 1"));
        assert!(body2.contains("stacked pull request"));

        // Third commit depends on first and second
        let body3 = build_dependency_body(2, &commits, None).unwrap();
        assert!(body3.contains("- #101"));
        assert!(body3.contains("- #102"));
    }

    #[test]
    fn test_build_dependency_body_reorder_scenario() {
        // Initial stack: Master -> C1 -> C2 -> C3
        let mut stack = vec![
            make_commit(1, Some(101), "Commit 1"),
            make_commit(2, Some(102), "Commit 2"),
            make_commit(3, Some(103), "Commit 3"),
        ];

        // PR1: No dependencies
        assert_eq!(build_dependency_body(0, &stack, None), None);

        // PR2: Depends on PR1
        let body2 = build_dependency_body(1, &stack, None).unwrap();
        assert!(body2.contains("- #101"));
        assert!(!body2.contains("#102"));
        assert!(!body2.contains("#103"));

        // PR3: Depends on PR1 and PR2
        let body3 = build_dependency_body(2, &stack, None).unwrap();
        assert!(body3.contains("- #101"));
        assert!(body3.contains("- #102"));
        assert!(!body3.contains("#103"));

        // Reorder stack: Master -> C1 -> C3 -> C2
        stack.swap(1, 2);

        // PR3: Now depends only on PR1
        let body3_new = build_dependency_body(1, &stack, None).unwrap();
        assert!(body3_new.contains("- #101"));
        assert!(!body3_new.contains("#102"));
        assert!(!body3_new.contains("#103"));

        // PR2: Now depends on PR1 and PR3
        let body2_new = build_dependency_body(2, &stack, None).unwrap();
        assert!(body2_new.contains("- #101"));
        assert!(body2_new.contains("- #103"));
        assert!(!body2_new.contains("#102"));
    }

    #[test]
    fn test_build_dependency_body_with_cherry_pick() {
        let mut stack = vec![
            make_commit(1, Some(101), "Commit 1"),
            make_commit(2, Some(102), "Commit 2"),
            make_commit(3, Some(103), "Commit 3"),
        ];

        // C2 is cherry-picked
        stack[1].is_cherry_pick = true;

        // PR1: No dependencies
        assert_eq!(build_dependency_body(0, &stack, None), None);

        // PR2: Cherry-picked, so no dependencies
        assert_eq!(build_dependency_body(1, &stack, None), None);

        // PR3: Depends on PR2 and PR1 (even though PR2 is a cherry-pick)
        let body3 = build_dependency_body(2, &stack, None).unwrap();
        assert!(body3.contains("- #102"));
        assert!(body3.contains("- #101"));
    }

    #[test]
    fn test_build_dependency_body_retains_merged() {
        let mut commits = vec![
            make_commit(1, Some(101), "Commit 1"),
            make_commit(2, Some(102), "Commit 2"),
            make_commit(3, Some(103), "Commit 3"),
        ];
        // Pull request 101 was merged and is no longer in the stack.
        let initial_body = build_dependency_body(2, &commits, None).unwrap();
        assert!(initial_body.contains("- #101"), "Should include dependency PR #101");

        commits.remove(0); // PR 101 was merged, so no longer in dependencies
        let new_body = build_dependency_body(1, &commits, Some(initial_body.as_str())).unwrap();
        assert!(new_body.contains("- #101"), "Should retain merged PR #101");
        assert!(new_body.contains("- #102"), "Should include current dependency PR #102");

        // Ensure #101 is before #102 if it was before in the existing body
        let pos101 = new_body.find("#101").unwrap();
        let pos102 = new_body.find("#102").unwrap();
        assert!(pos101 < pos102);
    }

    #[test]
    fn test_build_dependency_body_preserves_manual_edits() {
        let commits = vec![
            make_commit(1, Some(101), "Commit 1"),
            make_commit(2, Some(102), "Commit 2"),
        ];
        let existing_body = "<!-- spr-dependencies -->\n\
                             MANUAL NOTE: THIS IS IMPORTANT\n\n\
                             <!-- spr-dependencies-list-start -->\n\
                             - #100\n\
                             <!-- spr-dependencies-list-end -->\n\
                             ANOTHER MANUAL NOTE";

        let body = build_dependency_body(1, &commits, Some(existing_body)).unwrap();
        
        assert!(body.contains("MANUAL NOTE: THIS IS IMPORTANT"));
        assert!(body.contains("ANOTHER MANUAL NOTE"));
        assert!(body.contains("- #100"), "Should retain merged/manual PR #100");
        assert!(body.contains("- #101"), "Should include current dependency PR #101");
        assert!(!body.contains("- #102"), "Should not include itself");
        
        // Ensure markers are still there
        assert!(body.contains(LIST_START_MARKER));
        assert!(body.contains(LIST_END_MARKER));
        
        // Verify manual addition to the list is preserved
        let existing_body_with_manual_pr = "<!-- spr-dependencies -->\n\
                                            <!-- spr-dependencies-list-start -->\n\
                                            - #999\n\
                                            - #101\n\
                                            <!-- spr-dependencies-list-end -->";
        let body2 = build_dependency_body(1, &commits, Some(existing_body_with_manual_pr)).unwrap();
        assert!(body2.contains("- #999"));
        assert!(body2.contains("- #101"));
    }
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
) -> Result<()> {
    // Parsed commit message of the local commit
    let message = &mut local_commit.message;

    // Check if the local commit is based directly on the master branch.
    let directly_based_on_master = local_commit.parent_oid == master_base_oid;

    // Determine the trees the Pull Request branch and the base branch should
    // have when we're done here.
    let (new_head_tree, new_base_tree) = if !opts.cherry_pick
        || directly_based_on_master
    {
        // Unless the user tells us to --cherry-pick, these should be the trees
        // of the current commit and its parent.
        // If the current commit is directly based on master (i.e.
        // directly_based_on_master is true), then we can do this here even when
        // the user tells us to --cherry-pick, because we would cherry pick the
        // current commit onto its parent, which gives us the same tree as the
        // current commit has, and the master base is the same as this commit's
        // parent.
        let head_tree = git.get_tree_oid_for_commit(local_commit.oid)?;
        let base_tree = git.get_tree_oid_for_commit(local_commit.parent_oid)?;

        (head_tree, base_tree)
    } else {
        // Cherry-pick the current commit onto master
        let index = git.cherrypick(local_commit.oid, master_base_oid)?;

        if index.has_conflicts() {
            bail!(
                "This commit cannot be cherry-picked on {master}.",
                master = config.master_ref.branch_name(),
            );
        }

        // This is the tree we are getting from cherrypicking the local commit
        // on master.
        let cherry_pick_tree = git.write_index(index)?;
        let master_tree = git.get_tree_oid_for_commit(master_base_oid)?;

        (cherry_pick_tree, master_tree)
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

    if let Some(ref pr) = pull_request {
        local_commit.is_cherry_pick = pr.base.is_master_branch();
    }

    if local_commit.pull_request_number.is_none() || opts.update_message {
        validate_commit_message(message, config)?;
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

    // Parse "Reviewers" section, if this is a new Pull Request
    let mut requested_reviewers = PullRequestRequestReviewers::default();

    if local_commit.pull_request_number.is_none()
        && let Some(reviewers) = message.get(&MessageSection::Reviewers)
    {
        let reviewers = parse_name_list(reviewers);
        let mut checked_reviewers = Vec::new();

        for reviewer in reviewers {
            // Teams are indicated with a leading #
            if let Some(slug) = reviewer.strip_prefix('#') {
                if let Ok(team) =
                    GitHub::get_github_team((&config.owner).into(), slug.into())
                        .await
                {
                    requested_reviewers
                        .team_reviewers
                        .push(team.slug.to_string());

                    checked_reviewers.push(reviewer);
                } else {
                    bail!(
                        "Reviewers field contains unknown team '{}'",
                        reviewer,
                    );
                }
            } else if let Ok(user) =
                GitHub::get_github_user(reviewer.clone()).await
            {
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
            } else {
                bail!("Reviewers field contains unknown user '{}'", reviewer);
            }
        }

        message.insert(MessageSection::Reviewers, checked_reviewers.join(", "));
    }

    // Get the name of the existing Pull Request branch, or constuct one if
    // there is none yet.

    let title = message
        .get(&MessageSection::Title)
        .map(|t| &t[..])
        .unwrap_or("");

    let pull_request_branch = match &pull_request {
        Some(pr) => pr.head.clone(),
        None => {
            config.new_github_branch(&gh.remote().find_unused_branch_name(
                &config.branch_prefix,
                &slugify(title),
            )?)
        }
    };

    // Get the tree ids of the current head of the Pull Request, as well as the
    // base, and the commit id of the master commit this PR is currently based
    // on.
    // If there is no pre-existing Pull Request, we fill in the equivalent
    // values.
    let (pr_head_oid, pr_head_tree, pr_base_oid, pr_base_tree, pr_master_base) =
        if let Some(pr) = &pull_request {
            let pr_head_tree = git.get_tree_oid_for_commit(pr.head_oid)?;

            let current_master_oid =
                gh.remote().fetch_branch(config.master_ref.branch_name())?;
            let pr_base_oid =
                git.repo().merge_base(pr.head_oid, pr.base_oid)?;
            let pr_base_tree = git.get_tree_oid_for_commit(pr_base_oid)?;

            let pr_master_base =
                git.repo().merge_base(pr.head_oid, current_master_oid)?;

            (
                pr.head_oid,
                pr_head_tree,
                pr_base_oid,
                pr_base_tree,
                pr_master_base,
            )
        } else {
            let master_base_tree =
                git.get_tree_oid_for_commit(master_base_oid)?;
            (
                master_base_oid,
                master_base_tree,
                master_base_oid,
                master_base_tree,
                master_base_oid,
            )
        };
    let needs_merging_master = pr_master_base != master_base_oid;

    // At this point we can check if we can exit early because no update to the
    // existing Pull Request is necessary
    if let Some(ref pull_request) = pull_request {
        // So there is an existing Pull Request...
        if !needs_merging_master
            && pr_head_tree == new_head_tree
            && pr_base_tree == new_base_tree
        {
            // ...and it does not need a rebase, and the trees of both Pull
            // Request branch and base are all the right ones.
            output("✅", "No update necessary")?;

            if opts.update_message {
                // However, the user requested to update the commit message on
                // GitHub

                let mut pull_request_updates: PullRequestUpdate =
                    Default::default();
                pull_request_updates.update_message(pull_request, message);

                if !pull_request_updates.is_empty() {
                    // ...and there are actual changes to the message
                    gh.update_pull_request(
                        pull_request.number,
                        pull_request_updates,
                    )
                    .await?;
                    output("✍", "Updated commit message on GitHub")?;
                }
            }

            return Ok(());
        }
    }

    // Check if there is a base branch on GitHub already. That's the case when
    // there is an existing Pull Request, and its base is not the master branch.
    let base_branch = if let Some(ref pr) = pull_request {
        if pr.base.is_master_branch() {
            None
        } else {
            Some(pr.base.clone())
        }
    } else {
        None
    };

    // We are going to construct `pr_base_parent: Option<Oid>`.
    // The value will be the commit we have to merge into the new Pull Request
    // commit to reflect changes in the parent of the local commit (by rebasing
    // or changing commits between master and this one, although technically
    // that's also rebasing).
    // If it's `None`, then we will not merge anything into the new Pull Request
    // commit.
    // If we are updating an existing PR, then there are three cases here:
    // (1) the parent tree of this commit is unchanged and we do not need to
    //     merge in master, which means that the local commit was amended, but
    //     not rebased. We don't need to merge anything into the Pull Request
    //     branch.
    // (2) the parent tree has changed, but the parent of the local commit is on
    //     master (or we are cherry-picking) and we are not already using a base
    //     branch: in this case we can merge the master commit we are based on
    //     into the PR branch, without going via a base branch. Thus, we don't
    //     introduce a base branch here and the PR continues to target the
    //     master branch.
    // (3) the parent tree has changed, and we need to use a base branch (either
    //     because one was already created earlier, or we find that we are not
    //     directly based on master now): we need to construct a new commit for
    //     the base branch. That new commit's tree is always that of that local
    //     commit's parent (thus making sure that the difference between base
    //     branch and pull request branch are exactly the changes made by the
    //     local commit, thus the changes we want to have reviewed). The new
    //     commit may have one or two parents. The previous base is always a
    //     parent (that's either the current commit on an existing base branch,
    //     or the previous master commit the PR was based on if there isn't a
    //     base branch already). In addition, if the master commit this commit
    //     is based on has changed, (i.e. the local commit got rebased on newer
    //     master in the meantime) then we have to merge in that master commit,
    //     which will be the second parent.
    // If we are creating a new pull request then `pr_base_tree` (the current
    // base of the PR) was set above to be the tree of the master commit the
    // local commit is based one, whereas `new_base_tree` is the tree of the
    // parent of the local commit. So if the local commit for this new PR is on
    // master, those two are the same (and we want to apply case 1). If the
    // commit is not directly based on master, we have to create this new PR
    // with a base branch, so that is case 3.

    let (pr_base_parent, base_branch) =
        if pr_base_tree == new_base_tree && !needs_merging_master {
            // Case 1
            (None, base_branch)
        } else if base_branch.is_none()
            && (directly_based_on_master || opts.cherry_pick)
        {
            // Case 2
            (Some(master_base_oid), None)
        } else {
            // Case 3

            // We are constructing a base branch commit.
            // One parent of the new base branch commit will be the current base
            // commit, that could be either the top commit of an existing base
            // branch, or a commit on master.
            let mut parents = vec![pr_base_oid];

            // If we need to rebase on master, make the master commit also a
            // parent (except if the first parent is that same commit, we don't
            // want duplicates in `parents`).
            if needs_merging_master && pr_base_oid != master_base_oid {
                parents.push(master_base_oid);
            }

            let new_base_branch_commit = git.create_derived_commit(
                local_commit.parent_oid,
                &format!(
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
                ),
                new_base_tree,
                &parents[..],
            )?;

            // If `base_branch` is `None` (which means a base branch does not exist
            // yet), then make a `GitHubBranch` with a new name for a base branch
            let base_branch = if let Some(base_branch) = base_branch {
                base_branch
            } else {
                config.new_github_branch(&gh.remote().find_unused_branch_name(
                    &config.branch_prefix,
                    &format!(
                        "{}.{}",
                        config.master_ref.branch_name(),
                        &slugify(title),
                    ),
                )?)
            };

            (Some(new_base_branch_commit), Some(base_branch))
        };

    let mut github_commit_message = opts.message.clone();
    if pull_request.is_some() && github_commit_message.is_none() {
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

    // Construct the new commit for the Pull Request branch. First parent is the
    // current head commit of the Pull Request (we set this to the master base
    // commit earlier if the Pull Request does not yet exist)
    let mut pr_commit_parents = vec![pr_head_oid];

    // If we prepared a commit earlier that needs merging into the Pull Request
    // branch, then that commit is a parent of the new Pull Request commit.
    if let Some(oid) = pr_base_parent {
        // ...unless if that's the same commit as the one we added to
        // pr_commit_parents first.
        if pr_commit_parents.first() != Some(&oid) {
            pr_commit_parents.push(oid);
        }
    }

    // Create the new commit
    let pr_commit = git.create_derived_commit(
        local_commit.oid,
        &format!(
            "{}\n\nCreated using spr {}",
            github_commit_message
                .as_ref()
                .map(|s| &s[..])
                .unwrap_or("[𝘀𝗽𝗿] initial version"),
            env!("CARGO_PKG_VERSION"),
        ),
        new_head_tree,
        &pr_commit_parents[..],
    )?;

    let mut push_specs = vec![PushSpec {
        oid: Some(pr_commit),
        remote_ref: pull_request_branch.on_github(),
    }];

    if let Some(pull_request) = pull_request {
        // We are updating an existing Pull Request

        if needs_merging_master {
            output(
                "⚾",
                &format!(
                    "Commit was rebased - updating Pull Request #{}",
                    pull_request.number
                ),
            )?;
        } else {
            output(
                "🔁",
                &format!(
                    "Commit was changed - updating Pull Request #{}",
                    pull_request.number
                ),
            )?;
        }

        // Things we want to update in the Pull Request on GitHub
        let mut pull_request_updates: PullRequestUpdate = Default::default();

        if opts.update_message {
            pull_request_updates.update_message(&pull_request, message);
        }

        if let Some(base_branch) = base_branch {
            // We are using a base branch.

            if let Some(base_branch_commit) = pr_base_parent {
                // ...and we prepared a new commit for it, so we need to push an
                // update of the base branch.
                push_specs.push(PushSpec {
                    oid: Some(base_branch_commit),
                    remote_ref: base_branch.on_github(),
                });
            }

            // Push the new commit onto the Pull Request branch (and also the
            // new base commit, if we added that to push_specs above).
            gh.remote()
                .push_to_remote(push_specs.as_slice())
                .context("git push failed".to_string())?;

            // If the Pull Request's base is not set to the base branch yet,
            // change that now.
            if pull_request.base.branch_name() != base_branch.branch_name() {
                pull_request_updates.base =
                    Some(base_branch.branch_name().to_string());
            }
        } else {
            // The Pull Request is against the master branch. In that case we
            // only need to push the update to the Pull Request branch.
            gh.remote()
                .push_to_remote(push_specs.as_slice())
                .context("git push failed".to_string())?;
        }

        if !pull_request_updates.is_empty() {
            gh.update_pull_request(pull_request.number, pull_request_updates)
                .await?;
        }
    } else {
        // We are creating a new Pull Request.

        // If there's a base branch, add it to the push
        if let (Some(base_branch), Some(base_branch_commit)) =
            (&base_branch, pr_base_parent)
        {
            push_specs.push(PushSpec {
                oid: Some(base_branch_commit),
                remote_ref: base_branch.on_github(),
            });
        }
        // Push the pull request branch and the base branch if present
        gh.remote()
            .push_to_remote(push_specs.as_slice())
            .context("git push failed".to_string())?;

        // Then call GitHub to create the Pull Request.
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
                pull_request_number, &pull_request_url,
            ),
        )?;

        message.insert(MessageSection::PullRequest, pull_request_url);
        local_commit.pull_request_number = Some(pull_request_number);
        local_commit.is_cherry_pick = opts.cherry_pick;

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
