/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use graphql_client::{GraphQLQuery, Response};
use serde::Deserialize;

use crate::{
    error::{Error, Result, ResultExt},
    git::Git,
    message::{
        build_github_body, parse_message, MessageSection, MessageSectionsMap,
    },
};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct GitHub {
    config: crate::config::Config,
    git: crate::git::Git,
    graphql_client: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub state: PullRequestState,
    pub title: String,
    pub body: Option<String>,
    pub sections: MessageSectionsMap,
    pub base: GitHubBranch,
    pub head: GitHubBranch,
    pub base_oid: git2::Oid,
    pub head_oid: git2::Oid,
    pub merge_commit: Option<git2::Oid>,
    pub reviewers: HashMap<String, ReviewStatus>,
    pub review_status: Option<ReviewStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewStatus {
    Requested,
    Approved,
    Rejected,
}

#[derive(serde::Serialize, Default, Debug)]
pub struct PullRequestUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<PullRequestState>,
}

impl PullRequestUpdate {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.body.is_none()
            && self.base.is_none()
            && self.state.is_none()
    }

    pub fn update_message(
        &mut self,
        pull_request: &PullRequest,
        message: &MessageSectionsMap,
    ) {
        let title = message.get(&MessageSection::Title);
        if title.is_some() && title != Some(&pull_request.title) {
            self.title = title.cloned();
        }

        let body = build_github_body(message);
        if pull_request.body.as_ref() != Some(&body) {
            self.body = Some(body);
        }
    }
}

#[derive(serde::Serialize, Default, Debug)]
pub struct PullRequestRequestReviewers {
    pub reviewers: Vec<String>,
    pub team_reviewers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestState {
    Open,
    Closed,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct UserWithName {
    pub login: String,
    pub name: Option<String>,
    #[serde(default)]
    pub is_collaborator: bool,
}

#[derive(Debug, Clone)]
pub struct PullRequestMergeability {
    pub base: GitHubBranch,
    pub head_oid: git2::Oid,
    pub mergeable: Option<bool>,
    pub merge_commit: Option<git2::Oid>,
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/pullrequest_query.graphql",
    response_derives = "Debug"
)]
pub struct PullRequestQuery;
type GitObjectID = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/pullrequest_mergeability_query.graphql",
    response_derives = "Debug"
)]
pub struct PullRequestMergeabilityQuery;

impl GitHub {
    pub fn new(
        config: crate::config::Config,
        git: crate::git::Git,
        graphql_client: reqwest::Client,
    ) -> Self {
        Self {
            config,
            git,
            graphql_client,
        }
    }

    async fn get_github_user(login: String) -> Result<UserWithName> {
        octocrab::instance()
            .get::<UserWithName, _, _>(format!("users/{}", login), None::<&()>)
            .await
            .map_err(Error::from)
    }

    pub async fn get_pull_request(self, number: u64) -> Result<PullRequest> {
        let GitHub {
            config,
            git,
            graphql_client,
        } = self;

        let variables = pull_request_query::Variables {
            name: config.repo(),
            owner: config.owner(),
            number: number as i64,
        };
        let request_body = PullRequestQuery::build_query(variables);
        let res = graphql_client
            .post("https://api.github.com/graphql")
            .json(&request_body)
            .send()
            .await?;
        let response_body: Response<pull_request_query::ResponseData> =
            res.json().await?;

        if let Some(errors) = response_body.errors {
            let error =
                Err(Error::new(format!("fetching PR #{number} failed")));
            return errors
                .into_iter()
                .fold(error, |err, e| err.context(e.to_string()));
        }

        let pr = response_body
            .data
            .ok_or_else(|| Error::new("failed to fetch PR"))?
            .repository
            .ok_or_else(|| Error::new("failed to find repository"))?
            .pull_request
            .ok_or_else(|| Error::new("failed to find PR"))?;

        let base = config.new_github_branch_from_ref(&pr.base_ref_name)?;
        let head = config.new_github_branch_from_ref(&pr.head_ref_name)?;

        Git::fetch_from_remote(&[&head], &config.origin_remote_name()).await?;
        Git::fetch_from_remote(&[&base], &config.upstream_remote_name())
            .await?;

        let base_oid = git.resolve_reference(base.local())?;
        let head_oid = git.resolve_reference(head.local())?;

        let mut sections = parse_message(&pr.body, MessageSection::Summary);

        let title = pr.title.trim().to_string();
        sections.insert(
            MessageSection::Title,
            if title.is_empty() {
                String::from("(untitled)")
            } else {
                title
            },
        );

        sections.insert(
            MessageSection::PullRequest,
            config.pull_request_url(number),
        );

        let reviewers: HashMap<String, ReviewStatus> = pr
            .latest_opinionated_reviews
            .iter()
            .flat_map(|all_reviews| &all_reviews.nodes)
            .flatten()
            .flatten()
            .flat_map(|review| {
                let user_name = review.author.as_ref()?.login.clone();
                let status = match review.state {
                    pull_request_query::PullRequestReviewState::APPROVED => ReviewStatus::Approved,
                    pull_request_query::PullRequestReviewState::CHANGES_REQUESTED => ReviewStatus::Rejected,
                    _ => ReviewStatus::Requested,
                };
                Some((user_name, status))
            })
            .collect();

        let review_status = match pr.review_decision {
            Some(pull_request_query::PullRequestReviewDecision::APPROVED) => Some(ReviewStatus::Approved),
            Some(pull_request_query::PullRequestReviewDecision::CHANGES_REQUESTED) => Some(ReviewStatus::Rejected),
            Some(pull_request_query::PullRequestReviewDecision::REVIEW_REQUIRED) => Some(ReviewStatus::Requested),
            _ => None,
        };

        let requested_reviewers: Vec<String> = pr.review_requests
            .iter()
            .flat_map(|x| &x.nodes)
            .flatten()
            .flatten()
            .flat_map(|x| &x.requested_reviewer)
            .flat_map(|reviewer| {
              type UserType = pull_request_query::PullRequestQueryRepositoryPullRequestReviewRequestsNodesRequestedReviewer;
              match reviewer {
                UserType::User(user) => Some(user.login.clone()),
                UserType::Team(team) => Some(format!("#{}", team.slug)),
                _ => None,
              }
            })
            .chain(reviewers.keys().cloned())
            .collect::<HashSet<String>>() // de-duplicate
            .into_iter()
            .collect();

        sections.insert(
            MessageSection::Reviewers,
            requested_reviewers.iter().fold(String::new(), |out, slug| {
                if out.is_empty() {
                    slug.to_string()
                } else {
                    format!("{}, {}", out, slug)
                }
            }),
        );

        if review_status == Some(ReviewStatus::Approved) {
            sections.insert(
                MessageSection::ReviewedBy,
                reviewers
                    .iter()
                    .filter_map(|(k, v)| {
                        if v == &ReviewStatus::Approved {
                            Some(k)
                        } else {
                            None
                        }
                    })
                    .fold(String::new(), |out, slug| {
                        if out.is_empty() {
                            slug.to_string()
                        } else {
                            format!("{}, {}", out, slug)
                        }
                    }),
            );
        }

        Ok::<_, Error>(PullRequest {
            number: pr.number as u64,
            state: match pr.state {
                pull_request_query::PullRequestState::OPEN => {
                    PullRequestState::Open
                }
                _ => PullRequestState::Closed,
            },
            title: pr.title,
            body: Some(pr.body),
            sections,
            base,
            head,
            base_oid,
            head_oid,
            reviewers,
            review_status,
            merge_commit: pr
                .merge_commit
                .and_then(|sha| git2::Oid::from_str(&sha.oid).ok()),
        })
    }

    pub async fn create_pull_request(
        &self,
        message: &MessageSectionsMap,
        base_ref_name: String,
        head_ref_name: String,
        draft: bool,
    ) -> Result<u64> {
        let number = octocrab::instance()
            .pulls(self.config.owner(), self.config.repo())
            .create(
                message
                    .get(&MessageSection::Title)
                    .unwrap_or(&String::new()),
                head_ref_name,
                base_ref_name,
            )
            .body(build_github_body(message))
            .draft(Some(draft))
            .send()
            .await?
            .number;

        Ok(number)
    }

    pub async fn update_pull_request(
        &self,
        number: u64,
        updates: PullRequestUpdate,
    ) -> Result<()> {
        octocrab::instance()
            .patch::<octocrab::models::pulls::PullRequest, _, _>(
                format!(
                    "repos/{}/{}/pulls/{}",
                    self.config.owner(),
                    self.config.repo(),
                    number
                ),
                Some(&updates),
            )
            .await?;

        Ok(())
    }

    pub async fn request_reviewers(
        &self,
        number: u64,
        reviewers: PullRequestRequestReviewers,
    ) -> Result<()> {
        #[derive(Deserialize)]
        struct Ignore {}
        let _: Ignore = octocrab::instance()
            .post(
                format!(
                    "repos/{}/{}/pulls/{}/requested_reviewers",
                    self.config.owner(),
                    self.config.repo(),
                    number
                ),
                Some(&reviewers),
            )
            .await?;

        Ok(())
    }

    pub async fn get_reviewers(
        &self,
    ) -> Result<HashMap<String, Option<String>>> {
        let github = self.clone();

        let (users, teams): (
            Vec<UserWithName>,
            octocrab::Page<octocrab::models::teams::RequestedTeam>,
        ) = futures_lite::future::try_zip(
            async {
                let users = octocrab::instance()
                    .get::<Vec<octocrab::models::User>, _, _>(
                        format!(
                            "repos/{}/{}/collaborators",
                            &github.config.owner(),
                            &github.config.repo()
                        ),
                        None::<&()>,
                    )
                    .await?;

                let user_names = futures::future::join_all(
                    users.into_iter().map(|u| GitHub::get_github_user(u.login)),
                )
                .await
                .into_iter()
                .collect::<Result<Vec<_>>>()?;

                Ok::<_, Error>(user_names)
            },
            async {
                Ok(octocrab::instance()
                    .teams(&github.config.owner())
                    .list()
                    .send()
                    .await
                    .ok()
                    .unwrap_or_default())
            },
        )
        .await?;

        let mut map = HashMap::new();

        for user in users {
            map.insert(user.login, user.name);
        }

        for team in teams {
            map.insert(format!("#{}", team.slug), team.description);
        }

        Ok::<_, Error>(map)
    }

    pub async fn get_pull_request_mergeability(
        &self,
        number: u64,
    ) -> Result<PullRequestMergeability> {
        let variables = pull_request_mergeability_query::Variables {
            name: self.config.repo(),
            owner: self.config.owner(),
            number: number as i64,
        };
        let request_body = PullRequestMergeabilityQuery::build_query(variables);
        let res = self
            .graphql_client
            .post("https://api.github.com/graphql")
            .json(&request_body)
            .send()
            .await?;
        let response_body: Response<
            pull_request_mergeability_query::ResponseData,
        > = res.json().await?;

        if let Some(errors) = response_body.errors {
            let error = Err(Error::new(format!(
                "querying PR #{number} mergeability failed"
            )));
            return errors
                .into_iter()
                .fold(error, |err, e| err.context(e.to_string()));
        }

        let pr = response_body
            .data
            .ok_or_else(|| Error::new("failed to fetch PR"))?
            .repository
            .ok_or_else(|| Error::new("failed to find repository"))?
            .pull_request
            .ok_or_else(|| Error::new("failed to find PR"))?;

        Ok::<_, Error>(PullRequestMergeability {
            base: self.config.new_github_branch_from_ref(&pr.base_ref_name)?,
            head_oid: git2::Oid::from_str(&pr.head_ref_oid)?,
            mergeable: match pr.mergeable {
                pull_request_mergeability_query::MergeableState::CONFLICTING => Some(false),
                pull_request_mergeability_query::MergeableState::MERGEABLE => Some(true),
                pull_request_mergeability_query::MergeableState::UNKNOWN => None,
                _ => None,
            },
            merge_commit: pr
            .merge_commit
            .and_then(|sha| git2::Oid::from_str(&sha.oid).ok()),
        })
    }
}

#[derive(Debug, Clone)]
pub struct GitHubBranch {
    ref_on_github: String,
    ref_local: String,
    is_master_branch: bool,
}

impl GitHubBranch {
    pub fn new_from_ref(
        ghref: &str,
        remote_name: &str,
        master_branch_name: &str,
    ) -> Result<Self> {
        let ref_on_github = if ghref.starts_with("refs/heads/") {
            ghref.to_string()
        } else if ghref.starts_with("refs/") {
            return Err(Error::new(format!(
                "Ref '{ghref}' does not refer to a branch"
            )));
        } else {
            format!("refs/heads/{ghref}")
        };

        // The branch name is `ref_on_github` with the `refs/heads/` prefix
        // (length 11) removed
        let branch_name = &ref_on_github[11..];
        let ref_local = format!("refs/remotes/{remote_name}/{branch_name}");
        let is_master_branch = branch_name == master_branch_name;

        Ok(Self {
            ref_on_github,
            ref_local,
            is_master_branch,
        })
    }

    pub fn new_from_branch_name(
        branch_name: &str,
        remote_name: &str,
        master_branch_name: &str,
    ) -> Self {
        Self {
            ref_on_github: format!("refs/heads/{branch_name}"),
            ref_local: format!("refs/remotes/{remote_name}/{branch_name}"),
            is_master_branch: branch_name == master_branch_name,
        }
    }

    pub fn on_github(&self) -> &str {
        &self.ref_on_github
    }

    pub fn local(&self) -> &str {
        &self.ref_local
    }

    pub fn is_master_branch(&self) -> bool {
        self.is_master_branch
    }

    pub fn branch_name(&self) -> &str {
        // The branch name is `ref_on_github` with the `refs/heads/` prefix
        // (length 11) removed
        &self.ref_on_github[11..]
    }
}

#[derive(Debug, Clone)]
pub enum GitHubRemote {
    /// Represents both the fork origin that a collaborator works from,
    /// and the forked upstream repo that a collaborator creates PRs to.
    FromFork {
        origin_owner: String,
        origin_repo: String,
        origin_remote_name: String,
        origin_master_ref: GitHubBranch,
        upstream_owner: String,
        upstream_repo: String,
        upstream_remote_name: String,
        upstream_master_ref: GitHubBranch,
    },
    /// Represents a single repo with no forks.
    NoFork {
        owner: String,
        repo: String,
        remote_name: String,
        master_ref: GitHubBranch,
    },
}

impl GitHubRemote {
    pub fn new(
        origin_owner: String,
        origin_repo: String,
        origin_remote_name: String,
        origin_master_branch: String,
        upstream_owner: Option<String>,
        upstream_repo: Option<String>,
        upstream_remote_name: Option<String>,
        upstream_master_branch: Option<String>,
    ) -> Self {
        let origin_master_ref = GitHubBranch::new_from_branch_name(
            &origin_master_branch,
            &origin_remote_name,
            &origin_master_branch,
        );
        // We have to check all of these optional values.
        // We don't want to get into a situation where only one exists,
        // since it's unclear what that means semantically.
        match (
            upstream_owner,
            upstream_repo,
            upstream_remote_name,
            upstream_master_branch,
        ) {
            (
                Some(owner),
                Some(repo),
                Some(remote_name),
                Some(master_branch),
            ) => Self::FromFork {
                origin_owner,
                origin_repo,
                origin_remote_name,
                origin_master_ref,
                upstream_owner: owner,
                upstream_repo: repo,
                upstream_remote_name: remote_name.clone(),
                upstream_master_ref: GitHubBranch::new_from_branch_name(
                    &master_branch,
                    &remote_name,
                    &master_branch,
                ),
            },
            _ => Self::NoFork {
                owner: origin_owner,
                repo: origin_repo,
                remote_name: origin_remote_name,
                master_ref: origin_master_ref,
            },
        }
    }

    pub fn owner(&self) -> String {
        match self {
            Self::FromFork { upstream_owner, .. } => upstream_owner.clone(),
            Self::NoFork { owner, .. } => owner.clone(),
        }
    }

    pub fn repo(&self) -> String {
        match self {
            Self::FromFork { upstream_repo, .. } => upstream_repo.clone(),
            Self::NoFork { repo, .. } => repo.clone(),
        }
    }

    pub fn origin_remote_name(&self) -> String {
        match self {
            Self::FromFork {
                origin_remote_name, ..
            } => origin_remote_name.clone(),
            Self::NoFork { remote_name, .. } => remote_name.clone(),
        }
    }

    pub fn upstream_remote_name(&self) -> String {
        match self {
            Self::FromFork {
                upstream_remote_name,
                ..
            } => upstream_remote_name.clone(),
            Self::NoFork { remote_name, .. } => remote_name.clone(),
        }
    }

    pub fn origin_master_ref(&self) -> GitHubBranch {
        match self {
            Self::FromFork {
                origin_master_ref, ..
            } => origin_master_ref.clone(),
            Self::NoFork { master_ref, .. } => master_ref.clone(),
        }
    }

    pub fn upstream_master_ref(&self) -> GitHubBranch {
        match self {
            Self::FromFork {
                upstream_master_ref,
                ..
            } => upstream_master_ref.clone(),
            Self::NoFork { master_ref, .. } => master_ref.clone(),
        }
    }

    pub fn pull_request_head(&self, branch: GitHubBranch) -> String {
        match self {
            Self::FromFork { origin_owner, .. } => {
                format!("{}:{}", origin_owner, branch.on_github())
            }
            Self::NoFork { .. } => branch.on_github().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_new_from_ref_with_branch_name() {
        let r =
            GitHubBranch::new_from_ref("foo", "github-remote", "masterbranch")
                .unwrap();
        assert_eq!(r.on_github(), "refs/heads/foo");
        assert_eq!(r.local(), "refs/remotes/github-remote/foo");
        assert_eq!(r.branch_name(), "foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_new_from_ref_with_master_branch_name() {
        let r = GitHubBranch::new_from_ref(
            "masterbranch",
            "github-remote",
            "masterbranch",
        )
        .unwrap();
        assert_eq!(r.on_github(), "refs/heads/masterbranch");
        assert_eq!(r.local(), "refs/remotes/github-remote/masterbranch");
        assert_eq!(r.branch_name(), "masterbranch");
        assert!(r.is_master_branch());
    }

    #[test]
    fn test_new_from_ref_with_ref_name() {
        let r = GitHubBranch::new_from_ref(
            "refs/heads/foo",
            "github-remote",
            "masterbranch",
        )
        .unwrap();
        assert_eq!(r.on_github(), "refs/heads/foo");
        assert_eq!(r.local(), "refs/remotes/github-remote/foo");
        assert_eq!(r.branch_name(), "foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_new_from_ref_with_master_ref_name() {
        let r = GitHubBranch::new_from_ref(
            "refs/heads/masterbranch",
            "github-remote",
            "masterbranch",
        )
        .unwrap();
        assert_eq!(r.on_github(), "refs/heads/masterbranch");
        assert_eq!(r.local(), "refs/remotes/github-remote/masterbranch");
        assert_eq!(r.branch_name(), "masterbranch");
        assert!(r.is_master_branch());
    }

    #[test]
    fn test_new_from_branch_name() {
        let r = GitHubBranch::new_from_branch_name(
            "foo",
            "github-remote",
            "masterbranch",
        );
        assert_eq!(r.on_github(), "refs/heads/foo");
        assert_eq!(r.local(), "refs/remotes/github-remote/foo");
        assert_eq!(r.branch_name(), "foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_new_from_master_branch_name() {
        let r = GitHubBranch::new_from_branch_name(
            "masterbranch",
            "github-remote",
            "masterbranch",
        );
        assert_eq!(r.on_github(), "refs/heads/masterbranch");
        assert_eq!(r.local(), "refs/remotes/github-remote/masterbranch");
        assert_eq!(r.branch_name(), "masterbranch");
        assert!(r.is_master_branch());
    }

    #[test]
    fn test_new_from_ref_with_edge_case_ref_name() {
        let r = GitHubBranch::new_from_ref(
            "refs/heads/refs/heads/foo",
            "github-remote",
            "masterbranch",
        )
        .unwrap();
        assert_eq!(r.on_github(), "refs/heads/refs/heads/foo");
        assert_eq!(r.local(), "refs/remotes/github-remote/refs/heads/foo");
        assert_eq!(r.branch_name(), "refs/heads/foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_new_from_edge_case_branch_name() {
        let r = GitHubBranch::new_from_branch_name(
            "refs/heads/foo",
            "github-remote",
            "masterbranch",
        );
        assert_eq!(r.on_github(), "refs/heads/refs/heads/foo");
        assert_eq!(r.local(), "refs/remotes/github-remote/refs/heads/foo");
        assert_eq!(r.branch_name(), "refs/heads/foo");
        assert!(!r.is_master_branch());
    }

    #[test]
    fn test_github_remote_owner_without_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            None,
            None,
            None,
            None,
        );

        assert_eq!(&remote.owner(), "acme");
    }

    #[test]
    fn test_github_remote_owner_with_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream-origin".into()),
            Some("upstream-main".into()),
        );

        assert_eq!(&remote.owner(), "upstream-acme");
    }

    #[test]
    fn test_github_remote_repo_without_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            None,
            None,
            None,
            None,
        );

        assert_eq!(&remote.repo(), "codez");
    }

    #[test]
    fn test_github_remote_repo_with_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream-origin".into()),
            Some("upstream-main".into()),
        );

        assert_eq!(&remote.repo(), "upstream-codez");
    }

    #[test]
    fn test_github_remote_origin_remote_name() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            None,
            None,
            None,
            None,
        );

        assert_eq!(&remote.origin_remote_name(), "origin");
    }

    #[test]
    fn test_github_remote_upstream_remote_name_without_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            None,
            None,
            None,
            None,
        );

        assert_eq!(&remote.upstream_remote_name(), "origin");
    }

    #[test]
    fn test_github_remote_upstream_remote_name_with_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream-origin".into()),
            Some("upstream-main".into()),
        );

        assert_eq!(&remote.upstream_remote_name(), "upstream-origin");
    }

    #[test]
    fn test_github_remote_origin_master_ref() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            None,
            None,
            None,
            None,
        );

        assert_eq!(
            remote.origin_master_ref().local(),
            "refs/remotes/origin/main"
        );
    }

    #[test]
    fn test_github_remote_upstream_master_ref_without_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            None,
            None,
            None,
            None,
        );

        assert_eq!(
            remote.upstream_master_ref().local(),
            "refs/remotes/origin/main"
        );
    }

    #[test]
    fn test_github_remote_upstream_master_ref_with_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream-origin".into()),
            Some("upstream-main".into()),
        );

        assert_eq!(
            remote.upstream_master_ref().local(),
            "refs/remotes/upstream-origin/upstream-main"
        );
    }

    #[test]
    fn test_github_remote_pull_request_head_without_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            None,
            None,
            None,
            None,
        );
        let branch = GitHubBranch::new_from_branch_name(
            "branch_name",
            "origin",
            "master",
        );

        assert_eq!(remote.pull_request_head(branch), "refs/heads/branch_name");
    }

    #[test]
    fn test_github_remote_pull_request_head_with_fork() {
        let remote = GitHubRemote::new(
            "acme".into(),
            "codez".into(),
            "origin".into(),
            "main".into(),
            Some("upstream-acme".into()),
            Some("upstream-codez".into()),
            Some("upstream-origin".into()),
            Some("upstream-main".into()),
        );
        let branch = GitHubBranch::new_from_branch_name(
            "branch_name",
            "origin",
            "master",
        );

        assert_eq!(
            remote.pull_request_head(branch),
            "acme:refs/heads/branch_name"
        );
    }
}
