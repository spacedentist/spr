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
    owner: String,
    repo: String,
    remote_name: String,
    master_ref: GitHubBranch,
    upstream_owner: Option<String>,
    upstream_repo: Option<String>,
    upstream_remote_name: Option<String>,
    upstream_master_ref: Option<GitHubBranch>,
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
        upstream_owner: Option<String>,
        upstream_repo: Option<String>,
        upstream_remote_name: Option<String>,
        upstream_master_branch: Option<String>,
        branch_prefix: String,
        require_approval: bool,
        require_test_plan: bool,
    ) -> Self {
        let master_ref = GitHubBranch::new_from_branch_name(
            &master_branch,
            &remote_name,
            &master_branch,
        );
        let upstream_master_ref =
            match (upstream_remote_name.clone(), upstream_master_branch) {
                (Some(remote), Some(branch_name)) => {
                    Some(GitHubBranch::new_from_branch_name(
                        &branch_name,
                        &remote,
                        &branch_name,
                    ))
                }
                _ => None,
            };
        Self {
            owner,
            repo,
            remote_name,
            master_ref,
            upstream_owner,
            upstream_repo,
            upstream_remote_name,
            upstream_master_ref,
            branch_prefix,
            require_approval,
            require_test_plan,
        }
    }

    /// Attempts to return the upstream owner if there is a fork.
    /// Falls back to returning the origin owner if there is no fork,
    /// since the upstream and the origin should be the same thing in this case.
    pub fn owner(&self) -> String {
        // We have to check all of these optional values.
        // We don't want to get into a situation where only one exists,
        // since it's unclear what that means semantically.
        // TODO: Extract this data clump to make this easier to deal with.
        match (
            &self.upstream_owner,
            &self.upstream_repo,
            &self.upstream_remote_name,
            &self.upstream_master_ref,
        ) {
            (Some(owner), Some(_), Some(_), Some(_)) => owner.clone(),
            _ => self.owner.clone(),
        }
    }

    /// Attempts to return the upstream repo if there is a fork.
    /// Falls back to returning the origin repo if there is no fork,
    /// since the upstream and the origin should be the same thing in this case.
    pub fn repo(&self) -> String {
        // We have to check all of these optional values.
        // We don't want to get into a situation where only one exists,
        // since it's unclear what that means semantically.
        // TODO: Extract this data clump to make this easier to deal with.
        match (
            &self.upstream_owner,
            &self.upstream_repo,
            &self.upstream_remote_name,
            &self.upstream_master_ref,
        ) {
            (Some(_), Some(repo), Some(_), Some(_)) => repo.clone(),
            _ => self.repo.clone(),
        }
    }

    /// Always returns the origin remote name no matter if there is a fork.
    pub fn origin_remote_name(&self) -> String {
        self.remote_name.clone()
    }

    /// Attempts to return the upstream remote name if there is a fork.
    /// Falls back to returning the origin remote name if there is no fork,
    /// since the upstream and the origin should be the same thing in this case.
    pub fn upstream_remote_name(&self) -> String {
        // We have to check all of these optional values.
        // We don't want to get into a situation where only one exists,
        // since it's unclear what that means semantically.
        // TODO: Extract this data clump to make this easier to deal with.
        match (
            &self.upstream_owner,
            &self.upstream_repo,
            &self.upstream_remote_name,
            &self.upstream_master_ref,
        ) {
            (Some(_), Some(_), Some(upstream_remote_name), Some(_)) => {
                upstream_remote_name.clone()
            }
            _ => self.remote_name.clone(),
        }
    }

    /// Always returns the origin "master" ref no matter if there is a fork.
    ///
    /// This currently has no uses outside of [Config],
    /// so it doesn't warrant renaming [Config.master_ref] to distinguish between the two uses.
    fn origin_master_ref(&self) -> GitHubBranch {
        self.master_ref.clone()
    }

    /// Attempts to return the upstream "master" ref if there is a fork.
    /// Falls back to returning the origin "master" ref if there is no fork,
    /// since the upstream and the origin should be the same thing in this case.
    pub fn master_ref(&self) -> GitHubBranch {
        // We have to check all of these optional values.
        // We don't want to get into a situation where only one exists,
        // since it's unclear what that means semantically.
        // TODO: Extract this data clump to make this easier to deal with.
        match (
            &self.upstream_owner,
            &self.upstream_repo,
            &self.upstream_remote_name,
            &self.upstream_master_ref,
        ) {
            (Some(_), Some(_), Some(_), Some(upstream_master_ref)) => {
                upstream_master_ref.clone()
            }
            _ => self.master_ref.clone(),
        }
    }

    /// Constructs the appropriate PR head depending on the remotes.
    /// If there is a fork, it will look like `<origin>:<branch>`.
    /// If there is no fork, it will look like `<branch>`.
    pub fn pull_request_head(&self, branch: GitHubBranch) -> String {
        // We have to check all of these optional values.
        // We don't want to get into a situation where only one exists,
        // since it's unclear what that means semantically.
        // TODO: Extract this data clump to make this easier to deal with.
        match (
            &self.upstream_owner,
            &self.upstream_repo,
            &self.upstream_remote_name,
            &self.upstream_master_ref,
        ) {
            (Some(_), Some(_), Some(_), Some(_)) => {
                format!("{}:{}", self.owner, branch.on_github())
            }
            _ => branch.on_github().to_string(),
        }
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
        let config_without_fork = crate::config::Config::new(
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
        );

        assert_eq!(&config_without_fork.owner(), "acme");

        let config_without_repo = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            None,
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_without_repo.owner(), "acme");

        let config_without_remote_name = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_without_remote_name.owner(), "acme");

        let config_without_master_branch = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_without_master_branch.owner(), "acme");

        let config_with_fork = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            Some("upstream-master".into()),
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_with_fork.owner(), "upstream-acme");
    }

    #[test]
    fn test_repo() {
        let config_without_fork = crate::config::Config::new(
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
        );

        assert_eq!(&config_without_fork.repo(), "codez");

        let config_without_repo = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            None,
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_without_repo.repo(), "codez");

        let config_without_remote_name = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_without_remote_name.repo(), "codez");

        let config_without_master_branch = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_without_master_branch.repo(), "codez");

        let config_with_fork = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            Some("upstream-master".into()),
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_with_fork.repo(), "upstream-codez");
    }

    #[test]
    fn test_upstream_remote_name() {
        let config_without_fork = crate::config::Config::new(
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
        );

        assert_eq!(&config_without_fork.upstream_remote_name(), "origin");

        let config_without_upstream_remote_name = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            None,
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            &config_without_upstream_remote_name.upstream_remote_name(),
            "origin"
        );

        let config_without_remote_name = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            &config_without_remote_name.upstream_remote_name(),
            "origin"
        );

        let config_without_master_branch = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            &config_without_master_branch.upstream_remote_name(),
            "origin"
        );

        let config_with_fork = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            Some("upstream-master".into()),
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(&config_with_fork.upstream_remote_name(), "upstream");
    }

    #[test]
    fn test_master_ref() {
        let config_without_fork = crate::config::Config::new(
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
        );

        assert_eq!(
            config_without_fork.master_ref().local(),
            "refs/remotes/origin/master"
        );

        let config_without_upstream_remote_name = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            None,
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            config_without_upstream_remote_name.master_ref().local(),
            "refs/remotes/origin/master"
        );

        let config_without_remote_name = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            config_without_remote_name.master_ref().local(),
            "refs/remotes/origin/master"
        );

        let config_without_master_branch = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            config_without_master_branch.master_ref().local(),
            "refs/remotes/origin/master"
        );

        let config_with_fork = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            Some("upstream-master".into()),
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            config_with_fork.master_ref().local(),
            "refs/remotes/upstream/upstream-master"
        );
    }

    #[test]
    fn test_pull_request_head() {
        let branch = GitHubBranch::new_from_branch_name(
            "branch_name",
            "origin",
            "master",
        );
        let config_without_fork = crate::config::Config::new(
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
        );

        assert_eq!(
            &config_without_fork.pull_request_head(branch.clone()),
            "refs/heads/branch_name"
        );

        let config_without_upstream_remote_name = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            None,
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            &config_without_upstream_remote_name
                .pull_request_head(branch.clone()),
            "refs/heads/branch_name"
        );

        let config_without_remote_name = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            None,
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            &config_without_remote_name.pull_request_head(branch.clone()),
            "refs/heads/branch_name"
        );

        let config_without_master_branch = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            None,
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            &config_without_master_branch.pull_request_head(branch.clone()),
            "refs/heads/branch_name"
        );

        let config_with_fork = crate::config::Config::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "master".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream".into()),
            Some("upstream-master".into()),
            "spr/foo/".into(),
            false,
            true,
        );

        assert_eq!(
            &config_with_fork.pull_request_head(branch.clone()),
            "acme:refs/heads/branch_name"
        );
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
