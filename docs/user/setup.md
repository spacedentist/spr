# Set up spr

In the repo you want to use spr in, run `spr init`; this will ask you several questions.

First, `spr init` asks for the GitHub repository. It suggests the one that your `origin` remote points to; if your repository has no such remote, enter the repository as `owner/repo` (for one on github.com), or as `github://HOST/owner/repo` for one on [GitHub Enterprise](#github-enterprise).

You'll need to authorise spr with your GitHub account. `spr init` will guide you through the process (see [Authentication](#authentication) below).

The rest of the settings that `spr init` asks for have sensible defaults, so almost all users can simply accept the defaults.

`spr init` doesn't ask how pull requests get merged or stacked. The defaults are spr's original workflow: squash-merging, and synthetic base branches for stacked pull requests. If your team merges pull requests with merge commits, or you want to use GitHub's stacked pull requests, see [Choose a Merge Method and Stacking Mode](./stacking-modes.md).

See the [Configuration](../reference/configuration.md) reference page for full details about the available settings.

After initial setup, you can update your settings in several ways:

- Simply rerun `spr init`. The defaults it suggests will be your existing settings, so you can easily change only what you need to.

- Use `git config spr.<key> <value>`, e.g. `git config spr.mergeMethod merge` ([docs here](https://git-scm.com/docs/git-config)).

- Edit the `[spr]` section of `.git/config` directly.

## Authentication

spr needs a GitHub token to talk to GitHub on your behalf. It's stored in the `spr.auth.github.token` [setting](../reference/configuration.md) (for GitHub Enterprise, `spr.auth.github.HOST.token`).

On github.com, `spr init` asks how to get one: by logging you in with GitHub (the default), or by pasting a token (see [Using your own token](#using-your-own-token)). Logging in with GitHub works like this: it shows a link and a code to enter there. (It also tries to open the link in your web browser, but you can open it anywhere, e.g. when you run `spr init` on a server via SSH.) This authorises spr's OAuth app with these scopes:

- `repo`: pushing pull request branches, and creating, updating and merging pull requests. GitHub has no narrower scope for this, which is why the authorisation page lists access to much more (e.g. wikis) — spr doesn't use that.
- `read:org`: looking up teams named as reviewers (`Reviewers: #team`).
- `workflow`: pushing pull request branches that change GitHub Actions workflows (`.github/workflows/`). GitHub refuses those pushes without it.

Older development versions of spr also asked for the `user` scope ("Update all user data"), which spr doesn't need. To get rid of it, revoke spr under [Authorized OAuth Apps](https://github.com/settings/applications) in your GitHub settings, and run `spr init` again.

### Using your own token

You can also give spr a token of your own, e.g. if your organisation [restricts access by OAuth apps](https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions): paste it when `spr init` asks for it, or configure it before running `spr init` — if a token is configured already, `spr init` keeps using it, as long as it works and has the scopes above. To use the same token in all your repositories, configure it globally:

- The token of the GitHub CLI has the right scopes:

  ```shell
  git config --global spr.auth.github.token "$(gh auth token)"
  ```

- A classic [personal access token](https://github.com/settings/tokens) with the scopes `repo`, `read:org` and `workflow`.

- A [fine-grained personal access token](https://github.com/settings/personal-access-tokens), which lets you limit access to particular repositories and permissions. spr needs these repository permissions: _Contents_, _Pull requests_ and _Workflows_ (read and write); and for team reviewers, the organisation permission _Members_ (read). GitHub doesn't tell spr the permissions of such a token, so `spr init` can't check them.

## GitHub Enterprise

spr works with GitHub Enterprise Server, and with GitHub Enterprise Cloud with data residency (`SUBDOMAIN.ghe.com`). Repositories there are written with their host, e.g. `github://ghe.example.com/owner/repo` (`spr init` suggests that, if your `origin` remote points there), and spr's target is then e.g. `github://ghe.example.com/owner/repo:main`. spr talks to the host's API (`https://HOST/api/v3` and `https://HOST/api/graphql` for GitHub Enterprise Server, `https://api.SUBDOMAIN.ghe.com` for GHE.com).

Logging in with GitHub only works on github.com, so `spr init` asks you for a token instead. It shows a link that creates a classic personal access token with the right scopes (`https://HOST/settings/tokens/new?scopes=repo,read:org,workflow&description=spr`). If you use the GitHub CLI with your GitHub Enterprise host, you can take its token instead (`gh auth token --hostname HOST`). The token is stored in `spr.auth.github.HOST.token`, e.g.:

```shell
git config --global spr.auth.github.ghe.example.com.token "$(gh auth token --hostname ghe.example.com)"
```

GitHub's stacked pull requests are only on github.com so far, so the stacking mode `github-stack` doesn't work with GitHub Enterprise; the other [stacking modes](./stacking-modes.md) do.

Support for GitHub Enterprise is new, and tested against a simulation of its API only. If something doesn't work, please [open an issue](https://github.com/spacedentist/spr/issues).
