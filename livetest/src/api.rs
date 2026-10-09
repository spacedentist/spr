//! The GitHub API, as far as the live tests need it: synchronous on the
//! outside (the tests are sequential anyway), with raw JSON values, so tests
//! can check whatever they need.

use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, bail, eyre};
use http::{Method, StatusCode};
use serde_json::{Value, json};

/// API version for GitHub's stacked pull requests API (see spr's
/// `GitHub::stacks_api_request`)
const STACKS_API_VERSION: &str = "2026-03-10";

pub struct Api {
    runtime: tokio::runtime::Runtime,
    octocrab: octocrab::Octocrab,
    pub owner: String,
    pub repo: String,
}

/// A response: status, headers and JSON body (`Null` if empty)
pub struct Response {
    pub status: StatusCode,
    pub headers: http::HeaderMap,
    pub body: Value,
}

impl Response {
    fn ok(self, what: &str) -> Result<Value> {
        if !self.status.is_success() {
            bail!("{what} failed: {} {}", self.status, self.body);
        }
        Ok(self.body)
    }
}

/// What the tests need to know about a Pull Request
#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub state: String,
    pub title: String,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
}

impl PullRequest {
    fn from_json(value: &Value) -> Result<Self> {
        let string = |pointer: &str| -> Result<String> {
            value
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(String::from)
                .ok_or_else(|| eyre!("Pull Request without {pointer}"))
        };
        Ok(PullRequest {
            number: value["number"]
                .as_u64()
                .ok_or_else(|| eyre!("Pull Request without number"))?,
            state: string("/state")?,
            title: string("/title")?,
            head_ref: string("/head/ref")?,
            head_sha: string("/head/sha")?,
            base_ref: string("/base/ref")?,
        })
    }

    pub fn is_open(&self) -> bool {
        self.state == "open"
    }
}

impl Api {
    pub fn new(token: &str, owner: &str, repo: &str) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let octocrab = {
            let _guard = runtime.enter();
            octocrab::Octocrab::builder()
                .personal_token(token.to_string())
                .build()?
        };
        Ok(Api {
            runtime,
            octocrab,
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    /// A request to `path` (relative to `https://api.github.com`)
    pub fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        api_version: Option<&'static str>,
    ) -> Result<Response> {
        self.runtime.block_on(async {
            let mut request = self.octocrab.build_request(
                http::request::Builder::new().method(method).uri(path),
                body,
            )?;
            if let Some(version) = api_version {
                request.headers_mut().insert(
                    "x-github-api-version",
                    http::HeaderValue::from_static(version),
                );
            }
            let response = self.octocrab.execute(request).await?;
            let status = response.status();
            let headers = response.headers().clone();
            let text = self.octocrab.body_to_string(response).await?;
            let body = if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap_or(Value::String(text))
            };
            Ok(Response {
                status,
                headers,
                body,
            })
        })
    }

    /// A request to `/repos/OWNER/REPO/{path}`
    fn repo_request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Response> {
        let mut path = format!("/repos/{}/{}/{path}", self.owner, self.repo);
        if path.ends_with('/') {
            // The repository itself
            path.pop();
        }
        self.request(method, &path, body, None)
    }

    pub fn default_branch(&self) -> Result<String> {
        let repo = self
            .repo_request(Method::GET, "", None)?
            .ok("Reading the repository")?;
        repo["default_branch"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| eyre!("Repository without default branch"))
    }

    /// Whether the file exists on the branch
    pub fn file_exists(&self, branch: &str, path: &str) -> Result<bool> {
        let response = self.repo_request(
            Method::GET,
            &format!("contents/{path}?ref={branch}"),
            None,
        )?;
        match response.status {
            StatusCode::NOT_FOUND => Ok(false),
            _ => response.ok("Looking for a file").map(|_| true),
        }
    }

    /// The commit a branch points to, if it exists
    pub fn branch_sha(&self, branch: &str) -> Result<Option<String>> {
        let response = self.repo_request(
            Method::GET,
            &format!("git/ref/heads/{branch}"),
            None,
        )?;
        if response.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = response.ok("Reading a branch")?;
        Ok(body["object"]["sha"].as_str().map(String::from))
    }

    pub fn create_branch(&self, branch: &str, sha: &str) -> Result<()> {
        self.repo_request(
            Method::POST,
            "git/refs",
            Some(&json!({ "ref": format!("refs/heads/{branch}"), "sha": sha })),
        )?
        .ok("Creating a branch")
        .map(|_| ())
    }

    /// Delete a branch (it's fine if it doesn't exist)
    pub fn delete_branch(&self, branch: &str) -> Result<()> {
        let response = self.repo_request(
            Method::DELETE,
            &format!("git/refs/heads/{branch}"),
            None,
        )?;
        match response.status {
            StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY => Ok(()),
            _ => response.ok("Deleting a branch").map(|_| ()),
        }
    }

    /// The branches whose names start with the prefix
    pub fn branches_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let refs = self
            .repo_request(
                Method::GET,
                &format!("git/matching-refs/heads/{prefix}"),
                None,
            )?
            .ok("Listing branches")?;
        Ok(refs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["ref"].as_str())
            .filter_map(|r| r.strip_prefix("refs/heads/"))
            .map(String::from)
            .collect())
    }

    pub fn pull_request(&self, number: u64) -> Result<PullRequest> {
        let body = self
            .repo_request(Method::GET, &format!("pulls/{number}"), None)?
            .ok("Reading a Pull Request")?;
        PullRequest::from_json(&body)
    }

    /// All open Pull Requests of the repository
    pub fn open_pull_requests(&self) -> Result<Vec<PullRequest>> {
        let mut result = Vec::new();
        for page in 1.. {
            let body = self
                .repo_request(
                    Method::GET,
                    &format!("pulls?state=open&per_page=100&page={page}"),
                    None,
                )?
                .ok("Listing Pull Requests")?;
            let prs = body.as_array().cloned().unwrap_or_default();
            if prs.is_empty() {
                break;
            }
            for pr in &prs {
                result.push(PullRequest::from_json(pr)?);
            }
        }
        Ok(result)
    }

    pub fn close_pull_request(&self, number: u64) -> Result<()> {
        self.repo_request(
            Method::PATCH,
            &format!("pulls/{number}"),
            Some(&json!({ "state": "closed" })),
        )?
        .ok("Closing a Pull Request")
        .map(|_| ())
    }

    /// The open GitHub stack the Pull Request is in, if any
    pub fn stack_of_pull_request(&self, number: u64) -> Result<Option<Value>> {
        let response = self.request(
            Method::GET,
            &format!(
                "/repos/{}/{}/stacks?pull_request={number}",
                self.owner, self.repo
            ),
            None,
            Some(STACKS_API_VERSION),
        )?;
        if response.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let stacks = response.ok("Looking up stacks")?;
        Ok(stacks
            .as_array()
            .into_iter()
            .flatten()
            .find(|stack| stack["open"].as_bool() == Some(true))
            .cloned())
    }

    pub fn unstack(&self, stack_number: u64) -> Result<()> {
        self.request(
            Method::POST,
            &format!(
                "/repos/{}/{}/stacks/{stack_number}/unstack",
                self.owner, self.repo
            ),
            None,
            Some(STACKS_API_VERSION),
        )?
        .ok("Dissolving a stack")
        .map(|_| ())
    }

    /// The login of the token's user, and the token's OAuth scopes (`None`
    /// if it has none, e.g. a fine-grained personal access token)
    pub fn token_info(&self) -> Result<(String, Option<String>)> {
        let response = self.request(Method::GET, "/user", None, None)?;
        let scopes = response
            .headers
            .get("x-oauth-scopes")
            .map(|value| value.to_str().map(String::from))
            .transpose()?;
        let user = response.ok("Reading the user")?;
        let login = user["login"]
            .as_str()
            .ok_or_else(|| eyre!("User without login"))?;
        Ok((login.to_string(), scopes))
    }
}

/// Wait until `check` returns `Some`, checking every second. GitHub takes a
/// moment to reflect some changes (e.g. Pull Request heads after a push).
pub fn wait_for<T>(
    what: &str,
    timeout: Duration,
    mut check: impl FnMut() -> Result<Option<T>>,
) -> Result<T> {
    let start = Instant::now();
    loop {
        if let Some(value) = check()? {
            return Ok(value);
        }
        if start.elapsed() > timeout {
            return Err(eyre!("Timed out waiting for {what}"));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
