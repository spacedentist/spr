/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::collections::HashSet;

use crate::{
    error::Result,
    github::{GitHubBranch, GitHubRemote},
    utils::slugify,
};

#[derive(Clone, Debug)]
pub struct Config {
    remote: GitHubRemote,
    pub branch_prefix: String,
    pub require_approval: bool,
    pub require_test_plan: bool,
}

impl Config {
    pub fn new(
        origin_owner: String,
        origin_repo: String,
        origin_remote_name: String,
        origin_master_branch: String,
        upstream_owner: Option<String>,
        upstream_repo: Option<String>,
        upstream_remote_name: Option<String>,
        upstream_master_branch: Option<String>,
        branch_prefix: String,
        require_approval: bool,
        require_test_plan: bool,
    ) -> Self {
        let remote = GitHubRemote::new(
            origin_owner,
            origin_repo,
            origin_remote_name,
            origin_master_branch,
            upstream_owner,
            upstream_repo,
            upstream_remote_name,
            upstream_master_branch,
        );
        Self {
            remote,
            branch_prefix,
            require_approval,
            require_test_plan,
        }
    }

    pub fn owner(&self) -> String {
        self.remote.owner()
    }

    pub fn repo(&self) -> String {
        self.remote.repo()
    }

    pub fn origin_remote_name(&self) -> String {
        self.remote.origin_remote_name()
    }

    pub fn upstream_remote_name(&self) -> String {
        self.remote.origin_remote_name()
    }

    pub fn master_ref(&self) -> GitHubBranch {
        self.remote.upstream_master_ref()
    }

    fn origin_master_ref(&self) -> GitHubBranch {
        self.remote.origin_master_ref()
    }

    pub fn pull_request_head(&self, branch: GitHubBranch) -> String {
        self.remote.pull_request_head(branch)
    }

    pub fn pull_request_url(&self, number: u64) -> String {
        format!(
            "https://github.com/{owner}/{repo}/pull/{number}",
            owner = &self.owner(),
            repo = &self.repo()
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
            if self.owner() == caps.get(1).unwrap().as_str()
                && self.repo() == caps.get(2).unwrap().as_str()
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
            &format!("{}.{}", self.master_ref().branch_name(), &slugify(title)),
        )
    }

    fn find_unused_branch_name(
        &self,
        existing_ref_names: &HashSet<String>,
        slug: &str,
    ) -> String {
        let remote_name = &self.origin_remote_name();
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
            &self.origin_remote_name(),
            self.origin_master_ref().branch_name(),
        )
    }

    pub fn new_github_branch(&self, branch_name: &str) -> GitHubBranch {
        GitHubBranch::new_from_branch_name(
            branch_name,
            &self.origin_remote_name(),
            self.origin_master_ref().branch_name(),
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
            None,
            None,
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        )
    }

    #[test]
    fn test_owner() {
        let gh = config_factory();

        assert_eq!(&gh.owner(), "acme");
    }

    #[test]
    fn test_repo() {
        let gh = config_factory();

        assert_eq!(&gh.repo(), "codez");
    }

    #[test]
    fn test_origin_remote_name() {
        let gh = config_factory();

        assert_eq!(&gh.origin_remote_name(), "origin");
    }

    #[test]
    fn test_upstream_remote_name_without_fork() {
        let gh = config_factory();

        assert_eq!(&gh.upstream_remote_name(), "origin");
    }

    #[test]
    fn test_upstream_remote_name_with_fork() {
        let gh = Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream-origin".into()),
            Some("upstream-master".into()),
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&gh.upstream_remote_name(), "origin");
    }

    #[test]
    fn test_master_ref() {
        let gh = config_factory();

        assert_eq!(gh.master_ref().local(), "refs/remotes/origin/master");
    }

    #[test]
    fn test_pull_request_head() {
        let gh = config_factory();
        let branch = GitHubBranch::new_from_branch_name(
            "branch_name",
            "origin",
            "master",
        );

        assert_eq!(gh.pull_request_head(branch), "refs/heads/branch_name");
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
