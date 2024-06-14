/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A command-line tool for submitting and updating GitHub Pull Requests from
//! local Git commits that may be amended and rebased. Pull Requests can be
//! stacked to allow for a series of code reviews of interdependent code.

use clap::{Parser, Subcommand};
use reqwest::{self, header};
use spr::{
    commands,
    error::{Error, Result},
    output::output,
};

#[derive(Parser, Debug)]
#[clap(
    name = "spr",
    version,
    about = "Submit pull requests for individual, amendable, rebaseable commits to GitHub"
)]
pub struct Cli {
    /// Change to DIR before performing any operations
    #[clap(long, value_name = "DIR")]
    cd: Option<String>,

    /// GitHub personal access token (if not given taken from git config
    /// spr.githubAuthToken)
    #[clap(long)]
    github_auth_token: Option<String>,

    /// GitHub repository ('org/name', if not given taken from config
    /// spr.githubRepository)
    #[clap(long)]
    github_repository: Option<String>,

    /// The name of the centrally shared branch into which the pull requests are merged
    /// spr.githubMasterBranch)
    #[clap(long)]
    github_master_branch: Option<String>,

    /// prefix to be used for branches created for pull requests (if not given
    /// taken from git config spr.branchPrefix, defaulting to
    /// 'spr/<GITHUB_USERNAME>/')
    #[clap(long)]
    branch_prefix: Option<String>,

    #[clap(subcommand)]
    command: Commands,
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

    /// Close a Pull request
    Close(commands::close::CloseOptions),
}

#[derive(Debug, thiserror::Error)]
pub enum OptionsError {
    #[error(
        "GitHub repository must be given as 'OWNER/REPO', but given value was '{0}'"
    )]
    InvalidRepository(String),
}

pub async fn spr() -> Result<()> {
    let cli = Cli::parse();

    if let Some(path) = &cli.cd {
        if let Err(err) = std::env::set_current_dir(path) {
            eprintln!("Could not change directory to {:?}", &path);
            return Err(err.into());
        }
    }

    if let Commands::Init = cli.command {
        return commands::init::init().await;
    }

    let repo = git2::Repository::discover(std::env::current_dir()?)?;

    let git_config = repo.config()?;

    let github_repository = match cli.github_repository {
        Some(v) => Ok(v),
        None => git_config.get_string("spr.githubRepository"),
    }?;

    let github_master_branch = match cli.github_master_branch {
        Some(v) => Ok::<String, git2::Error>(v),
        None => git_config
            .get_string("spr.githubMasterBranch")
            .or_else(|_| Ok("master".to_string())),
    }?;

    let (github_owner, github_repo) = {
        let captures = lazy_regex::regex!(r#"^([\w\-\.]+)/([\w\-\.]+)$"#)
            .captures(&github_repository)
            .ok_or_else(|| {
                OptionsError::InvalidRepository(github_repository.clone())
            })?;
        (
            captures.get(1).unwrap().as_str().to_string(),
            captures.get(2).unwrap().as_str().to_string(),
        )
    };

    let github_remote_name = git_config
        .get_string("spr.githubRemoteName")
        .unwrap_or_else(|_| "origin".to_string());
    let branch_prefix = git_config.get_string("spr.branchPrefix")?;
    let require_approval = git_config
        .get_bool("spr.requireApproval")
        .ok()
        .unwrap_or(false);
    let require_test_plan = git_config
        .get_bool("spr.requireTestPlan")
        .ok()
        .unwrap_or(true);

    let config = spr::config::Config::new(
        github_owner,
        github_repo,
        github_remote_name,
        github_master_branch,
        branch_prefix,
        require_approval,
        require_test_plan,
    );

    let git = spr::git::Git::new(repo);

    if let Commands::Format(opts) = cli.command {
        return commands::format::format(opts, &git, &config).await;
    }

    let github_auth_token = match cli.github_auth_token {
        Some(v) => Ok(v),
        None => git_config.get_string("spr.githubAuthToken"),
    }?;

    octocrab::initialise(
        octocrab::Octocrab::builder().personal_token(github_auth_token.clone()),
    )?;

    let mut headers = header::HeaderMap::new();
    headers.insert(header::ACCEPT, "application/json".parse()?);
    headers.insert(
        header::USER_AGENT,
        format!("spr/{}", env!("CARGO_PKG_VERSION")).try_into()?,
    );
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {}", github_auth_token).parse()?,
    );

    let graphql_client = reqwest::Client::builder()
        .default_headers(headers)
        .build()?;

    let mut gh = spr::github::GitHub::new(
        config.clone(),
        git.clone(),
        graphql_client.clone(),
    );

    match cli.command {
        Commands::Diff(opts) => {
            commands::diff::diff(opts, &git, &mut gh, &config).await?
        }
        Commands::Land(opts) => {
            commands::land::land(opts, &git, &mut gh, &config).await?
        }
        Commands::Amend(opts) => {
            commands::amend::amend(opts, &git, &mut gh, &config).await?
        }
        Commands::List => commands::list::list(graphql_client, &config).await?,
        Commands::Patch(opts) => {
            commands::patch::patch(opts, &git, &mut gh, &config).await?
        }
        Commands::Close(opts) => {
            commands::close::close(opts, &git, &mut gh, &config).await?
        }
        // The following commands are executed above and return from this
        // function before it reaches this match.
        Commands::Init | Commands::Format(_) => (),
    };

    Ok::<_, Error>(())
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Err(error) = spr().await {
        for message in error.messages() {
            output("🛑", message)?;
        }
        std::process::exit(1);
    }

    Ok(())
}
