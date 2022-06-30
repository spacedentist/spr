/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::collections::HashSet;

use crate::{error::Result, github::GitHubBranch, utils::slugify};

#[derive(Clone, Debug)]
pub struct Config {
    pub owner: String,
    pub repo: String,
    pub remote_name: String,
    pub master_ref: GitHubBranch,
    pub branch_prefix: String,
    pub require_approval: bool,
    pub require_test_plan: bool,
}

impl Config {
    pub fn new(
        owner: String,
        repo: String,
        remote_name: String,
        master_branch: String,
        branch_prefix: String,
        require_approval: bool,
        require_test_plan: bool,
    ) -> Self {
        let master_ref = GitHubBranch::new_from_branch_name(
            &master_branch,
            &remote_name,
            &master_branch,
        );
        Self {
            owner,
            repo,
            remote_name,
            master_ref,
            branch_prefix,
            require_approval,
            require_test_plan,
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
            r#"^\s*https?://github.com/([\w\-]+)/([\w\-]+)/pull/(\d+)([/?#].*)?\s*$"#
        );
        let m = regex.captures(text);
        if let Some(caps) = m {
            if self.owner == caps.get(1).unwrap().as_str()
                && self.repo == caps.get(2).unwrap().as_str()
            {
                return Some(caps.get(3).unwrap().as_str().parse().unwrap());
            }
        }

        None
    }

    pub fn get_new_branch_name(
        &self,
        existing_ref_names: &HashSet<String>,
        title: &str,
    ) -> String {
        self.find_unused_branch_name(existing_ref_names, &slugify(title))
    }

    pub fn get_base_branch_name(
        &self,
        existing_ref_names: &HashSet<String>,
        title: &str,
    ) -> String {
        self.find_unused_branch_name(
            existing_ref_names,
            &format!("{}.{}", self.master_ref.branch_name(), &slugify(title)),
        )
    }

    fn find_unused_branch_name(
        &self,
        existing_ref_names: &HashSet<String>,
        slug: &str,
    ) -> String {
        let remote_name = &self.remote_name;
        let branch_prefix = &self.branch_prefix;
        let mut branch_name = format!("{branch_prefix}{slug}");
        let mut suffix = 0;

        loop {
            let remote_ref =
                format!("refs/remotes/{remote_name}/{branch_name}");

            if !existing_ref_names.contains(&remote_ref) {
                return branch_name;
            }

            suffix += 1;
            branch_name = format!("{branch_prefix}{slug}-{suffix}");
        }
    }

    pub fn new_github_branch_from_ref(
        &self,
        ghref: &str,
    ) -> Result<GitHubBranch> {
        GitHubBranch::new_from_ref(
            ghref,
            &self.remote_name,
            self.master_ref.branch_name(),
        )
    }

    pub fn new_github_branch(&self, branch_name: &str) -> GitHubBranch {
        GitHubBranch::new_from_branch_name(
            branch_name,
            &self.remote_name,
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
            "origin".into(),
            "master".into(),
            "spr/foo/".into(),
            false,
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
}
