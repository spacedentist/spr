# Create and Land a Simple PR

This section details the process of putting a single commit up for review, and landing it (pushing it upstream). It assumes you don't have multiple reviews in flight at the same time. That situation is covered in [another guide](./stack.md), but you should be familiar with this single-review workflow before reading that one.

1. Pull `main` from upstream, and check it out.

2. Make your change, and run `git commit`. See [this guide](./commit-message.md) for what to put in your commit message.

3. Run `spr diff`. This will create a PR for your HEAD commit.

4. Wait for reviewers to approve. If you need to make changes:

   1. Make whatever changes you need in your working copy.
   2. Amend them into your HEAD commit with `git commit --amend`.
   3. Run `spr diff`. If you changed the commit message in the previous step, you will need to add the flag `--update-message`; see [this guide](./commit-message.md) for more detail.

      This will update the PR with the new version of your HEAD commit. spr will prompt you for a short message that describes what you changed. You can also pass the update message on the command line using the `--message`/`-m` flag of `spr diff`.

5. Once your PR is approved, run `spr land` to push it upstream.

The above instructions have you committing directly to your local `main`. Doing so will keep things simpler when you have multiple reviews in flight. However, spr does not require that you commit directly to `main`. You can make branches if you prefer. `spr land` will always push your commit to upstream `main`, regardless of which local branch it was on. Note that `spr land` won't delete your feature branch.

## When you update

When you run `spr diff` to update an existing PR, your update will be added to the PR as a new commit, so that reviewers can see exactly what changed. The new commit's message will be what you entered in step 4.3 of the instructions above.

The individual commits that you see in the PR are solely for the benefit of reviewers; they will not be reflected in the commit history when the PR is landed. The commit that eventually lands on upstream `main` will always be a single commit, whose message is the title and description from the PR.

## Updating before landing

If you amend your local commit before landing, you must run `spr diff` to update the PR before landing, or else `spr land` will fail.

This is because `spr land` checks to make sure that the following two operations result in exactly the same tree:

- Merging the PR directly into upstream `main`.
- Cherry-picking your HEAD commit onto upstream `main`.

This check prevents `spr land` from either landing or silently dropping unreviewed changes.

## Conflicts on landing

`spr land` may fail with conflicts; for example, there may have been new changes pushed to upstream `main` since you last rebased, and those changes conflict with your PR. In this case:

1. Rebase your PR onto latest upstream `main`, resolving conflicts in the process.

2. Run `spr diff` to update the PR.

3. Run `spr land` again.

Note that even if your local commit (and your PR) is not based on the latest upstream `main`, landing will still succeed as long as there are no conflicts with the actual latest upstream `main`.
