![spr](./spr.svg)

# Introduction

spr is a command line tool for using a stacked-diff workflow with GitHub.

The idea behind spr is that your local branch management should not be dictated by your code-review tool. You should be able to send out code for review in individual commits, not branches. You make branches only when you want to, not because you _have_ to for every code review.

If you've used Phabricator and its command-line tool `arc`, you'll find spr very familiar.

To get started, see the [installation instructions](./user/installation.md), and the [first-time setup](./user/setup.md). (You'll need to go through setup in each repo where you want to use spr.)

## Workflow overview

In spr's workflow, you send out individual commits for review, not entire branches. This is the most basic version:

1. Make your change as a single commit, directly on your local `main`[^master] branch.

2. Run `spr diff` to send out your commit for review on GitHub.

3. If you need to make updates in response to feedback, amend your commit, and run `spr diff` again to send those updates to GitHub.

   Similarly, you can rebase onto newer upstream `main` and run `spr diff` to reflect any resulting changes to your commit.

4. Once reviewers have approved, run `spr land`. This will put your commit on top of the latest `main` and push it upstream.

In practice, you're likely to have more complex situations: multiple commits being reviewed, and possibly in-review commits that depend on others. You may need to make updates to any of these commits, or land them in any order.

spr can handle all of that, without requiring any particular way of organizing your local repo. See the guides in the "How To" section for instructions on using spr in those situations:

- [Simple PRs](./user/simple.md): no more than one review in flight on any branch.
- [Stacked PRs](./user/stack.md): multiple reviews in flight at once on your local `main`.

## Rationale

The reason to use spr is that it allows you to use whatever local branching scheme you want, instead of being forced to create a branch for every review. In particular, you can commit everything directly on your local `main`. This greatly simplifies rebasing: rather than rebasing every review branch individually, you can simply rebase your local `main` onto upstream `main`.

You can make branches locally if you want, and it's not uncommon for spr users to do so. You could even make a branch for every review if you don't want to use the stacked-PR workflow. It doesn't matter to spr.

One reasonable position is to make small changes directly on `main`, but make branches for larger, more complex changes. The branch keeps the work-in-progress isolated while you get it to a reviewable state, making lots of small commits that aren't individually reviewable. Once the branch as a whole is reviewable, you can squash it down to a single commit, which you can send out for review (either from the branch or cherry-picked onto `main`).

### Why Review Commits?

The principle behind spr is **one commit per logical change**. Each commit should be able to stand on its own: it should have a coherent thesis and be a complete change in and of itself. It should have a clear summary, description, and test plan. It should leave the codebase in a consistent state: building and passing tests, etc.

In addition, ideally, it shouldn't be possible to further split a commit into multiple commits that each stand on their own. If you _can_ split a commit that way, you should.

What follows from those principles is the idea that **commits, not branches, should be the unit of code review**. The above description of a commit also describes the ideal code review: a single, well-described change that leaves the codebase in a consistent state, and that cannot be subdivided further.

If the commit is the unit of code review, then, why should the code review tool require that you make branches? spr's answer is: it shouldn't.

Following the one-commit-per-change principle maintains the invariant that checking out any commit on `main` gives you a codebase that has been reviewed _in that state_, and that builds and passes tests, etc. This makes it easy to revert changes, and to bisect.

[^master]: Git's default branch name is `master`, but GitHub's is now `main`, so we'll use `main` throughout this documentation.
