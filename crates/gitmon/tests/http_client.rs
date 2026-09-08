//! Transport-level behaviour: rate-limit mapping and `Link` pagination.

mod common;

use common::{engine_for, fixture, mock_get, server, Resp};
use gitmon::{ErrorKind, GitHubClient};
use mockito::Matcher;

/// 7. 403 with an exhausted budget is `rate_limited`, carrying `reset_at`.
#[test]
fn an_exhausted_rate_limit_carries_its_reset_time() {
    let reset_at: i64 = 1_760_000_000;
    let mut srv = server();
    let _mock = mock_get(
        &mut srv,
        "/user",
        &Resp::status(403, fixture("error_rate_limited.json"))
            .header("x-ratelimit-limit", "5000")
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", &reset_at.to_string()),
    );
    let (engine, _dir) = engine_for(&srv);

    let err = engine.verify_token().unwrap_err();
    assert_eq!(err.kind, ErrorKind::RateLimited);
    assert_eq!(err.reset_at, Some(reset_at));
    assert!(err.message.contains("rate limit"), "message was {:?}", err.message);
}

/// A 403 with budget left is a permission problem, not a rate limit.
#[test]
fn a_forbidden_response_with_budget_left_is_an_auth_error() {
    let mut srv = server();
    let _mock = mock_get(
        &mut srv,
        "/user",
        &Resp::status(403, r#"{"message":"Resource not accessible by personal access token"}"#)
            .header("x-ratelimit-remaining", "4999"),
    );
    let (engine, _dir) = engine_for(&srv);

    let err = engine.verify_token().unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
    assert_eq!(err.reset_at, None);
}

#[test]
fn a_verified_credential_returns_the_login() {
    let mut srv = server();
    let _mock = mock_get(&mut srv, "/user", &Resp::ok(fixture("user.json")));
    let (engine, _dir) = engine_for(&srv);
    assert_eq!(engine.verify_token().unwrap(), "alice");
}

#[test]
fn no_credential_is_an_auth_error_without_a_request() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    engine.set_token(None);
    let err = engine.verify_token().unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
}

/// 9. Pagination follows `Link: rel="next"` and stops after ten pages.
#[test]
fn pagination_follows_link_headers_and_stops_at_ten_pages() {
    let mut srv = server();
    let base = srv.url();

    // Page 1 is the request with no `page` parameter at all.
    let first = srv
        .mock("GET", "/paged")
        .match_query(Matcher::Missing)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("link", &format!("<{base}/paged?page=2>; rel=\"next\""))
        .with_body(r#"[{"page":1}]"#)
        .create();

    // Pages 2..=11 each advertise a next page, so the client would keep walking
    // for ever if it did not stop itself.
    let mut pages = Vec::new();
    for page in 2..=11 {
        pages.push(
            srv.mock("GET", "/paged")
                .match_query(Matcher::UrlEncoded("page".into(), page.to_string()))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_header("link", &format!("<{base}/paged?page={}>; rel=\"next\"", page + 1))
                .with_body(format!(r#"[{{"page":{page}}}]"#))
                .create(),
        );
    }
    // The eleventh fetch must never happen.
    let page_eleven = srv
        .mock("GET", "/paged")
        .match_query(Matcher::UrlEncoded("page".into(), "11".into()))
        .expect(0)
        .with_status(200)
        .with_body(r#"[{"page":11}]"#)
        .create();

    let client = GitHubClient::with_base_url(&base);
    let items: Vec<serde_json::Value> = client.get_paged("/paged").unwrap();

    let seen: Vec<i64> =
        items.iter().map(|item| item["page"].as_i64().expect("page number")).collect();
    assert_eq!(seen, (1..=10).collect::<Vec<i64>>(), "ten pages, in order, no gaps");
    first.assert();
    page_eleven.assert();
}

/// A list endpoint with no `Link` header is a single page.
#[test]
fn a_single_page_response_stops_immediately() {
    let mut srv = server();
    let _mock = mock_get(&mut srv, "/paged", &Resp::ok(r#"[{"page":1},{"page":2}]"#));
    let client = GitHubClient::with_base_url(&srv.url());
    let items: Vec<serde_json::Value> = client.get_paged("/paged").unwrap();
    assert_eq!(items.len(), 2);
}

/// The page cap silently drops whatever it did not read, which a poll can live
/// with (its window is small and the newest page is the one that matters) but a
/// backfill cannot: a historical window read short loses events nothing will
/// ever fetch again. `get_paged_capped` is where that becomes visible.
#[test]
fn the_page_cap_is_reported_but_a_short_list_is_not() {
    let mut srv = server();
    let base = srv.url();

    // Ten pages, every one of them still advertising another: the cap bites.
    let first = srv
        .mock("GET", "/capped")
        .match_query(Matcher::Missing)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("link", &format!("<{base}/capped?page=2>; rel=\"next\""))
        .with_body(r#"[{"page":1}]"#)
        .create();
    let mut pages = Vec::new();
    for page in 2..=10 {
        pages.push(
            srv.mock("GET", "/capped")
                .match_query(Matcher::UrlEncoded("page".into(), page.to_string()))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_header("link", &format!("<{base}/capped?page={}>; rel=\"next\"", page + 1))
                .with_body(format!(r#"[{{"page":{page}}}]"#))
                .create(),
        );
    }

    let client = GitHubClient::with_base_url(&base);
    let (items, truncated): (Vec<serde_json::Value>, bool) =
        client.get_paged_capped("/capped", |_| true).unwrap();
    assert_eq!(items.len(), 10);
    assert!(truncated, "ten pages with an eleventh on offer is a truncated read");
    first.assert();

    // One page and no `Link` at all: complete, not truncated.
    let mut plain = server();
    let _mock = mock_get(&mut plain, "/paged", &Resp::ok(r#"[{"page":1}]"#));
    let client = GitHubClient::with_base_url(&plain.url());
    let (items, truncated): (Vec<serde_json::Value>, bool) =
        client.get_paged_capped("/paged", |_| true).unwrap();
    assert_eq!(items.len(), 1);
    assert!(!truncated, "a single complete page is not truncated");

    // Stopping early on `keep` is finding the window's edge, not truncation.
    let mut walked = server();
    let _mock = mock_get(&mut walked, "/paged", &Resp::ok(r#"[{"page":1},{"page":2}]"#));
    let client = GitHubClient::with_base_url(&walked.url());
    let (items, truncated): (Vec<serde_json::Value>, bool) = client
        .get_paged_capped("/paged", |item: &serde_json::Value| item["page"].as_i64() == Some(1))
        .unwrap();
    assert_eq!(items.len(), 1);
    assert!(!truncated, "walking to the edge of the window is a complete read");
}
