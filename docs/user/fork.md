# Create PRs from a Fork

Working from a fork is effectively the same as working without a fork. The main difference is how the repo is configured.

There are two configuration values that need to be set to work with a fork: `spr.githubUpstreamRemoteName` and `spr.githubUpstreamRepository`. `spr.githubUpstreamRemoteName` is the name of the git remote the fork is based on (this tends to be called `upstream`). `spr.githubUpstreamRepository` is the name of the repository the fork is based on; it must be in `owner/repo` format.

For example, let's say you wanted to create PRs from a fork of the `spr` repo. You'd fork and clone the `getcord/spr` repo. This should give you a local copy of our fork. You'd need to set the configuration values for everything to work:
```shell
# Add the remote for the `getcord/spr` repo on GitHub.
$ git remote add upstream git@github.com:getcord/spr.git
$ git config spr.githubUpstreamRemoteName upstream
$ git config spr.githubUpstreamRepository getcord/spr
```

At this point, you should be able to follow the [simple][] workflow to create PRs from your fork. The [stack][] workflow does not currently work from a fork.

[simple]: ./simple.md
[stack]: ./stack.md
