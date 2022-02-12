use std::io::Write;

use crate::{
    error::{Error, Result},
    github::{
        PullRequestRequestReviewers, PullRequestState, PullRequestUpdate,
    },
    message::{build_github_body, validate_commit_message, MessageSection},
    output::{output, write_commit_title},
    utils::{parse_name_list, remove_all_parens},
};
use git2::Oid;
use indoc::formatdoc;

#[derive(Debug, clap::Parser)]
pub struct DiffOptions {
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

    /// Stack this Pull Request onto another one. Valid values are a Pull
    /// Request number, or 'parent' to look up the Pull Request number from the
    /// parent commit. To turn an existing Pull Request into one that is not
    /// stacked, use '--stack none'.
    #[clap(long, rename_all = "lower")]
    stack: Option<StackOption>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiffOptionsError {
    #[error(
        "valid values are `none`, `parent` or a number, but was set to `{0}`"
    )]
    Stack(String),
}

#[derive(Debug, clap::ArgEnum, PartialEq, Eq)]
enum StackOption {
    None,
    Parent,
    Number(u64),
}

impl std::str::FromStr for StackOption {
    type Err = DiffOptionsError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "none" => Ok(StackOption::None),
            "parent" => Ok(StackOption::Parent),
            _ => s
                .parse::<u64>()
                .map(StackOption::Number)
                .map_err(|_| DiffOptionsError::Stack(s.into())),
        }
    }
}

pub async fn diff(
    opts: DiffOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    let mut prepared_commits = git.get_prepared_commits(config).await?;

    let base_oid = prepared_commits[0].parent_oid;

    let mut prepared_commit = match prepared_commits.pop() {
        Some(c) => c,
        None => {
            output("👋", "Branch is empty - nothing to do. Good bye!")?;
            return Ok(());
        }
    };

    let parent_commit = prepared_commits.pop();
    drop(prepared_commits);

    write_commit_title(&prepared_commit)?;

    let message = &mut prepared_commit.message;

    let mut stack_on_number: Option<u64>;

    // Evaluate the --stack option
    if opts.stack == Some(StackOption::None) {
        stack_on_number = None;
    } else if opts.stack == Some(StackOption::Parent) {
        let parent = match &parent_commit {
            Some(c) => c,
            None => {
                return Err(Error::new("--stack=parent was given, but parent is not on branch. Did you mean --stack=none?"));
            }
        };
        if let Some(number) = parent.pull_request_number {
            stack_on_number = Some(number);
        } else {
            return Err(Error::new("--stack=parent was given, but parent commit has no Pull Request"));
        }
    } else if let Some(StackOption::Number(number)) = opts.stack {
        stack_on_number = Some(number);
    } else {
        // No --stack option was given, extract information from commit message
        stack_on_number = message
            .get(&MessageSection::StackedOn)
            .map(|text| config.parse_pull_request_field(text))
            .flatten();
    }

    // If this is a new Pull Request and the commit message has a "Reviewers"
    // section, then start getting a list of eligible reviewers in the
    // background;
    let eligible_reviewers = if prepared_commit.pull_request_number.is_none()
        && message.contains_key(&MessageSection::Reviewers)
    {
        Some(gh.get_reviewers())
    } else {
        None
    };

    if let Some(number) = prepared_commit.pull_request_number {
        output("#️⃣ ", &format!("Pull Request #{}", number))?;
    }

    if prepared_commit.pull_request_number.is_none() || opts.update_message {
        validate_commit_message(message)?;
    }

    // Load Pull Request information
    let pr_future = prepared_commit
        .pull_request_number
        .map(|number| gh.get_pull_request(number, git));
    let stacked_on_pull_request = if let Some(number) = stack_on_number {
        Some(gh.get_pull_request(number, git).await??)
    } else {
        None
    };
    let pull_request = if let Some(f) = pr_future {
        Some(f.await??)
    } else {
        None
    };

    if let Some(pr) = &pull_request {
        if pr.state == PullRequestState::Closed {
            return Err(Error::new(formatdoc!(
                "Pull request is closed. If you want to open a new one, \
                 remove the 'Pull Request' section from the commit message."
            )));
        }
    }

    if let Some(pr) = &stacked_on_pull_request {
        if pr.state == PullRequestState::Closed {
            if opts.stack.is_some() {
                return Err(Error::new(formatdoc!(
                    "--stack option given, but referred to a closed pull \
                     request"
                )));
            } else {
                stack_on_number = None;
            }
        }
    }

    if opts.stack.is_none() {
        if let Some(pr) = &pull_request {
            let requested_base: &str = match &stacked_on_pull_request {
                Some(stacked_on) => &stacked_on.head,
                None => &config.master_ref,
            };

            if pr.base != requested_base {
                return Err(Error::new(formatdoc!(
                    "Please use the --stack option to clarify what this \
                     pull request should be stacked on. (You can pass a \
                     Pull Request number or 'none' to the --stack option.)"
                )));
            }
        }
    }

    let mut requested_reviewers = PullRequestRequestReviewers::default();

    if let (Some(task), Some(reviewers)) =
        (eligible_reviewers, message.get(&MessageSection::Reviewers))
    {
        let eligible_reviewers = task.await??;

        let reviewers = parse_name_list(reviewers);
        let mut checked_reviewers = Vec::new();

        for reviewer in reviewers {
            if let Some(entry) = eligible_reviewers.get(&reviewer) {
                if let Some(slug) = reviewer.strip_prefix('#') {
                    requested_reviewers.team_reviewers.push(slug.to_string());
                } else {
                    requested_reviewers.reviewers.push(reviewer.clone());
                }

                if let Some(name) = entry {
                    checked_reviewers.push(format!(
                        "{} ({})",
                        reviewer,
                        remove_all_parens(name)
                    ));
                } else {
                    checked_reviewers.push(reviewer);
                }
            } else {
                return Err(Error::new(format!(
                    "Reviewers field contains unknown user/team '{}'",
                    reviewer
                )));
            }
        }

        message.insert(MessageSection::Reviewers, checked_reviewers.join(", "));
    }

    // If we are stacking, we want to base the GitHub commit for this pull
    // request on the current state of the stacked-on pull request. If not
    // stacking, we are basing on the parent of our local branch (the commit in
    // the remote master lineage from which the branch branches off).
    let (base_oid, base_github_ref) = match &stacked_on_pull_request {
        Some(stacked_on) => (stacked_on.head_oid, stacked_on.head.clone()),
        None => (base_oid, format!("refs/heads/{}", &config.master_branch)),
    };

    let index = git.cherrypick(prepared_commit.oid, base_oid).await?;

    if index.has_conflicts() {
        if let Some(number) = stack_on_number {
            return Err(Error::new(formatdoc!(
                "
                This commit cannot be applied on top of the current version of \
                Pull Request #{number}.
                You either need to update the other pull request first or you \
                have to stack this commit on a different pull request to \
                include intermediate commits.
            "
            )));
        } else {
            let master_branch = &config.master_branch;
            return Err(Error::new(formatdoc!(
                "This commit cannot be applied directly on the target branch \
                 '{master_branch}'.
                 It probably depends on changes in intermediate commits.
                 Use the --stack option to declare what Pull Request this one \
                 should be stacked on."
            )));
        }
    }

    // Update 'Stacked On' section of commit message
    if let Some(number) = stack_on_number {
        message
            .insert(MessageSection::StackedOn, config.pull_request_url(number));
        output(
            "🥞",
            &format!(
                "This Pull Request is stacked on Pull Request #{}",
                number
            ),
        )?;
    } else {
        message.remove(&MessageSection::StackedOn);
        output(
            "🛬",
            &format!(
                "This Pull Request is for landing on the '{}' branch",
                config.master_branch
            ),
        )?;
    }

    // This is the tree we are getting from cherrypicking the local commit
    // on the selected base (master or stacked-on Pull Request).
    let tree_oid = git.write_index(index).await?;

    // Parents of the new Pull Request commit
    let mut new_pull_request_commit_parents = Vec::<Oid>::new();

    // Ref name on the GitHub side where we push the new commit (i.e. the Pull
    // Request branch). This remains null if we don't want to push to GitHub
    // (that's when a Pull Request exists and is up-to-date)
    let mut github_ref = None::<String>;

    if let Some(pr) = &pull_request {
        // update existing Pull Request

        // Is the tree we get from cherrypicking above different at all from the
        // current commit on the Pull Request?
        let update_tree =
            git.get_tree_oid_for_commit(pr.head_oid).await? != tree_oid;

        // This Pull Request should be based on the commit with oid `base_oid`. Is
        // that one already in the direct lineage of the current Pull Request
        // commit? (If it isn't we definitely need to update this Pull Request to
        // merge in that base.)
        let remerge_base = !git.is_based_on(pr.head_oid, base_oid).await?;

        if update_tree || remerge_base {
            if opts.message.is_none() {
                return Err(Error::new(formatdoc!(
                    "When updating an existing pull request, you must \
                     pass the --message option"
                )));
            }

            // First parent of the new Pull Request commit is always the previous
            // Pull Request commit.
            new_pull_request_commit_parents.push(pr.head_oid);

            // If we need to merge the current base back in, it is the second parent
            // of the new commit.
            if remerge_base {
                new_pull_request_commit_parents.push(base_oid);
            }

            // The Pull Request branch is taken from `pullRequest`
            github_ref = Some(pr.head.clone());

            if remerge_base {
                output(
                    "⚾",
                    &format!(
                        "Commit was rebased - updating Pull Request #{}",
                        pr.number
                    ),
                )?;
            } else {
                output(
                    "🔁",
                    &format!(
                        "Commit was changed - updating Pull Request #{}",
                        pr.number
                    ),
                )?;
            }
        } else {
            output("✅", "No update necessary")?;
        }
    } else {
        // Create new Pull Request

        // Only parent of this first commit on the Pull Request brach is the base.
        new_pull_request_commit_parents.push(base_oid);

        // Construct the name of the new Pull Request branch.
        github_ref = Some(format!(
            "refs/heads/{}",
            config.get_new_branch_name(
                &git.get_all_ref_names().await?,
                message
                    .get(&MessageSection::Title)
                    .map(|t| &t[..])
                    .unwrap_or("")
            )
        ));
    }

    if let Some(github_ref) = github_ref {
        // Create the new commit for this Pull Request.
        let new_pr_commit_oid = git
            .create_pull_request_commit(
                prepared_commit.oid,
                opts.message.as_ref().map(|s| &s[..]),
                tree_oid,
                &new_pull_request_commit_parents[..],
            )
            .await?;

        // And push it to GitHub.
        let git_push = async_process::Command::new("git")
            .arg("push")
            .arg("--")
            .arg(&config.remote_name)
            .arg(format!("{}:{}", new_pr_commit_oid, github_ref))
            .stdout(async_process::Stdio::null())
            .stderr(async_process::Stdio::piped())
            .output()
            .await?;

        if !git_push.status.success() {
            console::Term::stderr().write_all(&git_push.stderr)?;
            return Err(Error::new("git push failed"));
        }

        if pull_request.is_none() {
            let pull_request_number = gh
                .create_pull_request(
                    message,
                    base_github_ref.clone(),
                    github_ref,
                    opts.draft,
                )
                .await?;

            output(
                "✨",
                &format!(
                    "Created new Pull Request #{}: {}",
                    pull_request_number,
                    config.pull_request_url(pull_request_number)
                ),
            )?;
            message.insert(
                MessageSection::PullRequest,
                config.pull_request_url(pull_request_number),
            );

            gh.request_reviewers(pull_request_number, requested_reviewers)
                .await?;
        }
    }

    if let Some(pull_request) = pull_request {
        let mut updates: PullRequestUpdate = Default::default();

        if pull_request.base != base_github_ref {
            updates.base = Some(base_github_ref);
        }

        if opts.update_message {
            let title = message.get(&MessageSection::Title);
            if title != Some(&pull_request.title) {
                updates.title = title.cloned();
            }
        }

        let body = if opts.update_message {
            build_github_body(message)
        } else {
            let mut sections = pull_request.sections.clone();

            if let Some(stacked_on) = message.get(&MessageSection::StackedOn) {
                sections.insert(MessageSection::StackedOn, stacked_on.clone());
            } else {
                sections.remove(&MessageSection::StackedOn);
            }

            build_github_body(&sections)
        };

        if pull_request.body.as_ref() != Some(&body) {
            updates.body = Some(body);
        }

        if updates.base.is_some()
            || updates.title.is_some()
            || updates.body.is_some()
        {
            gh.update_pull_request(pull_request.number, updates).await?;
        }
    }

    git.rewrite_commit_messages(&mut [prepared_commit], None)
        .await
}
