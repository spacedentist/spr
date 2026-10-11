//! Manual tests: they guide the tester through what to do and check, and
//! check automatically what they can.

use std::{
    collections::BTreeSet,
    io::Write as _,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use color_eyre::eyre::{Result, WrapErr, bail};

use crate::{
    api::Api,
    env::TestEnv,
    report::{Outcome, Report},
    util::{Repo, parse_scopes},
};

/// The scopes `spr init` asks for when logging in
const EXPECTED_SCOPES: [&str; 3] = ["read:org", "repo", "workflow"];

/// The permissions GitHub's authorisation page lists for these scopes
const EXPECTED_PERMISSIONS: [&str; 3] = [
    "Read org and team membership, read org projects",
    "Full control of private repositories",
    "Update github action workflows",
];

/// `manual init`: logging in with `spr init` (github.com) or giving it a
/// token (GitHub Enterprise), and how it treats configured tokens. With
/// `automated_only`, only the variants that need no help. Returns whether
/// everything passed.
pub fn init(
    api: &Api,
    spr: &Path,
    token: &str,
    automated_only: bool,
) -> Result<bool> {
    let repository = &api.repository;
    let host = &repository.host;
    // spr's OAuth app for logging in is on github.com only
    let log_in = repository.is_github_com();
    let env = TestEnv::new(spr, token, repository)?;
    env.clone()?;
    env.check_pristine()?;
    install_browser_opener(&env)?;
    let default_branch = api.default_branch()?;
    let (tests_login, _) = api.token_info()?;

    let mut report = Report::new();
    let mut add = |name: &str, result: Result<()>| {
        let outcome = match result {
            Ok(()) => Outcome::Passed,
            Err(error) => Outcome::Failed(format!("{error:#}")),
        };
        report.add(name, outcome, Duration::ZERO);
    };

    if automated_only {
        println!(
            "\nThe variants of manual init that need no help, with the tests' \
             token…\n"
        );
    } else {
        println!("{}", intro(repository, log_in));
        prompt("Press Return to start spr init (Ctrl-C to cancel)… ")?;

        // The real thing, interactively
        let status = env
            .spr_command()
            .arg("init")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        println!();

        add(
            "spr init succeeded",
            if status.success() {
                Ok(())
            } else {
                Err(color_eyre::eyre::eyre!("spr init failed ({status})"))
            },
        );
        let new_token = env.configured_token()?;
        let login = check_token(&mut add, api, new_token.as_deref(), log_in)?;
        add(
            "settings",
            check_settings(&env, &default_branch, login.as_deref()),
        );
        if log_in {
            add(
                "authorisation page",
                ask(&format!(
                    "Did GitHub's authorisation page list exactly these \
                     permissions?\n{}\n\
                     [y = yes, n = no, s = didn't see the page] ",
                    EXPECTED_PERMISSIONS
                        .iter()
                        .map(|permission| format!("  - {permission}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ))?,
            );
        }

        println!("\nNow some variants, without your help…\n");

        // Running spr init again keeps the token
        add(
            "init again keeps the token",
            keeps_token(&env, login.as_deref().unwrap_or("")),
        );
    }

    // A token that doesn't work: spr init says so, and gets another one
    let broken_token = "ghp_notarealtoken0000000000000000000000";
    if log_in {
        // Logging in, also without a web browser: the link and code are
        // shown anyway
        env.set_token(broken_token)?;
        add(
            "broken token, no browser",
            broken_token_without_browser(&env),
        );
    }
    // Pasting a token (here: the tests' own)
    env.set_token(broken_token)?;
    add(
        "broken token, pasting another",
        broken_token_paste(&env, token, &tests_login, log_in),
    );

    // The tests' own token (e.g. from `gh auth token`) is kept (#240)
    env.set_token_in_config()?;
    add("the tests' token is kept", keeps_token(&env, &tests_login));

    // Accepting all suggestions writes the settings
    add(
        "init with the suggested settings",
        accepts_suggestions(&env, &default_branch, &tests_login),
    );

    if log_in {
        // spr.auth.github.token replaces spr.githubAuthToken: spr init keeps
        // the token, under the new key
        env.set_old_token_in_config()?;
        add(
            "spr.githubAuthToken is replaced",
            replaces_old_token(&env, token, &tests_login),
        );
    }

    let passed = report.print_summary();
    if automated_only {
        println!("\nThe test's clone was removed.");
    } else if log_in {
        println!(
            "\nThe authorisation of spr in your GitHub settings stays: that's \
             what spr uses from now on. The test's clone was removed."
        );
    } else {
        println!(
            "\nThe token you created stays: remove it in your settings on \
             {host} if you don't need it anymore. The test's clone was \
             removed."
        );
    }
    Ok(passed)
}

/// What the tester has to do in `manual init`
fn intro(repository: &Repo, log_in: bool) -> String {
    let host = &repository.host;
    if log_in {
        format!(
            "\nManual test: logging in with `spr init`\n\
             =======================================\n\n\
             spr init will run in a fresh clone of {repository}, without any \
             spr settings:\n\n\
             1. It asks for the repository, suggesting the clone's: accept it \
                (press Return).\n\
             2. As there's no token, it asks how to get one: choose to log in \
                with GitHub (press Return).\n\
             3. It shows a link and a code, and opens the link in your web \
                browser.\n\
             4. Enter the code there. GitHub then shows what spr asks for \
                (unless you've authorised spr with these permissions before). \
                Check that it lists exactly these permissions:\n{}\n\
             5. Authorise spr. spr init then greets you and asks for the rest \
                of its settings: accept all the suggested values (press \
                Return).\n\n\
             Afterwards, this test checks the settings and the new token, and \
             tries some variants on its own.\n\n\
             (Tip: to see the authorisation page, first revoke spr under \
             https://github.com/settings/applications.)\n",
            EXPECTED_PERMISSIONS
                .iter()
                .map(|permission| format!("   - {permission}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    } else {
        format!(
            "\nManual test: giving `spr init` a token\n\
             ======================================\n\n\
             spr init will run in a fresh clone of {repository}, without any \
             spr settings:\n\n\
             1. It asks for the repository, suggesting the clone's: accept it \
                (press Return).\n\
             2. As there's no token, it explains how to get one for {host}, \
                and asks for it. Create one as it says (check that the link \
                works and asks for the scopes repo, read:org and workflow), \
                and paste it. (Or paste the output of `gh auth token \
                --hostname {host}`.)\n\
             3. spr init then greets you and asks for the rest of its \
                settings: accept all the suggested values (press Return).\n\n\
             Afterwards, this test checks the settings and the token, and \
             tries some variants on its own.\n"
        )
    }
}

/// Check the token spr init got: it works, and has the expected scopes —
/// exactly those spr asks for when logging in, otherwise at least the
/// required ones (or none: a fine-grained token). Returns the login of its
/// user.
fn check_token(
    add: &mut impl FnMut(&str, Result<()>),
    api: &Api,
    token: Option<&str>,
    logged_in: bool,
) -> Result<Option<String>> {
    let Some(token) = token else {
        add(
            "new token",
            Err(color_eyre::eyre::eyre!(
                "No token in {}",
                api.repository.token_key()
            )),
        );
        return Ok(None);
    };
    let (login, scopes) = match Api::new(token, &api.repository)?.token_info() {
        Ok(info) => info,
        Err(error) => {
            add(
                "new token",
                Err(error.wrap_err("The new token doesn't work")),
            );
            return Ok(None);
        }
    };
    let expected: BTreeSet<String> =
        EXPECTED_SCOPES.into_iter().map(String::from).collect();
    let result = match scopes.as_deref().map(parse_scopes) {
        Some(scopes) if logged_in && scopes != expected => {
            Err(color_eyre::eyre::eyre!(
                "Expected the scopes {expected:?}, got {scopes:?}"
            ))
        }
        Some(scopes) if !scopes.is_superset(&expected) => {
            Err(color_eyre::eyre::eyre!(
                "Expected at least the scopes {expected:?}, got {scopes:?}"
            ))
        }
        None if logged_in => {
            Err(color_eyre::eyre::eyre!("The new token has no scopes"))
        }
        _ => Ok(()),
    };
    add("new token's scopes", result);
    Ok(Some(login))
}

/// Check the settings spr init wrote (the suggested values were accepted)
fn check_settings(
    env: &TestEnv,
    default_branch: &str,
    login: Option<&str>,
) -> Result<()> {
    let get = |key: &str| env.git(&["config", "--get", &format!("spr.{key}")]);
    let mut problems = Vec::new();
    let mut expect = |key: &str, expected: &str| match get(key) {
        Ok(value) if value == expected => {}
        Ok(value) => problems
            .push(format!("spr.{key} is {value:?}, expected {expected:?}")),
        Err(_) => problems.push(format!("spr.{key} isn't set")),
    };
    expect(
        "target",
        &format!("{}:{default_branch}", env.repository().notation()),
    );
    if let Some(login) = login {
        expect("branchPrefix", &format!("spr/{login}/"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("{}", problems.join("\n"))
    }
}

/// Run spr init in a terminal, accepting the suggested repository and
/// answering the further questions given, until the output contains
/// `until`
fn init_in_terminal(
    env: &TestEnv,
    answers: &[(&str, &str)],
    until: &str,
) -> Result<String> {
    let mut command = env.spr_command();
    command.arg("init");
    init_command_in_terminal(env, command, answers, until)
}

fn init_command_in_terminal(
    env: &TestEnv,
    command: std::process::Command,
    answers: &[(&str, &str)],
    until: &str,
) -> Result<String> {
    let mut all_answers = vec![("GitHub repository", "\r")];
    all_answers.extend_from_slice(answers);
    let output = env.run_in_terminal(
        command,
        &all_answers,
        until,
        Duration::from_secs(30),
    )?;
    Ok(output.all())
}

/// spr init keeps the configured token: it greets the user right away,
/// without asking for a token. (It's stopped at its next question.)
fn keeps_token(env: &TestEnv, login: &str) -> Result<()> {
    let text = init_in_terminal(env, &[], "Hello")?;
    if text.contains("authenticate spr")
        || text.contains("How should spr get a token")
        || !text.contains(&format!("Hello {login}"))
    {
        bail!("spr init didn't keep the token:\n{text}");
    }
    Ok(())
}

/// With a broken token and no way to open a web browser, spr init says why
/// it gets a new token, logs in, and shows the link and the code. (It's
/// stopped while waiting for the login.)
fn broken_token_without_browser(env: &TestEnv) -> Result<()> {
    let no_browser = env.home().join("no-browser");
    std::fs::create_dir_all(&no_browser)?;
    let mut command = env.spr_command();
    command.arg("init").env("PATH", &no_browser);
    let text = init_command_in_terminal(
        env,
        command,
        // Log in (the default)
        &[("How should spr get a token", "\r")],
        "< < < < <",
    )?;
    for expected in [
        "doesn't work (Bad credentials)",
        "https://github.com/login/device",
        "and enter code",
    ] {
        if !text.contains(expected) {
            bail!("Expected {expected:?} in the output:\n{text}");
        }
    }
    if text.contains("should open in your web browser") {
        bail!("spr init claims to have opened a web browser:\n{text}");
    }
    Ok(())
}

/// With a broken token, spr init says why it gets a new one, explains how
/// to create one, and takes the pasted token (if `choose`, after choosing
/// to paste one)
fn broken_token_paste(
    env: &TestEnv,
    token: &str,
    login: &str,
    choose: bool,
) -> Result<()> {
    let host = &env.repository().host;
    let typed_token = format!("{token}\r");
    let mut answers = Vec::new();
    if choose {
        // The second choice: arrow down, Return
        answers.push(("How should spr get a token", "\x1b[B\r"));
    }
    answers.push(("Token", typed_token.as_str()));
    let text = init_in_terminal(env, &answers, "Hello")?;
    let gh_command = if env.repository().is_github_com() {
        "`gh auth token`".to_string()
    } else {
        format!("`gh auth token --hostname {host}`")
    };
    for expected in [
        "doesn't work (Bad credentials)",
        &format!(
            "https://{host}/settings/tokens/new?scopes=repo,read:org,workflow"
        ),
        &gh_command,
        &format!("Hello {login}"),
    ] {
        if !text.contains(expected) {
            bail!("Expected {expected:?} in the output:\n{text}");
        }
    }
    if env.configured_token()?.as_deref() != Some(token) {
        bail!("spr init didn't configure the pasted token");
    }
    Ok(())
}

/// spr init with all its suggestions accepted (and a token configured)
/// writes the settings
fn accepts_suggestions(
    env: &TestEnv,
    default_branch: &str,
    login: &str,
) -> Result<()> {
    let mut command = env.spr_command();
    command.arg("init");
    let output = env.run_in_terminal(
        command,
        &[
            ("GitHub repository", "\r"),
            ("Target branch", "\r"),
            ("Branch prefix", "\r"),
        ],
        // Until it's done
        "\0",
        Duration::from_secs(30),
    )?;
    if !output.success {
        bail!("spr init failed:\n{}", output.all());
    }
    check_settings(env, default_branch, Some(login))
}

/// spr init takes the token in spr.githubAuthToken, which
/// spr.auth.github.token replaces
fn replaces_old_token(env: &TestEnv, token: &str, login: &str) -> Result<()> {
    let text = init_in_terminal(env, &[], "Hello")?;
    for expected in [
        "Replaced spr.githubAuthToken with spr.auth.github.token",
        &format!("Hello {login}"),
    ] {
        if !text.contains(expected) {
            bail!("Expected {expected:?} in the output:\n{text}");
        }
    }
    if env.configured_token()?.as_deref() != Some(token) {
        bail!("spr init didn't keep the token as spr.auth.github.token");
    }
    let old = env.run({
        let mut command = env.command("git");
        command.args(["config", "--get", "spr.githubAuthToken"]);
        command
    })?;
    if old.success {
        bail!("spr.githubAuthToken is still set");
    }
    Ok(())
}

/// Let spr open links in the tester's web browser, although it runs with a
/// home directory of its own: a wrapper for `xdg-open` (Linux) that runs it
/// with the tester's home directory and desktop session.
fn install_browser_opener(env: &TestEnv) -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let Some(xdg_open) = find_in_path("xdg-open") else {
        return Ok(());
    };
    let mut variables = String::new();
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_DIRS",
        "XDG_RUNTIME_DIR",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "BROWSER",
    ] {
        if let Ok(value) = std::env::var(name) {
            variables.push_str(&format!("{name}={} ", shell_quote(&value)));
        }
    }
    let script = format!(
        "#!/bin/sh\n# Open links with the tester's own environment\n\
         exec env {variables}{} \"$@\"\n",
        shell_quote(&xdg_open.to_string_lossy())
    );
    let path = env.bin_dir().join("xdg-open");
    std::fs::write(&path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(0o755),
        )?;
    }
    Ok(())
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn prompt(text: &str) -> Result<String> {
    print!("{text}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .wrap_err("Reading from the terminal failed")?;
    Ok(line.trim().to_lowercase())
}

/// Ask the tester a yes/no question. "Skip" counts as passed.
fn ask(question: &str) -> Result<Result<()>> {
    loop {
        match prompt(question)?.as_str() {
            "y" | "yes" | "s" | "skip" => return Ok(Ok(())),
            "n" | "no" => {
                return Ok(Err(color_eyre::eyre::eyre!(
                    "The tester saw something else"
                )));
            }
            _ => continue,
        }
    }
}
