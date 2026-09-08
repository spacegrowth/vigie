//! Blocking GitHub REST client: auth headers, pagination, and error mapping.
//!
//! The client owns no database handle and the engine never holds the store lock
//! across a call into here.

pub mod models;

use crate::error::{EngineError, ErrorKind, Result};
use serde::de::DeserializeOwned;
use std::sync::{Mutex, RwLock};

/// `Link: rel="next"` is followed at most this many times per endpoint per poll.
pub const MAX_PAGES: usize = 10;

pub const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Where the OAuth device flow lives. Not the REST API host.
pub const DEFAULT_OAUTH_BASE_URL: &str = "https://github.com";

/// Sent on every request, API and OAuth alike.
pub const USER_AGENT: &str = concat!("Vigie/", env!("CARGO_PKG_VERSION"), " (macOS)");
const API_VERSION: &str = "2022-11-28";
const ACCEPT: &str = "application/vnd.github+json";

/// What a conditional GET came back with.
///
/// A `304 Not Modified` is the whole point of sending `If-None-Match`: GitHub
/// answers with an empty body and charges nothing against the rate limit, so an
/// unchanged listing costs a round trip and no budget at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conditional<T> {
    /// GitHub matched the ETag that went out: nothing changed, and there is no
    /// body to read.
    NotModified,
    /// A real response. `etag` is the tag to send next time (absent when GitHub
    /// sent none), and `poll_interval` is the `X-Poll-Interval` hint, in
    /// seconds, when the endpoint offered one.
    Fresh { value: T, etag: Option<String>, poll_interval: Option<u64> },
}

/// One list read: the items kept, and whether [`MAX_PAGES`] cut the walk short
/// with GitHub still offering a `rel="next"`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

/// A rate-limit reading captured off one response: `x-ratelimit-remaining`
/// beside the two headers that give it meaning, `x-ratelimit-limit` (the
/// budget size — 5,000/hour for a token, smaller for unauthenticated
/// requests) and `x-ratelimit-reset` (unix seconds the budget refills). The
/// three are read together off the same response so they never drift apart:
/// a `remaining` from one account paired with another's `reset_at` would say
/// something true about neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitReading {
    pub remaining: u32,
    /// `None` on the rare response that carries `x-ratelimit-remaining` but
    /// not `x-ratelimit-limit` — callers fall back to GitHub's documented
    /// default (5,000) rather than treating it as unknown.
    pub limit: Option<u32>,
    /// `None` when GitHub sent no `x-ratelimit-reset` alongside `-remaining`.
    pub reset_at: Option<i64>,
}

/// A blocking GitHub client. `Send + Sync`; share one per process.
pub struct GitHubClient {
    http: reqwest::blocking::Client,
    /// Root of the REST API. Overridden in tests to point at a mock server.
    pub base_url: String,
    /// Root of the OAuth device-flow endpoints, which live on `github.com`
    /// rather than `api.github.com`. Overridden in tests the same way.
    pub oauth_base_url: String,
    /// In-memory only. Never written to the store, never logged.
    credential: RwLock<Option<String>>,
    last_rate_limit: Mutex<Option<RateLimitReading>>,
}

impl GitHubClient {
    pub fn new() -> Self {
        GitHubClient::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: &str) -> Self {
        GitHubClient::with_base_urls(base_url, DEFAULT_OAUTH_BASE_URL)
    }

    /// Both roots overridden; tests point them at the same mock server.
    pub fn with_base_urls(base_url: &str, oauth_base_url: &str) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()
            .expect("blocking HTTP client builds");
        GitHubClient {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            oauth_base_url: oauth_base_url.trim_end_matches('/').to_string(),
            credential: RwLock::new(None),
            last_rate_limit: Mutex::new(None),
        }
    }

    /// Stored in memory for the life of the process only.
    pub fn set_token(&self, token: Option<String>) {
        *self.credential.write().expect("credential lock") = normalize_token(token);
    }

    /// The same client aimed at the same hosts, but authenticating as somebody
    /// else — one per account, so a poll can send each repo's own token.
    ///
    /// The underlying `reqwest` client is shared (it is an `Arc` inside), so the
    /// connection pool and the timeout are the one this was cloned from; only
    /// the credential and the rate-limit reading are the clone's own. That
    /// separation is the point: GitHub budgets rate limits per token, so two
    /// accounts must not overwrite each other's `x-ratelimit-remaining`.
    pub fn with_token(&self, token: Option<String>) -> GitHubClient {
        GitHubClient {
            http: self.http.clone(),
            base_url: self.base_url.clone(),
            oauth_base_url: self.oauth_base_url.clone(),
            credential: RwLock::new(normalize_token(token)),
            last_rate_limit: Mutex::new(None),
        }
    }

    pub fn has_token(&self) -> bool {
        self.credential.read().expect("credential lock").is_some()
    }

    /// `x-ratelimit-remaining` from the most recent response, if any.
    pub fn rate_limit_remaining(&self) -> Option<u32> {
        self.rate_limit().map(|r| r.remaining)
    }

    /// The full rate-limit reading — remaining, limit and reset time — from
    /// the most recent response that carried one. `None` before this
    /// credential's first response, or if every response so far has been a
    /// 304 (which carries no live reading; see [`record_rate_limit`]).
    ///
    /// [`record_rate_limit`]: Self::record_rate_limit
    pub fn rate_limit(&self) -> Option<RateLimitReading> {
        *self.last_rate_limit.lock().expect("rate limit lock")
    }

    /// The absolute URL a relative path resolves to. Public because the ETag
    /// cache is keyed by the full URL a request goes to, and the poller has to
    /// name that key without sending the request.
    pub fn url_for(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else if let Some(rest) = path.strip_prefix('/') {
            format!("{}/{}", self.base_url, rest)
        } else {
            format!("{}/{}", self.base_url, path)
        }
    }

    /// Absolute URL for an OAuth device-flow path, e.g. `/login/device/code`.
    pub fn oauth_url(&self, path: &str) -> String {
        format!("{}/{}", self.oauth_base_url, path.trim_start_matches('/'))
    }

    /// POST an `application/x-www-form-urlencoded` body and read JSON back.
    ///
    /// `url` may be absolute (the device-flow endpoints are on `github.com`, not
    /// `api.github.com`) or a path relative to [`base_url`](Self::base_url).
    /// No `Authorization` header is sent: these endpoints authenticate from the
    /// form fields, and an unrelated token must not leak into them.
    pub fn post_form<T: DeserializeOwned>(&self, url: &str, fields: &[(&str, &str)]) -> Result<T> {
        let url = self.url_for(url);
        let resp = self
            .http
            .post(&url)
            .header("Accept", "application/json")
            .form(fields)
            .send()
            .map_err(|e| EngineError::network(e.to_string()))?;
        // Deliberately not recorded against the REST rate-limit budget: the
        // OAuth host does not send `x-ratelimit-*` headers.
        if !resp.status().is_success() {
            return Err(map_error_response(resp));
        }
        let body = resp.text().map_err(|e| EngineError::network(e.to_string()))?;
        parse_json(&body, &url)
    }

    /// GET one URL, mapping transport and HTTP failures onto `EngineError`.
    ///
    /// With `etag` set the request carries `If-None-Match`, and `Ok(None)` is
    /// GitHub answering `304 Not Modified`: the stored tag still describes the
    /// response, the body is empty, and the request cost nothing against the
    /// rate limit. Without an `etag` a 304 cannot happen — there is nothing for
    /// GitHub to match against — so every other caller reads through
    /// [`send`](Self::send) and never sees the `None`.
    ///
    /// The rate-limit headers are read from every response *except* a 304.
    /// GitHub does not charge for a 304 and the `x-ratelimit-remaining` it
    /// sends beside one describes a budget this request did not spend, so
    /// recording it would overwrite the live reading with a stale number.
    fn send_conditional(
        &self,
        url: &str,
        etag: Option<&str>,
    ) -> Result<Option<reqwest::blocking::Response>> {
        let mut req = self
            .http
            .get(url)
            .header("Accept", ACCEPT)
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(etag) = etag {
            req = req.header("If-None-Match", etag);
        }
        if let Some(secret) = self.credential.read().expect("credential lock").as_ref() {
            req = req.header("Authorization", format!("Bearer {secret}"));
        }
        let resp = req.send().map_err(|e| EngineError::network(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(None);
        }
        self.record_rate_limit(&resp);
        if resp.status().is_success() {
            return Ok(Some(resp));
        }
        Err(map_error_response(resp))
    }

    /// An unconditional GET. A 304 here would mean GitHub matched a tag nothing
    /// sent, so it is reported as an unreadable response rather than silently
    /// turned into an empty body.
    fn send(&self, url: &str) -> Result<reqwest::blocking::Response> {
        self.send_conditional(url, None)?.ok_or_else(|| {
            EngineError::network(format!("{url} answered 304 to an unconditional request"))
        })
    }

    /// Reads `x-ratelimit-remaining` off every response that carries one,
    /// together with `-limit` and `-reset` from the *same* response — never
    /// called on a 304 (see [`send_conditional`](Self::send_conditional)),
    /// whose rate-limit headers describe a budget this request did not spend.
    fn record_rate_limit(&self, resp: &reqwest::blocking::Response) {
        if let Some(remaining) = header_u32(resp.headers(), "x-ratelimit-remaining") {
            let limit = header_u32(resp.headers(), "x-ratelimit-limit");
            let reset_at = header_i64(resp.headers(), "x-ratelimit-reset");
            *self.last_rate_limit.lock().expect("rate limit lock") =
                Some(RateLimitReading { remaining, limit, reset_at });
        }
    }

    /// GET a single JSON document.
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.url_for(path);
        let resp = self.send(&url)?;
        let body = resp.text().map_err(|e| EngineError::network(e.to_string()))?;
        parse_json(&body, &url)
    }

    /// GET a single JSON document, sending `etag` as `If-None-Match` when one
    /// is known. `Conditional::NotModified` means the caller's stored copy is
    /// still current.
    pub fn get_conditional<T: DeserializeOwned>(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<T>> {
        let url = self.url_for(path);
        let Some(resp) = self.send_conditional(&url, etag)? else {
            return Ok(Conditional::NotModified);
        };
        let etag = header_string(resp.headers(), "etag");
        let poll_interval = header_u64(resp.headers(), "x-poll-interval");
        let body = resp.text().map_err(|e| EngineError::network(e.to_string()))?;
        Ok(Conditional::Fresh { value: parse_json(&body, &url)?, etag, poll_interval })
    }

    /// GET every page of a list endpoint, following `Link: rel="next"`.
    pub fn get_paged<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        Ok(self.get_paged_capped(path, |_| true)?.0)
    }

    /// Like [`get_paged`](Self::get_paged), but stops at the first item for which
    /// `keep` returns false. Used for the `sort=updated&direction=desc` PR walk,
    /// which stops as soon as it sees a PR older than the poll window.
    pub fn get_paged_while<T, F>(&self, path: &str, keep: F) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
        F: FnMut(&T) -> bool,
    {
        Ok(self.get_paged_capped(path, keep)?.0)
    }

    /// The paged walk every other pagination helper is built on, returning what
    /// it read *and* whether [`MAX_PAGES`] cut it short — a `true` flag means
    /// GitHub still offered a `rel="next"` when the cap ran out, so the list is
    /// incomplete. A poll ignores that (its window is small and the newest page
    /// is the one that matters); a backfill reports it, because a truncated
    /// historical window silently loses events nothing will ever fetch again.
    ///
    /// Stopping early on `keep` is *not* truncation: that walk found the edge
    /// of the window it was asked for and read everything inside it.
    pub fn get_paged_capped<T, F>(&self, path: &str, keep: F) -> Result<(Vec<T>, bool)>
    where
        T: DeserializeOwned,
        F: FnMut(&T) -> bool,
    {
        match self.get_paged_conditional(path, None, keep)? {
            Conditional::Fresh { value, .. } => Ok((value.items, value.truncated)),
            // Unreachable: no `If-None-Match` went out, so GitHub had nothing
            // to match and `send_conditional` would have raised the 304 itself.
            Conditional::NotModified => Err(EngineError::network(format!(
                "{path} answered 304 to an unconditional request"
            ))),
        }
    }

    /// The paged walk, with the first page sent conditionally.
    ///
    /// The ETag goes out on page one *only*. An ETag identifies one URL, and
    /// the `rel="next"` pages are different URLs whose tags nothing stored — so
    /// asking them conditionally would be sending a tag that cannot match. A
    /// 304 on page one short-circuits the whole listing to
    /// [`Conditional::NotModified`]: page two is never requested, because a
    /// listing GitHub says is unchanged has no changed pages under it.
    ///
    /// The ETag handed back is page one's, for the same reason: it is the only
    /// one the next poll can use.
    pub fn get_paged_conditional<T, F>(
        &self,
        path: &str,
        etag: Option<&str>,
        mut keep: F,
    ) -> Result<Conditional<Paged<T>>>
    where
        T: DeserializeOwned,
        F: FnMut(&T) -> bool,
    {
        let mut out: Vec<T> = Vec::new();
        let mut url = self.url_for(path);
        let mut truncated = false;
        let mut first_etag: Option<String> = None;
        let mut poll_interval: Option<u64> = None;
        for page in 0..MAX_PAGES {
            let sent_etag = if page == 0 { etag } else { None };
            let Some(resp) = self.send_conditional(&url, sent_etag)? else {
                return Ok(Conditional::NotModified);
            };
            if page == 0 {
                first_etag = header_string(resp.headers(), "etag");
            }
            // `Option`'s ordering puts `None` below every `Some`, so this keeps
            // the largest hint any page carried.
            poll_interval = poll_interval.max(header_u64(resp.headers(), "x-poll-interval"));
            let next = next_link(resp.headers());
            let body = resp.text().map_err(|e| EngineError::network(e.to_string()))?;
            let items: Vec<T> = parse_json(&body, &url)?;
            for item in items {
                if !keep(&item) {
                    return Ok(Conditional::Fresh {
                        value: Paged { items: out, truncated: false },
                        etag: first_etag,
                        poll_interval,
                    });
                }
                out.push(item);
            }
            match next {
                // A next link on the last page the cap allows is the cap
                // biting: there was more and it was not read.
                Some(next_url) => {
                    if page + 1 == MAX_PAGES {
                        truncated = true;
                    }
                    url = next_url;
                }
                None => break,
            }
        }
        Ok(Conditional::Fresh {
            value: Paged { items: out, truncated },
            etag: first_etag,
            poll_interval,
        })
    }
}

impl Default for GitHubClient {
    fn default() -> Self {
        GitHubClient::new()
    }
}

/// A blank or whitespace-only token is no token at all, not an empty `Bearer`.
fn normalize_token(token: Option<String>) -> Option<String> {
    token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

fn parse_json<T: DeserializeOwned>(body: &str, url: &str) -> Result<T> {
    serde_json::from_str(body).map_err(|e| {
        EngineError::new(ErrorKind::Network, format!("unreadable response from {url}: {e}"))
    })
}

/// Maps a non-2xx response onto the contract's error kinds.
fn map_error_response(resp: reqwest::blocking::Response) -> EngineError {
    let status = resp.status().as_u16();
    let remaining = header_u32(resp.headers(), "x-ratelimit-remaining");
    let reset_at = header_i64(resp.headers(), "x-ratelimit-reset");
    // Read before `response_message` consumes the response.
    let sso_authorize_url = header_str(resp.headers(), "x-github-sso")
        .and_then(|v| v.split("url=").nth(1).map(|u| u.trim().to_string()));
    let message = response_message(resp).unwrap_or_else(|| format!("GitHub returned {status}"));

    match status {
        401 => EngineError::auth(message),
        404 => EngineError::not_found(message),
        403 | 429 if remaining == Some(0) => EngineError::rate_limited(message, reset_at),
        // A 429 without an exhausted primary budget is a secondary rate limit.
        429 => EngineError::rate_limited(message, reset_at),
        // Any other 403 is a permission problem, not a budget one — and NOT a
        // sign-in problem, which is how the app used to word every 403. The
        // two that actually happen:
        //   * an organisation enforcing SAML single sign-on: the token is
        //     valid, but has not been authorised for that org. GitHub says so
        //     in an `x-github-sso` header carrying the URL that authorises it.
        //   * a token without access to that repository at all.
        // Both are actionable, and neither is fixed by signing in again.
        403 => {
            match sso_authorize_url.as_deref() {
                Some(url) => EngineError::auth(format!(
                    "That organisation requires single sign-on. Your sign-in is fine — the token \
                     needs authorising for it: {url}"
                )),
                // An organisation with OAuth app access restrictions — on by
                // default for every new org — blocks this app until an owner
                // approves it. GitHub says so in prose; the fix is one click,
                // so point at it rather than leaving the reader to hunt
                // through settings.
                None if message.contains("OAuth App access restrictions")
                    || message.contains("third-party access")
                    || message.contains("restricting-access") =>
                {
                    EngineError::auth(format!(
                        "That organisation has not approved Vigie yet — your sign-in is fine. \
                         Request or grant access under Authorized OAuth Apps, then add the repo \
                         again: https://github.com/settings/connections/applications"
                    ))
                }
                None => EngineError::auth(format!(
                    "That account is signed in, but cannot see this. GitHub said: {message}"
                )),
            }
        }
        s if (500..=599).contains(&s) => EngineError::network(message),
        _ => EngineError::invalid(message),
    }
}

/// GitHub error bodies are `{"message": "...", ...}`.
fn response_message(resp: reqwest::blocking::Response) -> Option<String> {
    let status = resp.status().as_u16();
    let body = resp.text().ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
    let message = parsed.get("message")?.as_str()?.to_string();
    Some(format!("{message} (HTTP {status})"))
}

fn header_str<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn header_string(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    header_str(headers, name).map(str::to_string)
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    header_str(headers, name)?.trim().parse().ok()
}

fn header_u32(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u32> {
    header_str(headers, name)?.trim().parse().ok()
}

fn header_i64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<i64> {
    header_str(headers, name)?.trim().parse().ok()
}

/// Extracts the `rel="next"` URL from a `Link` header.
fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let header = header_str(headers, "link")?;
    parse_next_link(header)
}

fn parse_next_link(header: &str) -> Option<String> {
    for part in header.split(',') {
        let mut segments = part.split(';');
        let url_segment = segments.next()?.trim();
        let is_next = segments.any(|s| {
            let s = s.trim().replace(' ', "");
            s == "rel=\"next\"" || s == "rel=next"
        });
        if is_next {
            let url = url_segment.trim_start_matches('<').trim_end_matches('>');
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_next_link;

    #[test]
    fn finds_next_and_ignores_other_rels() {
        let header = "<https://api.github.com/x?page=2>; rel=\"next\", \
                      <https://api.github.com/x?page=9>; rel=\"last\"";
        assert_eq!(parse_next_link(header).unwrap(), "https://api.github.com/x?page=2");
    }

    #[test]
    fn absent_next_is_none() {
        let header = "<https://api.github.com/x?page=1>; rel=\"prev\"";
        assert_eq!(parse_next_link(header), None);
    }
}
