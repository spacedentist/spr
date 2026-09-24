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

impl std::fmt::Display for MergeMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Merge => "merge",
        })
    }
}

impl Config {
    pub fn new(
        owner: String,
        repo: String,
        master_branch: String,
        branch_prefix: String,
        auth_token: String,
        require_approval: bool,
        merge_method: MergeMethod,
    ) -> Self {
        let master_ref =
            GitHubBranch::new_from_branch_name(&master_branch, &master_branch);
        Self {
            owner,
            repo,
            master_ref,
            branch_prefix,
            auth_token,
            require_approval,
            merge_method,
        }
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
        )
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
}
