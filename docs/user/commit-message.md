# Format and Update Commit Messages

You should format your commit messages like this:

```
One-line title

Then a description, which may be multiple lines long.
This describes the change you are making with this commit.

Test Plan: how to test the change in this commit.

The test plan can also be several lines long.

Reviewers: github-username-a, github-username-b
```

The first line will be the title of the PR created by `spr diff`, and the rest of the lines except for the `Reviewers` line will be the PR description (i.e. the content of the first comment). The GitHub users named on the `Reviewers` line will be added to the PR as reviewers.

The `Test Plan` section is required to be present; `spr diff` will fail with an error if it isn't.

## Updating the commit message

When you create a PR with `spr diff`, **the PR becomes the source of truth** for the title and description. When you land a commit with `spr land`, its commit message will be amended to match the PR's title and description, regardless of what is in your local repo.

If you want to update the title or description, there are two ways to do so:

- Modify the PR through GitHub's UI.

- Amend the commit message locally, then run `spr diff --update-message`. _Note that this does not update reviewers_; that must be done in the GitHub UI. If you amend the commit message but don't include the `--update-message` flag, you'll get an error.

If you want to go the other way --- that is, make your local commit message match the PR's title and description --- you can run `spr amend`.

## Further information

### Fields added by spr

At various stages of a commit's lifecycle, `spr` will add lines to the commit message:

- After first creating a PR, `spr diff` will amend the commit message to include a line like this at the end:

  ```
  Pull Request: https://github.com/example/project/pull/123
  ```

  The presence or absence of this line is how `spr diff` knows whether a commit already has a PR created for it, and thus whether it should create a new PR or update an existing one.

- `spr land` will amend the commit message to exactly match the title/description of the PR (just as `spr amend` does), as well as adding a line like this:
  ```
  Reviewed By: github-username-a
  ```
  This line names the GitHub users who approved the PR.

### Example commit message lifecycle

This is what a commit message should look like when you first commit it, before running `spr` at all:

```
Add feature

This is a really cool feature! It's going to be great.

Test Plan:
- Run tests
- Use the feature

Reviewers: user-a, coworker-b
```

After running `spr diff` to create a PR, the local commit message will be amended to include a link to the PR:

```
Add feature

This is a really cool feature! It's going to be great.

Test Plan:
- Run tests
- Use the feature

Reviewers: user-a, coworker-b

Pull Request: https://github.com/example/my-thing/pull/123
```

In this state, running `spr diff` again will update PR 123.

Running `spr land` will amend the commit message to have the exact title/description of PR 123, add the list of users who approved the PR, then land the commit. In this case, suppose only `coworker-b` approved:

```
Add feature

This is a really cool feature! It's going to be great.

Test Plan:
- Run tests
- Use the feature

Reviewers: user-a, coworker-b

Reviewed By: coworker-b

Pull Request: https://github.com/example/my-thing/pull/123
```

### Reformatting the commit message

spr is fairly permissive in parsing your commit message: it is case-insensitive, and it mostly ignores whitespace. You can run `spr format` to rewrite your HEAD commit's message to be in a canonical format.

This command does not touch GitHub; it doesn't matter whether the commit has a PR created for it or not.

Note that `spr land` will write the message of the commit it lands in the canonical format; you don't need to do so yourself before landing.
