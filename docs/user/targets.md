# Target Another Branch

PRs get merged into the default target, the branch configured as `spr.target` (e.g. `github:example/project:main`). To create PRs for another branch, e.g. a fix for a release branch, give it with `--target`:

```shell
git switch -c fix-for-release origin/release/1.2
# ... make your change, and commit
spr --target release/1.2 diff
```

A branch name alone means that branch in the default target's repository. The full notation also works, for a branch in another repository on github.com: `--target github:example/other-project:main`.

## The `Spr-Target` trailer

`spr diff` records the target in the commit message:

```
Spr-Target: github:example/project:release/1.2
```

So later commands know it without `--target`: `spr diff` to update the PR, `spr land` to land it into `release/1.2`, and so on. Each command works for the target of the HEAD commit: the one in its `Spr-Target` trailer, or the default target if it has none.

You can also write the trailer yourself, e.g. with `git commit --amend`, in either notation. Like the `Spr-Id` trailer, it's only in your local commit: not in the PR's description, and not in the commit that lands.

## Stacks

All commits of a stack have the same target: commits on top of the HEAD commit's target branch, up to the HEAD commit, form the stack.

- spr stops (in `spr diff`, `spr land`, `spr amend`, …) if a commit in the stack has a `Spr-Target` trailer for another target, or a PR for another target. (Commits with neither take the target of the stack.)
- `spr diff --all` (and `--refs`) stops if any of the commits it works on has another target. To give them all the trailer, run `spr --target BRANCH diff --all`.
- `--target` must not contradict the HEAD commit's `Spr-Target` trailer.
- spr doesn't change the target of an existing PR: if the trailer doesn't match the PR's base branch, `spr diff` stops. Change the base on GitHub, or remove the `Pull-request` trailer to create a new PR.

## Cherry-picking

With `spr diff --cherry-pick`, each commit goes to its own target, so one run can create PRs for different targets:

```shell
spr diff --cherry-pick --refs HEAD~2..HEAD
```

Each commit is cherry-picked onto the commit of its target branch that the local branch is based on (the merge base). `--cherry-pick` can't be combined with `--all`: use `--refs`. `spr land --cherry-pick` lands the HEAD commit into its target, whatever the targets of the commits below it.
