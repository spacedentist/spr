//! A controlled environment for running spr: a temporary directory with its
//! own home directory (so no global Git configuration applies), a fresh
//! clone of the test repository, and a cleared process environment.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use color_eyre::eyre::{Result, WrapErr, bail, eyre};

use crate::util::mask;

pub struct TestEnv {
    dir: Option<tempfile::TempDir>,
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    pub repo_dir: PathBuf,
    spr: PathBuf,
    token: String,
}

/// The result of running a program
pub struct RunOutput {
    pub success: bool,
    /// stdout and stderr, with the token masked
    pub stdout: String,
    pub stderr: String,
}

impl RunOutput {
    fn from_output(output: Output, token: &str) -> Self {
        RunOutput {
            success: output.status.success(),
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
    /// A new environment in a fresh temporary directory
    pub fn new(spr: &Path, token: &str) -> Result<Self> {
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
        })
    }

    /// Keep the temporary directory (e.g. to look into a failed test)
    pub fn keep(&mut self) -> &Path {
        if let Some(dir) = self.dir.take() {
            let _ = dir.keep();
        }
        &self.root
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
        let mut command = self.command(&self.spr);
        command.args(args);
        self.run(command)
    }

    /// Run spr, failing if it fails. Returns its output.
    pub fn spr_ok(&self, args: &[&str]) -> Result<String> {
        let output = self.spr(args)?;
        if !output.success {
            bail!("spr {} failed:\n{}", args.join(" "), output.all());
        }
        Ok(output.all())
    }

    pub fn run(&self, mut command: Command) -> Result<RunOutput> {
        let output = command.output().wrap_err_with(|| {
            format!("Running {:?} failed", command.get_program())
        })?;
        Ok(RunOutput::from_output(output, &self.token))
    }

    /// Clone the repository (`OWNER/REPO`) into the environment, with a
    /// plain HTTPS URL as `origin` and nothing else configured but a commit
    /// identity
    pub fn clone(&self, repo: &str) -> Result<()> {
        let url = format!("https://github.com/{repo}.git");
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

    /// Set `spr.githubAuthToken` in the test repository. Written to the
    /// config file directly, so the token isn't on the command line of a
    /// process (where others could see it).
    pub fn set_token_in_config(&self) -> Result<()> {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.repo_dir.join(".git/config"))?;
        writeln!(file, "[spr]\n\tgithubAuthToken = \"{}\"", self.token)?;
        Ok(())
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
