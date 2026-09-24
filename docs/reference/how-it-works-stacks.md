# How it works - Stacked PRs

This section describes what `spr` does behind the scenes when you have a stack of commits on your local branch, each of which gets its own pull request. It builds on [How it works - Simple PR](./how-it-works-simple.md).

## The local branch is the source of truth

With `spr`, your pull requests are defined by the commits on your local branch. The only thing `spr` adds to a commit is the `Pull-request` trailer in its commit message, which links the commit to its pull request.

Every time you run `spr diff`, it projects the current state of each commit onto the pull request branches on GitHub: it adds a commit to the pull request branch that makes the branch reflect the local commit, and, if necessary, merges in whatever the local commit is now based on (a newer master commit, or the updated pull request of its parent commit).

`spr` never rewrites the history of a pull request branch. That way, reviewers can see every revision of a pull request, and what changed between revisions. But `spr` also doesn't rely on the branches staying unchanged between two runs of `spr diff`: it always looks at the current state of a pull request on GitHub, and adds to it whatever it takes to reflect the local commit.

## Stacking modes

A commit that is directly based on the master branch gets a pull request that targets the master branch. For a commit that is stacked on other commits, there are three options (the `spr.stackingMode` config option):

- **`base-branches`**: `spr` creates a synthetic base branch for the pull request, which contains the changes of all commits below. The pull request targets that base branch, so it only shows the changes of its own commit. Changes to the commits below appear on the base branch as commits titled "[𝘀𝗽𝗿] changes introduced through rebase". This keeps pull request timelines readable when pull requests are squash-merged, but doesn't work with merge commits: the synthetic commits would end up in the history of the master branch.

- **`chain`**: the pull request targets the pull request branch of its parent commit, and `spr` merges the head of that pull request into it. This is the classic way of stacking pull requests on GitHub, and works well with merge commits. With squash-merging, a pull request's timeline shows the commits of the pull requests below it once those have been squash-merged, because the squash commit on master is unrelated to the original commits.

- **`github-stack`**: like `chain`, and the pull requests are also linked as a stack using GitHub's [stacked pull requests](https://docs.github.com/en/pull-requests/get-started/about-stacked-prs) feature.

## What GitHub does when a pull request in a stack is merged

The following is based on experiments with GitHub's stacked pull requests preview, and may change.

When the bottom pull request of a stack on GitHub is merged (by `spr land`, or in the GitHub UI):

1. GitHub changes the base of the next pull request to the stack's base branch (master).

2. GitHub **rebases the branches of all remaining pull requests** onto the result of the merge and force-pushes them. This happens with both squash-merging and merge commits. Like `git rebase`, it drops merge commits, so the history of those branches becomes linear. Review comments move to the rebased commits and are not marked as outdated.

3. If rebasing a branch has conflicts (e.g. because the master branch has changed in the meantime in a way that conflicts with a pull request further up the stack), GitHub leaves that branch alone. Its pull request then shows the changes of the merged pull request, too, and has conflicts. Once you rebase your local branch and resolve the conflicts, `spr diff` merges the new master commit into the pull request branch, which brings the pull request back into a clean state.

Rebasing and force-pushing is not what `spr` would do, as it loses the history of the pull request branches. But it's not a problem for `spr` either: as described above, `spr` doesn't need the pull request branches to be unchanged. After rebasing your local branch, `spr diff` finds the rebased pull requests up to date, or adds to them as usual.

When `spr land` lands a stack with merge commits, GitHub creates a single merge commit on master for all the pull requests it lands at once.

## Changing the stack

GitHub doesn't allow changing the base of a pull request that is part of a stack. When a commit's parent changes (e.g. you reordered commits, dropped one, or it's now directly based on master), `spr diff` dissolves the stack before changing the base of the pull request, and in the `github-stack` mode creates a new stack at the end.
