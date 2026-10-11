//! A controlled environment for running spr: a temporary directory with its
//! own home directory (so no global Git configuration applies), a fresh
//! clone of the test repository, and a cleared process environment.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use color_eyre::eyre::{Result, WrapErr, bail, eyre};

use crate::util::{Repo, mask};

pub struct TestEnv {
    dir: Option<tempfile::TempDir>,
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    pub repo_dir: PathBuf,
    spr: PathBuf,
    token: String,
    /// The test repository
    repository: Repo,
}

/// The result of running a program
pub struct RunOutput {
    pub success: bool,
    pub code: Option<i32>,
    /// stdout and stderr, with the token masked
    pub stdout: String,
    pub stderr: String,
}

impl RunOutput {
    fn from_output(output: Output, token: &str) -> Self {
        RunOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: mask(&String::from_utf8_lossy(&output.stdout), token),
            stderr: mask(&String::from_utf8_lossy(&output.stderr), token),
        }
    }

    /// stdout and stderr together
    pub fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

impl TestEnv {
    /// A new environment in a fresh temporary directory, for the test
    /// repository
    pub fn new(spr: &Path, token: &str, repository: &Repo) -> Result<Self> {
        let dir = tempfile::Builder::new().prefix("spr-livetest-").tempdir()?;
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        let bin = root.join("bin");
        std::fs::create_dir_all(home.join(".config"))?;
        std::fs::create_dir_all(&bin)?;
        Ok(TestEnv {
            dir: Some(dir),
            repo_dir: root.join("repo"),
            root,
            home,
            bin,
            spr: spr.to_path_buf(),
            token: token.to_string(),
            repository: repository.clone(),
        })
    }

    /// Keep the temporary directory (e.g. to look into a failed test)
    pub fn keep(&mut self) -> &Path {
        if let Some(dir) = self.dir.take() {
            let _ = dir.keep();
        }
        &self.root
    }

    /// The test repository
    pub fn repository(&self) -> &Repo {
        &self.repository
    }

    /// The home directory of the environment
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// A directory for programs that should take precedence over the ones in
    /// `PATH` (e.g. a browser opener)
    pub fn bin_dir(&self) -> &Path {
        &self.bin
    }

    /// A command with a cleared environment: only `PATH` (with our `bin`
    /// directory first), our own home directory, and a few harmless
    /// variables. In the test repository, if it exists.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut path = OsString::from(&self.bin);
        if let Some(system_path) = std::env::var_os("PATH") {
            path.push(":");
            path.push(system_path);
        }
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("LANG", "C.UTF-8")
            // For the credential helper of the git commands (see `git`)
            .env("SPR_LIVETEST_GIT_TOKEN", &self.token)
            .stdin(Stdio::null());
        for name in ["TMPDIR", "TERM", "SSL_CERT_FILE", "NIX_SSL_CERT_FILE"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if self.repo_dir.exists() {
            command.current_dir(&self.repo_dir);
        } else {
            command.current_dir(&self.root);
        }
        command
    }

    /// A git command that can talk to GitHub: the token comes from an
    /// environment variable via a credential helper, so it's neither in the
    /// command line nor stored anywhere.
    pub fn git_command(&self, args: &[&str]) -> Command {
        let mut command = self.command("git");
        command
            .arg("-c")
            .arg("credential.helper=")
            .arg("-c")
            .arg(
                "credential.helper=!f() { test \"$1\" = get && \
                 echo username=x-access-token && \
                 echo \"password=$SPR_LIVETEST_GIT_TOKEN\"; }; f",
            )
            .args(args);
        command
    }

    /// Run git, failing if it fails. Returns its stdout, trimmed.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        let output = self.run(self.git_command(args))?;
        if !output.success {
            bail!("git {} failed:\n{}", args.join(" "), output.all());
        }
        Ok(output.stdout.trim().to_string())
    }

    /// Run spr (output captured)
    pub fn spr(&self, args: &[&str]) -> Result<RunOutput> {
        crate::log::step(format!(
            "$ {}",
            crate::log::command_line("spr", args)
        ));
        let before = self.head();
        let mut command = self.command(&self.spr);
        command.args(args);
        let output = self.run(command)?;

        // spr rewrites local commits (e.g. adding trailers), lands them,
        // or switches branches: show it
        let after = self.head();
        if let (Some(before), Some(after)) = (before, after)
            && before != after
        {
            crate::log::step(format!(
                "HEAD {} → {}",
                crate::log::short(&before),
                crate::log::short(&after)
            ));
        }
        Ok(output)
    }

    /// The HEAD commit of the test repository, if there is one
    fn head(&self) -> Option<String> {
        self.git(&["rev-parse", "--verify", "--quiet", "HEAD"]).ok()
    }

    /// Run spr, failing if it fails. Returns its output.
    pub fn spr_ok(&self, args: &[&str]) -> Result<String> {
        let output = self.spr(args)?;
        if !output.success {
            bail!("spr {} failed:\n{}", args.join(" "), output.all());
        }
        Ok(output.all())
    }

    /// Run spr, failing if it succeeds. Returns its output.
    pub fn spr_fails(&self, args: &[&str]) -> Result<String> {
        let output = self.spr(args)?;
        if output.success {
            bail!(
                "spr {} succeeded, but should have failed:\n{}",
                args.join(" "),
                output.all()
            );
        }
        crate::log::step("✓ it failed, as expected");
        Ok(output.all())
    }

    /// A command for spr, to run it differently (e.g. interactively)
    pub fn spr_command(&self) -> Command {
        self.command(&self.spr)
    }

    /// Run a command in a terminal (a pseudo-terminal, so it can ask
    /// questions) until its output contains `until`, or the timeout
    /// expires, then stop it. For interactive commands, and ones that would
    /// wait forever, e.g. for a login. Answers questions on the way: when
    /// the output (after the previous answer) contains the first text of an
    /// answer, it types the second (e.g. `"\r"` for Return).
    pub fn run_in_terminal(
        &self,
        command: Command,
        answers: &[(&str, &str)],
        until: &str,
        timeout: std::time::Duration,
    ) -> Result<RunOutput> {
        use std::io::{Read as _, Write as _};
        fn pty_error(error: impl std::fmt::Display) -> color_eyre::Report {
            eyre!("{error:#}")
        }

        let pty = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 50,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(pty_error)?;
        let mut builder =
            portable_pty::CommandBuilder::new(command.get_program());
        builder.args(command.get_args());
        // The same environment (`command` clears it)
        builder.env_clear();
        for (name, value) in command.get_envs() {
            if let Some(value) = value {
                builder.env(name, value);
            }
        }
        if command.get_envs().all(|(name, _)| name != "TERM") {
            builder.env("TERM", "xterm-256color");
        }
        if let Some(dir) = command.get_current_dir() {
            builder.cwd(dir);
        }
        let mut child = pty.slave.spawn_command(builder).map_err(pty_error)?;
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().map_err(pty_error)?;
        let mut writer = pty.master.take_writer().map_err(pty_error)?;
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = output.clone();
        std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                collected.lock().unwrap().extend_from_slice(&buffer[..n]);
            }
        });
        let text =
            || String::from_utf8_lossy(&output.lock().unwrap()).into_owned();

        let start = std::time::Instant::now();
        let mut answers = answers.iter();
        let mut next_answer = answers.next();
        let mut answered_up_to = 0;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            let seen = text();
            if let Some((question, answer)) = next_answer
                && seen[answered_up_to..].contains(question)
            {
                // Give it a moment to wait for the answer
                std::thread::sleep(std::time::Duration::from_millis(300));
                writer.write_all(answer.as_bytes())?;
                writer.flush()?;
                answered_up_to = seen.len();
                next_answer = answers.next();
            }
            if seen.contains(until) || start.elapsed() > timeout {
                // Give it a moment to finish its output
                std::thread::sleep(std::time::Duration::from_millis(500));
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        Ok(RunOutput {
            success: status.as_ref().is_some_and(|status| status.success()),
            code: status.map(|status| status.exit_code() as i32),
            stdout: mask(&text(), &self.token),
            stderr: String::new(),
        })
    }

    pub fn run(&self, mut command: Command) -> Result<RunOutput> {
        let output = command.output().wrap_err_with(|| {
            format!("Running {:?} failed", command.get_program())
        })?;
        Ok(RunOutput::from_output(output, &self.token))
    }

    /// Clone the test repository into the environment, with a plain HTTPS
    /// URL as `origin` and nothing else configured but a commit identity
    pub fn clone(&self) -> Result<()> {
        let url = format!("{}.git", self.repository.web_url());
        let target = self.repo_dir.to_string_lossy().into_owned();
        self.git(&["clone", "--quiet", &url, &target])?;
        self.git(&["config", "user.name", "spr livetest"])?;
        self.git(&["config", "user.email", "livetest@example.com"])?;
        Ok(())
    }

    /// Set spr settings in the test repository
    pub fn configure(&self, settings: &[(&str, &str)]) -> Result<()> {
        for (key, value) in settings {
            self.git(&["config", &format!("spr.{key}"), value])
                .wrap_err_with(|| format!("Setting spr.{key} failed"))?;
        }
        Ok(())
    }

    /// Set spr's token for the host (`spr.auth.github[.HOST].token`) in the
    /// test repository to the token of the tests
    pub fn set_token_in_config(&self) -> Result<()> {
        self.set_token(&self.token)
    }

    /// Set spr's token for the host in the test repository
    pub fn set_token(&self, token: &str) -> Result<()> {
        self.unset_token()?;
        self.append_to_config(&format!(
            "[spr \"{}\"]\n\ttoken = \"{token}\"",
            self.repository.token_subsection()
        ))
    }

    /// Set the tests' token as `spr.githubAuthToken`, the option that spr
    /// read before it supported other hosts than github.com
    pub fn set_old_token_in_config(&self) -> Result<()> {
        self.unset_token()?;
        self.append_to_config(&format!(
            "[spr]\n\tgithubAuthToken = \"{}\"",
            self.token
        ))
    }

    /// Append to the test repository's Git configuration. Written to the
    /// file directly, so tokens aren't on the command line of a process
    /// (where others could see them).
    fn append_to_config(&self, text: &str) -> Result<()> {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.repo_dir.join(".git/config"))?;
        writeln!(file, "{text}")?;
        Ok(())
    }

    /// Remove spr's tokens from the test repository (also the old option)
    pub fn unset_token(&self) -> Result<()> {
        for key in [self.repository.token_key().as_str(), "spr.githubAuthToken"]
        {
            // Exit code 5: it wasn't set
            let output = self.run({
                let mut command = self.command("git");
                command.args(["config", "--unset-all", key]);
                command
            })?;
            if !output.success && output.code != Some(5) {
                bail!(
                    "Removing the token from the config failed: {}",
                    output.all()
                );
            }
        }
        Ok(())
    }

    /// The token configured for the host in the test repository (e.g. by
    /// `spr init`). Not to be printed!
    pub fn configured_token(&self) -> Result<Option<String>> {
        let output = self
            .command("git")
            .args(["config", "--get", &self.repository.token_key()])
            .output()?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|token| !token.is_empty()))
    }

    /// Fail if spr would see any `spr.*` setting from outside the test
    /// repository (e.g. from a global or system Git configuration)
    pub fn check_pristine(&self) -> Result<()> {
        let output = self.run({
            let mut command = self.command("git");
            command.args([
                "config",
                "--show-origin",
                "--get-regexp",
                "^spr\\.",
            ]);
            command
        })?;
        let outside: Vec<&str> = output
            .stdout
            .lines()
            .filter(|line| !line.starts_with("file:.git/config"))
            .collect();
        if !outside.is_empty() {
            return Err(eyre!(
                "spr settings from outside the test repository would \
                 influence the test (e.g. in a global or system Git \
                 configuration):\n  {}",
                outside.join("\n  ")
            ));
        }
        Ok(())
    }
}
