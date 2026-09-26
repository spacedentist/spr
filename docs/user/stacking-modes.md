# Choose a Merge Method and Stacking Mode

spr has two settings that shape how your pull requests look on GitHub and how they end up on `main`: the **merge method** (`spr.mergeMethod`) and the **stacking mode** (`spr.stackingMode`). This page explains the options, why they exist, and what their trade-offs are. For the settings themselves, see [Configuration](../reference/configuration.md).

If you're unsure: the defaults (`squash` and `base-branches`) are spr's original workflow. If your team merges pull requests with merge commits, use `merge` (which implies `chain`).

## One commit per unit of work

spr's original workflow, modelled after Phabricator, revolves around one idea: **a unit of work is one commit**.

- While you develop a change, it is a single commit on your local branch. You amend it and rebase it as needed.
- `spr diff` turns that commit into a pull request. Every update you make becomes an additional commit on the pull request branch, so reviewers can follow what changed.
- `spr land` squash-merges the pull request, so the change becomes a single commit on `main` again, with the title and description of the pull request.

The commits on the pull request branch are scaffolding for the review. What remains in the history of `main` is exactly one commit per reviewed change. That's why squash-merging is spr's default merge method.

## Merge methods

### `squash` (default)

Every pull request becomes a single commit on `main`. This completes the one-commit-per-change cycle described above, and keeps the history of `main` linear and easy to bisect and revert.

The catch comes with stacked pull requests: the squash commit on `main` is a new commit, unrelated to the commits of the pull request branch it was made from. So for a pull request stacked on top, GitHub still sees all the commits of the landed pull request as part of its own history. How spr deals with that is what the stacking modes are about.

### `merge`

Pull requests are merged with a merge commit, so all the commits of the pull request branch become part of the history of `main`.

Some teams have decided to merge pull requests this way. One of spr's selling points is that you don't need your whole team to adopt it: to your colleagues, your pull requests are ordinary pull requests. With `mergeMethod = merge`, you can use spr's workflow and still blend in with the conventions of your repository.

The merge method also records the convention of your repository for spr: pull requests may be merged in the GitHub UI, too, not only with `spr land`.

## Stacking modes

A commit that is directly based on `main` gets a pull request that targets `main`. The stacking mode determines what happens for a commit that is stacked on other commits that haven't landed yet.

### Synthetic base branches (`base-branches`)

The pull request targets a synthetic base branch that spr creates for it. That branch contains the changes of all commits below, so the pull request only shows the changes of its own commit.

The purpose of synthetic base branches is to **engineer a readable pull request timeline** when pull requests are squash-merged. Consider a stack of commits A, B and C, each with a pull request. Without synthetic base branches, once A has been squash-merged, the pull requests of B and C show all the commits of A's pull request in their timelines: "initial version", every update in response to review, every rebase. Once B is squash-merged too, C's timeline shows B's commits as well. In a big stack, that quickly becomes confusing.

With synthetic base branches, changes coming from the commits below show up on the base branch as commits labelled "[𝘀𝗽𝗿] changes to main this commit is based on" and "[𝘀𝗽𝗿] changes introduced through rebase", which are easy to put into context. And there are fewer of them: if you make changes all over a stack and then update the whole stack with `spr diff --all`, each pull request gets a single synthetic commit for the whole update, even the one at the top.

Synthetic base branches have one more advantage: since spr constructs the base branch anyway, a pull request can be based on anything. You can submit a commit from the middle of a local branch for review, without submitting the commits below it. Before such a pull request can land, its base and `main` have to agree: either you rebase the commit locally onto `main`, or the commits it depends on land first. Once the commit is directly based on `main`, `spr diff` changes the pull request to target `main` and deletes the synthetic base branch.

The downsides:

- A pull request with a synthetic base branch must not be merged in the GitHub UI: that would merge it into its base branch, not into `main`. Use `spr land`.
- With merge commits, the synthetic commits would become part of the history of `main`. That's why spr doesn't allow `base-branches` together with `mergeMethod = merge`.

### Chained pull requests (`chain`)

The pull request targets the pull request branch of its parent commit. The pull requests of a stack form a chain, with the bottom one targeting `main`. This is the classic way of stacking pull requests on GitHub, and it's what the GitHub UI and other tools expect.

With merge commits, chaining is the natural choice: once the bottom pull request is merged, its commits are part of the history of `main`, so the next pull request's timeline only contains its own commits. The need for synthetic base branches goes away. (Their other advantage, submitting any commit on its own, remains, but most local branches are based on `main` and get submitted in full anyway.)

With squash-merging, chaining gives the correct result on `main`, but the timelines of the pull requests further up a stack show the commits of the pull requests below them once those have been squash-merged, as described above.

### GitHub stacks (`github-stack`)

This is chain mode, and on top of that, spr links the pull requests of a chain as a stack using GitHub's [stacked pull requests](https://docs.github.com/en/pull-requests/get-started/about-stacked-prs) feature, which is in public preview. You have to opt in to this mode.

The advantages:

- GitHub shows the stack on each pull request page.
- Branch protection and CI are evaluated for every pull request in the stack against the stack's base branch.
- `spr land` can land several pull requests at once: it lands the pull request of the current commit together with all pull requests below it.
- With squash-merging, the pull request timelines stay readable, because GitHub rebases the remaining pull requests after each merge.

The downside is how GitHub achieves that last point: after merging a pull request of a stack, GitHub rebases the branches of the remaining pull requests and force-pushes them, with both merge methods. spr itself never force-pushes pull request branches, to keep their history intact. GitHub's force-pushes don't cause problems for spr, though, and in our tests review comments survived them. See [How it works - Stacked PRs](../reference/how-it-works-stacks.md) for details.

## Choosing

|                         | `base-branches`                                                          | `chain`                                                   | `github-stack`                                                           |
| ----------------------- | ------------------------------------------------------------------------ | --------------------------------------------------------- | ------------------------------------------------------------------------ |
| **`squash`**            | Default. Readable timelines, one commit per change on `main`. Land with `spr land` only. | Correct result, but cluttered timelines after landing.     | Readable timelines, stack shown on GitHub; GitHub force-pushes branches. |
| **`merge`**             | Not allowed.                                                             | Default for `merge`. Clean timelines and history.          | Stack shown on GitHub; GitHub force-pushes branches.                     |

- You want spr's original workflow: `squash` with `base-branches` (the defaults).
- Your team uses merge commits: `merge`, which defaults to `chain`.
- You want GitHub's stack UI and landing whole stacks at once, and don't mind GitHub rewriting pull request branches: `github-stack`, with either merge method.

You can change the stacking mode at any time. `spr diff` switches existing pull requests over to the configured mode as it updates them.
