# Set up spr

In the repo you want to use spr in, run `spr init`; this will ask you several questions.

You'll need to authorise spr with your GitHub account. `spr init` will guide you through the process.

The rest of the settings that `spr init` asks for have sensible defaults, so almost all users can simply accept the defaults. `spr init` suggests the GitHub repository that your `origin` remote points to; if your repository has no such remote, enter the repository as `owner/repo`.

`spr init` doesn't ask how pull requests get merged or stacked. The defaults are spr's original workflow: squash-merging, and synthetic base branches for stacked pull requests. If your team merges pull requests with merge commits, or you want to use GitHub's stacked pull requests, see [Choose a Merge Method and Stacking Mode](./stacking-modes.md).

See the [Configuration](../reference/configuration.md) reference page for full details about the available settings.

After initial setup, you can update your settings in several ways:

- Simply rerun `spr init`. The defaults it suggests will be your existing settings, so you can easily change only what you need to.

- Use `git config spr.<key> <value>`, e.g. `git config spr.mergeMethod merge` ([docs here](https://git-scm.com/docs/git-config)).

- Edit the `[spr]` section of `.git/config` directly.
