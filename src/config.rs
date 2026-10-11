use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, WrapErr as _, bail};

use crate::forge::Branch;
use crate::message::{CommitMessage, SPR_TARGET_TRAILER};
use crate::target::{ForgeKind, Repository, Target};

#[derive(Clone)]
pub struct Config {
    /// Where the Pull Requests of the commits spr works on get merged: the
    /// target of the HEAD commit (see `commit_target`)
    pub target: Target,
    /// The target of commits without a `Spr-Target` trailer: given with
    /// `--target`, or else the configured default target
    pub default_target: Target,
    /// The default target in the Git config (`spr.target`). Commits for
    /// other targets get a `Spr-Target` trailer, and a branch name alone in
    /// one means a branch in this target's repository.
    pub configured_target: Target,
    /// The owner and name of the target repository on GitHub (as long as
    /// spr only supports GitHub forges)
    pub owner: String,
    pub repo: String,
    pub master_ref: Branch,
    pub branch_prefix: String,
    pub auth_tokens: AuthTokens,
    pub require_approval: bool,
    pub use_commit_title_for_initial_commit: bool,
    pub merge_method: MergeMethod,
    pub stacking_mode: StackingMode,
    pub spr_ids: bool,
}

// Not derived, so the auth token doesn't end up in debug output (e.g. logs).
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("target", &self.target.to_string())
            .field("default_target", &self.default_target.to_string())
            .field("configured_target", &self.configured_target.to_string())
            .field("branch_prefix", &self.branch_prefix)
            .field("auth_tokens", &self.auth_tokens)
            .field("require_approval", &self.require_approval)
            .field("merge_method", &self.merge_method)
            .field("stacking_mode", &self.stacking_mode)
            .field(
                "use_commit_title_for_initial_commit",
                &self.use_commit_title_for_initial_commit,
            )
            .field("spr_ids", &self.spr_ids)
            .finish()
    }
}

/// How Pull Requests get merged into the master branch in this repository.
///
/// This is used by `spr land`, but also tells spr what the repository's
/// convention is, as Pull Requests may also be merged in the GitHub UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMethod {
    /// Squash all commits of the Pull Request into one commit on master
    Squash,
    /// Merge the Pull Request branch into master with a merge commit
    Merge,
}

impl std::str::FromStr for MergeMethod {
    type Err = color_eyre::eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "squash" => Ok(MergeMethod::Squash),
            "merge" => Ok(MergeMethod::Merge),
            _ => bail!(
                "Merge method must be either 'squash' or 'merge', but given \
                 value was '{s}'"
            ),
        }
    }
}

/// How spr sets up Pull Requests for commits that are not directly based on
/// the master branch, i.e. that are stacked on other commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackingMode {
    /// The Pull Request targets a synthetic base branch created by spr, which
    /// reflects the changes the commit is based on. This keeps the Pull
    /// Request's timeline readable after squash-merging, but does not work
    /// with merge commits.
    BaseBranches,
    /// The Pull Request targets the Pull Request branch of the parent commit,
    /// so Pull Requests form a chain.
    Chain,
    /// Like `Chain`, and the Pull Requests of a chain are also linked as a
    /// stack on GitHub (stacked pull requests feature).
    GitHubStack,
}

impl StackingMode {
    /// Whether Pull Requests of stacked commits are chained, i.e. target the
    /// Pull Request branch of the parent commit.
    pub fn is_chained(self) -> bool {
        matches!(self, StackingMode::Chain | StackingMode::GitHubStack)
    }

    /// The stacking mode used if none is configured
    pub fn default_for(merge_method: MergeMethod) -> Self {
        match merge_method {
            MergeMethod::Squash => StackingMode::BaseBranches,
            MergeMethod::Merge => StackingMode::Chain,
        }
    }
}

impl std::str::FromStr for StackingMode {
    type Err = color_eyre::eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "base-branches" => Ok(StackingMode::BaseBranches),
            "chain" => Ok(StackingMode::Chain),
            "github-stack" => Ok(StackingMode::GitHubStack),
            _ => bail!(
                "Stacking mode must be one of 'base-branches', 'chain' and \
                 'github-stack', but given value was '{s}'"
            ),
        }
    }
}

impl std::fmt::Display for StackingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StackingMode::BaseBranches => "base-branches",
            StackingMode::Chain => "chain",
            StackingMode::GitHubStack => "github-stack",
        })
    }
}

impl std::fmt::Display for MergeMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Merge => "merge",
        })
    }
}

impl Config {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: Target,
        branch_prefix: String,
        auth_tokens: AuthTokens,
        require_approval: bool,
        merge_method: MergeMethod,
        stacking_mode: StackingMode,
        use_commit_title_for_initial_commit: bool,
        spr_ids: bool,
    ) -> Result<Self> {
        if merge_method == MergeMethod::Merge
            && stacking_mode == StackingMode::BaseBranches
        {
            // Merging Pull Requests that use base branches with a merge commit
            // would bring the commits on the base branches, which spr
            // constructed, into the history of the master branch.
            bail!(
                "Stacking mode '{stacking_mode}' cannot be used with merge \
                 method '{merge_method}'"
            );
        }

        let (owner, repo) = github_owner_and_repo(&target)?;
        let master_ref =
            Branch::new_from_branch_name(target.branch(), target.branch());
        Ok(Self {
            default_target: target.clone(),
            configured_target: target.clone(),
            target,
            owner,
            repo,
            master_ref,
            branch_prefix,
            auth_tokens,
            require_approval,
            merge_method,
            stacking_mode,
            use_commit_title_for_initial_commit,
            spr_ids,
        })
    }

    /// This configuration for another target (of the commits spr works on)
    pub fn for_target(&self, target: Target) -> Result<Self> {
        let (owner, repo) = github_owner_and_repo(&target)?;
        Ok(Self {
            master_ref: Branch::new_from_branch_name(
                target.branch(),
                target.branch(),
            ),
            target,
            owner,
            repo,
            ..self.clone()
        })
    }

    /// The target of a local commit: the one in its `Spr-Target` trailer,
    /// or the default target. Also returns whether the commit has the
    /// trailer.
    pub fn commit_target(
        &self,
        message: &CommitMessage,
    ) -> Result<(Target, bool)> {
        match message.get_trailer(SPR_TARGET_TRAILER) {
            Some(text) if !text.trim().is_empty() => {
                let target = Target::parse_with_default(
                    text.trim(),
                    Some(&self.configured_target),
                )
                .wrap_err_with(|| {
                    format!("Invalid {SPR_TARGET_TRAILER} trailer: {text}")
                })?;
                Ok((target, true))
            }
            _ => Ok((self.default_target.clone(), false)),
        }
    }

    pub fn pull_request_url(&self, number: u64) -> String {
        pull_request_url(self.target.repository(), number)
    }

    pub fn parse_pull_request_field(&self, text: &str) -> Option<u64> {
        parse_pull_request_field(self.target.repository(), text)
    }

    pub fn new_branch_from_ref(&self, name: &str) -> Result<Branch> {
        Branch::new_from_ref(name, self.master_ref.branch_name())
    }

    /// Whether the given branch is a base branch that spr created for a Pull
    /// Request, i.e. a synthetic branch reflecting the changes the Pull
    /// Request's commit is based on.
    ///
    /// Base branch names are the branch prefix, followed by the master branch
    /// name, a dot, and a slug of the commit title. Pull Request branch names
    /// never contain a dot after the prefix, as slugs don't contain dots.
    pub fn is_spr_base_branch(&self, branch: &Branch) -> bool {
        branch
            .branch_name()
            .strip_prefix(&self.branch_prefix)
            .and_then(|rest| rest.strip_prefix(self.master_ref.branch_name()))
            .is_some_and(|rest| rest.starts_with('.'))
    }

    pub fn new_branch(&self, branch_name: &str) -> Branch {
        Branch::new_from_branch_name(branch_name, self.master_ref.branch_name())
    }
}

/// The owner and name of a target's repository, as long as spr only
/// supports GitHub (github.com, and GitHub Enterprise on other hosts)
fn github_owner_and_repo(target: &Target) -> Result<(String, String)> {
    let repository = target.repository();
    if repository.forge() != ForgeKind::GitHub {
        bail!(
            "Target {target}: spr supports repositories on GitHub only (so \
             far)"
        );
    }
    let (owner, repo) = repository
        .path()
        .split_once('/')
        .expect("GitHub repository paths are OWNER/REPO");
    Ok((owner.to_string(), repo.to_string()))
}

/// The tokens spr authenticates with on forges, from the Git configuration:
/// `spr.auth.<forge>.token` for a forge's public instance (e.g.
/// `spr.auth.github.token` for github.com), `spr.auth.<forge>.<host>.token`
/// for any other host (e.g. `spr.auth.github.ghe.example.com.token`).
#[derive(Clone, Default)]
pub struct AuthTokens {
    /// The tokens by forge and host, as in their key (e.g. `github`,
    /// `github.ghe.example.com`), in lower case
    tokens: BTreeMap<String, String>,
}

// Not derived, so the tokens don't end up in debug output (e.g. logs).
impl std::fmt::Debug for AuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.tokens.keys().map(|key| (key, "[redacted]")))
            .finish()
    }
}

impl AuthTokens {
    /// The tokens configured in the Git configuration. For setups from
    /// before spr supported other hosts, `spr.githubAuthToken` is the token
    /// for github.com, unless `spr.auth.github.token` is set.
    pub fn from_git_config(git_config: &git2::Config) -> Result<Self> {
        // Git keeps the case of subsections (the part between the first and
        // the last dot), but host names are case-insensitive. So look at
        // all spr settings, and read each token key found with its own
        // case, which gives the value with the highest priority.
        let mut names = BTreeSet::new();
        let mut entries = git_config.entries(Some("^spr\\."))?;
        while let Some(entry) = entries.next() {
            if let Ok(name) = entry?.name() {
                names.insert(name.to_string());
            }
        }

        let mut tokens = BTreeMap::new();
        for name in names {
            let lower = name.to_ascii_lowercase();
            let Some(forge_and_host) = lower
                .strip_prefix("spr.auth.")
                .and_then(|rest| rest.strip_suffix(".token"))
            else {
                continue;
            };
            if let Ok(token) = git_config.get_string(&name)
                && !token.is_empty()
            {
                tokens.entry(forge_and_host.to_string()).or_insert(token);
            }
        }

        if let Ok(token) = git_config.get_string(OLD_TOKEN_OPTION)
            && !token.is_empty()
        {
            tokens
                .entry(ForgeKind::GitHub.name().to_string())
                .or_insert(token);
        }

        Ok(Self { tokens })
    }

    /// The token for the forge and host of the repository
    pub fn get(&self, repository: &Repository) -> Option<&str> {
        self.tokens
            .get(&auth_forge_and_host(repository))
            .map(String::as_str)
    }

    /// Use the token for the forge and host of the repository (e.g. given
    /// on the command line)
    pub fn set(&mut self, repository: &Repository, token: String) {
        self.tokens.insert(auth_forge_and_host(repository), token);
    }
}

/// The forge and host of a repository, as in the Git configuration key of
/// its token: the forge alone for its public instance
fn auth_forge_and_host(repository: &Repository) -> String {
    let forge = repository.forge().name();
    if repository.is_on_public_instance() {
        forge.to_string()
    } else {
        format!("{forge}.{}", repository.host())
    }
}

/// The Git configuration key of the token for the forge and host of a
/// repository, e.g. `spr.auth.github.token` (github.com) or
/// `spr.auth.github.ghe.example.com.token`
pub fn auth_token_key(repository: &Repository) -> String {
    format!("spr.auth.{}.token", auth_forge_and_host(repository))
}

/// The option that `spr.auth.github.token` replaces
pub const OLD_TOKEN_OPTION: &str = "spr.githubAuthToken";

/// The URL of a Pull Request in the given repository
pub fn pull_request_url(repository: &Repository, number: u64) -> String {
    format!("{}/pull/{number}", repository.web_url())
}

/// The number of a Pull Request in the given repository, from the value of
/// a `Pull-request` trailer: the Pull Request's URL, or its number
/// (`#12`). `None` if it's neither, or the URL is of another repository.
pub fn parse_pull_request_field(
    repository: &Repository,
    text: &str,
) -> Option<u64> {
    if text.is_empty() {
        return None;
    }

    let regex = lazy_regex::regex!(r#"^\s*#?\s*(\d+)\s*$"#);
    let m = regex.captures(text);
    if let Some(caps) = m {
        return caps.get(1).unwrap().as_str().parse().ok();
    }

    let regex = lazy_regex::regex!(
        r#"^\s*https?://([^/\s]+)/([\w\-\.]+)/([\w\-\.]+)/pull/(\d+)([/?#].*)?\s*$"#
    );
    let m = regex.captures(text);
    if let Some(caps) = m
        // Host names, and GitHub's owner and repository names, are
        // case-insensitive
        && repository
            .host()
            .eq_ignore_ascii_case(caps.get(1).unwrap().as_str())
        && repository.path().eq_ignore_ascii_case(&format!(
            "{}/{}",
            caps.get(2).unwrap().as_str(),
            caps.get(3).unwrap().as_str()
        ))
    {
        return caps.get(4).unwrap().as_str().parse().ok();
    }

    None
}

/// The default target in the Git configuration: `spr.target`, or for setups
/// from before spr knew targets, `spr.githubRepository` (`OWNER/REPO` on
/// github.com) and `spr.githubMasterBranch` (default: `master`). `None` if
/// neither `spr.target` nor `spr.githubRepository` is set.
pub fn configured_target(git_config: &git2::Config) -> Result<Option<Target>> {
    let get = |key: &str| {
        git_config
            .get_string(key)
            .ok()
            .filter(|value| !value.is_empty())
    };

    if let Some(target) = get("spr.target") {
        return target.parse().map(Some).wrap_err("Invalid spr.target");
    }

    let Some(repository) = get("spr.githubRepository") else {
        return Ok(None);
    };
    let repository = Repository::new(ForgeKind::GitHub, None, &repository)
        .wrap_err("Invalid spr.githubRepository")?;
    let branch = get("spr.githubMasterBranch");
    Target::new(repository, branch.as_deref().unwrap_or("master"))
        .wrap_err("Invalid spr.githubMasterBranch")
        .map(Some)
}

/// Options that `spr.target` replaces
pub const OLD_TARGET_OPTIONS: [&str; 2] =
    ["spr.githubRepository", "spr.githubMasterBranch"];

/// A note on the deprecated options in the Git configuration (those that
/// `spr.target` and `spr.auth.github.token` replace), with how to replace
/// or remove them. `None` if none of them is set.
pub fn deprecation_note(git_config: &git2::Config) -> Option<String> {
    let notes: Vec<String> = [
        target_deprecation_note(git_config),
        token_deprecation_note(git_config),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!notes.is_empty()).then(|| notes.join("\n"))
}

/// The `git config` option for writing to where an option is set, e.g.
/// `--global ` (with a space, empty for the repository's configuration).
/// `None` if the option isn't set.
fn config_scope(git_config: &git2::Config, key: &str) -> Option<&'static str> {
    let level = git_config.get_entry(key).ok()?.level();
    Some(match level {
        git2::ConfigLevel::Global | git2::ConfigLevel::XDG => "--global ",
        git2::ConfigLevel::System => "--system ",
        git2::ConfigLevel::Worktree => "--worktree ",
        _ => "",
    })
}

fn token_deprecation_note(git_config: &git2::Config) -> Option<String> {
    let scope = config_scope(git_config, OLD_TOKEN_OPTION)?;
    let new = "spr.auth.github.token";
    let unset = format!("git config {scope}--unset {OLD_TOKEN_OPTION}");
    if git_config.get_entry(new).is_ok() {
        return Some(format!(
            "{OLD_TOKEN_OPTION} is deprecated, and ignored as {new} is set. \
             To remove it:\n{unset}"
        ));
    }
    // Copied with Git, so the token isn't shown
    Some(format!(
        "{OLD_TOKEN_OPTION} is deprecated: {new} replaces it. Run `spr init` \
         to update the configuration, or:\n\
         git config {scope}{new} \"$(git config {scope}{OLD_TOKEN_OPTION})\"\n\
         {unset}"
    ))
}

fn target_deprecation_note(git_config: &git2::Config) -> Option<String> {
    let old: Vec<(&str, &str)> = OLD_TARGET_OPTIONS
        .into_iter()
        .filter_map(|key| Some((key, config_scope(git_config, key)?)))
        .collect();
    if old.is_empty() {
        return None;
    }

    let names = old
        .iter()
        .map(|(key, _)| *key)
        .collect::<Vec<_>>()
        .join(" and ");
    let unset = old
        .iter()
        .map(|(key, scope)| format!("git config {scope}--unset {key}"))
        .collect::<Vec<_>>()
        .join("\n");

    if git_config.get_entry("spr.target").is_ok() {
        return Some(format!(
            "{names} {are} deprecated, and ignored as spr.target is set. \
             To remove {them}:\n{unset}",
            are = if old.len() == 1 { "is" } else { "are" },
            them = if old.len() == 1 { "it" } else { "them" },
        ));
    }

    let target = match configured_target(git_config) {
        Ok(Some(target)) => target.to_string(),
        _ => "github:OWNER/REPO:BRANCH".to_string(),
    };
    Some(format!(
        "{names} {are} deprecated: spr.target replaces {them}. Run `spr \
         init` to update the configuration, or:\n\
         git config {scope}spr.target {target}\n{unset}",
        are = if old.len() == 1 { "is" } else { "are" },
        them = if old.len() == 1 { "it" } else { "them" },
        scope = old[0].1,
    ))
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    fn tokens() -> AuthTokens {
        let mut tokens = AuthTokens::default();
        tokens.set(&"github:acme/codez".parse().unwrap(), "xyz".into());
        tokens
    }

    fn config_factory() -> Config {
        crate::config::Config::new(
            "github:acme/codez:master".parse().unwrap(),
            "spr/foo/".into(),
            tokens(),
            false,
            MergeMethod::Squash,
            StackingMode::BaseBranches,
            true,
            true,
        )
        .unwrap()
    }

    #[test]
    fn test_pull_request_url() {
        let gh = config_factory();

        assert_eq!(
            &gh.pull_request_url(123),
            "https://github.com/acme/codez/pull/123"
        );
    }

    #[test]
    fn test_parse_pull_request_field_empty() {
        let gh = config_factory();

        assert_eq!(gh.parse_pull_request_field(""), None);
        assert_eq!(gh.parse_pull_request_field("   "), None);
        assert_eq!(gh.parse_pull_request_field("\n"), None);
    }

    #[test]
    fn test_parse_pull_request_field_number() {
        let gh = config_factory();

        assert_eq!(gh.parse_pull_request_field("123"), Some(123));
        assert_eq!(gh.parse_pull_request_field("   123 "), Some(123));
        assert_eq!(gh.parse_pull_request_field("#123"), Some(123));
        assert_eq!(gh.parse_pull_request_field(" # 123"), Some(123));
    }

    #[test]
    fn test_parse_pull_request_field_too_large() {
        let gh = config_factory();

        assert_eq!(
            gh.parse_pull_request_field("123456789012345678901234567890"),
            None
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/acme/codez/pull/123456789012345678901234567890"
            ),
            None
        );
    }

    #[test]
    fn test_parse_pull_request_field_url() {
        let gh = config_factory();

        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/acme/codez/pull/123"
            ),
            Some(123)
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "  https://github.com/acme/codez/pull/123  "
            ),
            Some(123)
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/acme/codez/pull/123/"
            ),
            Some(123)
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/acme/codez/pull/123?x=a"
            ),
            Some(123)
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/acme/codez/pull/123/foo"
            ),
            Some(123)
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/acme/codez/pull/123#abc"
            ),
            Some(123)
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/Acme/CODEZ/pull/123"
            ),
            Some(123)
        );
    }

    #[test]
    fn test_parse_pull_request_field_other_repository() {
        let gh = config_factory();

        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/acme/other/pull/123"
            ),
            None
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "https://github.com/other/codez/pull/123"
            ),
            None
        );
        assert_eq!(
            gh.parse_pull_request_field(
                "https://ghe.example.com/acme/codez/pull/123"
            ),
            None
        );
        assert_eq!(gh.parse_pull_request_field("PR 123"), None);
    }

    #[test]
    fn test_config_only_for_github() {
        let config = |target: &str| {
            Config::new(
                target.parse().unwrap(),
                "spr/foo/".into(),
                tokens(),
                false,
                MergeMethod::Squash,
                StackingMode::BaseBranches,
                false,
                true,
            )
        };
        let c = config("github:acme/codez:main").unwrap();
        assert_eq!((c.owner.as_str(), c.repo.as_str()), ("acme", "codez"));
        assert_eq!(c.master_ref.branch_name(), "main");
        assert_eq!(
            c.pull_request_url(12),
            "https://github.com/acme/codez/pull/12"
        );
        let c = config("github://ghe.example.com/acme/codez:main").unwrap();
        assert_eq!((c.owner.as_str(), c.repo.as_str()), ("acme", "codez"));
        assert_eq!(
            c.pull_request_url(12),
            "https://ghe.example.com/acme/codez/pull/12"
        );
        assert_eq!(
            c.parse_pull_request_field(&c.pull_request_url(12)),
            Some(12)
        );
        assert_eq!(
            c.parse_pull_request_field("https://github.com/acme/codez/pull/12"),
            None
        );
        assert!(config("gitlab:acme/codez:main").is_err());
    }

    #[test]
    fn test_parse_merge_method() {
        assert_eq!(
            "squash".parse::<MergeMethod>().unwrap(),
            MergeMethod::Squash
        );
        assert_eq!("Merge".parse::<MergeMethod>().unwrap(), MergeMethod::Merge);
        assert!("rebase".parse::<MergeMethod>().is_err());
        assert!("".parse::<MergeMethod>().is_err());
    }

    #[test]
    fn test_is_spr_base_branch() {
        let config = config_factory();

        let is_base =
            |name: &str| config.is_spr_base_branch(&config.new_branch(name));

        assert!(is_base("spr/foo/master.fix-the-bug"));
        assert!(is_base("spr/foo/master.fix-the-bug-1"));
        assert!(!is_base("spr/foo/fix-the-bug"));
        assert!(!is_base("spr/foo/masterful-change"));
        assert!(!is_base("spr/bar/master.fix-the-bug"));
        assert!(!is_base("master"));
        assert!(!is_base("release-1.0"));
    }

    #[test]
    fn test_stacking_mode() {
        assert_eq!(
            "chain".parse::<StackingMode>().unwrap(),
            StackingMode::Chain
        );
        assert_eq!(
            "base-branches".parse::<StackingMode>().unwrap(),
            StackingMode::BaseBranches
        );
        assert_eq!(
            "github-stack".parse::<StackingMode>().unwrap(),
            StackingMode::GitHubStack
        );
        assert!("stack".parse::<StackingMode>().is_err());

        assert!(!StackingMode::BaseBranches.is_chained());
        assert!(StackingMode::Chain.is_chained());
        assert!(StackingMode::GitHubStack.is_chained());

        assert_eq!(
            StackingMode::default_for(MergeMethod::Squash),
            StackingMode::BaseBranches
        );
        assert_eq!(
            StackingMode::default_for(MergeMethod::Merge),
            StackingMode::Chain
        );
    }

    #[test]
    fn test_merge_method_and_stacking_mode_combinations() {
        let config = |merge_method, stacking_mode| {
            Config::new(
                "github:acme/codez:master".parse().unwrap(),
                "spr/foo/".into(),
                tokens(),
                false,
                merge_method,
                stacking_mode,
                false,
                true,
            )
        };

        assert!(
            config(MergeMethod::Squash, StackingMode::BaseBranches).is_ok()
        );
        assert!(config(MergeMethod::Squash, StackingMode::Chain).is_ok());
        assert!(config(MergeMethod::Merge, StackingMode::Chain).is_ok());
        assert!(config(MergeMethod::Squash, StackingMode::GitHubStack).is_ok());
        assert!(config(MergeMethod::Merge, StackingMode::GitHubStack).is_ok());
        assert!(
            config(MergeMethod::Merge, StackingMode::BaseBranches).is_err()
        );
    }

    #[test]
    fn test_configured_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = |settings: &[(&str, &str)]| {
            let path = dir.path().join("config");
            let _ = std::fs::remove_file(&path);
            let mut git_config = git2::Config::open(&path).unwrap();
            for (key, value) in settings {
                git_config.set_str(key, value).unwrap();
            }
            configured_target(&git_config)
                .map(|target| target.map(|target| target.to_string()))
        };

        assert_eq!(target(&[]).unwrap(), None);
        assert_eq!(
            target(&[("spr.target", "github:acme/codez:main")]).unwrap(),
            Some("github:acme/codez:main".into())
        );
        assert!(target(&[("spr.target", "acme/codez")]).is_err());

        // The options from before spr.target
        assert_eq!(
            target(&[("spr.githubRepository", "acme/codez")]).unwrap(),
            Some("github:acme/codez:master".into())
        );
        assert_eq!(
            target(&[
                ("spr.githubRepository", "acme/codez"),
                ("spr.githubMasterBranch", "main"),
            ])
            .unwrap(),
            Some("github:acme/codez:main".into())
        );
        assert_eq!(
            target(&[("spr.githubMasterBranch", "main")]).unwrap(),
            None
        );
        assert!(target(&[("spr.githubRepository", "codez")]).is_err());

        // spr.target takes precedence
        assert_eq!(
            target(&[
                ("spr.target", "github:acme/new:trunk"),
                ("spr.githubRepository", "acme/old"),
                ("spr.githubMasterBranch", "main"),
            ])
            .unwrap(),
            Some("github:acme/new:trunk".into())
        );
    }

    #[test]
    fn test_deprecation_note() {
        let dir = tempfile::tempdir().unwrap();
        // A configuration from a global and a local file
        let note = |global: &[(&str, &str)], local: &[(&str, &str)]| {
            let mut git_config = git2::Config::new().unwrap();
            for (name, level, settings) in [
                ("global", git2::ConfigLevel::Global, global),
                ("local", git2::ConfigLevel::Local, local),
            ] {
                let path = dir.path().join(name);
                let _ = std::fs::remove_file(&path);
                let mut file = git2::Config::open(&path).unwrap();
                for (key, value) in settings {
                    file.set_str(key, value).unwrap();
                }
                git_config.add_file(&path, level, true).unwrap();
            }
            deprecation_note(&git_config)
        };

        assert_eq!(note(&[], &[]), None);
        assert_eq!(note(&[], &[("spr.target", "github:a/b:main")]), None);

        let n = note(
            &[],
            &[
                ("spr.githubRepository", "acme/codez"),
                ("spr.githubMasterBranch", "main"),
            ],
        )
        .unwrap();
        assert!(
            n.contains(
                "spr.githubRepository and spr.githubMasterBranch are deprecated"
            ),
            "{n}"
        );
        assert!(
            n.contains("\ngit config spr.target github:acme/codez:main\n"),
            "{n}"
        );
        assert!(
            n.contains("\ngit config --unset spr.githubRepository\n"),
            "{n}"
        );
        assert!(
            n.ends_with("\ngit config --unset spr.githubMasterBranch"),
            "{n}"
        );

        // In the global configuration: changed there
        let n = note(&[("spr.githubRepository", "acme/codez")], &[]).unwrap();
        assert!(n.contains("spr.githubRepository is deprecated"), "{n}");
        assert!(
            n.contains(
                "\ngit config --global spr.target github:acme/codez:master\n"
            ),
            "{n}"
        );
        assert!(
            n.ends_with("\ngit config --global --unset spr.githubRepository"),
            "{n}"
        );

        // Ignored, as spr.target is set
        let n = note(
            &[("spr.githubMasterBranch", "main")],
            &[("spr.target", "github:a/b:main")],
        )
        .unwrap();
        assert!(n.contains("ignored as spr.target is set"), "{n}");
        assert!(
            n.ends_with(
                ":\ngit config --global --unset spr.githubMasterBranch"
            ),
            "{n}"
        );
    }

    #[test]
    fn test_auth_tokens() {
        let dir = tempfile::tempdir().unwrap();
        // Tokens in a global and a local file
        let tokens = |global: &[(&str, &str)], local: &[(&str, &str)]| {
            let mut git_config = git2::Config::new().unwrap();
            for (name, level, settings) in [
                ("global", git2::ConfigLevel::Global, global),
                ("local", git2::ConfigLevel::Local, local),
            ] {
                let path = dir.path().join(name);
                let _ = std::fs::remove_file(&path);
                let mut file = git2::Config::open(&path).unwrap();
                for (key, value) in settings {
                    file.set_str(key, value).unwrap();
                }
                git_config.add_file(&path, level, true).unwrap();
            }
            AuthTokens::from_git_config(&git_config).unwrap()
        };
        let github: Repository = "github:acme/codez".parse().unwrap();
        let ghe: Repository =
            "github://ghe.example.com/acme/codez".parse().unwrap();
        let ghe_port: Repository =
            "github://ghe.example.com:8443/acme/codez".parse().unwrap();

        assert_eq!(auth_token_key(&github), "spr.auth.github.token");
        assert_eq!(
            auth_token_key(&ghe),
            "spr.auth.github.ghe.example.com.token"
        );
        assert_eq!(
            auth_token_key(&ghe_port),
            "spr.auth.github.ghe.example.com:8443.token"
        );

        let t = tokens(&[], &[]);
        assert_eq!(t.get(&github), None);

        let t = tokens(
            &[
                ("spr.auth.github.token", "global"),
                ("spr.auth.github.ghe.example.com.token", "ghe-global"),
            ],
            &[
                ("spr.auth.github.token", "local"),
                ("spr.auth.github.ghe.example.com:8443.token", "ghe-port"),
            ],
        );
        // The repository's configuration wins, as usual
        assert_eq!(t.get(&github), Some("local"));
        assert_eq!(t.get(&ghe), Some("ghe-global"));
        assert_eq!(t.get(&ghe_port), Some("ghe-port"));

        // Hosts are case-insensitive, and so are Git's section and variable
        // names
        let t =
            tokens(&[("Spr.Auth.GitHub.GHE.Example.COM.Token", "ghe")], &[]);
        assert_eq!(t.get(&ghe), Some("ghe"));
        assert_eq!(t.get(&github), None);

        // The deprecated option, unless the new one is set
        let t = tokens(&[("spr.githubAuthToken", "old")], &[]);
        assert_eq!(t.get(&github), Some("old"));
        assert_eq!(t.get(&ghe), None);
        let t = tokens(
            &[("spr.auth.github.token", "new")],
            &[("spr.githubAuthToken", "old")],
        );
        assert_eq!(t.get(&github), Some("new"));

        // Given on the command line
        let mut t = tokens(&[("spr.auth.github.token", "configured")], &[]);
        t.set(&github, "given".into());
        assert_eq!(t.get(&github), Some("given"));
    }

    #[test]
    fn test_token_deprecation_note() {
        let dir = tempfile::tempdir().unwrap();
        let note = |global: &[(&str, &str)], local: &[(&str, &str)]| {
            let mut git_config = git2::Config::new().unwrap();
            for (name, level, settings) in [
                ("global", git2::ConfigLevel::Global, global),
                ("local", git2::ConfigLevel::Local, local),
            ] {
                let path = dir.path().join(name);
                let _ = std::fs::remove_file(&path);
                let mut file = git2::Config::open(&path).unwrap();
                for (key, value) in settings {
                    file.set_str(key, value).unwrap();
                }
                git_config.add_file(&path, level, true).unwrap();
            }
            deprecation_note(&git_config)
        };

        assert_eq!(note(&[("spr.auth.github.token", "secret")], &[]), None);

        let n = note(&[("spr.githubAuthToken", "secret")], &[]).unwrap();
        assert!(!n.contains("secret"), "{n}");
        assert!(
            n.contains(
                "\ngit config --global spr.auth.github.token \"$(git config \
                 --global spr.githubAuthToken)\"\n"
            ),
            "{n}"
        );
        assert!(
            n.ends_with("\ngit config --global --unset spr.githubAuthToken"),
            "{n}"
        );

        let n = note(
            &[("spr.auth.github.token", "secret")],
            &[("spr.githubAuthToken", "secret")],
        )
        .unwrap();
        assert!(!n.contains("secret"), "{n}");
        assert!(n.contains("ignored as spr.auth.github.token is set"), "{n}");
        assert!(
            n.ends_with(":\ngit config --unset spr.githubAuthToken"),
            "{n}"
        );

        // Together with the target options: one note after the other
        let n = note(
            &[],
            &[
                ("spr.githubRepository", "acme/codez"),
                ("spr.githubAuthToken", "secret"),
            ],
        )
        .unwrap();
        assert!(
            n.contains(
                "--unset spr.githubRepository\nspr.githubAuthToken is \
                 deprecated"
            ),
            "{n}"
        );
    }

    #[test]
    fn test_debug_output_does_not_contain_auth_token() {
        let config = config_factory();
        let debug = format!("{config:?}");
        assert!(!debug.contains("xyz"), "{debug}");
        assert!(debug.contains("[redacted]"));
    }
}
