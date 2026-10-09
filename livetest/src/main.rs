//! Live tests of spr against GitHub.
//!
//! These tests run the spr binary in a controlled environment against a
//! real GitHub repository, so they need a GitHub token and a repository
//! meant for testing. That's why they're a program of their own, not Rust
//! tests: `cargo test` doesn't run them. Some tests are manual: they guide
//! you through what to do and check, e.g. logging in with `spr init`.
//!
//! Run `cargo run -p spr-livetest -- --help` for how to use it. (This is
//! the binary `spr-github-livetest`; other forges would get their own.)

mod api;
mod env;
mod report;
mod run;
mod scenarios;
mod util;

use std::{
    path::PathBuf,
    process::{Command, ExitCode},
    time::Instant,
};

use clap::{Parser, Subcommand};
use color_eyre::eyre::{Result, bail, eyre};

use crate::{
    api::Api,
    env::TestEnv,
    report::{Outcome, Report},
    run::Run,
    util::mask,
};

/// Live tests of spr against GitHub.
///
/// The tests run in a GitHub repository meant for testing: its default
/// branch must contain a file `.spr-livetest`. Each run works on branches of
/// its own (`spr/livetest/<run-id>/…`, and `livetest/<run-id>/main` instead
/// of the default branch), and removes them at the end.
#[derive(Parser)]
#[command(name = "spr-github-livetest")]
struct Cli {
    /// The GitHub repository to run the tests in
    #[arg(long, value_name = "OWNER/REPO")]
    repo: String,

    /// GitHub token for the tests. Better pass it in the environment
    /// variable SPR_GITHUB_LIVETEST_TOKEN: on the command line, others can
    /// see it in the list of processes. E.g. to use the token of the GitHub
    /// CLI: `SPR_GITHUB_LIVETEST_TOKEN=$(gh auth token) …`
    #[arg(long, value_name = "TOKEN")]
    token: Option<String>,

    /// The spr binary to test. Default: build it from this workspace with
    /// cargo.
    #[arg(long, value_name = "PATH")]
    spr: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run automated live tests
    Live {
        /// The tests to run (default: all)
        tests: Vec<String>,

        /// List the tests instead of running them
        #[arg(long)]
        list: bool,

        /// If a test fails, keep its temporary directory and everything the
        /// run created on GitHub, for debugging (remove it later with
        /// `cleanup`)
        #[arg(long)]
        keep: bool,
    },

    /// Remove what test runs left behind in the repository (e.g. after
    /// `--keep`, or when interrupted)
    Cleanup {
        /// The run to clean up (default: all runs)
        run_id: Option<String>,
    },
}

fn main() -> ExitCode {
    let _ = color_eyre::install();
    let cli = Cli::parse();

    // Listing the tests needs no token
    if let Commands::Live { list: true, .. } = cli.command {
        for scenario in scenarios::all() {
            println!("{:24} {}", scenario.name, scenario.description);
        }
        return ExitCode::SUCCESS;
    }

    let token = match resolve_token(cli.token.as_deref()) {
        Ok(token) => token,
        Err(error) => {
            eprintln!("Error: {}", describe(&error));
            return ExitCode::FAILURE;
        }
    };
    match run(cli, &token) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("Error: {}", mask(&describe(&error), &token));
            ExitCode::FAILURE
        }
    }
}

/// Run the command. Returns whether all tests passed.
fn run(cli: Cli, token: &str) -> Result<bool> {
    let (owner, repo) = util::parse_repo(&cli.repo)?;
    let api = Api::new(token, &owner, &repo)?;

    match cli.command {
        Commands::Live { tests, keep, .. } => {
            let selected = select(scenarios::all(), &tests)?;
            let spr = spr_binary(cli.spr)?;
            run_live(&api, &cli.repo, &spr, token, &selected, keep)
        }
        Commands::Cleanup { run_id } => {
            let ids = match run_id {
                Some(id) if util::is_run_id(&id) => [id].into(),
                Some(id) => bail!("{id:?} is not a run ID"),
                None => run::leftover_runs(&api)?,
            };
            if ids.is_empty() {
                println!("Nothing to clean up.");
            }
            for id in &ids {
                println!("Cleaning up run {id}");
            }
            run::cleanup_runs(&api, &ids)?;
            Ok(true)
        }
    }
}

/// The scenarios with the given names (all, if none are given)
fn select(
    all: Vec<scenarios::Scenario>,
    names: &[String],
) -> Result<Vec<scenarios::Scenario>> {
    if names.is_empty() {
        return Ok(all);
    }
    for name in names {
        if !all.iter().any(|scenario| scenario.name == name) {
            bail!("There is no test {name:?} (see `live --list`)");
        }
    }
    Ok(all
        .into_iter()
        .filter(|scenario| names.iter().any(|name| name == scenario.name))
        .collect())
}

fn run_live(
    api: &Api,
    repo: &str,
    spr: &std::path::Path,
    token: &str,
    selected: &[scenarios::Scenario],
    keep: bool,
) -> Result<bool> {
    if selected.is_empty() {
        println!("No tests to run.");
        return Ok(true);
    }

    let run = Run::start(api)?;
    println!(
        "Test run {} in {repo} (branches {}…, target branch {})\n",
        run.id, run.prefix, run.target_branch
    );

    let mut report = Report::new();
    for scenario in selected {
        let start = Instant::now();
        let mut env = TestEnv::new(spr, token)?;
        let result = setup(&env, repo, &run, scenario.name).and_then(|()| {
            (scenario.run)(&scenarios::Ctx {
                env: &env,
                api,
                run: &run,
            })
        });
        let outcome = match result {
            Ok(()) => Outcome::Passed,
            Err(error) => {
                let mut message = mask(&describe(&error), token);
                if keep {
                    message.push_str(&format!(
                        "\n(kept: {})",
                        env.keep().display()
                    ));
                }
                Outcome::Failed(message)
            }
        };
        report.add(scenario.name, outcome, start.elapsed());
    }

    if keep && report.failed() > 0 {
        println!(
            "\nKept what the run created on GitHub. To remove it: \
             spr-github-livetest --repo {repo} cleanup {}",
            run.id
        );
    } else {
        run.cleanup(api)?;
    }
    Ok(report.print_summary())
}

/// Set up the environment of a scenario: a clone of the repository on a
/// local branch `work` based on the run's target branch, with spr
/// configured
fn setup(env: &TestEnv, repo: &str, run: &Run, test: &str) -> Result<()> {
    env.clone(repo)?;
    env.git(&[
        "switch",
        "--quiet",
        "-c",
        "work",
        &format!("origin/{}", run.target_branch),
    ])?;
    env.configure(&[
        ("githubRepository", repo),
        ("githubMasterBranch", &run.target_branch),
        ("branchPrefix", &run.test_prefix(test)),
    ])?;
    env.set_token_in_config()?;
    env.check_pristine()
}

/// An error and its causes, one per line
fn describe(error: &color_eyre::Report) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\ncaused by: ")
}

/// The environment variable for the token
const TOKEN_VARIABLE: &str = "SPR_GITHUB_LIVETEST_TOKEN";

/// The token: given explicitly, on the command line or (better) in the
/// environment. The tests never help themselves to a token (e.g. from the
/// GitHub CLI), so they can't use an account you didn't mean them to.
fn resolve_token(given: Option<&str>) -> Result<String> {
    if let Some(token) = given {
        return Ok(token.to_string());
    }
    match std::env::var(TOKEN_VARIABLE) {
        Ok(token) if !token.trim().is_empty() => Ok(token.trim().to_string()),
        _ => bail!(
            "No GitHub token given. Set the environment variable \
             {TOKEN_VARIABLE}, e.g. to use the token of the GitHub CLI:\n\n  \
             {TOKEN_VARIABLE}=$(gh auth token) cargo run -p spr-livetest -- …"
        ),
    }
}

/// The spr binary: given, or built from this workspace
fn spr_binary(given: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = given {
        if !path.is_file() {
            bail!("{} doesn't exist", path.display());
        }
        return Ok(path);
    }
    let workspace = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    println!("Building spr…");
    let output = Command::new(cargo)
        .current_dir(workspace)
        .args([
            "build",
            "--package",
            "spr",
            "--bin",
            "spr",
            "--message-format=json-render-diagnostics",
        ])
        .stderr(std::process::Stdio::inherit())
        .output()?;
    if !output.status.success() {
        bail!("Building spr failed");
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter(|message| message["target"]["name"] == "spr")
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| eyre!("Building spr didn't produce a binary"))
}
