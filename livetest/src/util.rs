//! Pure helper functions (unit-tested)

use color_eyre::eyre::{Result, bail};

/// Replace every occurrence of `secret` in `text`, so tokens never end up in
/// output or error messages
pub fn mask(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, "<token>")
}

/// Split `OWNER/REPO`
pub fn parse_repo(repo: &str) -> Result<(String, String)> {
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    match repo.split_once('/') {
        Some((owner, name)) if valid(owner) && valid(name) => {
            Ok((owner.to_string(), name.to_string()))
        }
        _ => bail!("The repository must be given as OWNER/REPO, not {repo:?}"),
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
/// (`spr.githubMasterBranch`), so landing never touches the default branch
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask() {
        assert_eq!(mask("a gho_x b gho_x", "gho_x"), "a <token> b <token>");
        assert_eq!(mask("nothing", ""), "nothing");
    }

    #[test]
    fn test_parse_repo() {
        assert_eq!(
            parse_repo("spacedentist/spr-test").unwrap(),
            ("spacedentist".into(), "spr-test".into())
        );
        for bad in ["spr-test", "a/b/c", "/b", "a/", "a b/c"] {
            assert!(parse_repo(bad).is_err(), "{bad}");
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
}
