# Identify Commits with Spr-Ids

spr can give a local commit a stable ID, a line like this at the end of its commit message:

```
Spr-Id: 7f3a91c0d2e84b5f9a6c1e0b3d7f2a48
```

This page explains what the ID is, what spr uses it for, and when you might want it.

## A stable identity for a unit of work

In spr's workflow, a change is one commit on your local branch, which you amend and rebase as often as needed (see [One commit per unit of work](stacking-modes.md#one-commit-per-unit-of-work)). Each time, Git creates a new commit, with a new hash. So what makes it "the same" change?

Once the change has a pull request, the `Pull-request` trailer links the local commit to it. But that's the identity of the pull request on GitHub, not of your local commit: it only exists once the pull request does, and pull request numbers are only unique within one repository.

A `Spr-Id` is the local commit's own identity:

- **Stable.** It's part of the commit message, so it survives amending, rebasing and cherry-picking, and stays the same for the whole life of the change, until it lands.
- **Random.** It isn't derived from the commit's content (which changes), but generated when the ID is added: 128 random bits, as 32 hex digits.
- **Local.** It only appears in your local commit message. It isn't part of the pull request's description, nor of the commits spr pushes to the pull request branch, nor of the commit that lands on `main`.

If you know Gerrit, this is its `Change-Id`; Jujutsu has change IDs for the same reason.

## Local state for a pull request in flight

With a stable ID, spr can address an in-flight pull request locally, and keep information about it in your repository. It stores it in Git refs under `refs/spr/<id>/`.

Currently, that's one ref: **`refs/spr/<id>/head`, the head of the pull request branch that your local commit is known to match**: the head spr last pushed when updating the pull request, or the head of the pull request when `spr patch` checked it out.

This is different from the remote-tracking branch (`refs/remotes/origin/…`): that one moves whenever you fetch, whether or not your local commit has taken in what changed. The ref under `refs/spr/` only moves when spr has acted on the pull request.

Refs are a natural place for this: they're per repository (shared by all its worktrees), they keep the commit they point to from being garbage-collected, and you can look at them with ordinary Git commands:

```shell
git for-each-ref refs/spr
```

`spr land` and `spr close` delete the refs of the pull requests they land or close. Otherwise, spr leaves them alone: a commit with the ID might still exist on another branch, in a stash or in another worktree, and spr can't know when it's gone for good. Refs are tiny, so leftovers do no harm.

## What the record is for: changes others push to your pull request

spr never force-pushes: each update of a pull request is a new commit on top of its current head. So if someone else pushes to your pull request — a colleague fixing a typo, a CI bot fixing the formatting, GitHub's "Update branch" button — their commits stay in the pull request's history.

But spr's next commit gets the tree of your local commit, which doesn't contain their changes. So your next `spr diff` would quietly revert them.

To notice that, spr needs to know which state of the pull request your local commit corresponds to — and that's what the record is. If the pull request's head is still the one recorded, nothing happened that your local commit doesn't know about. If it moved, spr works out whether the changes are already contained locally: e.g. GitHub only rebased the branch, or you already applied the same changes to your local commit.

If they aren't, `spr diff` stops, and leaves the pull request as it is:

```
Pull Request #123 has changes that aren't in your local commit (e.g. somebody else pushed to it). Updating it would revert them.
```

Then you can:

- **Pull their changes.** `spr pull` applies them to your local commit (see below).
- **Overwrite them.** `spr diff --force` updates the pull request with your local commit anyway, as if you force-pushed your local commit to the pull request. (spr still doesn't force-push: their commits stay in the pull request's history, and the new commit reverts their changes.)

With `spr diff --all`, it stops at the first commit whose pull request has changes, and `spr diff --dry-run` tells you which ones would stop.

A special case is a pull request that is based on a newer `main` than your local commit, e.g. after somebody used GitHub's "Update branch" button. `spr diff` then asks you to rebase your local commit onto the current `main` first. `--force` doesn't help here: the pull request would then show your change _and_ the reversal of everything that's new on `main`. After rebasing, `spr diff` checks again, and only stops if there are other changes, too.

## Pulling changes others pushed: `spr pull`

`spr pull` takes what changed on the pull request since spr last pushed it, and applies it to your local commit, with a three-way merge. Like with `git pull`, you end up with their changes in your local branch — but no remote commits end up on it: your local commit is amended instead, as if you had made their changes yourself. Changes in your local commit that you haven't pushed yet are kept.

```
$ spr pull
1a2b3c4 Add feature
  ⬇️  Pulling changes from Pull Request #123:
      src/feature.rs | 2 +-
      1 file changed, 1 insertion(+), 1 deletion(-)
  ✅  Pulled the changes into your local branch
  ↩️  To undo:
      git reset --keep 1a2b3c4…
      git update-ref refs/spr/7f3a…/head 9f8e7d6…
```

It records the pull request's head, so the next `spr diff` goes ahead and updates the pull request with your local commit, which now contains their changes.

Like `spr diff`, it works on the HEAD commit, or with `--all` on all commits of your branch, from the bottom up (rebasing the commits above an amended one). `--dry-run` shows what it would pull. It needs commits with an ID (and a record), and a clean working tree.

`spr pull` is meant for the common case: somebody made a small fix to your pull request. It stops at the first sign of trouble, without changing anything:

- **Conflicts.** If their changes conflict with your local commit, `spr pull` creates a commit with just their changes, for you to cherry-pick, and tells you how: `git cherry-pick --no-commit <commit>`, resolve the conflicts with your usual Git tools, `git commit --amend --no-edit`, and update the pull request with `spr diff --force`. (With `--all`, if the conflict is in a commit below the HEAD commit, it tells you how to apply them there with `git rebase -i`.)
- **Merges or rewritten history.** spr only pulls on its own if the pull request's history since spr last pushed it is a chain of plain commits — then it knows exactly what changed. If it has merge commits (e.g. from GitHub's "Update branch") or was rewritten (force-pushed, e.g. by GitHub after landing a stacked pull request below it), `spr pull` stops. If the pull request is based on a newer `main` than your local commit, rebase your local branch onto that `main` commit first. Then `spr pull --force` pulls anyway; check the result.

If something unexpected came in, the diffstat shows it, and the commands under "To undo" restore your local commit and spr's record.

And if a pull request is in a state where you'd rather sort things out by hand, `spr patch <number>` checks out its current state as a new local branch.

## When to use Spr-Ids

Without a `Spr-Id`, spr works as it always has: nothing is recorded, nothing is checked, and your next `spr diff` overwrites changes that others pushed to your pull request.

IDs are opt-in, because the trailer is clutter in your commit messages if you work on your pull requests alone. You can add one to just the pull requests that others work on, too:

- `spr diff --spr-id` adds an ID to the commit, if it doesn't have one yet, when creating or updating its pull request. With `--all`, it adds one to every commit it works on.
- `spr patch --spr-id` gives the commit it creates an ID. That's useful when you check out a colleague's pull request to work on it, or your own pull request on another machine.
- With `git config spr.sprIds true`, `spr diff` and `spr patch` always add an ID.

Once a commit has an ID, spr uses it, whatever the setting. It's kept by `spr amend` and `spr format`, and when you amend or rebase the commit with Git. To stop using it, delete the line from the commit message.

## Lifecycle

| command                   | `Spr-Id` trailer                                                       | `refs/spr/<id>/head`                                                                |
| ------------------------- | ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `spr diff`                | added with `--spr-id` or if `spr.sprIds` is `true`, unless present     | set to the head it pushed, or the current head if the pull request needed no update |
| `spr diff --dry-run`      | unchanged                                                              | unchanged                                                                           |
| `spr patch`               | the new commit gets an ID with `--spr-id` or if `spr.sprIds` is `true` | set to the head of the pull request                                                 |
| `spr pull`                | kept                                                                   | set to the head of the pull request, once its changes are in the local commit       |
| `spr amend`, `spr format` | kept                                                                   | unchanged                                                                           |
| `spr close`               | removed                                                                | deleted                                                                             |
| `spr land`                | gone with the local commit                                             | deleted                                                                             |

The ref only exists for commits that have an ID.

A `Spr-Id` that isn't 32 lowercase hex digits (e.g. after editing it by hand) is ignored, as if there was none. When spr adds an ID, it replaces it.
