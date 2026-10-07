# Set up spr

In the repo you want to use spr in, run `spr init`; this will ask you several questions.

You'll need to authorise spr with your GitHub account. `spr init` will guide you through the process (see [Authentication](#authentication) below).

The rest of the settings that `spr init` asks for have sensible defaults, so almost all users can simply accept the defaults. `spr init` suggests the GitHub repository that your `origin` remote points to; if your repository has no such remote, enter the repository as `owner/repo`.

`spr init` doesn't ask how pull requests get merged or stacked. The defaults are spr's original workflow: squash-merging, and synthetic base branches for stacked pull requests. If your team merges pull requests with merge commits, or you want to use GitHub's stacked pull requests, see [Choose a Merge Method and Stacking Mode](./stacking-modes.md).

See the [Configuration](../reference/configuration.md) reference page for full details about the available settings.

After initial setup, you can update your settings in several ways:

- Simply rerun `spr init`. The defaults it suggests will be your existing settings, so you can easily change only what you need to.

- Use `git config spr.<key> <value>`, e.g. `git config spr.mergeMethod merge` ([docs here](https://git-scm.com/docs/git-config)).

- Edit the `[spr]` section of `.git/config` directly.

## Authentication

spr needs a GitHub token to talk to GitHub on your behalf. It's stored in the `spr.githubAuthToken` [setting](../reference/configuration.md).

By default, `spr init` gets one by logging you in with GitHub: it shows a link and a code to enter there. (It also tries to open the link in your web browser, but you can open it anywhere, e.g. when you run `spr init` on a server via SSH.) This authorises spr's OAuth app with these scopes:

- `repo`: pushing pull request branches, and creating, updating and merging pull requests. GitHub has no narrower scope for this, which is why the authorisation page lists access to much more (e.g. wikis) — spr doesn't use that.
- `read:org`: looking up teams named as reviewers (`Reviewers: #team`).
- `workflow`: pushing pull request branches that change GitHub Actions workflows (`.github/workflows/`). GitHub refuses those pushes without it.

Older development versions of spr also asked for the `user` scope ("Update all user data"), which spr doesn't need. To get rid of it, revoke spr under [Authorized OAuth Apps](https://github.com/settings/applications) in your GitHub settings, and run `spr init` again.

### Using your own token

If a token is configured already, `spr init` keeps using it, as long as it works and has the scopes above. So you can also set a token yourself before running `spr init`, e.g. if your organisation [restricts access by OAuth apps](https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions):

- The token of the GitHub CLI has the right scopes:

  ```shell
  git config spr.githubAuthToken "$(gh auth token)"
  ```

- A classic [personal access token](https://github.com/settings/tokens) with the scopes `repo`, `read:org` and `workflow`.

- A [fine-grained personal access token](https://github.com/settings/personal-access-tokens), which lets you limit access to particular repositories and permissions. spr needs these repository permissions: _Contents_, _Pull requests_ and _Workflows_ (read and write); and for team reviewers, the organisation permission _Members_ (read). GitHub doesn't tell spr the permissions of such a token, so `spr init` can't check them.
