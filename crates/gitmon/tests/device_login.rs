//! The GitHub OAuth device flow, per the "Sign-in" section of `docs/CONTRACT.md`.
//!
//! Both the REST root and the OAuth root point at the same mock server, so a
//! single `ServerGuard` can serve `/login/device/code`, `/login/oauth/access_token`
//! and `/user` together.

mod common;

use common::{fixture, server, Resp};
use gitmon::{DeviceLogin, DeviceLoginStatus, ErrorKind};
use mockito::{Matcher, Mock, ServerGuard};
use tempfile::TempDir;

const CLIENT_ID: &str = "Iv1.testclientid";
const DEVICE_CODE: &str = "3584d83530557fdd1f46af8289938c8ef79f9dc5";
/// Asserted against the crate's own constant rather than a copy: the header
/// carries the crate version, so a version bump must not fail this test, but a
/// *silent* rename of the product string still would — every request below
/// still has to match this exact value.
const USER_AGENT: &str = gitmon::github::USER_AGENT;
const VERIFICATION_URI_COMPLETE: &str = "https://github.com/login/device?user_code=WDJB-MJHT";

/// An engine whose REST *and* OAuth roots are the mock server, and which starts
/// with no credential — a device login is how one arrives.
fn signed_out_engine(server: &ServerGuard) -> (gitmon::Engine, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = gitmon::Engine::open_with_base_urls(dir.path(), &server.url(), &server.url())
        .expect("engine opens");
    (engine, dir)
}

/// The `POST /login/device/code` mock, asserting the exact form fields the
/// contract specifies plus `Accept: application/json` and no `Authorization`.
fn mock_device_code(srv: &mut ServerGuard, response: &Resp) -> Mock {
    srv.mock("POST", "/login/device/code")
        .match_header("accept", "application/json")
        .match_header("content-type", "application/x-www-form-urlencoded")
        .match_header("authorization", Matcher::Missing)
        .match_body(Matcher::AllOf(vec![
            Matcher::UrlEncoded("client_id".into(), CLIENT_ID.into()),
            Matcher::UrlEncoded("scope".into(), "repo read:org read:user".into()),
        ]))
        .with_status(response.status)
        .with_header("content-type", "application/json")
        .with_body(&response.body)
        .create()
}

/// The `POST /login/oauth/access_token` mock, with the same field assertions.
fn mock_access_token(srv: &mut ServerGuard, response: &Resp) -> Mock {
    srv.mock("POST", "/login/oauth/access_token")
        .match_header("accept", "application/json")
        .match_header("authorization", Matcher::Missing)
        .match_body(Matcher::AllOf(vec![
            Matcher::UrlEncoded("client_id".into(), CLIENT_ID.into()),
            Matcher::UrlEncoded("device_code".into(), DEVICE_CODE.into()),
            Matcher::UrlEncoded(
                "grant_type".into(),
                "urn:ietf:params:oauth:grant-type:device_code".into(),
            ),
        ]))
        .with_status(response.status)
        .with_header("content-type", "application/json")
        .with_body(&response.body)
        .create()
}

/// The device-code endpoint's documented JSON response, including the
/// prefilled `verification_uri_complete` GitHub sends alongside the plain one.
fn device_code_body() -> String {
    format!(
        r#"{{"device_code":"{DEVICE_CODE}","user_code":"WDJB-MJHT",
            "verification_uri":"https://github.com/login/device",
            "verification_uri_complete":"{VERIFICATION_URI_COMPLETE}",
            "expires_in":900,"interval":5}}"#
    )
}

/// The same response from a server that omits `verification_uri_complete`.
fn device_code_body_without_complete_uri() -> String {
    format!(
        r#"{{"device_code":"{DEVICE_CODE}","user_code":"WDJB-MJHT",
            "verification_uri":"https://github.com/login/device",
            "expires_in":900,"interval":5}}"#
    )
}

/// GitHub returns device-flow errors with HTTP 200 and an OAuth error body.
fn oauth_error(code: &str, description: &str) -> Resp {
    Resp::ok(format!(
        r#"{{"error":"{code}","error_description":"{description}",
            "error_uri":"https://docs.github.com/x"}}"#
    ))
}

#[test]
fn start_device_login_posts_the_contract_fields_and_maps_the_response() {
    let mut srv = server();
    let mock = mock_device_code(&mut srv, &Resp::ok(device_code_body()));
    let (engine, _dir) = signed_out_engine(&srv);

    let login = engine.start_device_login(CLIENT_ID).expect("device code issued");

    mock.assert();
    assert_eq!(login.device_code, DEVICE_CODE);
    assert_eq!(login.user_code, "WDJB-MJHT");
    assert_eq!(login.verification_uri, "https://github.com/login/device");
    assert_eq!(login.verification_uri_complete.as_deref(), Some(VERIFICATION_URI_COMPLETE));
    assert_eq!(login.expires_in, 900);
    assert_eq!(login.interval, 5);
}

#[test]
fn a_device_code_response_without_the_prefilled_uri_yields_none() {
    let mut srv = server();
    let mock = mock_device_code(&mut srv, &Resp::ok(device_code_body_without_complete_uri()));
    let (engine, _dir) = signed_out_engine(&srv);

    let login = engine.start_device_login(CLIENT_ID).expect("device code issued");

    mock.assert();
    assert_eq!(login.verification_uri_complete, None);
    // The rest of the handshake is unaffected by the missing key.
    assert_eq!(login.device_code, DEVICE_CODE);
    assert_eq!(login.verification_uri, "https://github.com/login/device");
}

#[test]
fn a_device_login_serialises_with_the_contract_key_order() {
    let login = DeviceLogin {
        device_code: DEVICE_CODE.to_string(),
        user_code: "WDJB-MJHT".to_string(),
        verification_uri: "https://github.com/login/device".to_string(),
        verification_uri_complete: Some(VERIFICATION_URI_COMPLETE.to_string()),
        expires_in: 900,
        interval: 5,
    };
    // A literal copy of the `DeviceLogin` line in `docs/CONTRACT.md`: if a key
    // is renamed or reordered, this fails.
    let expected = format!(
        concat!(
            r#"{{"device_code":"{code}","user_code":"WDJB-MJHT","#,
            r#""verification_uri":"https://github.com/login/device","#,
            r#""verification_uri_complete":"{complete}","#,
            r#""expires_in":900,"interval":5}}"#,
        ),
        code = DEVICE_CODE,
        complete = VERIFICATION_URI_COMPLETE,
    );
    let json = serde_json::to_string(&login).expect("serialises");
    assert_eq!(json, expected);
    assert_eq!(serde_json::from_str::<DeviceLogin>(&json).unwrap(), login, "round trip");

    // An absent prefilled URI is `null` on the wire, per the contract's
    // `verification_uri_complete: string|null`.
    let bare = DeviceLogin { verification_uri_complete: None, ..login };
    let json = serde_json::to_string(&bare).expect("serialises");
    assert!(json.contains(r#""verification_uri_complete":null"#), "{json}");
    assert_eq!(serde_json::from_str::<DeviceLogin>(&json).unwrap(), bare, "round trip");
}

#[test]
fn start_device_login_reports_a_refused_flow_as_an_auth_error() {
    let mut srv = server();
    let _mock = mock_device_code(
        &mut srv,
        &oauth_error("device_flow_disabled", "Device flow is not enabled for this app."),
    );
    let (engine, _dir) = signed_out_engine(&srv);

    let err = engine.start_device_login(CLIENT_ID).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
    assert_eq!(err.message, "Device flow is not enabled for this app.");
}

#[test]
fn an_unauthorised_device_login_is_pending_not_an_error() {
    // `authorization_pending` and `slow_down` both mean "keep waiting"; the
    // engine never sleeps, so the two are indistinguishable to the caller.
    for code in ["authorization_pending", "slow_down"] {
        let mut srv = server();
        let mock = mock_access_token(&mut srv, &oauth_error(code, "The user has not yet entered."));
        let (engine, _dir) = signed_out_engine(&srv);

        let status = engine.poll_device_login(CLIENT_ID, DEVICE_CODE).expect("poll succeeds");
        mock.assert();
        assert_eq!(status, DeviceLoginStatus::Pending, "error code was {code}");
    }
}

#[test]
fn a_dead_device_login_is_an_auth_error_carrying_githubs_message() {
    for (code, description) in [
        ("expired_token", "The device code has expired."),
        ("access_denied", "The authorization request was denied."),
    ] {
        let mut srv = server();
        let _mock = mock_access_token(&mut srv, &oauth_error(code, description));
        let (engine, _dir) = signed_out_engine(&srv);

        let err = engine.poll_device_login(CLIENT_ID, DEVICE_CODE).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Auth, "error code was {code}");
        assert_eq!(err.message, description, "error code was {code}");
        assert_eq!(err.reset_at, None);
    }
}

#[test]
fn an_authorised_device_login_returns_the_token_and_login_and_sets_the_credential() {
    let token = "gho_mock0000000000000000deadbeef";
    let mut srv = server();
    let token_mock = mock_access_token(
        &mut srv,
        &Resp::ok(format!(
            r#"{{"access_token":"{token}","token_type":"bearer","scope":"repo,read:org"}}"#
        )),
    );
    // The `/user` call the engine makes on success must carry the fresh token,
    // and it is the same credential `verify_token` uses afterwards.
    let user_mock = srv
        .mock("GET", "/user")
        .match_header("authorization", format!("Bearer {token}").as_str())
        .expect(2)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(fixture("user.json"))
        .create();
    let (engine, _dir) = signed_out_engine(&srv);

    let status = engine.poll_device_login(CLIENT_ID, DEVICE_CODE).expect("poll succeeds");

    token_mock.assert();
    assert_eq!(
        status,
        DeviceLoginStatus::Ok { token: token.to_string(), login: "alice".to_string() }
    );
    // The token is now live on the engine: the PAT path works with it unchanged.
    assert_eq!(engine.verify_token().unwrap(), "alice");
    user_mock.assert();
}

#[test]
fn a_pending_status_serialises_as_the_contract_says() {
    assert_eq!(
        serde_json::to_value(DeviceLoginStatus::Pending).unwrap(),
        serde_json::json!({"status": "pending"})
    );
    assert_eq!(
        serde_json::to_value(DeviceLoginStatus::Ok {
            token: "gho_x".to_string(),
            login: "alice".to_string(),
        })
        .unwrap(),
        serde_json::json!({"status": "ok", "token": "gho_x", "login": "alice"})
    );
}

#[test]
fn every_request_identifies_itself_as_vigie() {
    let mut srv = server();
    // One GET on the REST root and one POST on the OAuth root: both must carry
    // the renamed User-Agent.
    let get_mock = srv
        .mock("GET", "/user")
        .match_header("user-agent", USER_AGENT)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(fixture("user.json"))
        .create();
    let post_mock = srv
        .mock("POST", "/login/device/code")
        .match_header("user-agent", USER_AGENT)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(device_code_body())
        .create();
    // The constant itself is part of the contract's Auth rules: the product
    // name and platform, and deliberately no URL in it.
    assert!(USER_AGENT.starts_with("Vigie/"), "user agent is {USER_AGENT:?}");
    assert!(USER_AGENT.ends_with(" (macOS)"), "user agent is {USER_AGENT:?}");
    assert!(!USER_AGENT.contains("http"), "user agent must carry no URL: {USER_AGENT:?}");
    let (engine, _dir) = signed_out_engine(&srv);

    engine.start_device_login(CLIENT_ID).expect("device code issued");
    engine.set_token(Some("ghp_pat".to_string()));
    engine.verify_token().expect("user fetched");

    post_mock.assert();
    get_mock.assert();
}
