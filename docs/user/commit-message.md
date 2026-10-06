# Format and Update Commit Messages

You should format your commit messages like this:

```
One-line title

Then a description, which may be multiple lines long.
This describes the change you are making with this commit.

Reviewers: github-username-a, github-username-b
```

The first line will be the title of the PR created by `spr diff`, and the rest of the message except for the `Reviewers` line will be the PR description (i.e. the content of the first comment). The GitHub users named on the `Reviewers` line will be added to the PR as reviewers. That line is a [git trailer](https://git-scm.com/docs/git-interpret-trailers): a key-value line in the last paragraph of the commit message.

## Updating the commit message

When you create a PR with `spr diff`, **the PR becomes the source of truth** for the title and description. When you land a commit with `spr land`, its commit message will be amended to match the PR's title and description, regardless of what is in your local repo.

If you want to update the title or description, there are two ways to do so:

- Modify the PR through GitHub's UI.

- Amend the commit message locally, then run `spr diff --update-message`. _Note that this does not update reviewers_; that must be done in the GitHub UI. If you amend the commit message but don't include the `--update-message` flag, `spr diff` leaves the title and description on GitHub as they are, and warns you that they differ from your local commit message.

If you want to go the other way --- that is, make your local commit message match the PR's title and description --- you can run `spr amend`.

## Further information

### Trailers added by spr

At various stages of a commit's lifecycle, `spr` will add trailers to the commit message:

- After first creating a PR, `spr diff` will amend the commit message to include a trailer like this:

  ```
  Pull-request: https://github.com/example/project/pull/123
  ```

  The presence or absence of this trailer is how `spr diff` knows whether a commit already has a PR created for it, and thus whether it should create a new PR or update an existing one.

- `spr land` will amend the commit message to exactly match the title/description of the PR (just as `spr amend` does), as well as adding a trailer like this:

  ```
  Reviewed-by: github-username-a
  ```

  This trailer names the GitHub users who approved the PR.

- `spr diff --spr-id` (or any `spr diff`, if the `spr.sprIds` [config option](../reference/configuration.md) is set) adds a trailer like this, giving the local commit an ID:

  ```
  Spr-Id: 7f3a91c0d2e84b5f9a6c1e0b3d7f2a48
  ```

  It gives the commit a stable identity across amends and rebases (like Gerrit's `Change-Id`), which spr uses to keep track of the PR's state locally. It only appears in your local commit message: not in the PR, and not in the commit that lands. See [Identify Commits with Spr-Ids](spr-ids.md).

All of these (`Pull-request`, `Reviewers`, `Reviewed-by`, `Spr-Id`) follow git's trailer conventions, so you can also read and edit them with tools like [`git interpret-trailers`](https://git-scm.com/docs/git-interpret-trailers).

### Backwards compatibility

Older versions of spr didn't use trailers, and wrote these lines with spaces instead of hyphens (`Pull Request:` instead of `Pull-request:`, and `Reviewed By:` instead of `Reviewed-by:`). Current versions of spr can read commit messages written by older versions and will automatically recognize these old-style lines. When spr updates a commit message (e.g., when running `spr diff` or `spr land`), it will rewrite them as trailers.

If you have old commits in the previous format, they will continue to work seamlessly — spr will read them correctly and update them to the new format when it next modifies the commit message.

### Example commit message lifecycle

This is what a commit message should look like when you first commit it, before running `spr` at all:

```
Add feature

This is a really cool feature! It's going to be great.

Reviewers: user-a, coworker-b
```

After running `spr diff` to create a PR, the local commit message will be amended to include a link to the PR:

```
Add feature

This is a really cool feature! It's going to be great.

Reviewers: user-a, coworker-b
Pull-request: https://github.com/example/my-thing/pull/123
```

In this state, running `spr diff` again will update PR 123.

Running `spr land` will amend the commit message to have the exact title/description of PR 123, add the list of users who approved the PR, then land the commit. In this case, suppose only `coworker-b` approved:

```
Add feature

This is a really cool feature! It's going to be great.

Pull-request: https://github.com/example/my-thing/pull/123
Reviewers: user-a, coworker-b
Reviewed-by: coworker-b
```

### Reformatting the commit message

spr is fairly permissive in parsing your commit message: it is case-insensitive, and it mostly ignores whitespace. You can run `spr format` to rewrite your HEAD commit's message to be in a canonical format.

This command does not touch GitHub; it doesn't matter whether the commit has a PR created for it or not.

Note that `spr land` will write the message of the commit it lands in the canonical format; you don't need to do so yourself before landing.
