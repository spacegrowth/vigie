//! Response shapes, trimmed to the fields the engine reads.
//!
//! Every field the engine needs is `Option` where GitHub may legitimately omit
//! it (deleted users, unmerged PRs, pending reviews), so a partial payload
//! degrades instead of failing the whole poll.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub login: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoResponse {
    pub name: String,
    pub owner: User,
    pub html_url: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitUser {
    pub name: Option<String>,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommitPayload {
    pub message: String,
    pub author: Option<GitUser>,
    pub committer: Option<GitUser>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub html_url: String,
    pub commit: CommitPayload,
    /// The GitHub account matched to the git author, when there is one.
    pub author: Option<User>,
    pub committer: Option<User>,
    /// Single-commit endpoint only; the list endpoint omits both of these.
    pub stats: Option<CommitStats>,
    pub files: Option<Vec<DiffEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommitStats {
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
}

/// One changed file. GitHub calls the path `filename`; the contract calls it
/// `path`. `patch` is absent for binary and very large files.
#[derive(Debug, Clone, Deserialize)]
pub struct DiffEntry {
    pub filename: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub patch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub user: Option<User>,
    pub body: Option<String>,
    pub state: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    /// GitHub sends `false` for an ordinary PR and omits the key entirely from
    /// an issue-shaped payload, so a missing key means "not a draft".
    #[serde(default)]
    pub draft: bool,
    /// Present on the single-PR endpoint, absent from the list endpoint.
    pub merged_by: Option<User>,
    /// Whoever has been asked to review. Absent from some payloads (and always
    /// from an issue-shaped one), so `default` rather than `Option`: "nobody
    /// was asked" and "the key was not sent" mean the same thing to auto-watch.
    #[serde(default)]
    pub requested_reviewers: Vec<User>,
    pub head: Option<GitRef>,
    pub base: Option<GitRef>,
    /// Single-PR endpoint only; "Pull Request Simple" (the list shape) omits these.
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub changed_files: Option<u32>,
}

/// The `head`/`base` object of a pull request; only its branch name is read.
#[derive(Debug, Clone, Deserialize)]
pub struct GitRef {
    #[serde(rename = "ref")]
    pub ref_name: Option<String>,
}

/// Marker proving an `/issues` entry is really a pull request.
#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestRef {
    #[allow(dead_code)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub user: Option<User>,
    pub body: Option<String>,
    pub state: Option<String>,
    pub created_at: String,
    /// Set when this "issue" is actually a PR, which the contract excludes.
    pub pull_request: Option<PullRequestRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueComment {
    pub id: u64,
    pub html_url: String,
    pub body: Option<String>,
    pub user: Option<User>,
    pub created_at: String,
    /// `.../repos/{owner}/{repo}/issues/{number}` — the number is parsed off the end.
    pub issue_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewComment {
    pub id: u64,
    pub html_url: String,
    pub body: Option<String>,
    pub user: Option<User>,
    pub created_at: String,
    pub path: Option<String>,
    /// Null on an outdated comment, where `original_line` still points at the
    /// line it was written against.
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    /// `.../repos/{owner}/{repo}/pulls/{number}`
    pub pull_request_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Review {
    pub id: u64,
    pub html_url: String,
    pub body: Option<String>,
    pub user: Option<User>,
    pub state: String,
    /// Null for a pending review, which the engine skips.
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserRepo {
    pub name: String,
    pub owner: User,
    pub pushed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchIssueItem {
    /// `https://api.github.com/repos/{owner}/{name}`
    pub repository_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchIssuesResponse {
    pub items: Vec<SearchIssueItem>,
}

/// A device-flow endpoint answers with either its payload or an OAuth error
/// object — and GitHub returns the error with HTTP 200, so the discriminator is
/// the body, not the status. `Error` is tried first because its `error` field is
/// required and never appears in a success payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OAuthResult<T> {
    Error(OAuthError),
    Ok(T),
}

/// `{"error": "...", "error_description": "...", "error_uri": "..."}`.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthError {
    pub error: String,
    pub error_description: Option<String>,
}

impl OAuthError {
    /// GitHub's human-readable message, falling back to the machine code.
    pub fn message(&self) -> String {
        match self.error_description.as_deref().map(str::trim) {
            Some(description) if !description.is_empty() => description.to_string(),
            _ => self.error.clone(),
        }
    }
}

/// The success payload of `POST /login/oauth/access_token`.
#[derive(Debug, Clone, Deserialize)]
pub struct AccessTokenResponse {
    pub access_token: String,
}
