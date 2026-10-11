//! Targets: where Pull Requests get merged — forge, host, repository and
//! branch.
//!
//! The notation is `<forge>:[//<host>]<path>:<branch>`, for example
//! `github:spacedentist/spr:master` or
//! `gitlab://gitlab.example.com:8443/group/project:main`. Without
//! `//<host>`, it's the forge's public instance (e.g. github.com). The same
//! without `:<branch>` names a repository.

use color_eyre::eyre::{Result, bail, eyre};

/// The kinds of forges (Git hosting services) a target can be on
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForgeKind {
    GitHub,
    GitLab,
    Gitea,
    Forgejo,
    Bitbucket,
}

impl ForgeKind {
    const ALL: [ForgeKind; 5] = [
        ForgeKind::GitHub,
        ForgeKind::GitLab,
        ForgeKind::Gitea,
        ForgeKind::Forgejo,
        ForgeKind::Bitbucket,
    ];

    /// The forge's name in the target notation
    pub fn name(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "github",
            ForgeKind::GitLab => "gitlab",
            ForgeKind::Gitea => "gitea",
            ForgeKind::Forgejo => "forgejo",
            ForgeKind::Bitbucket => "bitbucket",
        }
    }

    /// The host of the forge's public instance, if there is one
    pub fn default_host(self) -> Option<&'static str> {
        match self {
            ForgeKind::GitHub => Some("github.com"),
            ForgeKind::GitLab => Some("gitlab.com"),
            ForgeKind::Gitea | ForgeKind::Forgejo => None,
            ForgeKind::Bitbucket => Some("bitbucket.org"),
        }
    }

    /// How many segments a repository path has: `None` for any number from
    /// two (GitLab: groups and subgroups, then the project)
    fn path_segments(self) -> Option<usize> {
        match self {
            ForgeKind::GitLab => None,
            _ => Some(2),
        }
    }

    fn path_format(self) -> &'static str {
        match self {
            ForgeKind::GitLab => "GROUP/…/PROJECT",
            ForgeKind::Bitbucket => "WORKSPACE/REPOSITORY",
            _ => "OWNER/REPOSITORY",
        }
    }
}

impl std::str::FromStr for ForgeKind {
    type Err = color_eyre::eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        ForgeKind::ALL
            .into_iter()
            .find(|forge| forge.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| {
                eyre!(
                    "Unknown forge '{s}', must be one of {}",
                    ForgeKind::ALL
                        .map(|forge| format!("'{}'", forge.name()))
                        .join(", ")
                )
            })
    }
}

impl std::fmt::Display for ForgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A repository on a forge, e.g. `github:spacedentist/spr`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    forge: ForgeKind,
    /// The host (lower case, possibly with a port), or `None` for the
    /// forge's public instance
    host: Option<String>,
    path: String,
}

impl Repository {
    /// The repository `path` (e.g. `owner/repo`) on `host`, or on the
    /// forge's public instance if `host` is `None`
    pub fn new(
        forge: ForgeKind,
        host: Option<&str>,
        path: &str,
    ) -> Result<Self> {
        let host = match host {
            Some(host) => {
                let host = host.to_ascii_lowercase();
                if !lazy_regex::regex_is_match!(
                    r#"^[a-z0-9]([a-z0-9\-\.]*[a-z0-9])?(:\d+)?$"#,
                    &host
                ) {
                    bail!("'{host}' isn't a valid host name");
                }
                // The public instance is always written without its host
                (Some(host.as_str()) != forge.default_host()).then_some(host)
            }
            None => {
                if forge.default_host().is_none() {
                    bail!(
                        "There's no public {forge} instance, so the host \
                         must be given: '{forge}://HOST/{}'",
                        forge.path_format()
                    );
                }
                None
            }
        };

        let segments: Vec<&str> = path.split('/').collect();
        let valid_segments = segments.iter().all(|segment| {
            lazy_regex::regex_is_match!(r#"^[\w\-\.]+$"#, segment)
                && *segment != "."
                && *segment != ".."
        });
        let valid_count = match forge.path_segments() {
            Some(count) => segments.len() == count,
            None => segments.len() >= 2,
        };
        if !valid_segments || !valid_count {
            bail!(
                "'{path}' isn't a valid {forge} repository, which must be \
                 given as '{}'",
                forge.path_format()
            );
        }

        Ok(Self {
            forge,
            host,
            path: path.to_string(),
        })
    }

    pub fn forge(&self) -> ForgeKind {
        self.forge
    }

    /// The host, e.g. `github.com`
    pub fn host(&self) -> &str {
        self.host
            .as_deref()
            .or(self.forge.default_host())
            .expect("forges without a public instance always have a host")
    }

    /// Whether the repository is on the forge's public instance
    pub fn is_on_public_instance(&self) -> bool {
        self.host.is_none()
    }

    /// The repository's path, e.g. `owner/repo`
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The address of the repository's web page, e.g.
    /// `https://github.com/owner/repo`. Also the base of its Git URL and its
    /// Pull Requests' addresses.
    pub fn web_url(&self) -> String {
        format!("https://{}/{}", self.host(), self.path)
    }
}

/// Splits `<forge>:[//<host>]<rest>` into its parts
fn split_forge_and_host(text: &str) -> Result<(ForgeKind, Option<&str>, &str)> {
    let (forge, rest) = text.split_once(':').ok_or_else(|| {
        eyre!("'{text}' must start with the forge, e.g. 'github:'")
    })?;
    let forge = forge.parse()?;
    match rest.strip_prefix("//") {
        Some(rest) => {
            let (host, rest) = rest.split_once('/').ok_or_else(|| {
                eyre!("'{text}' has a host, but no repository after it")
            })?;
            Ok((forge, Some(host), rest))
        }
        None => Ok((forge, None, rest)),
    }
}

impl std::str::FromStr for Repository {
    type Err = color_eyre::eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        let (forge, host, path) = split_forge_and_host(s)?;
        Repository::new(forge, host, path)
    }
}

impl std::fmt::Display for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.host {
            Some(host) => write!(f, "{}://{host}/{}", self.forge, self.path),
            None => write!(f, "{}:{}", self.forge, self.path),
        }
    }
}

/// Where Pull Requests get merged: a branch in a repository, e.g.
/// `github:spacedentist/spr:master`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    repository: Repository,
    branch: String,
}

impl Target {
    pub fn new(repository: Repository, branch: &str) -> Result<Self> {
        if !git2::Branch::name_is_valid(branch)? {
            bail!("'{branch}' isn't a valid branch name");
        }
        Ok(Self {
            repository,
            branch: branch.to_string(),
        })
    }

    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    /// The branch name, e.g. `master`
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Parse a target given by the user (`--target`, `Spr-Target`), where
    /// a branch name alone is a shorthand for that branch in the default
    /// target's repository. (Full targets always contain a colon, branch
    /// names never.)
    pub fn parse_with_default(
        s: &str,
        default: Option<&Target>,
    ) -> Result<Self> {
        if s.contains(':') {
            return s.parse();
        }
        match default {
            Some(default) => Target::new(default.repository.clone(), s),
            None => bail!(
                "'{s}' is a branch name only, but there's no default target \
                 (spr.target) for its repository. Give the full target, \
                 e.g. 'github:OWNER/REPO:{s}'."
            ),
        }
    }
}

impl std::str::FromStr for Target {
    type Err = color_eyre::eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        let (forge, host, rest) = split_forge_and_host(s)?;
        // Branch names can't contain colons, so the branch is everything
        // after the last one
        let (path, branch) = rest.rsplit_once(':').ok_or_else(|| {
            eyre!(
                "'{s}' has no branch: a target must be given as \
                 '{forge}:{}:BRANCH'",
                forge.path_format()
            )
        })?;
        Target::new(Repository::new(forge, host, path)?, branch)
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.repository, self.branch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(text: &str) -> Target {
        text.parse().unwrap()
    }

    #[test]
    fn test_parse_with_default() {
        let default = target("github:spacedentist/spr:master");
        assert_eq!(
            Target::parse_with_default("release/1.2", Some(&default)).unwrap(),
            target("github:spacedentist/spr:release/1.2")
        );
        assert_eq!(
            Target::parse_with_default("github:o/r:main", Some(&default))
                .unwrap(),
            target("github:o/r:main")
        );
        assert_eq!(
            Target::parse_with_default("github:o/r:main", None).unwrap(),
            target("github:o/r:main")
        );
        assert!(Target::parse_with_default("main", None).is_err());
        assert!(
            Target::parse_with_default("bad..branch", Some(&default)).is_err()
        );
    }

    #[test]
    fn test_parse_target() {
        let t = target("github:spacedentist/spr:master");
        assert_eq!(t.repository().forge(), ForgeKind::GitHub);
        assert_eq!(t.repository().host(), "github.com");
        assert!(t.repository().is_on_public_instance());
        assert_eq!(t.repository().path(), "spacedentist/spr");
        assert_eq!(t.branch(), "master");

        let t = target("github://ghe.bigcorp.com/owner/repo:release/1.2.3");
        assert_eq!(t.repository().host(), "ghe.bigcorp.com");
        assert!(!t.repository().is_on_public_instance());
        assert_eq!(t.repository().path(), "owner/repo");
        assert_eq!(t.branch(), "release/1.2.3");

        let t = target("gitlab://gitlab.example.com:8443/group/sub/proj:main");
        assert_eq!(t.repository().forge(), ForgeKind::GitLab);
        assert_eq!(t.repository().host(), "gitlab.example.com:8443");
        assert_eq!(t.repository().path(), "group/sub/proj");
        assert_eq!(t.branch(), "main");

        // Dots in a GitLab group: still a path on gitlab.com
        let t = target("gitlab:my.group/project:main");
        assert_eq!(t.repository().host(), "gitlab.com");
        assert_eq!(t.repository().path(), "my.group/project");

        let t = target("forgejo://codeberg.org/owner/repo:main");
        assert_eq!(t.repository().forge(), ForgeKind::Forgejo);
        assert_eq!(t.repository().host(), "codeberg.org");
    }

    #[test]
    fn test_target_normalisation() {
        // The forge's name and the host are case-insensitive, and the public
        // instance's host is left out
        assert_eq!(
            target("GitHub://GitHub.com/Owner/Repo:Main").to_string(),
            "github:Owner/Repo:Main"
        );
        assert_eq!(
            target("github://GHE.BigCorp.com/owner/repo:main").to_string(),
            "github://ghe.bigcorp.com/owner/repo:main"
        );
        for text in [
            "github:spacedentist/spr:master",
            "github://ghe.bigcorp.com/owner/repo:release/1.2.3",
            "gitlab://gitlab.example.com:8443/group/sub/proj:main",
            "gitlab:my.group/project:main",
            "gitea://gitea.example.com/owner/repo:main",
            "bitbucket:workspace/repo:main",
        ] {
            assert_eq!(target(text).to_string(), text);
        }
    }

    #[test]
    fn test_parse_invalid_target() {
        for text in [
            "",
            "spacedentist/spr:master",
            "hub:spacedentist/spr:master",
            "github:spacedentist/spr",
            "github:spacedentist/spr:",
            "github:spacedentist:master",
            "github:a/b/c:master",
            "github:/spr:master",
            "github:owner/../x:master",
            "github:owner/re po:master",
            "github:spacedentist/spr:bad..branch",
            "github://ghe.bigcorp.com:master",
            "github://ghe.bigcorp.com/owner:master",
            "github://bad_host/owner/repo:master",
            "github://ghe.bigcorp.com:port/owner/repo:master",
            "gitlab:project:main",
            "gitea:owner/repo:main",
            "forgejo:owner/repo:main",
        ] {
            assert!(
                text.parse::<Target>().is_err(),
                "{text:?} should be rejected"
            );
        }
    }

    #[test]
    fn test_parse_repository() {
        let r: Repository = "github:svenover/spr".parse().unwrap();
        assert_eq!(r.forge(), ForgeKind::GitHub);
        assert_eq!(r.path(), "svenover/spr");
        assert_eq!(r.to_string(), "github:svenover/spr");
        assert_eq!(r.web_url(), "https://github.com/svenover/spr");

        let r: Repository = "gitlab://gitlab.example.com:8443/group/sub/proj"
            .parse()
            .unwrap();
        assert_eq!(
            r.web_url(),
            "https://gitlab.example.com:8443/group/sub/proj"
        );

        assert!("github:svenover/spr:master".parse::<Repository>().is_err());
    }
}
