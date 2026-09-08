//! `add_repo`: spec parsing, validation against the API, and error mapping.

mod common;

use common::{engine_for, fixture, mock_get, mount, server, Resp, RepoMock};
use gitmon::ErrorKind;

#[test]
fn every_spec_form_resolves_to_the_same_repo() {
    let mut srv = server();
    let repo = RepoMock::quiet("acme", "platform");
    let _mocks = mount(&mut srv, &repo);
    // The real payload, rather than the synthetic one, proves the fields the
    // client reads are the ones GitHub actually sends.
    let _meta = mock_get(&mut srv, "/repos/acme/platform", &Resp::ok(fixture("repo.json")));
    let (engine, _dir) = engine_for(&srv);

    let specs = [
        "acme/platform",
        "https://github.com/acme/platform",
        "https://github.com/acme/platform.git",
        "https://github.com/acme/platform/pulls",
    ];

    let mut ids = Vec::new();
    for spec in specs {
        let added = engine.add_repo(spec, None).unwrap_or_else(|e| panic!("{spec:?} failed: {e}"));
        assert_eq!(added.owner, "acme", "spec {spec:?}");
        assert_eq!(added.name, "platform", "spec {spec:?}");
        assert_eq!(added.default_branch, "main", "spec {spec:?}");
        assert_eq!(added.url, "https://github.com/acme/platform", "spec {spec:?}");
        assert_eq!(added.last_polled_at, None);
        assert_eq!(added.last_error, None);
        ids.push(added.id);
    }

    // A duplicate add returns the existing row rather than a second one.
    assert!(ids.windows(2).all(|pair| pair[0] == pair[1]), "ids differed: {ids:?}");
    assert_eq!(engine.list_repos().len(), 1);
}

#[test]
fn an_unknown_repo_is_not_found() {
    let mut srv = server();
    let _mock = mock_get(
        &mut srv,
        "/repos/acme/ghost",
        &Resp::status(404, fixture("error_not_found.json")),
    );
    let (engine, _dir) = engine_for(&srv);

    let err = engine.add_repo("acme/ghost", None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    assert!(err.message.contains("Not Found"), "message was {:?}", err.message);
    assert_eq!(err.reset_at, None);
    assert!(engine.list_repos().is_empty());
}

#[test]
fn a_rejected_credential_is_an_auth_error() {
    let mut srv = server();
    let _mock = mock_get(
        &mut srv,
        "/repos/acme/platform",
        &Resp::status(401, fixture("error_bad_credentials.json")),
    );
    let (engine, _dir) = engine_for(&srv);

    let err = engine.add_repo("acme/platform", None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
    assert!(err.message.contains("Bad credentials"), "message was {:?}", err.message);
}

#[test]
fn a_spec_that_is_not_a_repo_never_reaches_the_network() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    // No mocks are mounted, so any request would fail differently.
    for spec in ["", "acme", "https://gitlab.com/acme/platform"] {
        let err = engine.add_repo(spec, None).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Invalid, "spec was {spec:?}");
    }
}
