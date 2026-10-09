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
    util::parse_scopes,
};

/// The scopes `spr init` asks for when logging in
const EXPECTED_SCOPES: [&str; 3] = ["read:org", "repo", "workflow"];

/// The permissions GitHub's authorisation page lists for these scopes
const EXPECTED_PERMISSIONS: [&str; 3] = [
    "Read org and team membership, read org projects",
    "Full control of private repositories",
    "Update github action workflows",
];

/// `manual init`: logging in with `spr init`, and how it treats configured
/// tokens. Returns whether everything passed.
pub fn init(api: &Api, repo: &str, spr: &Path, token: &str) -> Result<bool> {
    let env = TestEnv::new(spr, token)?;
    env.clone(repo)?;
    env.check_pristine()?;
    install_browser_opener(&env)?;
    let default_branch = api.default_branch()?;

    println!(
        "\nManual test: logging in with `spr init`\n\
         =======================================\n\n\
         spr init will run in a fresh clone of {repo}, without any spr \
         settings, so it starts by logging you in with GitHub:\n\n\
         1. It shows a link and a code, and opens the link in your web \
            browser.\n\
         2. Enter the code there. GitHub then shows what spr asks for (unless \
            you've authorised spr with these permissions before). Check that \
            it lists exactly these permissions:\n{}\n\
         3. Authorise spr. spr init then greets you and asks for its \
            settings: accept all the suggested values (press Return).\n\n\
         Afterwards, this test checks the settings and the new token, and \
         tries some variants on its own.\n\n\
         (Tip: to see the authorisation page, first revoke spr under \
         https://github.com/settings/applications.)\n",
        EXPECTED_PERMISSIONS
            .iter()
            .map(|permission| format!("   - {permission}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
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

    let mut report = Report::new();
    let mut add = |name: &str, result: Result<()>| {
        let outcome = match result {
            Ok(()) => Outcome::Passed,
            Err(error) => Outcome::Failed(format!("{error:#}")),
        };
        report.add(name, outcome, Duration::ZERO);
    };

    add(
        "spr init succeeded",
        if status.success() {
            Ok(())
        } else {
            Err(color_eyre::eyre::eyre!("spr init failed ({status})"))
        },
    );
    let new_token = env.configured_token()?;
    let login = check_token(&mut add, new_token.as_deref())?;
    add(
        "settings",
        check_settings(&env, repo, &default_branch, login.as_deref()),
    );
    add(
        "authorisation page",
        ask(&format!(
            "Did GitHub's authorisation page list exactly these permissions?\n{}\n\
             [y = yes, n = no, s = didn't see the page] ",
            EXPECTED_PERMISSIONS
                .iter()
                .map(|permission| format!("  - {permission}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))?,
    );

    println!("\nNow some variants, without your help…\n");

    // Running spr init again keeps the token
    add(
        "init again keeps the token",
        keeps_token(&env, login.as_deref().unwrap_or("")),
    );

    // A token that doesn't work: spr init says so, and logs in again. Also
    // without a web browser: the link and code are shown anyway.
    env.set_token("ghp_notarealtoken0000000000000000000000")?;
    add(
        "broken token, no browser",
        broken_token_without_browser(&env),
    );

    // The tests' own token (e.g. from `gh auth token`) is kept (#240)
    env.set_token_in_config()?;
    add("the tests' token is kept", keeps_token(&env, ""));

    let passed = report.print_summary();
    println!(
        "\nThe authorisation of spr in your GitHub settings stays: that's what \
         spr uses from now on. The test's clone was removed."
    );
    Ok(passed)
}

/// Check the token spr init got: it works and has exactly the expected
/// scopes. Returns the login of its user.
fn check_token(
    add: &mut impl FnMut(&str, Result<()>),
    token: Option<&str>,
) -> Result<Option<String>> {
    let Some(token) = token else {
        add(
            "new token",
            Err(color_eyre::eyre::eyre!("No token in spr.githubAuthToken")),
        );
        return Ok(None);
    };
    let (login, scopes) = match Api::new(token, "", "")?.token_info() {
        Ok(info) => info,
        Err(error) => {
            add(
                "new token",
                Err(error.wrap_err("The new token doesn't work")),
            );
            return Ok(None);
        }
    };
    let scopes = scopes.as_deref().map(parse_scopes).unwrap_or_default();
    let expected: BTreeSet<String> =
        EXPECTED_SCOPES.into_iter().map(String::from).collect();
    add(
        "new token's scopes",
        if scopes == expected {
            Ok(())
        } else {
            Err(color_eyre::eyre::eyre!(
                "Expected the scopes {expected:?}, got {scopes:?}"
            ))
        },
    );
    Ok(Some(login))
}

/// Check the settings spr init wrote (the suggested values were accepted)
fn check_settings(
    env: &TestEnv,
    repo: &str,
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
    expect("githubRepository", repo);
    expect("githubMasterBranch", default_branch);
    if let Some(login) = login {
        expect("branchPrefix", &format!("spr/{login}/"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("{}", problems.join("\n"))
    }
}

/// spr init keeps the configured token: it greets the user right away,
/// without logging in. (It's stopped at its first question.)
fn keeps_token(env: &TestEnv, login: &str) -> Result<()> {
    let output = env.run_until(
        {
            let mut command = env.spr_command();
            command.arg("init");
            command
        },
        "Hello",
        Duration::from_secs(30),
    )?;
    let text = output.all();
    if text.contains("authenticate spr")
        || !text.contains(&format!("Hello {login}"))
    {
        bail!("spr init didn't keep the token:\n{text}");
    }
    Ok(())
}

/// With a broken token and no way to open a web browser, spr init says why
/// it logs in again, and shows the link and the code. (It's stopped while
/// waiting for the login.)
fn broken_token_without_browser(env: &TestEnv) -> Result<()> {
    let no_browser = env.home().join("no-browser");
    std::fs::create_dir_all(&no_browser)?;
    let output = env.run_until(
        {
            let mut command = env.spr_command();
            command.arg("init").env("PATH", &no_browser);
            command
        },
        "< < < < <",
        Duration::from_secs(30),
    )?;
    let text = output.all();
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
