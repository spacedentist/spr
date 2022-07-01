# Set up spr

In the repo you want to use spr in, run `spr init`; this will ask you several questions.

You'll need to provide a GitHub personal access token (PAT) as the first step. [See the GitHub docs](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/creating-a-personal-access-token) on how to create one. `spr init` will tell you which scopes the token must have; make sure to set them correctly when creating the token.

The rest of the settings that `spr init` asks for have sensible defaults, so almost all users can simply accept the defaults. The most common situation where you would need to diverge from the defaults is if the remote representing GitHub is not called `origin`.

See the [Configuration](./reference/configuration.md) reference page for full details about the available settings.

After initial setup, you can update your settings in several ways:

- Simply rerun `spr init`. The defaults it suggests will be your existing settings, so you can easily change only what you need to.

- Use `git config --set` ([docs here](https://git-scm.com/docs/git-config)).

- Edit the `[spr]` section of `.git/config` directly.
