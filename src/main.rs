//! A command-line tool for submitting and updating GitHub Pull Requests from
//! local Git commits that may be amended and rebased. Pull Requests can be
//! stacked to allow for a series of code reviews of interdependent code.

use clap::{Parser, Subcommand};
use color_eyre::eyre::{Error, Result, WrapErr as _, bail, eyre};
use log::debug;
use spr::commands;
use spr::forge::Forge;
use spr::target::{ForgeKind, Repository, Target};

#[derive(Parser)]
#[clap(
    name = "spr",
    version,
    about = "Submit pull requests for individual, amendable, rebaseable commits to GitHub"
)]
pub struct Cli {
    /// Change to DIR before performing any operations
    #[clap(long, value_name = "DIR")]
    cd: Option<String>,

    /// Token for the forge and host of the target (if not given, taken
    /// from git config spr.auth.github.token for github.com, or
    /// spr.auth.github.HOST.token for GitHub Enterprise)
    #[clap(long)]
    auth_token: Option<String>,

    /// Deprecated: use --auth-token
    #[clap(long, hide = true)]
    github_auth_token: Option<String>,

    /// Where Pull Requests get merged, for commits without a Spr-Target
    /// trailer: 'FORGE:REPOSITORY:BRANCH' (e.g. 'github:owner/repo:main'),
    /// or a branch name alone for a branch in the repository of the default
    /// target (git config spr.target). `spr diff` adds a Spr-Target trailer
    /// to commits for other targets than the default one.
    #[clap(long)]
    target: Option<String>,

    /// Deprecated: use --target. GitHub repository ('org/name')
    #[clap(long, hide = true)]
    github_repository: Option<String>,

    /// Deprecated: use --target. The branch into which the pull requests
    /// are merged
    #[clap(long, hide = true)]
    github_master_branch: Option<String>,

    /// prefix to be used for branches created for pull requests (if not given
    /// taken from git config spr.branchPrefix, defaulting to
    /// 'spr/<GITHUB_USERNAME>/')
    #[clap(long)]
    branch_prefix: Option<String>,

    #[clap(subcommand)]
    command: Commands,
}

// Not derived, so the auth token doesn't end up in debug output (logs).
impl std::fmt::Debug for Cli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cli")
            .field("cd", &self.cd)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "github_auth_token",
                &self.github_auth_token.as_ref().map(|_| "[redacted]"),
            )
            .field("target", &self.target)
            .field("github_repository", &self.github_repository)
            .field("github_master_branch", &self.github_master_branch)
            .field("branch_prefix", &self.branch_prefix)
            .field("command", &self.command)
            .finish()
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Interactive assistant for configuring spr in a local GitHub-backed Git
    /// repository
    Init,

    /// Create a new or update an existing Pull Request on GitHub from the
    /// current HEAD commit
    Diff(commands::diff::DiffOptions),

    /// Reformat commit message
    Format(commands::format::FormatOptions),

    /// Land a reviewed Pull Request
    Land(commands::land::LandOptions),

    /// Update local commit message with content on GitHub
    Amend(commands::amend::AmendOptions),

    /// List open Pull Requests on GitHub and their review decision
    List,

    /// Create a new branch with the contents of an existing Pull Request
    Patch(commands::patch::PatchOptions),

    /// Apply changes others pushed to the Pull Request to the local commit
    Pull(commands::pull::PullOptions),

    /// Close a Pull request
    Close(commands::close::CloseOptions),

    /// Low-level commands for scripts, working on Git objects only
    /// (experimental)
    Plumbing(commands::plumbing::PlumbingOptions),
}

pub async fn spr() -> Result<()> {
    let cli = Cli::parse();
    debug!("Started with command line: {:?}", cli);

    if let Some(path) = &cli.cd
        && let Err(err) = std::env::set_current_dir(path)
    {
        eprintln!("Could not change directory to {:?}", path);
        return Err(err.into());
    }

    // Commands that don't need spr to be configured
    let command = match cli.command {
        Commands::Init => return commands::init::init().await,
        Commands::Plumbing(opts) => {
            let repo = git2::Repository::discover(std::env::current_dir()?)?;
            return commands::plumbing::plumbing(
                opts,
                &spr::git::Git::new(repo)?,
            );
        }
        command => command,
    };

    let repo = git2::Repository::discover(std::env::current_dir()?)?;

    let git_config = repo.config()?;

    // A required setting: given on the command line, or in the Git config
    let required = |value: Option<String>, key: &str| -> Result<String> {
        match value {
            Some(value) => Ok(value),
            None => git_config.get_string(key).map_err(|_| {
                eyre!(
                    "{key} is not configured. Run `spr init` to set up spr \
                     in this repository."
                )
            }),
        }
    };

    // The configured default target, where the deprecated command line
    // options can override its repository and branch
    let configured = spr::config::configured_target(&git_config)?;
    let configured = if cli.github_repository.is_some()
        || cli.github_master_branch.is_some()
    {
        if cli.target.is_some() {
            bail!(
                "--github-repository and --github-master-branch can't be \
                 used together with --target, which replaces them"
            );
        }
        spr::output::output(
            "⚠️",
            "--github-repository and --github-master-branch are \
             deprecated: use --target",
        )?;
        let repository = match cli.github_repository {
            Some(repository) => {
                Repository::new(ForgeKind::GitHub, None, &repository)?
            }
            None => configured
                .as_ref()
                .map(|target| target.repository().clone())
                .ok_or_else(not_configured)?,
        };
        let branch = cli
            .github_master_branch
            .or_else(|| configured.map(|target| target.branch().to_string()))
            .or_else(|| git_config.get_string("spr.githubMasterBranch").ok())
            .unwrap_or_else(|| "master".to_string());
        Some(Target::new(repository, &branch)?)
    } else {
        configured
    };

    // The target of commits without a Spr-Target trailer
    let cli_target = cli
        .target
        .as_deref()
        .map(|target| {
            Target::parse_with_default(target, configured.as_ref())
                .wrap_err("Invalid --target")
        })
        .transpose()?;
    let default_target = cli_target
        .clone()
        .or_else(|| configured.clone())
        .ok_or_else(not_configured)?;

    if let Some(note) = spr::config::deprecation_note(&git_config) {
        spr::output::output("⚠️", &note)?;
    }

    let branch_prefix = required(cli.branch_prefix, "spr.branchPrefix")?;

    let require_approval = git_config
        .get_bool("spr.requireApproval")
        .ok()
        .unwrap_or(false);
    let spr_ids = git_config.get_bool("spr.sprIds").unwrap_or(false);
    let use_commit_title_for_initial_commit = git_config
        .get_bool("spr.useCommitTitleForInitialCommit")
        .ok()
        .unwrap_or(false);

    let merge_method = match git_config.get_string("spr.mergeMethod") {
        Ok(value) => value.parse()?,
        Err(_) => spr::config::MergeMethod::Squash,
    };

    let stacking_mode = match git_config.get_string("spr.stackingMode") {
        Ok(value) => value.parse()?,
        Err(_) => spr::config::StackingMode::default_for(merge_method),
    };

    let cli_token = match (cli.auth_token, cli.github_auth_token) {
        (Some(_), Some(_)) => bail!(
            "--github-auth-token can't be used together with --auth-token, \
             which replaces it"
        ),
        (None, Some(token)) => {
            spr::output::output(
                "⚠️",
                "--github-auth-token is deprecated: use --auth-token",
            )?;
            Some(token)
        }
        (token, None) => token,
    };

    let mut config = spr::config::Config::new(
        default_target.clone(),
        branch_prefix,
        spr::config::AuthTokens::from_git_config(&git_config)?,
        require_approval,
        merge_method,
        stacking_mode,
        use_commit_title_for_initial_commit,
        spr_ids,
    )?;
    config.configured_target = configured.unwrap_or(default_target);

    // Commands that work on local commits work for the target of the HEAD
    // commit
    if !matches!(command, Commands::List | Commands::Patch(_))
        && let Some(message) = head_commit_message(&repo)
    {
        let (head_target, has_trailer) = config.commit_target(&message)?;
        if let Some(cli_target) = cli_target
            && head_target != cli_target
        {
            bail!(
                "The HEAD commit has the target {head_target} (Spr-Target \
                 trailer), not {cli_target} as given with --target"
            );
        }
        if has_trailer {
            config = config.for_target(head_target)?;
        }
    }
    // The token given on the command line is for the run's target
    if let Some(token) = cli_token {
        let repository = config.target.repository().clone();
        config.auth_tokens.set(&repository, token);
    }
    debug!("config: {:?}", config);

    let git = spr::git::Git::new(repo)?;

    // The forge implementation for the target's kind of forge
    match config.target.repository().forge() {
        ForgeKind::GitHub => {
            let gh = spr::github::GitHub::new(config.clone(), git.clone())?;
            run(command, &git, gh, &config).await
        }
        // `Config::new` refuses targets on other forges (so far)
        forge => Err(eyre!("{forge} is not supported (yet)")),
    }
}

fn not_configured() -> color_eyre::eyre::Report {
    eyre!(
        "spr.target is not configured. Run `spr init` to set up spr in this \
         repository."
    )
}

/// The message of the HEAD commit, if there is one
fn head_commit_message(
    repo: &git2::Repository,
) -> Option<spr::message::CommitMessage> {
    let commit = repo.head().ok()?.peel_to_commit().ok()?;
    Some(spr::message::CommitMessage::parse(
        &String::from_utf8_lossy(commit.message_bytes()),
    ))
}

/// Run a command that works with the forge
async fn run(
    command: Commands,
    git: &spr::git::Git,
    mut gh: impl Forge,
    config: &spr::config::Config,
) -> Result<()> {
    let gh = &mut gh;
    match command {
        Commands::Diff(opts) => {
            commands::diff::diff(opts, git, gh, config).await?
        }
        Commands::Land(opts) => {
            commands::land::land(opts, git, gh, config).await?
        }
        Commands::Amend(opts) => {
            commands::amend::amend(opts, git, gh, config).await?
        }
        Commands::List => commands::list::list(gh).await?,
        Commands::Patch(opts) => {
            commands::patch::patch(opts, git, gh, config).await?
        }
        Commands::Close(opts) => {
            commands::close::close(opts, git, gh, config).await?
        }
        Commands::Pull(opts) => {
            commands::pull::pull(opts, git, gh, config).await?
        }
        Commands::Format(opts) => {
            commands::format::format(opts, git, gh, config).await?
        }

        // The following commands are executed in `spr` and return before
        // they get here.
        Commands::Init | Commands::Plumbing(_) => (),
    };

    Ok::<_, Error>(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();

    tokio::task::LocalSet::new().run_until(spr()).await
}
