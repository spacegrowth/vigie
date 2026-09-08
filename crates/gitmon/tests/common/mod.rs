//! Shared harness: fixture loading and a mock GitHub server.
//!
//! Fixtures carry `{{TS-<seconds>}}` placeholders instead of fixed timestamps,
//! substituted at load time with "<seconds> ago". That keeps every fixture
//! inside the poll window no matter when the suite runs.
#![allow(dead_code)]

use gitmon::Engine;
use mockito::{Matcher, Mock, Server, ServerGuard};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Loads a fixture and resolves its relative timestamps.
pub fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read fixture {name}: {e}"));
    resolve_timestamps(&raw)
}

/// Replaces every `{{TS-<seconds>}}` with an RFC 3339 timestamp that many
/// seconds in the past.
fn resolve_timestamps(raw: &str) -> String {
    const OPEN: &str = "{{TS-";
    let now = chrono::Utc::now().timestamp();
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let tail = &rest[start + OPEN.len()..];
        let end = tail.find("}}").expect("unterminated {{TS-...}} in fixture");
        let offset: i64 = tail[..end].parse().expect("bad {{TS-...}} offset");
        out.push_str(&iso8601(now - offset));
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    out
}

pub fn iso8601(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .expect("valid timestamp")
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// One canned HTTP response.
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: usize,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

impl Resp {
    pub fn ok(body: impl Into<String>) -> Resp {
        Resp { status: 200, body: body.into(), headers: Vec::new() }
    }

    pub fn json_array() -> Resp {
        Resp::ok("[]")
    }

    pub fn status(status: usize, body: impl Into<String>) -> Resp {
        Resp { status, body: body.into(), headers: Vec::new() }
    }

    pub fn header(mut self, name: &str, value: &str) -> Resp {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// Every endpoint one repo's poll touches. Defaults are empty arrays, so a test
/// only sets the responses it cares about.
pub struct RepoMock {
    pub owner: String,
    pub name: String,
    pub meta: Resp,
    pub commits: Resp,
    pub pulls: Resp,
    pub issues: Resp,
    pub issue_comments: Resp,
    pub pr_comments: Resp,
    /// PR number -> reviews response.
    pub reviews: Vec<(u64, Resp)>,
}

impl RepoMock {
    /// A repo that exists but has had no activity.
    pub fn quiet(owner: &str, name: &str) -> RepoMock {
        RepoMock {
            owner: owner.to_string(),
            name: name.to_string(),
            meta: Resp::ok(repo_meta(owner, name)),
            commits: Resp::json_array(),
            pulls: Resp::json_array(),
            issues: Resp::json_array(),
            issue_comments: Resp::json_array(),
            pr_comments: Resp::json_array(),
            reviews: Vec::new(),
        }
    }

    /// The full fixture set: two commits, three PRs, one issue, three comments,
    /// one review — every `EventKind`, by alice and bob.
    pub fn busy(owner: &str, name: &str) -> RepoMock {
        RepoMock {
            commits: Resp::ok(fixture("commits.json")),
            pulls: Resp::ok(fixture("pulls.json")),
            issues: Resp::ok(fixture("issues.json")),
            issue_comments: Resp::ok(fixture("issue_comments.json")),
            pr_comments: Resp::ok(fixture("pr_review_comments.json")),
            reviews: vec![
                (101, Resp::ok(fixture("reviews_101.json"))),
                (102, Resp::json_array()),
                (103, Resp::json_array()),
            ],
            ..RepoMock::quiet(owner, name)
        }
    }

    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// A synthetic repo payload, so tests do not need a fixture per repo name.
pub fn repo_meta(owner: &str, name: &str) -> String {
    format!(
        r#"{{"id":1,"name":"{name}","full_name":"{owner}/{name}",
            "owner":{{"login":"{owner}","id":1,
                      "avatar_url":"https://avatars.githubusercontent.com/u/1?v=4"}},
            "html_url":"https://github.com/{owner}/{name}","default_branch":"main"}}"#
    )
}

/// Registers every endpoint for a repo. The returned mocks must stay alive for
/// the duration of the test.
///
/// Note that dropping a `Mock` does *not* unregister it — mockito keeps it on
/// the server until [`unmount`] removes it. A test that re-points a path part
/// way through has to unmount the old mocks, or mockito may keep answering
/// from them: it prefers whichever matching mock has not had its expected hits
/// yet, and falls back to the oldest match.
pub fn mount(server: &mut ServerGuard, repo: &RepoMock) -> Vec<Mock> {
    mount_inner(server, repo, None, None)
}

/// Takes mocks off the server for good, so a later mock on the same path is the
/// only one that can answer.
pub fn unmount(mocks: &[Mock]) {
    for mock in mocks {
        mock.remove();
    }
}

/// The same, but every endpoint insists on one account's bearer token.
///
/// A request carrying anybody else's token matches no mock and comes back 501,
/// so a test that mounts two repos this way fails loudly if the engine polls
/// one of them with the wrong account's credential.
pub fn mount_as(server: &mut ServerGuard, repo: &RepoMock, token: &str) -> Vec<Mock> {
    mount_inner(server, repo, Some(token), None)
}

/// Every `since` the commits endpoint has been asked for, oldest call first.
///
/// The poll window is otherwise invisible from outside the engine: the only
/// evidence that a rescan really widened it is the timestamp that went out on
/// the wire, so tests read it back from here.
pub type SinceLog = Arc<Mutex<Vec<i64>>>;

pub fn since_log() -> SinceLog {
    Arc::new(Mutex::new(Vec::new()))
}

/// The `since` values recorded so far, as unix seconds.
pub fn recorded(log: &SinceLog) -> Vec<i64> {
    log.lock().expect("since log").clone()
}

/// Like [`mount`], but the commits endpoint records the `since` it was asked
/// for before answering, so a test can watch the poll window move.
pub fn mount_logging_since(
    server: &mut ServerGuard,
    repo: &RepoMock,
    log: &SinceLog,
) -> Vec<Mock> {
    mount_inner(server, repo, None, Some(log.clone()))
}

/// Pulls `since=<rfc3339>` out of a request's query string. `%3A` is the only
/// escape the engine's encoder produces in a timestamp.
fn since_from_query(path_and_query: &str) -> Option<i64> {
    let (_, tail) = path_and_query.split_once("since=")?;
    let raw = tail.split('&').next()?.replace("%3A", ":").replace("%3a", ":");
    chrono::DateTime::parse_from_rfc3339(&raw).ok().map(|t| t.timestamp())
}

fn mount_inner(
    server: &mut ServerGuard,
    repo: &RepoMock,
    token: Option<&str>,
    since_log: Option<SinceLog>,
) -> Vec<Mock> {
    let slug = repo.slug();
    let commits = match since_log {
        // Recording happens in the response body callback rather than in a
        // request matcher: a matcher may be consulted for mocks that do not end
        // up answering, which would log windows nobody was actually served.
        Some(log) => {
            let body = repo.commits.body.clone();
            mock_builder(server, &format!("/repos/{slug}/commits"), &repo.commits, token)
                .with_body_from_request(move |request| {
                    if let Some(since) = since_from_query(request.path_and_query()) {
                        log.lock().expect("since log").push(since);
                    }
                    body.clone().into_bytes()
                })
                .create()
        }
        None => mock_get_as(server, &format!("/repos/{slug}/commits"), &repo.commits, token),
    };
    let mut mocks = vec![
        mock_get_as(server, &format!("/repos/{slug}"), &repo.meta, token),
        commits,
        mock_get_as(server, &format!("/repos/{slug}/pulls"), &repo.pulls, token),
        mock_get_as(server, &format!("/repos/{slug}/issues"), &repo.issues, token),
        mock_get_as(
            server,
            &format!("/repos/{slug}/issues/comments"),
            &repo.issue_comments,
            token,
        ),
        mock_get_as(server, &format!("/repos/{slug}/pulls/comments"), &repo.pr_comments, token),
    ];
    for (number, response) in &repo.reviews {
        mocks.push(mock_get_as(
            server,
            &format!("/repos/{slug}/pulls/{number}/reviews"),
            response,
            token,
        ));
    }
    mocks
}

pub fn mock_get(server: &mut ServerGuard, path: &str, response: &Resp) -> Mock {
    mock_get_as(server, path, response, None)
}

/// A canned GET that answers only a request carrying `If-None-Match: <etag>`.
///
/// A request without that exact header matches no mock and comes back 501, so
/// this is how a test proves a stored ETag really went out on the wire.
pub fn mock_get_if_none_match(
    server: &mut ServerGuard,
    path: &str,
    etag: &str,
    response: &Resp,
) -> Mock {
    mock_builder(server, path, response, None).match_header("if-none-match", etag).create()
}

/// The mirror image: a canned GET that answers only a request carrying *no*
/// `If-None-Match` at all — how a test proves the engine sent no tag.
pub fn mock_get_unconditional(server: &mut ServerGuard, path: &str, response: &Resp) -> Mock {
    mock_builder(server, path, response, None)
        .match_header("if-none-match", Matcher::Missing)
        .create()
}

/// A canned GET for one page of a paginated listing, matched on `page=<n>` in
/// the query so page two can be told apart from page one.
pub fn mock_get_page(
    server: &mut ServerGuard,
    path: &str,
    page: &str,
    response: &Resp,
) -> Mock {
    mock_builder(server, path, response, None)
        .match_query(Matcher::UrlEncoded("page".into(), page.into()))
        .create()
}

/// One canned GET, optionally answered only for `Authorization: Bearer <token>`.
pub fn mock_get_as(
    server: &mut ServerGuard,
    path: &str,
    response: &Resp,
    token: Option<&str>,
) -> Mock {
    mock_builder(server, path, response, token).create()
}

/// The same mock, still unregistered, so a caller can add to it before
/// `create()` registers it — builder methods on an already-created `Mock` edit
/// a local copy the server never sees.
fn mock_builder(
    server: &mut ServerGuard,
    path: &str,
    response: &Resp,
    token: Option<&str>,
) -> Mock {
    let mut mock = server
        .mock("GET", path)
        .match_query(Matcher::Any)
        .with_status(response.status)
        .with_header("content-type", "application/json")
        .with_body(&response.body);
    if let Some(token) = token {
        mock = mock.match_header("authorization", format!("Bearer {token}").as_str());
    }
    for (name, value) in &response.headers {
        mock = mock.with_header(name, value);
    }
    mock
}

/// An engine on a throwaway database, pointed at the mock server and holding a
/// dummy credential so requests are authenticated.
pub fn engine_for(server: &ServerGuard) -> (Engine, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let engine =
        Engine::open_with_base_url(dir.path(), &server.url()).expect("engine opens");
    engine.set_token(Some("test-credential".to_string()));
    (engine, dir)
}

/// An engine on a throwaway database, pointed at the mock server and holding no
/// credential at all — the state a fresh launch is in before the app hands back
/// the keychain's tokens.
pub fn signed_out_engine(server: &ServerGuard) -> (Engine, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = Engine::open_with_base_url(dir.path(), &server.url()).expect("engine opens");
    (engine, dir)
}

pub fn server() -> ServerGuard {
    Server::new()
}
