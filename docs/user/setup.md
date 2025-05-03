# Set up spr

In the repo you want to use spr in, run `spr init`; this will ask you several questions.

You'll need to authorise spr with your GitHub account. `spr init` will guide you through the process.

The rest of the settings that `spr init` asks for have sensible defaults, so almost all users can simply accept the defaults. The most common situation where you would need to diverge from the defaults is if the remote representing GitHub is not called `origin`.

See the [Configuration](../reference/configuration.md) reference page for full details about the available settings.

After initial setup, you can update your settings in several ways:

- Simply rerun `spr init`. The defaults it suggests will be your existing settings, so you can easily change only what you need to.

- Use `git config --set` ([docs here](https://git-scm.com/docs/git-config)).

- Edit the `[spr]` section of `.git/config` directly.
