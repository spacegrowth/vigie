//! Several GitHub accounts, per the "Accounts" section of `docs/CONTRACT.md`:
//! `add_account` and the orphan claim, per-repo tokens during a poll, the
//! signed-out repo, `remove_account`'s cascade, auto-watch across accounts, and
//! `add_repo`/`suggest_repos` picking an account.
//!
//! Every account here is reached the way the app reaches one — through
//! `add_account` against a mock GitHub — and every "polled with the right
//! token" claim is made by the mock itself: each repo's endpoints are mounted
//! with `mount_as`, which answers only for one bearer token, so polling a repo
//! with the wrong account's credential fails the test rather than passing it.

mod common;

use common::{
    engine_for, fixture, iso8601, mock_get, mock_get_as, mount, mount_as, server,
    signed_out_engine, unmount, RepoMock, Resp,
};
use gitmon::{Engine, ErrorKind, FilterMode, Settings, WatchSource};
use mockito::{Mock, ServerGuard};

const ALICE_TOKEN: &str = "tok-alice";
const BOB_TOKEN: &str = "tok-bob";

/// `/user` as one token sees it — the call `add_account` makes to find out who
/// a token belongs to. Matched on the token, so two accounts cannot be
/// resolved by one mock.
fn mock_user_as(srv: &mut ServerGuard, token: &str, login: &str) -> Mock {
    mock_get_as(
        srv,
        "/user",
        &Resp::ok(format!(
            r#"{{"login":"{login}","avatar_url":"https://avatars.githubusercontent.com/{login}"}}"#
        )),
        Some(token),
    )
}

fn filter_all(engine: &Engine) {
    let settings = engine.get_settings();
    engine.set_settings(Settings { filter_mode: FilterMode::All, ..settings }).unwrap();
}

/// One commit, so a poll of an otherwise quiet repo still proves it happened.
fn one_commit(slug: &str, sha: &str, actor: &str) -> Resp {
    let at = iso8601(chrono::Utc::now().timestamp() - 60);
    Resp::ok(format!(
        r#"[{{"sha":"{sha}",
              "html_url":"https://github.com/{slug}/commit/{sha}",
              "commit":{{"message":"Commit {sha}",
                         "author":{{"name":"{actor}","date":"{at}"}},
                         "committer":{{"name":"{actor}","date":"{at}"}}}},
              "author":{{"login":"{actor}","avatar_url":null}},
              "committer":{{"login":"{actor}","avatar_url":null}}}}]"#
    ))
}

// ---- add_account --------------------------------------------------------

/// The pre-v5 upgrade path: a repo added before accounts existed is unclaimed,
/// and the first `add_account` takes it. Re-adding the same account replaces
/// its token — proved by polling afterwards against mocks that accept only the
/// *new* one — and does not make a second row.
#[test]
fn add_account_claims_orphan_repos_and_re_adding_replaces_the_token() {
    let mut srv = server();
    // The legacy engine has one nameless token, so the repo it adds is mounted
    // without a token matcher; the poll below is mounted demanding the second
    // token, which is the assertion that the replacement took effect.
    let legacy_mocks = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let _alice = mock_user_as(&mut srv, ALICE_TOKEN, "alice");
    let (engine, _dir) = engine_for(&srv);

    let repo = engine.add_repo("acme/platform", None).unwrap();
    assert_eq!(repo.account_login, "", "no account exists yet, so the repo is unclaimed");
    assert!(engine.list_accounts().is_empty());

    let alice = engine.add_account(ALICE_TOKEN).expect("alice signs in");

    assert_eq!(alice.login, "alice");
    assert_eq!(
        alice.avatar_url.as_deref(),
        Some("https://avatars.githubusercontent.com/alice"),
        "the avatar comes off /user, not out of thin air"
    );
    assert!(alice.added_at > 0);
    assert_eq!(engine.list_accounts(), vec![alice.clone()]);
    assert_eq!(
        engine.list_repos()[0].account_login,
        "alice",
        "the orphan repo was claimed by the first account"
    );
    // And the login the app shows is now the account's, not just whatever
    // `verify_token` happened to resolve.
    assert_eq!(engine.signed_in_login().as_deref(), Some("alice"));

    // Sign in again with a fresh token, as the device flow does after the old
    // one is revoked.
    const REISSUED: &str = "tok-alice-reissued";
    let _reissued_user = mock_user_as(&mut srv, REISSUED, "alice");
    let again = engine.add_account(REISSUED).expect("alice signs in again");

    assert_eq!(engine.list_accounts(), vec![again.clone()], "one row, not two");
    assert_eq!(again.added_at, alice.added_at, "re-adding must not restamp added_at");

    // The proof the token was replaced: the poll's mocks accept only the new
    // one, so a stale token would leave the repo erroring instead of polling.
    // The untokened mocks have to come off the server first: mockito prefers a
    // matching mock that has not had its expected hits yet, so leaving them
    // registered would let any token through and prove nothing.
    unmount(&legacy_mocks);
    let mut poll_repo = RepoMock::quiet("acme", "platform");
    poll_repo.commits = one_commit("acme/platform", "c0ffee", "alice");
    let _polled_as_reissued = mount_as(&mut srv, &poll_repo, REISSUED);
    filter_all(&engine);

    let result = engine.poll_now();

    assert!(result.errors.is_empty(), "the reissued token did not reach GitHub: {result:?}");
    assert_eq!(result.new_events.len(), 1);
    assert_eq!(engine.list_repos()[0].last_error, None);
}

/// A token GitHub rejects leaves no account behind: an account row the app
/// could never poll with is worse than a failed sign-in.
#[test]
fn a_rejected_token_adds_no_account() {
    let mut srv = server();
    let _rejected = mock_get(
        &mut srv,
        "/user",
        &Resp::status(401, fixture("error_bad_credentials.json")),
    );
    let (engine, _dir) = signed_out_engine(&srv);

    let err = engine.add_account("tok-revoked").unwrap_err();

    assert_eq!(err.kind, ErrorKind::Auth);
    assert!(engine.list_accounts().is_empty());

    // And a blank token never reaches the network at all.
    assert_eq!(engine.add_account("   ").unwrap_err().kind, ErrorKind::Invalid);
}

// ---- polling with each repo's own token ---------------------------------

/// Two accounts, two repos, two credentials. Each repo's endpoints answer only
/// for its own account's bearer token, so this passes only if the poll sent
/// alice's token to alice's repo and bob's to bob's.
#[test]
fn two_accounts_poll_their_own_repos_with_their_own_tokens() {
    let mut srv = server();
    let _alice_user = mock_user_as(&mut srv, ALICE_TOKEN, "alice");
    let _bob_user = mock_user_as(&mut srv, BOB_TOKEN, "bob");

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = one_commit("acme/platform", "aaa111", "alice");
    let mut web = RepoMock::quiet("acme", "web");
    web.commits = one_commit("acme/web", "bbb222", "bob");
    let alice_mocks = mount_as(&mut srv, &platform, ALICE_TOKEN);
    let bob_mocks = mount_as(&mut srv, &web, BOB_TOKEN);

    let (engine, _dir) = signed_out_engine(&srv);
    filter_all(&engine);
    engine.add_account(ALICE_TOKEN).unwrap();
    engine.add_account(BOB_TOKEN).unwrap();
    let platform_repo = engine.add_repo("acme/platform", Some("alice")).unwrap();
    let web_repo = engine.add_repo("acme/web", Some("bob")).unwrap();

    assert_eq!(platform_repo.account_login, "alice");
    assert_eq!(web_repo.account_login, "bob");

    let result = engine.poll_now();

    assert!(result.errors.is_empty(), "a repo was polled with the wrong token: {result:?}");
    assert_eq!(result.new_events.len(), 2, "one commit each: {:#?}", result.new_events);
    assert!(
        alice_mocks.iter().all(|mock| mock.matched()),
        "some of alice's endpoints were never called with her token"
    );
    assert!(
        bob_mocks.iter().all(|mock| mock.matched()),
        "some of bob's endpoints were never called with his token"
    );
    // Each repo's events came back under its own repo, not merged into one.
    let by_repo = |id: u64| result.new_events.iter().filter(|e| e.repo_id == id).count();
    assert_eq!(by_repo(platform_repo.id), 1);
    assert_eq!(by_repo(web_repo.id), 1);
    // Both watermarks advanced, and neither repo carries an error.
    assert!(engine
        .list_repos()
        .iter()
        .all(|r| r.last_polled_at.is_some() && r.last_error.is_none()));
}

/// A relaunch before the keychain hands the tokens back: the accounts are in
/// the database, the tokens are not. The repo is skipped with `last_error =
/// "signed out"`, kept rather than dropped, its watermark left where it was —
/// and it polls normally the moment `set_account_token` arrives.
#[test]
fn a_repo_whose_account_has_no_token_is_skipped_until_the_token_arrives() {
    let mut srv = server();
    let _alice_user = mock_user_as(&mut srv, ALICE_TOKEN, "alice");
    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = one_commit("acme/platform", "aaa111", "alice");
    let _mocks = mount_as(&mut srv, &platform, ALICE_TOKEN);

    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open_with_base_url(dir.path(), &srv.url()).unwrap();
        engine.add_account(ALICE_TOKEN).unwrap();
        engine.add_repo("acme/platform", None).unwrap();
    }

    // A fresh process: the account persisted, the token did not.
    let engine = Engine::open_with_base_url(dir.path(), &srv.url()).unwrap();
    filter_all(&engine);
    assert_eq!(engine.list_accounts().len(), 1, "the account survived the relaunch");

    let skipped = engine.poll_now();

    assert!(skipped.new_events.is_empty());
    assert!(
        skipped.errors.is_empty(),
        "a signed-out repo is skipped, not reported as a poll failure: {:?}",
        skipped.errors
    );
    assert_eq!(skipped.rate_limit_remaining, None, "nothing was asked of GitHub");
    let repos = engine.list_repos();
    assert_eq!(repos.len(), 1, "a signed-out repo is never dropped");
    assert_eq!(repos[0].last_error.as_deref(), Some("signed out"));
    assert_eq!(repos[0].last_polled_at, None, "a skipped repo's watermark must not advance");

    // The app reads the keychain and hands the token over.
    engine.set_account_token("alice", ALICE_TOKEN);
    let polled = engine.poll_now();

    assert!(polled.errors.is_empty(), "{:?}", polled.errors);
    assert_eq!(polled.new_events.len(), 1, "the window it missed is still fetched");
    let repos = engine.list_repos();
    assert_eq!(repos[0].last_error, None, "the error cleared once the token arrived");
    assert!(repos[0].last_polled_at.is_some());
}

/// One signed-out account does not stop the other: its repo is skipped and the
/// account that still has a token polls as usual.
#[test]
fn a_signed_out_account_does_not_stop_the_other_one() {
    let mut srv = server();
    let _alice_user = mock_user_as(&mut srv, ALICE_TOKEN, "alice");
    let _bob_user = mock_user_as(&mut srv, BOB_TOKEN, "bob");
    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = one_commit("acme/platform", "aaa111", "alice");
    let _alice_mocks = mount_as(&mut srv, &platform, ALICE_TOKEN);
    let _bob_mocks = mount_as(&mut srv, &RepoMock::quiet("acme", "web"), BOB_TOKEN);

    let (engine, _dir) = signed_out_engine(&srv);
    filter_all(&engine);
    engine.add_account(ALICE_TOKEN).unwrap();
    engine.add_account(BOB_TOKEN).unwrap();
    let platform_repo = engine.add_repo("acme/platform", Some("alice")).unwrap();
    let web_repo = engine.add_repo("acme/web", Some("bob")).unwrap();

    // Bob's token is forgotten — his keychain item is gone, say — while his
    // account row stays.
    engine.set_account_token("bob", "");

    let result = engine.poll_now();

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.new_events.len(), 1, "alice's repo still polled");
    assert_eq!(result.new_events[0].repo_id, platform_repo.id);
    let repos = engine.list_repos();
    let web_row = repos.iter().find(|r| r.id == web_repo.id).unwrap();
    let platform_row = repos.iter().find(|r| r.id == platform_repo.id).unwrap();
    assert_eq!(web_row.last_error.as_deref(), Some("signed out"));
    assert_eq!(platform_row.last_error, None);
}

// ---- remove_account -----------------------------------------------------

/// Removing an account takes its repos, and their events and watches with
/// them, and leaves the other account's untouched.
#[test]
fn removing_an_account_cascades_its_repos_and_their_events() {
    let mut srv = server();
    let _alice_user = mock_user_as(&mut srv, ALICE_TOKEN, "alice");
    let _bob_user = mock_user_as(&mut srv, BOB_TOKEN, "bob");
    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = one_commit("acme/platform", "aaa111", "alice");
    let mut web = RepoMock::quiet("acme", "web");
    web.commits = one_commit("acme/web", "bbb222", "bob");
    let _alice_mocks = mount_as(&mut srv, &platform, ALICE_TOKEN);
    let _bob_mocks = mount_as(&mut srv, &web, BOB_TOKEN);

    let (engine, _dir) = signed_out_engine(&srv);
    filter_all(&engine);
    engine.add_account(ALICE_TOKEN).unwrap();
    engine.add_account(BOB_TOKEN).unwrap();
    let platform_repo = engine.add_repo("acme/platform", Some("alice")).unwrap();
    let web_repo = engine.add_repo("acme/web", Some("bob")).unwrap();
    engine.poll_now();
    assert_eq!(engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(), 2);

    engine.remove_account("alice").expect("alice removed");

    let accounts: Vec<String> =
        engine.list_accounts().into_iter().map(|a| a.login).collect();
    assert_eq!(accounts, vec!["bob"]);
    let repos = engine.list_repos();
    assert_eq!(repos.len(), 1, "alice's repo went with her: {repos:?}");
    assert_eq!(repos[0].id, web_repo.id);
    let events = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
    assert_eq!(events.len(), 1, "alice's repo's events cascaded");
    assert_eq!(events[0].repo_id, web_repo.id);
    assert!(
        engine.list_events(Some(platform_repo.id), None, None, FilterMode::All, false, None, 500).is_empty()
    );
    // Bob is still the signed-in user, and still pollable.
    assert_eq!(engine.signed_in_logins(), vec!["bob".to_string()]);

    assert_eq!(engine.remove_account("nobody").unwrap_err().kind, ErrorKind::NotFound);
}

// ---- auto-watch across accounts -----------------------------------------

/// Auto-watch matches *any* account's login: a PR the second account wrote,
/// in a repo the first account owns, is still the user's own PR.
#[test]
fn auto_watch_matches_the_second_accounts_login() {
    let mut srv = server();
    let _alice_user = mock_user_as(&mut srv, ALICE_TOKEN, "alice");
    let _bob_user = mock_user_as(&mut srv, BOB_TOKEN, "bob");

    let base = chrono::Utc::now().timestamp() - 100;
    let stamp = iso8601(base);
    let pull = |number: u64, author: &str, reviewers: &str| {
        format!(
            r#"{{"number":{number},"title":"PR {number}",
                 "html_url":"https://github.com/acme/platform/pull/{number}",
                 "user":{{"login":"{author}","avatar_url":null}},
                 "body":null,"state":"open",
                 "created_at":"{stamp}","updated_at":"{stamp}",
                 "closed_at":null,"merged_at":null,"merged_by":null,
                 "requested_reviewers":[{reviewers}],
                 "head":null,"base":null,
                 "additions":null,"deletions":null,"changed_files":null}}"#
        )
    };
    let mut platform = RepoMock::quiet("acme", "platform");
    // 101 is bob's own PR; 202 asks bob to review; 303 involves neither
    // account and must stay unwatched.
    platform.pulls = Resp::ok(format!(
        "[{},{},{}]",
        pull(101, "bob", ""),
        pull(202, "carol", r#"{"login":"bob","avatar_url":null}"#),
        pull(303, "carol", r#"{"login":"dave","avatar_url":null}"#),
    ));
    platform.reviews = vec![
        (101, Resp::json_array()),
        (202, Resp::json_array()),
        (303, Resp::json_array()),
    ];
    // The repo belongs to alice, so it is polled with *her* token — the PRs it
    // auto-watches are bob's all the same.
    let _mocks = mount_as(&mut srv, &platform, ALICE_TOKEN);

    let (engine, _dir) = signed_out_engine(&srv);
    filter_all(&engine);
    engine.add_account(ALICE_TOKEN).unwrap();
    engine.add_account(BOB_TOKEN).unwrap();
    assert_eq!(engine.signed_in_logins(), vec!["alice".to_string(), "bob".to_string()]);
    engine.add_repo("acme/platform", Some("alice")).unwrap();

    let polled = engine.poll_now();
    assert!(polled.errors.is_empty(), "{:?}", polled.errors);

    let mut watched: Vec<(u64, WatchSource)> =
        engine.list_watches().iter().map(|w| (w.number, w.source)).collect();
    watched.sort_unstable_by_key(|(number, _)| *number);
    assert_eq!(
        watched,
        vec![(101, WatchSource::Author), (202, WatchSource::Reviewer)],
        "the second account's PRs must auto-watch just like the first's"
    );
}

// ---- picking an account -------------------------------------------------

/// `add_repo` defaults to the only account, refuses to guess between two, and
/// refuses a login nobody is signed in as.
#[test]
fn add_repo_defaults_to_one_account_and_refuses_to_guess_between_two() {
    let mut srv = server();
    let _alice_user = mock_user_as(&mut srv, ALICE_TOKEN, "alice");
    let _bob_user = mock_user_as(&mut srv, BOB_TOKEN, "bob");
    let _platform = mount_as(&mut srv, &RepoMock::quiet("acme", "platform"), ALICE_TOKEN);
    let _web = mount_as(&mut srv, &RepoMock::quiet("acme", "web"), BOB_TOKEN);

    let (engine, _dir) = signed_out_engine(&srv);
    engine.add_account(ALICE_TOKEN).unwrap();

    // One account: the login may be left out, and mixed case still resolves.
    assert_eq!(engine.add_repo("acme/platform", None).unwrap().account_login, "alice");
    assert_eq!(engine.add_repo("acme/platform", Some("ALICE")).unwrap().account_login, "alice");
    assert_eq!(engine.list_repos().len(), 1, "the second add returned the same row");

    engine.add_account(BOB_TOKEN).unwrap();

    // Two accounts: naming one is now required.
    let err = engine.add_repo("acme/web", None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    assert!(err.message.contains("account_login"), "message was {:?}", err.message);
    assert_eq!(engine.list_repos().len(), 1, "the refused add stored nothing");

    assert_eq!(engine.add_repo("acme/web", Some("bob")).unwrap().account_login, "bob");

    // A login nobody is signed in as is invalid, and never reaches the network.
    let unknown = engine.add_repo("acme/platform", Some("carol")).unwrap_err();
    assert_eq!(unknown.kind, ErrorKind::Invalid);
    assert!(unknown.message.contains("carol"), "message was {:?}", unknown.message);
}

/// `suggest_repos` asks every account and merges the answers, deduping a repo
/// both can see; naming one account narrows it to that account's.
#[test]
fn suggest_repos_merges_every_account_and_narrows_to_one() {
    let mut srv = server();
    let _alice_user = mock_user_as(&mut srv, ALICE_TOKEN, "alice");
    let _bob_user = mock_user_as(&mut srv, BOB_TOKEN, "bob");

    let pushed = |owner: &str, name: &str| {
        format!(
            r#"{{"name":"{name}","owner":{{"login":"{owner}","avatar_url":null}},
                 "pushed_at":"{at}"}}"#,
            at = iso8601(chrono::Utc::now().timestamp() - 86_400)
        )
    };
    // `acme/shared` is visible to both accounts and must be suggested once.
    let _alice_repos = mock_get_as(
        &mut srv,
        "/user/repos",
        &Resp::ok(format!("[{},{}]", pushed("acme", "tray"), pushed("acme", "shared"))),
        Some(ALICE_TOKEN),
    );
    let _bob_repos = mock_get_as(
        &mut srv,
        "/user/repos",
        &Resp::ok(format!("[{},{}]", pushed("acme", "shared"), pushed("acme", "billing"))),
        Some(BOB_TOKEN),
    );
    let _search = mock_get(&mut srv, "/search/issues", &Resp::ok(r#"{"items":[]}"#));

    let (engine, _dir) = signed_out_engine(&srv);
    engine.add_account(ALICE_TOKEN).unwrap();
    engine.add_account(BOB_TOKEN).unwrap();

    let slugs = |suggestions: Vec<gitmon::RepoSuggestion>| -> Vec<String> {
        suggestions.iter().map(|s| format!("{}/{}", s.owner, s.name)).collect()
    };

    assert_eq!(
        slugs(engine.suggest_repos(None).unwrap()),
        vec!["acme/tray", "acme/shared", "acme/billing"],
        "every account, in account order, with the shared repo suggested once"
    );
    assert_eq!(
        slugs(engine.suggest_repos(Some("bob")).unwrap()),
        vec!["acme/shared", "acme/billing"]
    );

    // A login nobody is signed in as is invalid, not an empty list.
    assert_eq!(engine.suggest_repos(Some("carol")).unwrap_err().kind, ErrorKind::Invalid);
}
