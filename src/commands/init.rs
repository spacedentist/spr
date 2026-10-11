use color_eyre::eyre::{Result, WrapErr, bail};
use indoc::formatdoc;
use lazy_regex::regex;
use octocrab::FromResponse;
use secrecy::ExposeSecret as _;

use crate::config::{AuthTokens, OLD_TOKEN_OPTION, auth_token_key};
use crate::github::{ApiUrls, api_client, token_instructions};
use crate::output::output;
use crate::target::{ForgeKind, Repository, Target};

pub async fn init() -> Result<()> {
    output("👋", "Welcome to spr!")?;

    let path = std::env::current_dir()?;
    let repo = git2::Repository::discover(path.clone()).wrap_err(formatdoc!(
        "Could not open a Git repository in {:?}. Please run 'spr' from within \
         a Git repository.",
        path
    ))?;
    let mut config = repo.config()?;

    // The repository comes first, as it decides where to authenticate

    console::Term::stdout().write_line("")?;

    output(
        "❓",
        &formatdoc!(
            "What's the GitHub repository? Please enter 'OWNER/REPOSITORY' \
             for one on github.com (basically the bit that follows \
             'github.com/' in the address), or \
             'github://HOST/OWNER/REPOSITORY' for one on GitHub Enterprise."
        ),
    )?;

    // The configured target: spr.target, or the options it replaces
    let configured_target = crate::config::configured_target(&config)
        .ok()
        .flatten()
        .filter(|target| target.repository().forge() == ForgeKind::GitHub);
    let suggestion = configured_target
        .as_ref()
        .map(|target| target.repository().clone())
        .or_else(|| {
            // A suggestion from where the remote "origin" points to
            repo.find_remote("origin")
                .ok()
                .and_then(|remote| remote.url().ok().map(String::from))
                .and_then(|url| repository_from_url(&url))
        })
        .map(|repository| repository_input(&repository))
        .unwrap_or_default();

    let input = dialoguer::Input::<String>::new()
        .with_prompt("GitHub repository")
        .with_initial_text(suggestion)
        .validate_with(|input: &String| -> Result<()> {
            parse_repository(input).map(|_| ())
        })
        .interact_text()?;
    let repository = parse_repository(&input)?;
    let urls = ApiUrls::for_host(repository.host());

    // The token for the repository's host: keep a configured one if it
    // works and has the scopes spr needs, otherwise get one

    let token_key = auth_token_key(&repository);
    let configured_token = AuthTokens::from_git_config(&config)?
        .get(&repository)
        .map(String::from);

    let keep_token = match configured_token.as_deref() {
        None => false,
        Some(token) => check_token(&urls, token, "configured").await?,
    };

    let token = match configured_token {
        Some(token) if keep_token => token,
        _ => {
            console::Term::stdout().write_line("")?;
            // Logging in needs spr's OAuth app, which is on github.com only.
            // There, the user may still prefer to give a token (e.g. if
            // their organisation doesn't allow spr's app). Without a
            // terminal to ask, log in.
            let log_in = repository.is_on_public_instance()
                && (!console::Term::stdout().is_term()
                    || dialoguer::Select::new()
                        .with_prompt("How should spr get a token for GitHub?")
                        .items([
                            "Log in with GitHub in the web browser",
                            "Paste a token (e.g. from the GitHub CLI)",
                        ])
                        .default(0)
                        .interact()?
                        == 0);
            if log_in {
                log_in_with_github().await?
            } else {
                ask_for_token(&repository, &urls).await?
            }
        }
    };

    // Store the token under its key, unless it's there already (it may have
    // come from spr.githubAuthToken, or be new)
    if config.get_string(&token_key).ok().as_deref() != Some(token.as_str()) {
        config.set_str(&token_key, &token)?;
    }
    // spr.auth.github.token replaces spr.githubAuthToken: remove that from
    // the repository's configuration (as the target options below)
    if repository.is_on_public_instance() {
        let mut local_config = config.open_level(git2::ConfigLevel::Local)?;
        match local_config.remove(OLD_TOKEN_OPTION) {
            Ok(()) => output(
                "🧹",
                &format!("Replaced {OLD_TOKEN_OPTION} with {token_key}"),
            )?,
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    let octocrab = api_client(&urls.rest, &token)?;
    let github_user = octocrab.current().user().await?;

    output("👋", &formatdoc!("Hello {}!", github_user.login))?;

    // Target branch: the configured one if the repository stays the same,
    // otherwise the repository's default branch on GitHub

    let github_repo_info = octocrab
        .get::<octocrab::models::Repository, _, _>(
            format!("/repos/{}", repository.path()),
            None::<&()>,
        )
        .await
        .wrap_err_with(|| format!("Getting the repository {repository}"))?;
    let default_branch = github_repo_info
        .default_branch
        .unwrap_or_else(|| "master".to_string());

    console::Term::stdout().write_line("")?;

    output(
        "❓",
        &formatdoc!(
            "Which branch should Pull Requests be merged into? (The \
             repository's default branch is '{default_branch}'.)"
        ),
    )?;

    let branch = configured_target
        .filter(|target| target.repository() == &repository)
        .map(|target| target.branch().to_string())
        .unwrap_or(default_branch);
    let branch = dialoguer::Input::<String>::new()
        .with_prompt("Target branch")
        .with_initial_text(branch)
        .validate_with(|input: &String| -> Result<()> {
            Target::new(repository.clone(), input).map(|_| ())
        })
        .interact_text()?;
    let target = Target::new(repository, &branch)?;
    config.set_str("spr.target", &target.to_string())?;

    // spr.target replaces the old options: remove them from the
    // repository's configuration. (Not from the global one, where other
    // repositories may still rely on them; spr's deprecation note tells how
    // to replace them there.)
    let mut local_config = config.open_level(git2::ConfigLevel::Local)?;
    let mut removed = Vec::new();
    for key in crate::config::OLD_TARGET_OPTIONS {
        match local_config.remove(key) {
            Ok(()) => removed.push(key),
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !removed.is_empty() {
        output(
            "🧹",
            &format!(
                "Replaced {} with spr.target = {target}",
                removed.join(" and ")
            ),
        )?;
    }
    if let Some(note) = crate::config::deprecation_note(&config) {
        output("⚠️", &note)?;
    }

    // Pull Request branch prefix

    console::Term::stdout().write_line("")?;

    let branch_prefix = config
        .get_string("spr.branchPrefix")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("spr/{}/", github_user.login));

    output(
        "❓",
        &formatdoc!(
            "What prefix should be used when naming Pull Request branches?
             Good practice is to begin with 'spr/' as a general namespace \
             for spr-managed Pull Request branches. Continuing with the \
             GitHub user name is a good idea, so there is no danger of names \
             clashing with those of other users.
             The prefix should end with a good separator character (like '/' \
             or '-'), since commit titles will be appended to this prefix."
        ),
    )?;

    let branch_prefix = dialoguer::Input::<String>::new()
        .with_prompt("Branch prefix")
        .with_initial_text(branch_prefix)
        .validate_with(|input: &String| -> Result<()> {
            validate_branch_prefix(input)
        })
        .interact_text()?;

    config.set_str("spr.branchPrefix", &branch_prefix)?;

    Ok(())
}

/// Log in with GitHub (github.com) through spr's OAuth app (device flow).
/// Returns the new token.
async fn log_in_with_github() -> Result<String> {
    let client_id = "Ov23liD6WOMYlLy12wkg";

    let client = octocrab::OctocrabBuilder::new()
        .base_uri("https://github.com")?
        .add_header(
            http::HeaderName::from_static("accept"),
            "application/json".into(),
        )
        .build()?;

    let device_codes = client
        .authenticate_as_device(&client_id.into(), [REQUIRED_SCOPES.join(" ")])
        .await?;

    output(
        "🔑",
        &formatdoc!(
            "
        Okay, let's get started.

        To authenticate spr with GitHub, please go to

        -----> {} <-----

        and enter code

        > > > > > {} < < < < <",
            &device_codes.verification_uri,
            &device_codes.user_code,
        ),
    )?;

    // For convenience, open the link in the web browser. That's not possible
    // everywhere (e.g. on a server via SSH), and not needed: the link and
    // code above are all the user needs.
    if open::that_detached(&device_codes.verification_uri).is_ok() {
        output("🌐", "The link should open in your web browser now.")?;
    }

    let auth = device_codes
        .poll_until_available(&client, &client_id.into())
        .await?;
    Ok(auth.access_token.expose_secret().into())
}

/// Ask the user for a token for GitHub on the repository's host, until
/// they give one that works
async fn ask_for_token(
    repository: &Repository,
    urls: &ApiUrls,
) -> Result<String> {
    output(
        "🔑",
        &format!(
            "spr needs a token for {}. {}\n\
             Or simply paste the token here, and spr init configures it for \
             this repository.",
            repository.host(),
            token_instructions(repository),
        ),
    )?;
    loop {
        let token =
            dialoguer::Password::new().with_prompt("Token").interact()?;
        let token = token.trim();
        if !token.is_empty() && check_token(urls, token, "given").await? {
            return Ok(token.to_string());
        }
    }
}

/// The repository as the user enters it: `OWNER/REPO` on github.com, or in
/// the full notation (e.g. `github://ghe.example.com/OWNER/REPO`)
fn parse_repository(input: &str) -> Result<Repository> {
    let input = input.trim();
    let repository = if input.contains(':') {
        input.parse()?
    } else {
        Repository::new(ForgeKind::GitHub, None, input)?
    };
    if repository.forge() != ForgeKind::GitHub {
        bail!("spr supports repositories on GitHub only (so far)");
    }
    Ok(repository)
}

/// A repository as the user would enter it (see `parse_repository`)
fn repository_input(repository: &Repository) -> String {
    if repository.is_on_public_instance() {
        repository.path().to_string()
    } else {
        repository.to_string()
    }
}

/// The GitHub repository that a Git remote's URL points to, e.g.
/// `https://github.com/OWNER/REPO.git`, `git@github.com:OWNER/REPO.git` or
/// `ssh://git@ghe.example.com/OWNER/REPO.git`. On another host than
/// github.com, it's a guess that it's GitHub Enterprise there.
fn repository_from_url(url: &str) -> Option<Repository> {
    let (_, https_host, ssh_host, scp_host, path) = lazy_regex::regex_captures!(
        r#"^(?:https?://(?:[^@/]+@)?([^/]+)/|ssh://(?:[^@/]+@)?([^/:]+)(?::\d+)?/|(?:[^@/:]+@)?([^/:]+):)([\w\-\.]+/[\w\-\.]+?)(?:\.git)?/?$"#,
        url
    )?;
    let host = [https_host, ssh_host, scp_host]
        .into_iter()
        .find(|host| !host.is_empty())?
        .to_ascii_lowercase();
    // GitHub's SSH over the HTTPS port
    let host = match host.as_str() {
        "ssh.github.com" => "github.com".to_string(),
        _ => host,
    };
    Repository::new(ForgeKind::GitHub, Some(&host), path).ok()
}

fn validate_branch_prefix(branch_prefix: &str) -> Result<()> {
    // They can include slash / for hierarchical (directory) grouping, but no slash-separated component can begin with a dot . or end with the sequence .lock.
    if branch_prefix.contains("/.")
        || branch_prefix.contains(".lock/")
        || branch_prefix.ends_with(".lock")
        || branch_prefix.starts_with('.')
    {
        bail!(
            "Branch prefix cannot have slash-separated component beginning with a dot . or ending with the sequence .lock",
        );
    }

    if branch_prefix.contains("..") {
        bail!("Branch prefix cannot contain two consecutive dots anywhere.",);
    }

    if branch_prefix.chars().any(|c| c.is_ascii_control()) {
        bail!("Branch prefix cannot contain ASCII control sequence",);
    }

    let forbidden_chars_re = regex!(r"[ \~\^:?*\[\\]");
    if forbidden_chars_re.is_match(branch_prefix) {
        bail!("Branch prefix contains one or more forbidden characters.",);
    }

    if branch_prefix.contains("//") || branch_prefix.starts_with('/') {
        bail!(
            "Branch prefix contains multiple consecutive slashes or starts with slash.",
        );
    }

    if branch_prefix.contains("@{") {
        bail!("Branch prefix cannot contain the sequence @{{");
    }

    Ok(())
}

/// The OAuth scopes spr needs: `repo` for pushing branches and working with
/// Pull Requests, `read:org` for looking up teams named as reviewers, and
/// `workflow` for pushing Pull Request branches that change GitHub Actions
/// workflows (GitHub refuses those pushes otherwise).
const REQUIRED_SCOPES: [&str; 3] = ["repo", "read:org", "workflow"];

/// The required scopes that the given scopes of a token don't include
fn missing_scopes(scopes: &[String]) -> Vec<&'static str> {
    let has = |scope: &str| scopes.iter().any(|s| s == scope);
    REQUIRED_SCOPES
        .into_iter()
        .filter(|&required| match required {
            // Broader scopes include `read:org`
            "read:org" => {
                !(has("read:org") || has("write:org") || has("admin:org"))
            }
            _ => !has(required),
        })
        .collect()
}

/// Check whether to use a token (`which` one: e.g. "configured"). Explains
/// why not, if it shouldn't be used.
async fn check_token(urls: &ApiUrls, token: &str, which: &str) -> Result<bool> {
    let response: octocrab::Result<AuthScopes> = api_client(&urls.rest, token)?
        .get("/user", None::<&()>)
        .await;

    match response {
        Ok(AuthScopes { scopes: None }) => {
            // Fine-grained personal access tokens have permissions instead
            // of scopes, which GitHub doesn't tell us about
            output(
                "🔑",
                &format!(
                    "Using the {which} GitHub token. It has no OAuth scopes \
                     (e.g. a fine-grained personal access token), so spr \
                     can't check its permissions."
                ),
            )?;
            Ok(true)
        }
        Ok(AuthScopes {
            scopes: Some(scopes),
        }) => {
            let missing = missing_scopes(&scopes);
            if missing.is_empty() {
                return Ok(true);
            }
            output(
                "⚠️",
                &format!(
                    "The {which} GitHub token is missing scopes that spr \
                     needs ({}), so let's get a new one.",
                    missing.join(", ")
                ),
            )?;
            Ok(false)
        }
        Err(error) => {
            // For errors from GitHub, its message (e.g. "Bad credentials")
            let reason = match &error {
                octocrab::Error::GitHub { source, .. } => {
                    source.message.clone()
                }
                error => error.to_string(),
            };
            output(
                "⚠️",
                &format!(
                    "The {which} GitHub token doesn't work ({reason}), so \
                     let's get a new one."
                ),
            )?;
            Ok(false)
        }
    }
}

#[derive(Debug)]
struct AuthScopes {
    /// The token's OAuth scopes, or `None` if it has none (e.g. a
    /// fine-grained personal access token)
    scopes: Option<Vec<String>>,
}

impl FromResponse for AuthScopes {
    async fn from_response<B>(
        response: http::Response<B>,
    ) -> octocrab::Result<Self>
    where
        B: http_body::Body<Data = bytes::Bytes, Error = octocrab::Error> + Send,
    {
        let scopes = response
            .headers()
            .get("x-oauth-scopes")
            .map(|v| v.to_str())
            .transpose()
            .map_err(|err| octocrab::Error::Other {
                source: Box::new(err),
                backtrace: std::backtrace::Backtrace::capture(),
            })?
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>()
            });
        Ok(AuthScopes { scopes })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        missing_scopes, parse_repository, repository_from_url,
        repository_input, validate_branch_prefix,
    };

    #[test]
    fn test_repository_from_url() {
        let repository = |url: &str| {
            repository_from_url(url).map(|repository| repository.to_string())
        };
        for url in [
            "https://github.com/acme/codez.git",
            "https://github.com/acme/codez",
            "https://github.com/acme/codez/",
            "https://user@GitHub.com/acme/codez.git",
            "git@github.com:acme/codez.git",
            "github.com:acme/codez",
            "ssh://git@github.com/acme/codez.git",
            "ssh://git@ssh.github.com:443/acme/codez.git",
        ] {
            assert_eq!(repository(url).as_deref(), Some("github:acme/codez"));
        }
        assert_eq!(
            repository("https://ghe.example.com:8443/acme/my.repo.git")
                .as_deref(),
            Some("github://ghe.example.com:8443/acme/my.repo")
        );
        // The SSH port isn't the one for HTTPS
        assert_eq!(
            repository("ssh://git@ghe.example.com:2222/acme/codez.git")
                .as_deref(),
            Some("github://ghe.example.com/acme/codez")
        );
        assert_eq!(
            repository("git@acme.ghe.com:acme/codez.git").as_deref(),
            Some("github://acme.ghe.com/acme/codez")
        );
        for url in [
            "/home/user/codez",
            "../codez",
            "https://gitlab.example.com/group/sub/project.git",
        ] {
            assert_eq!(repository(url), None, "{url}");
        }
    }

    #[test]
    fn test_parse_repository() {
        let repository = parse_repository(" acme/codez ").unwrap();
        assert_eq!(repository.to_string(), "github:acme/codez");
        assert_eq!(repository_input(&repository), "acme/codez");

        let repository =
            parse_repository("github://GHE.example.com/acme/codez").unwrap();
        assert_eq!(
            repository.to_string(),
            "github://ghe.example.com/acme/codez"
        );
        assert_eq!(
            repository_input(&repository),
            "github://ghe.example.com/acme/codez"
        );

        assert!(parse_repository("gitlab:group/project").is_err());
        assert!(parse_repository("acme").is_err());
        assert!(parse_repository("ghe.example.com/acme/codez").is_err());
    }

    fn scopes(scopes: &[&str]) -> Vec<String> {
        scopes.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_missing_scopes() {
        // What `gh auth token` gives: enough, `user` isn't needed
        assert!(
            missing_scopes(&scopes(&["gist", "read:org", "repo", "workflow"]))
                .is_empty()
        );
        // Broader org scopes include read:org
        assert!(
            missing_scopes(&scopes(&["repo", "admin:org", "workflow"]))
                .is_empty()
        );
        assert_eq!(
            missing_scopes(&scopes(&["repo", "user"])),
            vec!["read:org", "workflow"]
        );
        // A classic token without any scopes
        assert_eq!(missing_scopes(&[]), vec!["repo", "read:org", "workflow"]);
    }

    #[test]
    fn test_branch_prefix_rules() {
        // Rules taken from https://git-scm.com/docs/git-check-ref-format
        // Note: Some rules don't need to be checked because the prefix is
        // always embedded into a larger context. For example, rule 9 in the
        // reference states that a _refname_ cannot be the single character @.
        // This rule is impossible to break purely via the branch prefix.
        let bad_prefixes: Vec<(&str, &str)> = vec![
            (
                "spr/.bad",
                "Cannot start slash-separated component with dot",
            ),
            (".bad", "Cannot start slash-separated component with dot"),
            ("spr/bad.lock", "Cannot end with .lock"),
            (
                "spr/bad.lock/some_more",
                "Cannot end slash-separated component with .lock",
            ),
            (
                "spr/b..ad/bla",
                "They cannot contain two consecutive dots anywhere",
            ),
            ("spr/bad//bla", "They cannot contain consecutive slashes"),
            ("/bad", "Prefix should not start with slash"),
            ("/bad@{stuff", "Prefix cannot contain sequence @{"),
        ];

        for (branch_prefix, reason) in bad_prefixes {
            assert!(
                validate_branch_prefix(branch_prefix).is_err(),
                "{}",
                reason
            );
        }

        let ok_prefix = "spr/some.lockprefix/with-stuff/foo";
        assert!(validate_branch_prefix(ok_prefix).is_ok());
    }

    #[test]
    fn test_branch_prefix_rejects_forbidden_characters() {
        // Here I'm mostly concerned about escaping / not escaping in the regex :p
        assert!(validate_branch_prefix("bad\x1F").is_err());
        assert!(validate_branch_prefix("notbad!").is_ok());
        assert!(
            validate_branch_prefix("bad /space").is_err(),
            "Reject space in prefix"
        );
        assert!(validate_branch_prefix("bad~").is_err(), "Reject tilde");
        assert!(validate_branch_prefix("bad^").is_err(), "Reject caret");
        assert!(validate_branch_prefix("bad:").is_err(), "Reject colon");
        assert!(validate_branch_prefix("bad?").is_err(), "Reject ?");
        assert!(validate_branch_prefix("bad*").is_err(), "Reject *");
        assert!(validate_branch_prefix("bad[").is_err(), "Reject [");
        assert!(validate_branch_prefix(r"bad\").is_err(), "Reject \\");
    }
}
