use color_eyre::eyre::{Result, bail};

use crate::github::GitHubBranch;

#[derive(Clone, Debug)]
pub struct Config {
    pub owner: String,
    pub repo: String,
    pub master_ref: GitHubBranch,
    pub branch_prefix: String,
    pub auth_token: String,
    pub require_approval: bool,
    pub merge_method: MergeMethod,
    pub stacking_mode: StackingMode,
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
        owner: String,
        repo: String,
        master_branch: String,
        branch_prefix: String,
        auth_token: String,
        require_approval: bool,
        merge_method: MergeMethod,
        stacking_mode: StackingMode,
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

        let master_ref =
            GitHubBranch::new_from_branch_name(&master_branch, &master_branch);
        Ok(Self {
            owner,
            repo,
            master_ref,
            branch_prefix,
            auth_token,
            require_approval,
            merge_method,
            stacking_mode,
        })
    }

    pub fn pull_request_url(&self, number: u64) -> String {
        format!(
            "https://github.com/{owner}/{repo}/pull/{number}",
            owner = &self.owner,
            repo = &self.repo
        )
    }

    pub fn parse_pull_request_field(&self, text: &str) -> Option<u64> {
        if text.is_empty() {
            return None;
        }

        let regex = lazy_regex::regex!(r#"^\s*#?\s*(\d+)\s*$"#);
        let m = regex.captures(text);
        if let Some(caps) = m {
            return Some(caps.get(1).unwrap().as_str().parse().unwrap());
        }

        let regex = lazy_regex::regex!(
            r#"^\s*https?://github.com/([\w\-\.]+)/([\w\-\.]+)/pull/(\d+)([/?#].*)?\s*$"#
        );
        let m = regex.captures(text);
        if let Some(caps) = m
            && self.owner == caps.get(1).unwrap().as_str()
            && self.repo == caps.get(2).unwrap().as_str()
        {
            return Some(caps.get(3).unwrap().as_str().parse().unwrap());
        }

        None
    }

    pub fn new_github_branch_from_ref(
        &self,
        ghref: &str,
    ) -> Result<GitHubBranch> {
        GitHubBranch::new_from_ref(ghref, self.master_ref.branch_name())
    }

    /// Whether the given branch is a base branch that spr created for a Pull
    /// Request, i.e. a synthetic branch reflecting the changes the Pull
    /// Request's commit is based on.
    ///
    /// Base branch names are the branch prefix, followed by the master branch
    /// name, a dot, and a slug of the commit title. Pull Request branch names
    /// never contain a dot after the prefix, as slugs don't contain dots.
    pub fn is_spr_base_branch(&self, branch: &GitHubBranch) -> bool {
        branch
            .branch_name()
            .strip_prefix(&self.branch_prefix)
            .and_then(|rest| rest.strip_prefix(self.master_ref.branch_name()))
            .is_some_and(|rest| rest.starts_with('.'))
    }

    pub fn new_github_branch(&self, branch_name: &str) -> GitHubBranch {
        GitHubBranch::new_from_branch_name(
            branch_name,
            self.master_ref.branch_name(),
        )
    }
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    fn config_factory() -> Config {
        crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "master".into(),
            "spr/foo/".into(),
            "xyz".into(),
            false,
            MergeMethod::Squash,
            StackingMode::BaseBranches,
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

        let is_base = |name: &str| {
            config.is_spr_base_branch(&config.new_github_branch(name))
        };

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
                "acme".into(),
                "codez".into(),
                "master".into(),
                "spr/foo/".into(),
                "xyz".into(),
                false,
                merge_method,
                stacking_mode,
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
}
