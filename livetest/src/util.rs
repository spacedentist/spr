//! Pure helper functions (unit-tested)

use std::collections::BTreeSet;

use color_eyre::eyre::{Result, eyre};

/// Replace every occurrence of `secret` in `text`, so tokens never end up in
/// output or error messages
pub fn mask(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, "<token>")
}

/// The test repository, on github.com or a GitHub Enterprise host
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    /// The host in lower case, e.g. `github.com`
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl Repo {
    /// The repository as given on the command line: `OWNER/REPO` on
    /// github.com, or in spr's notation `github:OWNER/REPO`, or
    /// `github://HOST/OWNER/REPO` on another host (GitHub Enterprise)
    pub fn parse(text: &str) -> Result<Self> {
        let usage = || {
            eyre!(
                "The repository must be given as OWNER/REPO (on github.com) \
                 or github://HOST/OWNER/REPO, not {text:?}"
            )
        };
        let (host, path) = match text.strip_prefix("github:") {
            Some(rest) => match rest.strip_prefix("//") {
                Some(rest) => {
                    let (host, path) =
                        rest.split_once('/').ok_or_else(usage)?;
                    (host.to_ascii_lowercase(), path)
                }
                None => ("github.com".to_string(), rest),
            },
            None => ("github.com".to_string(), text),
        };
        let valid = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        };
        let valid_host = valid(&host.replace(':', ""));
        match path.split_once('/') {
            Some((owner, name))
                if valid_host && valid(owner) && valid(name) =>
            {
                Ok(Repo {
                    host,
                    owner: owner.to_string(),
                    name: name.to_string(),
                })
            }
            _ => Err(usage()),
        }
    }

    pub fn is_github_com(&self) -> bool {
        self.host == "github.com"
    }

    /// The repository in spr's notation, e.g. `github:OWNER/REPO`
    pub fn notation(&self) -> String {
        if self.is_github_com() {
            format!("github:{}/{}", self.owner, self.name)
        } else {
            format!("github://{}/{}/{}", self.host, self.owner, self.name)
        }
    }

    /// The address of the repository, also the base of its Git URL
    pub fn web_url(&self) -> String {
        format!("https://{}/{}/{}", self.host, self.owner, self.name)
    }

    /// The base address of the REST API: github.com, GitHub Enterprise
    /// Cloud with data residency (`*.ghe.com`), or else GitHub Enterprise
    /// Server
    pub fn api_url(&self) -> String {
        if self.is_github_com() {
            "https://api.github.com".to_string()
        } else if self.host.ends_with(".ghe.com") {
            format!("https://api.{}", self.host)
        } else {
            format!("https://{}/api/v3", self.host)
        }
    }

    /// The subsection of spr's Git configuration with the token for the
    /// host (`spr.auth.github[.HOST].token`)
    pub fn token_subsection(&self) -> String {
        if self.is_github_com() {
            "auth.github".to_string()
        } else {
            format!("auth.github.{}", self.host)
        }
    }

    /// The Git configuration key of spr's token for the host
    pub fn token_key(&self) -> String {
        format!("spr.{}.token", self.token_subsection())
    }
}

impl std::fmt::Display for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.notation())
    }
}

/// The ID of a test run: the UTC time it started (sortable, readable), and a
/// suffix in case two runs start in the same second
pub fn run_id(unix_secs: u64, suffix: u32) -> String {
    format!("{}-{:04x}", utc_timestamp(unix_secs), suffix & 0xffff)
}

/// `YYYYMMDD-HHMMSS` in UTC
pub fn utc_timestamp(unix_secs: u64) -> String {
    let days = unix_secs / 86400;
    let secs = unix_secs % 86400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// The date of the given day since 1970-01-01 (Howard Hinnant's algorithm)
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// All branches a test run creates are under one of these prefixes, followed
/// by the run ID
pub const PR_BRANCH_ROOT: &str = "spr/livetest/";
pub const TARGET_BRANCH_ROOT: &str = "livetest/";

/// The branch prefix (`spr.branchPrefix`) of a test run
pub fn run_prefix(run_id: &str) -> String {
    format!("{PR_BRANCH_ROOT}{run_id}/")
}

/// The branch a test run uses instead of the repository's default branch
/// (the branch of `spr.target`), so landing never touches the default branch
pub fn target_branch(run_id: &str) -> String {
    format!("{TARGET_BRANCH_ROOT}{run_id}/main")
}

/// The ID of the test run that created a branch, if it was one
pub fn run_id_of_branch(branch: &str) -> Option<&str> {
    let rest = branch
        .strip_prefix(PR_BRANCH_ROOT)
        .or_else(|| branch.strip_prefix(TARGET_BRANCH_ROOT))?;
    let (id, _) = rest.split_once('/')?;
    is_run_id(id).then_some(id)
}

/// Whether `id` looks like an ID made by `run_id`
pub fn is_run_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(i, &b)| match i {
            8 | 15 => b == b'-',
            0..8 | 9..15 => b.is_ascii_digit(),
            _ => b.is_ascii_hexdigit(),
        })
}

/// The scopes in GitHub's `X-OAuth-Scopes` header
pub fn parse_scopes(header: &str) -> BTreeSet<String> {
    header
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask() {
        assert_eq!(mask("a gho_x b gho_x", "gho_x"), "a <token> b <token>");
        assert_eq!(mask("nothing", ""), "nothing");
    }

    #[test]
    fn test_repo() {
        let repo = Repo::parse("spacedentist/spr-test").unwrap();
        assert_eq!(
            repo,
            Repo {
                host: "github.com".into(),
                owner: "spacedentist".into(),
                name: "spr-test".into(),
            }
        );
        assert_eq!(repo.notation(), "github:spacedentist/spr-test");
        assert_eq!(repo.web_url(), "https://github.com/spacedentist/spr-test");
        assert_eq!(repo.api_url(), "https://api.github.com");
        assert_eq!(repo.token_key(), "spr.auth.github.token");
        assert_eq!(Repo::parse("github:spacedentist/spr-test").unwrap(), repo);

        let repo =
            Repo::parse("github://GHE.example.com:8443/acme/test").unwrap();
        assert_eq!(repo.notation(), "github://ghe.example.com:8443/acme/test");
        assert_eq!(repo.web_url(), "https://ghe.example.com:8443/acme/test");
        assert_eq!(repo.api_url(), "https://ghe.example.com:8443/api/v3");
        assert_eq!(
            repo.token_key(),
            "spr.auth.github.ghe.example.com:8443.token"
        );

        let repo = Repo::parse("github://acme.ghe.com/acme/test").unwrap();
        assert_eq!(repo.api_url(), "https://api.acme.ghe.com");

        for bad in [
            "spr-test",
            "a/b/c",
            "/b",
            "a/",
            "a b/c",
            "github://host",
            "github:///a/b",
            "gitlab:a/b",
        ] {
            assert!(Repo::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_utc_timestamp() {
        assert_eq!(utc_timestamp(0), "19700101-000000");
        // 2026-10-08 15:30:12 UTC
        assert_eq!(utc_timestamp(1791473412), "20261008-153012");
        // A leap day
        assert_eq!(utc_timestamp(951782400), "20000229-000000");
    }

    #[test]
    fn test_run_ids_and_branches() {
        let id = run_id(1791473412, 0x1a2b);
        assert_eq!(id, "20261008-153012-1a2b");
        assert!(is_run_id(&id));
        assert!(!is_run_id("20261008-153012"));
        assert!(!is_run_id("2026100a-153012-1a2b"));

        assert_eq!(run_prefix(&id), "spr/livetest/20261008-153012-1a2b/");
        assert_eq!(target_branch(&id), "livetest/20261008-153012-1a2b/main");
        assert_eq!(
            run_id_of_branch("spr/livetest/20261008-153012-1a2b/basic/x"),
            Some(id.as_str())
        );
        assert_eq!(run_id_of_branch(&target_branch(&id)), Some(id.as_str()));
        assert_eq!(run_id_of_branch("spr/spacedentist/x"), None);
        assert_eq!(run_id_of_branch("livetest/notarunid/main"), None);
        assert_eq!(run_id_of_branch("master"), None);
    }

    #[test]
    fn test_parse_scopes() {
        assert_eq!(
            parse_scopes("repo, read:org,workflow ,"),
            ["read:org", "repo", "workflow"]
                .into_iter()
                .map(String::from)
                .collect()
        );
        assert!(parse_scopes("").is_empty());
    }
}
