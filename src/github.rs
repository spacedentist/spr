//! The forge implementation for GitHub — github.com, GitHub Enterprise
//! Server, and GitHub Enterprise Cloud with data residency (`*.ghe.com`) —
//! via its REST and GraphQL APIs

use color_eyre::eyre::{Error, Result, WrapErr as _, bail, eyre};
use graphql_client::{GraphQLQuery, Response};
use serde::Deserialize;

use crate::{
    config::{Config, MergeMethod, StackingMode, auth_token_key},
    forge::{
        AsyncMergeStatus, Branch, Forge, OpenPullRequest, PullRequest,
        PullRequestMergeability, PullRequestRefs, PullRequestRequestReviewers,
        PullRequestStack, PullRequestState, PullRequestUpdate, ReviewStatus,
        Team, User, review_status_from_reviews,
    },
    git::Git,
    git_remote::GitRemote,
    message::CommitMessage,
    target::{Repository, Target},
};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone)]
pub struct GitHub {
    config: Config,
    git: Git,
    git_remote: GitRemote,
    /// The client for the REST API
    octocrab: octocrab::Octocrab,
    /// The client for the GraphQL API (at `/graphql` relative to its base)
    graphql: octocrab::Octocrab,
}

/// The base addresses of GitHub's APIs on a host
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiUrls {
    /// The base of the REST API
    pub rest: String,
    /// The base of the GraphQL API, which is at `/graphql` relative to it.
    /// (On GitHub Enterprise Server, the GraphQL API isn't below the REST
    /// API's base, and octocrab puts all paths below its base address.)
    pub graphql: String,
}

impl ApiUrls {
    /// The APIs of GitHub on the host: github.com, GitHub Enterprise Cloud
    /// with data residency (`SUBDOMAIN.ghe.com`), or else GitHub Enterprise
    /// Server
    pub fn for_host(host: &str) -> Self {
        if host == "github.com" {
            Self::at("https://api.github.com")
        } else if host.ends_with(".ghe.com") {
            Self::at(&format!("https://api.{host}"))
        } else {
            Self::enterprise_server(&format!("https://{host}"))
        }
    }

    /// Both APIs at the same base address (github.com and GHE.com)
    fn at(base: &str) -> Self {
        Self {
            rest: base.to_string(),
            graphql: base.to_string(),
        }
    }

    /// GitHub Enterprise Server at the address (e.g.
    /// `https://ghe.example.com`)
    fn enterprise_server(address: &str) -> Self {
        Self {
            rest: format!("{address}/api/v3"),
            graphql: format!("{address}/api"),
        }
    }
}

/// An octocrab client for one of GitHub's APIs
pub fn api_client(base: &str, token: &str) -> Result<octocrab::Octocrab> {
    Ok(octocrab::Octocrab::builder()
        .base_uri(base)?
        .personal_token(token.to_string())
        .build()?)
}

/// How to get a token for GitHub on the repository's host, ending with how
/// to configure it
pub fn token_instructions(repository: &Repository) -> String {
    let host = repository.host();
    let gh_host = if host == "github.com" {
        String::new()
    } else {
        format!(" --hostname {host}")
    };
    format!(
        "Create a personal access token (classic) with the scopes repo, \
         read:org and workflow, at\n\
         https://{host}/settings/tokens/new?scopes=repo,read:org,workflow&description=spr\n\
         or, if you use the GitHub CLI and are logged in there, get its token \
         with `gh auth token{gh_host}`. (Fine-grained tokens work too, but \
         spr can't check their permissions.)\n\
         To configure it for all your repositories:\n\
         git config --global {key} TOKEN",
        key = auth_token_key(repository),
    )
}

/// A Pull Request's description as GitHub returns it, with Unix line endings
/// and without surrounding whitespace, as in a commit message
fn normalize_body(body: &str) -> String {
    body.replace("\r\n", "\n").trim().to_string()
}

/// The version of the GitHub REST API that the stacked pull requests APIs
/// (stacks and asynchronous merging) require.
const STACKS_API_VERSION: &str = "2026-03-10";

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

#[allow(clippy::upper_case_acronyms)]
type URI = String;
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/gql/schema.docs.graphql",
    query_path = "src/gql/open_reviews.graphql",
    response_derives = "Debug"
)]
pub struct SearchQuery;

/// Whether an error is GitHub reporting that something doesn't exist
fn is_not_found(error: &octocrab::Error) -> bool {
    matches!(
        error,
        octocrab::Error::GitHub { source, .. }
            if source.status_code == http::StatusCode::NOT_FOUND
    )
}

impl GitHub {
    /// GitHub for the target in the configuration, with the token
    /// configured for its host
    pub fn new(config: Config, git: Git) -> Result<Self> {
        let urls = ApiUrls::for_host(config.target.repository().host());
        Self::with_api_urls(config, git, &urls)
    }

    /// GitHub with its APIs at the given addresses (e.g. a mock server in
    /// tests)
    pub fn with_api_urls(
        config: Config,
        git: Git,
        urls: &ApiUrls,
    ) -> Result<Self> {
        let repository = config.target.repository();
        let Some(token) = config.auth_tokens.get(repository) else {
            bail!(
                "There's no token for {} configured. Run `spr init`, or \
                 configure one yourself:\n{}",
                repository.host(),
                token_instructions(repository)
            );
        };
        // GitHub Enterprise doesn't have stacked pull requests (yet)
        if config.stacking_mode == StackingMode::GitHubStack
            && !repository.is_on_public_instance()
        {
            bail!(
                "The stacking mode 'github-stack' needs GitHub's stacked pull \
                 requests, which only github.com has (so far), not {}. Use \
                 'base-branches' or 'chain' (git config spr.stackingMode).",
                repository.host()
            );
        }

        let git_remote = GitRemote::new(
            git.repo().clone(),
            format!("{}.git", repository.web_url()),
            token.to_string(),
        );
        let octocrab = api_client(&urls.rest, token)?;
        let graphql = api_client(&urls.graphql, token)?;
        Ok(Self {
            config,
            git,
            git_remote,
            octocrab,
            graphql,
        })
    }

    /// Send a request to one of the stacked pull requests APIs, which
    /// require a newer API version than octocrab uses. Returns the parsed
    /// response, or `None` if the response has no body.
    async fn stacks_api_request<R, B>(
        &self,
        method: http::Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Option<R>>
    where
        R: serde::de::DeserializeOwned,
        B: serde::Serialize + ?Sized,
    {
        let octocrab = &self.octocrab;
        let uri = format!(
            "/repos/{}/{}/{}",
            self.config.owner, self.config.repo, path
        );
        let mut request = octocrab.build_request(
            http::request::Builder::new().method(method).uri(uri),
            body,
        )?;
        // `build_request` adds octocrab's default API version header, which
        // we replace.
        request.headers_mut().insert(
            "x-github-api-version",
            http::HeaderValue::from_static(STACKS_API_VERSION),
        );
        let response = octocrab.execute(request).await?;
        let response = octocrab::map_github_error(response).await?;
        let text = octocrab.body_to_string(response).await?;

        if text.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(serde_json::from_str(&text)?))
        }
    }
}

impl Forge for GitHub {
    fn for_target(&self, target: Target) -> Result<Self> {
        GitHub::new(self.config.for_target(target)?, self.git.clone())
    }

    fn config(&self) -> &Config {
        &self.config
    }

    fn git(&self) -> &Git {
        &self.git
    }

    fn remote(&self) -> &GitRemote {
        &self.git_remote
    }

    async fn get_user(&self, login: &str) -> Result<Option<User>> {
        #[derive(Deserialize)]
        struct UserWithName {
            login: String,
            name: Option<String>,
        }
        match self
            .octocrab
            .get::<UserWithName, _, _>(format!("/users/{login}"), None::<&()>)
            .await
        {
            Ok(user) => Ok(Some(User {
                login: user.login,
                name: user.name,
            })),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn get_team(&self, slug: &str) -> Result<Option<Team>> {
        match self.octocrab.teams(&self.config.owner).get(slug).await {
            Ok(team) => Ok(Some(Team { slug: team.slug })),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn get_pull_request(self, number: u64) -> Result<PullRequest> {
        let GitHub {
            config,
            git_remote,
            graphql,
            ..
        } = self;

        let variables = pull_request_query::Variables {
            name: config.repo.clone(),
            owner: config.owner.clone(),
            number: number as i64,
        };
        let request_body = PullRequestQuery::build_query(variables);
        let response_body: Response<pull_request_query::ResponseData> =
            graphql.post("/graphql", Some(&request_body)).await?;

        if let Some(errors) = response_body.errors {
            let error = Err(eyre!("fetching PR #{number} failed"));
            return errors
                .into_iter()
                .fold(error, |err, e| err.context(e.to_string()));
        }

        let pr = response_body
            .data
            .ok_or_else(|| eyre!("failed to fetch PR"))?
            .repository
            .ok_or_else(|| eyre!("failed to find repository"))?
            .pull_request
            .ok_or_else(|| eyre!("failed to find PR"))?;

        let base = config.new_branch_from_ref(&pr.base_ref_name)?;
        let head = config.new_branch_from_ref(&pr.head_ref_name)?;

        let branch_names: Vec<_> =
            [&base, &head].iter().map(|&b| b.branch_name()).collect();

        let [base_oid, head_oid] =
            git_remote.fetch_from_remote(&branch_names, &[])?[0..2]
        else {
            unreachable!();
        };

        let base_oid = base_oid
            .ok_or_else(|| eyre!("{} not found on GitHub", base.ref_name()))?;
        let head_oid = head_oid
            .ok_or_else(|| eyre!("{} not found on GitHub", head.ref_name()))?;

        // Titles and descriptions edited on GitHub can have surrounding
        // whitespace, and descriptions edited in the web interface have
        // Windows line endings. Normalise them, so they compare equal to the
        // local commit message (and `spr amend` doesn't write them into it).
        let pr_title = pr.title.trim().to_string();
        let body = normalize_body(&pr.body);

        let title = if pr_title.is_empty() {
            String::from("(untitled)")
        } else {
            pr_title.clone()
        };

        let mut message = CommitMessage::from_pull_request(title, &body);

        message.set_trailer(
            "Pull-request".to_string(),
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
            // No review decision: the repository doesn't require reviews.
            // Go by the reviews instead.
            _ => review_status_from_reviews(reviewers.values()),
        };

        // Sorted, so the resulting trailers don't change between runs of spr
        // (which would rewrite local commits for no reason).
        let requested_reviewers: BTreeSet<String> = pr.review_requests
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
            .collect();

        if !requested_reviewers.is_empty() {
            message.set_trailer(
                "Reviewers".to_string(),
                requested_reviewers
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }

        if review_status == Some(ReviewStatus::Approved) {
            let mut reviewed_by: Vec<&str> = reviewers
                .iter()
                .filter(|(_, status)| **status == ReviewStatus::Approved)
                .map(|(login, _)| login.as_str())
                .collect();
            reviewed_by.sort();
            let reviewed_by = reviewed_by.join(", ");
            if !reviewed_by.is_empty() {
                message.set_trailer("Reviewed-by".to_string(), reviewed_by);
            }
        }

        Ok::<_, Error>(PullRequest {
            number: pr.number as u64,
            state: match pr.state {
                pull_request_query::PullRequestState::OPEN => {
                    PullRequestState::Open
                }
                _ => PullRequestState::Closed,
            },
            title: pr_title,
            body: Some(body),
            message,
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

    async fn get_pull_request_refs(
        &self,
        number: u64,
    ) -> Result<PullRequestRefs> {
        let pull = self
            .octocrab
            .pulls(&self.config.owner, &self.config.repo)
            .get(number)
            .await?;

        Ok(PullRequestRefs {
            number,
            open: pull.state == Some(octocrab::models::IssueState::Open),
            head: pull.head.ref_field,
            base: pull.base.ref_field,
        })
    }

    async fn get_pull_request_mergeability(
        &self,
        number: u64,
    ) -> Result<PullRequestMergeability> {
        let variables = pull_request_mergeability_query::Variables {
            name: self.config.repo.clone(),
            owner: self.config.owner.clone(),
            number: number as i64,
        };
        let request_body = PullRequestMergeabilityQuery::build_query(variables);
        let response_body: Response<
            pull_request_mergeability_query::ResponseData,
        > = self.graphql.post("/graphql", Some(&request_body)).await?;

        if let Some(errors) = response_body.errors {
            let error = Err(eyre!("querying PR #{number} mergeability failed"));
            return errors.into_iter().fold(error, |err, e| err.wrap_err(e));
        }

        let pr = response_body
            .data
            .ok_or_else(|| eyre!("failed to fetch PR"))?
            .repository
            .ok_or_else(|| eyre!("failed to find repository"))?
            .pull_request
            .ok_or_else(|| eyre!("failed to find PR"))?;

        Ok::<_, Error>(PullRequestMergeability {
            base: self.config.new_branch_from_ref(&pr.base_ref_name)?,
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

    async fn list_open_pull_requests(&self) -> Result<Vec<OpenPullRequest>> {
        let variables = search_query::Variables {
            query: format!(
                "repo:{}/{} is:open is:pr author:@me archived:false",
                self.config.owner, self.config.repo
            ),
        };
        let request_body = SearchQuery::build_query(variables);
        let response_body: Response<search_query::ResponseData> =
            self.graphql.post("/graphql", Some(&request_body)).await?;

        let nodes = response_body
            .data
            .and_then(|data| data.search.nodes)
            .ok_or_else(|| eyre!("unexpected error"))?;

        Ok(nodes
            .into_iter()
            .flatten()
            .filter_map(|node| match node {
                search_query::SearchQuerySearchNodes::PullRequest(pr) => {
                    Some(pr)
                }
                _ => None,
            })
            .map(|pr| {
                use search_query::{
                    PullRequestReviewDecision as Decision,
                    PullRequestReviewState as State,
                };
                let review_status = match pr.review_decision {
                    Some(Decision::APPROVED) => Some(ReviewStatus::Approved),
                    Some(Decision::CHANGES_REQUESTED) => {
                        Some(ReviewStatus::Rejected)
                    }
                    Some(Decision::REVIEW_REQUIRED) => {
                        Some(ReviewStatus::Requested)
                    }
                    // Without required reviews, GitHub has no review
                    // decision: go by the reviews instead
                    _ => {
                        let reviews: Vec<ReviewStatus> = pr
                            .latest_opinionated_reviews
                            .iter()
                            .flat_map(|reviews| &reviews.nodes)
                            .flatten()
                            .flatten()
                            .map(|review| match review.state {
                                State::APPROVED => ReviewStatus::Approved,
                                State::CHANGES_REQUESTED => {
                                    ReviewStatus::Rejected
                                }
                                _ => ReviewStatus::Requested,
                            })
                            .collect();
                        review_status_from_reviews(&reviews)
                    }
                };
                OpenPullRequest {
                    title: pr.title,
                    url: pr.url,
                    review_status,
                }
            })
            .collect())
    }

    async fn list_pull_requests_with_base(
        &self,
        branch: &Branch,
    ) -> Result<Vec<u64>> {
        let pulls = self
            .octocrab
            .pulls(&self.config.owner, &self.config.repo)
            .list()
            .state(octocrab::params::State::Open)
            .base(branch.branch_name())
            .per_page(100)
            .send()
            .await?;
        let pulls = self.octocrab.all_pages(pulls).await?;
        Ok(pulls.into_iter().map(|pull| pull.number).collect())
    }

    async fn create_pull_request(
        &self,
        message: &CommitMessage,
        base_ref_name: String,
        head_ref_name: String,
        draft: bool,
    ) -> Result<u64> {
        let title = message.title();
        let title = if title.is_empty() {
            "(untitled)"
        } else {
            title
        };

        let number = self
            .octocrab
            .pulls(&self.config.owner, &self.config.repo)
            .create(title, head_ref_name, base_ref_name)
            .body(message.to_github_body())
            .draft(Some(draft))
            .send()
            .await?
            .number;

        Ok(number)
    }

    async fn update_pull_request(
        &self,
        number: u64,
        updates: PullRequestUpdate,
    ) -> Result<()> {
        self.octocrab
            .patch::<octocrab::models::pulls::PullRequest, _, _>(
                format!(
                    "/repos/{}/{}/pulls/{}",
                    self.config.owner, self.config.repo, number
                ),
                Some(&updates),
            )
            .await?;

        Ok(())
    }

    async fn request_reviewers(
        &self,
        number: u64,
        reviewers: PullRequestRequestReviewers,
    ) -> Result<()> {
        #[derive(Deserialize)]
        struct Ignore {}
        let _: Ignore = self
            .octocrab
            .post(
                format!(
                    "/repos/{}/{}/pulls/{}/requested_reviewers",
                    self.config.owner, self.config.repo, number
                ),
                Some(&reviewers),
            )
            .await?;

        Ok(())
    }

    async fn merge_pull_request(
        &self,
        number: u64,
        head_oid: git2::Oid,
        merge_method: MergeMethod,
        commit_title: &str,
        commit_message: &str,
    ) -> Result<Option<git2::Oid>> {
        let merge = self
            .octocrab
            .pulls(&self.config.owner, &self.config.repo)
            .merge(number)
            .method(match merge_method {
                MergeMethod::Squash => {
                    octocrab::params::pulls::MergeMethod::Squash
                }
                MergeMethod::Merge => {
                    octocrab::params::pulls::MergeMethod::Merge
                }
            })
            .title(commit_title)
            .message(commit_message)
            .sha(head_oid.to_string())
            .send()
            .await?;
        if !merge.merged {
            return Err(eyre!(
                "GitHub Pull Request merge failed: {}",
                merge.message.unwrap_or_default()
            ));
        }
        merge
            .sha
            .map(|sha| git2::Oid::from_str(&sha))
            .transpose()
            .map_err(Error::from)
    }

    fn supports_stacks(&self) -> bool {
        self.config.target.repository().is_on_public_instance()
    }

    async fn find_pull_request_stack(
        &self,
        number: u64,
    ) -> Result<Option<PullRequestStack>> {
        let stacks: Vec<PullRequestStack> = self
            .stacks_api_request(
                http::Method::GET,
                &format!("stacks?pull_request={number}"),
                None::<&()>,
            )
            .await?
            .unwrap_or_default();

        Ok(stacks.into_iter().find(|stack| stack.open))
    }

    async fn create_pull_request_stack(
        &self,
        numbers: &[u64],
    ) -> Result<PullRequestStack> {
        self.stacks_api_request(
            http::Method::POST,
            "stacks",
            Some(&serde_json::json!({ "pull_requests": numbers })),
        )
        .await?
        .ok_or_else(|| eyre!("Creating a Pull Request stack failed"))
    }

    async fn add_to_pull_request_stack(
        &self,
        stack_number: u64,
        numbers: &[u64],
    ) -> Result<()> {
        self.stacks_api_request::<serde_json::Value, _>(
            http::Method::POST,
            &format!("stacks/{stack_number}/add"),
            Some(&serde_json::json!({ "pull_requests": numbers })),
        )
        .await?;
        Ok(())
    }

    async fn unstack_pull_request_stack(
        &self,
        stack_number: u64,
    ) -> Result<()> {
        self.stacks_api_request::<serde_json::Value, _>(
            http::Method::POST,
            &format!("stacks/{stack_number}/unstack"),
            None::<&()>,
        )
        .await?;
        Ok(())
    }

    async fn merge_pull_request_async(
        &self,
        number: u64,
        head_oid: git2::Oid,
        merge_method: MergeMethod,
        commit_title: &str,
        commit_message: &str,
    ) -> Result<AsyncMergeStatus> {
        self.stacks_api_request(
            http::Method::PUT,
            &format!("pulls/{number}/merge-async"),
            Some(&serde_json::json!({
                "sha": head_oid.to_string(),
                "merge_method": merge_method.to_string(),
                "commit_title": commit_title,
                "commit_message": commit_message,
            })),
        )
        .await?
        .ok_or_else(|| {
            eyre!("Requesting merge of Pull Request #{number} failed")
        })
    }

    async fn get_async_merge_status(
        &self,
        number: u64,
        uuid: &str,
    ) -> Result<AsyncMergeStatus> {
        self.stacks_api_request(
            http::Method::GET,
            &format!("pulls/{number}/merge-async/{uuid}"),
            None::<&()>,
        )
        .await?
        .ok_or_else(|| {
            eyre!("Getting merge status of Pull Request #{number} failed")
        })
    }
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;
    use crate::test_utils::TestRepo;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config(
        target: &str,
        stacking_mode: StackingMode,
        token: Option<&str>,
    ) -> Config {
        let target: Target = target.parse().unwrap();
        let mut tokens = crate::config::AuthTokens::default();
        if let Some(token) = token {
            tokens.set(target.repository(), token.into());
        }
        Config::new(
            target,
            "spr/test/".into(),
            tokens,
            false,
            MergeMethod::Squash,
            stacking_mode,
            false,
            false,
        )
        .unwrap()
    }

    #[test]
    fn test_api_urls() {
        assert_eq!(
            ApiUrls::for_host("github.com"),
            ApiUrls {
                rest: "https://api.github.com".into(),
                graphql: "https://api.github.com".into(),
            }
        );
        // GitHub Enterprise Cloud with data residency
        assert_eq!(
            ApiUrls::for_host("acme.ghe.com"),
            ApiUrls {
                rest: "https://api.acme.ghe.com".into(),
                graphql: "https://api.acme.ghe.com".into(),
            }
        );
        // GitHub Enterprise Server
        assert_eq!(
            ApiUrls::for_host("ghe.example.com:8443"),
            ApiUrls {
                rest: "https://ghe.example.com:8443/api/v3".into(),
                graphql: "https://ghe.example.com:8443/api".into(),
            }
        );
    }

    /// On GitHub Enterprise Server, REST requests go below `/api/v3`, and
    /// GraphQL requests to `/api/graphql`, with the token for the host
    #[tokio::test]
    async fn test_enterprise_server_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/acme/codez/pulls"))
            .and(query_param("base", "main"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "data": { "search": { "nodes": [] } } }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let repo = TestRepo::new();
        let gh = GitHub::with_api_urls(
            config(
                "github://ghe.example.com/acme/codez:main",
                StackingMode::Chain,
                Some("secret"),
            ),
            repo.git.clone(),
            &ApiUrls::enterprise_server(&server.uri()),
        )
        .unwrap();
        let main = gh.config.new_branch("main");
        assert_eq!(
            gh.list_pull_requests_with_base(&main).await.unwrap(),
            Vec::<u64>::new()
        );
        assert!(gh.list_open_pull_requests().await.unwrap().is_empty());
        assert!(!gh.supports_stacks());
        // The mock server checks the expected requests when it's dropped
    }

    /// The stacks API gets the API version it needs
    #[tokio::test]
    async fn test_stacks_api_version() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/codez/stacks"))
            .and(query_param("pull_request", "12"))
            .and(header("x-github-api-version", STACKS_API_VERSION))
            .and(header("authorization", "Bearer secret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let repo = TestRepo::new();
        let gh = GitHub::with_api_urls(
            config(
                "github:acme/codez:main",
                StackingMode::GitHubStack,
                Some("secret"),
            ),
            repo.git.clone(),
            &ApiUrls::at(&server.uri()),
        )
        .unwrap();
        assert!(gh.supports_stacks());
        assert!(gh.find_pull_request_stack(12).await.unwrap().is_none());
    }

    #[test]
    fn test_no_github_stack_on_enterprise() {
        let repo = TestRepo::new();
        let error = GitHub::new(
            config(
                "github://ghe.example.com/acme/codez:main",
                StackingMode::GitHubStack,
                Some("secret"),
            ),
            repo.git.clone(),
        )
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("only github.com has (so far)"),
            "{error}"
        );
    }

    #[test]
    fn test_missing_token() {
        let repo = TestRepo::new();
        let error = GitHub::new(
            config(
                "github://ghe.example.com/acme/codez:main",
                StackingMode::Chain,
                None,
            ),
            repo.git.clone(),
        )
        .err()
        .unwrap()
        .to_string();
        assert!(
            error.starts_with("There's no token for ghe.example.com"),
            "{error}"
        );
        assert!(
            error.contains(
                "https://ghe.example.com/settings/tokens/new?scopes=repo,\
                 read:org,workflow&description=spr\n"
            ),
            "{error}"
        );
        assert!(
            error.contains("`gh auth token --hostname ghe.example.com`"),
            "{error}"
        );
        assert!(
            error.ends_with(
                "\ngit config --global spr.auth.github.ghe.example.com.token \
                 TOKEN"
            ),
            "{error}"
        );

        // On github.com
        let instructions =
            token_instructions(&"github:acme/codez".parse().unwrap());
        assert!(instructions.contains("`gh auth token`"), "{instructions}");
        assert!(
            instructions.ends_with("spr.auth.github.token TOKEN"),
            "{instructions}"
        );
    }

    #[test]
    fn test_normalize_body() {
        // As edited in GitHub's web interface
        let body = "First paragraph.\r\n\r\n- one\r\n- two\r\n";
        assert_eq!(normalize_body(body), "First paragraph.\n\n- one\n- two");

        // The same as the body of the local commit message
        let message =
            CommitMessage::parse("Title\n\nFirst paragraph.\n\n- one\n- two\n");
        assert_eq!(normalize_body(body), message.body());
    }
}
