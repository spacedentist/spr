# Stack Multiple PRs

The differences between spr's commit-based workflow and GitHub's default branch-based workflow are most apparent when you have multiple reviews in flight at the same time.

This guide assumes you're already familiar with the workflow for [simple, non-stacked PRs](./simple.md).

You'll use Git's [interactive rebase](https://git-scm.com/docs/git-rebase#_interactive_mode) quite often in managing stacked-PR situations. It's a very powerful tool for reordering and combining commits in a series.

This is the workflow for creating multiple PRs at the same time. This example only creates two, but the workflow works for arbitrarily deep stacks.

1. Make a change and commit it on `main`. We'll call this commit A.

2. Make another change and commit it on top of commit A. We'll call this commit B.

3. Run `spr diff --all`. This is equivalent to calling `spr diff` on each commit starting from `HEAD` and going to back to the first commit that is part of upstream `main`. Thus, it will create a PR for each of commits A and B.

4. Suppose you need to update commit A in response to review feedback. You would:

   1. Make the change and commit it on top of commit B, with a throwaway message.

   2. Run `git rebase --interactive`. This will bring up an editor that looks like this:

      ```
      pick 0a0a0a Commit A
      pick 1b1b1b Commit B
      pick 2c2c2c throwaway
      ```

      Modify it to look like this[^rebase-cmds]:

      ```
      pick 0a0a0a Commit A
      fixup 2c2c2c throwaway
      exec spr diff
      pick 1b1b1b Commit B
      ```

      This will (1) amend your latest commit into commit A, discarding the throwaway message and using commit A's message for the combined result; (2) run `spr diff` on the combined result; and (3) put commit B on top of the combined result.

5. You must land commit A before commit B. (See [the next section](#cherry-picking) for what to do if you want to be able to land B first.) To land commit A, you would:

   1. Run `git rebase --interactive`. The editor will start with this:

      ```
      pick 3a3a3a Commit A
      pick 4b4b4b Commit B
      ```

      Modify it to look like this:

      ```
      pick 3a3a3a Commit A
      exec spr land
      pick 4b4b4b Commit B
      ```

6. Now you're left with just commit B on top of upstream `main`, and you can use the non-stacked workflow to update and land it.

There are a few possible variations to note:

- Instead of a single run of `spr diff --all` at the beginning, you could run plain `spr diff` right after making each commit.

- Instead of step 4, you could use interactive rebase to swap the order of commits A and B (as long as B doesn't depend on A), and then simply use the non-stacked workflow to amend A and update the PR.

- In step 4.2, if you want to update the commit message of commit A, you could instead do the following interactive rebase:

  ```
  pick 0a0a0a Commit A
  squash 2c2c2c throwaway
  exec spr diff --update-message
  pick 1b1b1b Commit B
  ```

  The `squash` command will open an editor, where you can edit the message of the combined commit. The `--update-message` flag on the next line is important; see [this guide](./commit-message.md) for more detail.

## Cherry-picking

In the above example, you would not be able to land commit B before landing commit A, even if they were totally independent of each other.

First, some behind-the-scenes explanation. When you create the PR for commit B, `spr diff` will create a PR whose base branch is not `main`, but rather a synthetic branch that contains the difference between `main` and B's parent. This is so that the PR for B only shows the changes in B itself, rather than the entire difference between `main` and B.

When you run `spr land`, it checks that each of these two operations would produce _exactly the same tree_:

- Merging the PR directly into upstream `main`.
- Cherry-picking the local commit onto upstream `main`.

If those operations wouldn't result in the same tree, `spr land` fails. This is to prevent you from landing a commit whose contents aren't the same as what reviewers have seen.

In the above example, then, the PR for commit B has a synthetic base branch that contains the changes in commit A. Thus, if you tried to land B before A, `spr land`'s "merge PR vs. cherry-pick" check would fail.

If you want to be able to land commit B before A, do this:

1. Make commit A on top of `main` as before, and run `spr diff`.

2. Make commit B on top of A as before, and run `spr diff --cherry-pick`. The flag causes `spr diff` to create the PR as if B were cherry-picked onto upstream `main`, rather than creating the synthetic base branch. (This step will fail if B does not cherry-pick cleanly onto upstream `main`, which would imply that A and B are not truly independent.)

3. Once B is ready to land, you can do one of two things:

   - Run `spr land --cherry-pick`. (By default, `spr land` refuses to land a commit whose parent is not on upstream `main`; the flag makes it skip that check.)

   - Do an interactive rebase that puts B directly on top of upstream `main`, then runs `spr land`, then puts A on top of B.

## Rebasing the whole stack

One of the major advantages of committing everything to local `main` is that rebasing your work onto new upstream `main` commits is much simpler than if you had a branch for every in-flight review. The difference is especially pronounced if some of your reviews depend on others, which would entail dependent feature branches in a branch-based workflow.

Rebasing all your in-flight reviews and updating their PRs is as simple as:

1. Run `git pull --rebase` on `main`, resolving conflicts along the way as needed.

2. Run `spr diff --all`.

[^rebase-cmds]: You can shorten `exec` to `x`, `fixup` to `f`, and `squash` to `s`; they are spelled out here for clarity.
