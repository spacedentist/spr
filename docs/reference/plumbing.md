# Plumbing Commands

Like Git, spr has two kinds of commands. The **porcelain** commands (`spr diff`, `spr land`, …) do everything for you: they look at your local commits, talk to GitHub, push branches and update pull requests. The **plumbing** commands expose the building blocks the porcelain uses, for your own scripts, for example to use spr's way of constructing pull requests with another forge.

> **Experimental:** the plumbing commands are new, and their interface may still change.

## Principles

- Plumbing commands only work on Git objects. They don't move any refs (branches, `HEAD`), don't push or fetch, and don't talk to GitHub. Fetch before, and push afterwards.
- They don't need spr to be configured: they work in any Git repository, without `spr init`.
- They know nothing about pull request numbers or trailers in commit messages beyond passing the `Pull-request` trailer through in `spr plumbing stack`. Rewrite commit messages however you like, e.g. with `git interpret-trailers`.
- Commits and trees can be given in any form Git understands: commit hashes, branch names, `HEAD~2`, and for trees also `<commit>:` (the tree of a commit).
- Output is plain text by default (object IDs, one per line), or JSON with `--json`.

### Exit status

| status | kind               | meaning                                                                    |
| ------ | ------------------ | -------------------------------------------------------------------------- |
| 0      |                    | success                                                                    |
| 1      | `error`            | any other error                                                            |
| 2      | (command-specific) | `base-outdated`, `merge-commit` or `mismatch`, see the commands below      |
| 3      | `message-required` | `commit-pr` would create a head commit, but no message was given with `-m` |
| 4      | `conflict`         | a change can't be applied without conflicts                                |

Errors are printed to stderr. With `--json`, an error object is printed to stdout as well, so scripts parsing stdout always get JSON:

```json
{ "error": { "kind": "base-outdated", "message": "…" } }
```

## `spr plumbing commit-pr`

Creates the commits that make a pull request reflect a local commit, and prints the new head and base commit of the pull request (each is the input commit if it didn't need changing).

```
spr plumbing commit-pr --base <commit> --target <commit> [--head <commit>]
                       (--local <commit> | --tree <tree> --base-tree <tree>)
                       [--cherry-pick] [--fixed-base]
                       [-m <message>] [--base-message <message>]
                       [--author-from <commit>] [--plan] [--json]
```

- `--head`: the current head of the pull request branch. Leave it out for a new pull request: the head then starts off at `--base`.
- `--base`: the commit the pull request is currently based on.
- `--target`: the commit on the target branch (e.g. `main`) the change is based on. The base must contain it.
- `--local <commit>`: the local commit. A shorthand for `--tree <commit>: --base-tree <commit>~:`: the head of the pull request should have the tree of the local commit, and its base the tree of the local commit's parent.
- `--cherry-pick`: apply the change (from the base tree to the tree) onto `--target` instead. The base tree becomes the target's tree. Fails with `conflict` if the change doesn't apply cleanly.
- `--fixed-base`: don't add commits to the base. If the base doesn't have the base tree, or doesn't contain the target, fail with `base-outdated`.

What happens:

- If the base doesn't have the base tree or doesn't contain the target (and `--fixed-base` isn't given), a new base commit is created on top of the base, with the base tree. If the target isn't contained in the base yet, it's merged in as a second parent.
- If the head doesn't contain the (new) base, or doesn't have the tree, a new head commit is created on top of the head, with the tree, merging in the base if necessary.

`-m` gives the message of a new head commit, which is required if one is created. The new commits credit the author of `--local` (or `--author-from`), or the current user.

`--plan` doesn't create anything, but prints the commits that would be created, with their parents (`new-base` stands for the new base commit):

```
new-base <parent> [<parent>]
new-head <parent> [<parent>]
```

With `--json`, the output is `{"head": …, "base": …, "head_created": …, "base_created": …}`, or with `--plan`, `{"new_base_parents": …, "new_head_parents": …, "merges": …, "tree": …, "base_tree": …}`.

This is how `spr diff` uses it in its [stacking modes](../user/stacking-modes.md):

| situation                                         | `--base`                                      | `--fixed-base` |
| ------------------------------------------------- | --------------------------------------------- | -------------- |
| commit directly on the target, or `--cherry-pick` | the target commit                             | yes            |
| synthetic base branch (`base-branches`)           | merge base of the PR head and the base branch | no             |
| chained PRs (`chain`, `github-stack`)             | the head of the parent commit's PR            | yes            |

## `spr plumbing stack`

Lists the commits between the target and a commit (default: `HEAD`), from bottom to top: each commit, its parent, and its `Pull-request` trailer (or `-`).

```
spr plumbing stack --target <commit> [<commit>] [--json]
```

With `--json`, it prints an array of objects with the fields `commit`, `parent`, `title`, `pull_request` and `trailers`. Fails with `merge-commit` if there are merge commits in the range, which spr can't handle.

## `spr plumbing land-check`

Checks that merging a pull request into the target gives the same tree as applying the local commits to it, and prints that tree. That's the check `spr land` does before landing, to make sure it lands exactly the changes of the local commits, which reviewers have seen.

```
spr plumbing land-check --target <commit> --pr-head <commit> --local <commit>
                        [--since <commit>] [--json]
```

The changes of `<since>..<local>` are applied (default: `<local>`'s parent, i.e. just `<local>`). Fails with `mismatch` if the trees differ, or with `conflict` if either can't be done without conflicts.

## Example

Updating the pull requests of a stack on another forge could look like this:

```sh
git fetch origin
target=$(git rev-parse origin/main)

spr plumbing stack --target "$target" | while read commit parent pr; do
    # Look up the head and base of the pull request `$pr` on your forge
    # ($pr_head, $pr_base, $branch), then:
    { read head; read base; } < <(spr plumbing commit-pr --target "$target" \
        --head "$pr_head" --base "$pr_base" --local "$commit" -m "Update")
    git push origin "$head:refs/heads/$branch"
done
```
