//! `trait EngineApi` mirrors the "Rust engine API" (`gitmon::Engine`) section
//! of docs/CONTRACT.md, minus `Engine::open` (a constructor for the concrete
//! type, not part of the trait object interface). `commands.rs` delegates to
//! `Arc<dyn EngineApi>`, which is the real `gitmon::Engine` in every build and
//! `MockEngine` only under the `mock` feature plus `GITMON_MOCK=1`.
//!
//! The trait carries exactly the methods the app calls today — which now
//! includes watches and `get_pull_files` (docs/CONTRACT.md "Watched threads"
//! and "Reading in-app"), wired for the Watched view and the detail pane's
//! Commits/Files tabs.
//!
//! Instance methods take `&self` (not `&mut self`): the contract describes
//! one shared, `Send + Sync` engine instance per process, so any interior
//! mutability lives inside the implementation.
//!
//! Every method here blocks — most of them on the network — so no caller
//! may run one on the UI thread; `commands.rs` and `poller.rs` push each
//! call onto a blocking task.

use gitmon::types::{
    Account, BackfillResult, CommitDetail, CommitFile, DeviceLogin, DeviceLoginStatus, Digest,
    Event, FilterMode, OpenPull, PersonSuggestion, PollResult, Repo, RepoSuggestion, Settings,
    Team, Thread, Watch,
};
use gitmon::EngineError;

pub trait EngineApi: Send + Sync {
    /// In-memory only; never persisted by the engine (the app persists it
    /// to the OS keychain separately and calls this to hand it over).
    fn set_token(&self, token: Option<String>);

    /// Resolves and remembers the signed-in login. The app calls this once
    /// after installing a token from the keychain, because it is the
    /// verification — not `set_token` — that tells the engine who is signed
    /// in, and auto-watch needs to know.
    fn verify_token(&self) -> Result<String, EngineError>;

    // ---- accounts (docs/CONTRACT.md "Accounts") --------------------------

    /// Every signed-in account, in the order they were added.
    fn list_accounts(&self) -> Vec<Account>;
    /// Verifies `token`, upserts the account it belongs to, and claims every
    /// repo no account has claimed yet. Adding an already-known account
    /// replaces its token and returns the existing row.
    fn add_account(&self, token: &str) -> Result<Account, EngineError>;
    /// Forgets an account, its repos, and their events and watches.
    /// `not_found` when there is no such account.
    fn remove_account(&self, login: &str) -> Result<(), EngineError>;
    /// Hands the engine a token the app read back from that account's own
    /// keychain item. In memory only, and deliberately unverified — see
    /// `lib.rs`'s startup, which calls this once per account without
    /// spending a `/user` request on each.
    fn set_account_token(&self, login: &str, token: &str);
    /// "Sign out": drops the account's token from memory for this session,
    /// touching neither its row nor its repos, events or watches — unlike
    /// `remove_account`. Its repos poll-skip as "signed out" until it signs
    /// back in.
    fn forget_account_token(&self, login: &str);

    /// The login the engine remembers, or `None` when nothing has resolved
    /// one in this process. Never a network call.
    fn current_login(&self) -> Option<String>;

    /// GitHub OAuth device flow — see CONTRACT.md "Sign-in". This is the
    /// only way the app obtains a token; there is no personal-access-token
    /// path anywhere in the app.
    fn start_device_login(&self, client_id: &str) -> Result<DeviceLogin, EngineError>;
    fn poll_device_login(
        &self,
        client_id: &str,
        device_code: &str,
    ) -> Result<DeviceLoginStatus, EngineError>;

    fn list_teams(&self) -> Vec<Team>;
    fn create_team(&self, name: &str, logins: &[String]) -> Result<Team, EngineError>;
    fn update_team(&self, team: Team) -> Result<(), EngineError>;
    fn delete_team(&self, team_id: u64) -> Result<(), EngineError>;
    fn reorder_teams(&self, ids: &[u64]) -> Result<(), EngineError>;
    fn import_org_team(&self, org: &str, team_slug: &str) -> Result<Team, EngineError>;
    fn suggest_people(
        &self,
        query: &str,
        team_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PersonSuggestion>, EngineError>;
    /// `suggest_people`'s mirror image: `team_id`, when given, keeps only
    /// that team's members instead of excluding them — see
    /// `gitmon::Engine::suggest_active_people`.
    fn suggest_active_people(
        &self,
        query: &str,
        team_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PersonSuggestion>, EngineError>;
    fn suggest_repos(&self) -> Result<Vec<RepoSuggestion>, EngineError>;

    /// `account_login` names which signed-in account the repo is filed
    /// under; `None` resolves to the sole account (or leaves the repo
    /// unclaimed with none signed in yet) — see `resolve_add_account` on the
    /// real engine.
    fn add_repo(&self, spec: &str, account_login: Option<&str>) -> Result<Repo, EngineError>;
    /// Deletes the repo and everything collected for it. The destructive
    /// one — `set_repo_hidden` is the reversible middle state.
    fn remove_repo(&self, repo_id: u64) -> Result<(), EngineError>;
    /// The repos the app shows and the poller spends requests on: hidden ones
    /// are not here (see `gitmon::Engine::list_repos`).
    fn list_repos(&self) -> Vec<Repo>;
    /// The hidden ones, for the Repos view's own "Hidden" section.
    fn list_hidden_repos(&self) -> Vec<Repo>;
    /// Hides or unhides a repo: out of every view and every poll while
    /// hidden, with its stored history untouched throughout.
    fn set_repo_hidden(&self, repo_id: u64, hidden: bool) -> Result<(), EngineError>;

    fn poll_now(&self) -> PollResult;

    /// Fetches the next older window of history for one repo, or every repo —
    /// see CONTRACT.md "Backfill". Its events land already seen and it never
    /// moves the poll watermark, so it can only ever add to the past.
    fn backfill(
        &self,
        repo_id: Option<u64>,
        span_secs: i64,
    ) -> Result<BackfillResult, EngineError>;

    /// `mode` is the feed's "My team / Everyone" view filter (docs/CONTRACT.md
    /// "Filter") — applied on every read, unlike the old ingestion filter it
    /// replaced, so switching it is instant and never loses anything.
    #[allow(clippy::too_many_arguments)] // mirrors gitmon::Engine::list_events's five composable filters
    fn list_events(
        &self,
        repo_id: Option<u64>,
        actor: Option<&str>,
        team_id: Option<u64>,
        mode: FilterMode,
        watched_only: bool,
        before_id: Option<u64>,
        limit: u32,
    ) -> Vec<Event>;
    fn mark_seen(&self, ids: &[u64]) -> Result<(), EngineError>;
    fn unseen_count(&self) -> u64;

    /// In-app reading: the full PR/issue conversation, live (never from
    /// stored events alone) — see CONTRACT.md "Reading in-app".
    fn get_thread(&self, repo_id: u64, number: u64) -> Result<Thread, EngineError>;
    /// In-app reading: one commit's message, file list and per-file patches.
    fn get_commit(&self, repo_id: u64, sha: &str) -> Result<CommitDetail, EngineError>;
    /// In-app reading: a PR's changed files with per-file unified diffs, for
    /// the detail pane's Files tab — see CONTRACT.md "Reading in-app".
    fn get_pull_files(&self, repo_id: u64, number: u64) -> Result<Vec<CommitFile>, EngineError>;

    /// Every watched thread, newest `since` first — see CONTRACT.md "Watched
    /// threads".
    fn list_watches(&self) -> Vec<Watch>;
    /// Records a manual watch; idempotent on an already-watched thread.
    fn watch_thread(&self, repo_id: u64, number: u64) -> Result<Watch, EngineError>;
    /// Stops watching; `not_found` if the thread wasn't watched.
    fn unwatch_thread(&self, repo_id: u64, number: u64) -> Result<(), EngineError>;
    /// Unwatches every watch whose state is not `open`; returns how many.
    fn clear_closed_watches(&self) -> u64;

    /// A period summary of stored events — see CONTRACT.md "Digests". The
    /// caller resolves the calendar window to unix seconds; the engine does
    /// only the aggregation, never calendar math. `tz_offset_secs` is the
    /// app's own UTC offset and decides only where a day starts, for `series`
    /// and `hours`.
    fn digest(
        &self,
        start: i64,
        end: i64,
        team_id: Option<u64>,
        actor: Option<&str>,
        tz_offset_secs: i32,
    ) -> Result<Digest, EngineError>;

    /// Every pull request open right now, oldest first — see CONTRACT.md
    /// "Pulls". Read from the store, never from GitHub.
    fn open_pulls(
        &self,
        team_id: Option<u64>,
        actor: Option<&str>,
        reviewer: Option<&str>,
    ) -> Result<Vec<OpenPull>, EngineError>;

    fn get_settings(&self) -> Settings;
    fn set_settings(&self, s: Settings) -> Result<(), EngineError>;
}

/// The real engine is the implementation: every method forwards straight to
/// `gitmon::Engine`'s identically-named one, so this adapter adds no
/// behaviour of its own and cannot drift from the contract.
impl EngineApi for gitmon::Engine {
    fn set_token(&self, token: Option<String>) {
        gitmon::Engine::set_token(self, token)
    }

    fn verify_token(&self) -> Result<String, EngineError> {
        gitmon::Engine::verify_token(self)
    }

    fn list_accounts(&self) -> Vec<Account> {
        gitmon::Engine::list_accounts(self)
    }

    fn add_account(&self, token: &str) -> Result<Account, EngineError> {
        gitmon::Engine::add_account(self, token)
    }

    fn remove_account(&self, login: &str) -> Result<(), EngineError> {
        gitmon::Engine::remove_account(self, login)
    }

    fn set_account_token(&self, login: &str, token: &str) {
        gitmon::Engine::set_account_token(self, login, token)
    }

    fn forget_account_token(&self, login: &str) {
        gitmon::Engine::forget_account_token(self, login)
    }

    fn current_login(&self) -> Option<String> {
        gitmon::Engine::signed_in_login(self)
    }

    fn start_device_login(&self, client_id: &str) -> Result<DeviceLogin, EngineError> {
        gitmon::Engine::start_device_login(self, client_id)
    }

    fn poll_device_login(
        &self,
        client_id: &str,
        device_code: &str,
    ) -> Result<DeviceLoginStatus, EngineError> {
        gitmon::Engine::poll_device_login(self, client_id, device_code)
    }

    fn list_teams(&self) -> Vec<Team> {
        gitmon::Engine::list_teams(self)
    }

    fn create_team(&self, name: &str, logins: &[String]) -> Result<Team, EngineError> {
        gitmon::Engine::create_team(self, name, logins)
    }

    fn update_team(&self, team: Team) -> Result<(), EngineError> {
        gitmon::Engine::update_team(self, team)
    }

    fn delete_team(&self, team_id: u64) -> Result<(), EngineError> {
        gitmon::Engine::delete_team(self, team_id)
    }

    fn reorder_teams(&self, ids: &[u64]) -> Result<(), EngineError> {
        gitmon::Engine::reorder_teams(self, ids)
    }

    fn import_org_team(&self, org: &str, team_slug: &str) -> Result<Team, EngineError> {
        gitmon::Engine::import_org_team(self, org, team_slug)
    }

    fn suggest_people(
        &self,
        query: &str,
        team_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PersonSuggestion>, EngineError> {
        gitmon::Engine::suggest_people(self, query, team_id, limit)
    }

    fn suggest_active_people(
        &self,
        query: &str,
        team_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PersonSuggestion>, EngineError> {
        gitmon::Engine::suggest_active_people(self, query, team_id, limit)
    }

    fn suggest_repos(&self) -> Result<Vec<RepoSuggestion>, EngineError> {
        gitmon::Engine::suggest_repos(self, None)
    }

    fn add_repo(&self, spec: &str, account_login: Option<&str>) -> Result<Repo, EngineError> {
        gitmon::Engine::add_repo(self, spec, account_login)
    }

    fn remove_repo(&self, repo_id: u64) -> Result<(), EngineError> {
        gitmon::Engine::remove_repo(self, repo_id)
    }

    fn list_repos(&self) -> Vec<Repo> {
        gitmon::Engine::list_repos(self)
    }

    fn list_hidden_repos(&self) -> Vec<Repo> {
        gitmon::Engine::list_hidden_repos(self)
    }

    fn set_repo_hidden(&self, repo_id: u64, hidden: bool) -> Result<(), EngineError> {
        gitmon::Engine::set_repo_hidden(self, repo_id, hidden)
    }

    fn poll_now(&self) -> PollResult {
        gitmon::Engine::poll_now(self)
    }

    fn backfill(
        &self,
        repo_id: Option<u64>,
        span_secs: i64,
    ) -> Result<BackfillResult, EngineError> {
        gitmon::Engine::backfill(self, repo_id, span_secs)
    }

    fn list_events(
        &self,
        repo_id: Option<u64>,
        actor: Option<&str>,
        team_id: Option<u64>,
        mode: FilterMode,
        watched_only: bool,
        before_id: Option<u64>,
        limit: u32,
    ) -> Vec<Event> {
        gitmon::Engine::list_events(
            self,
            repo_id,
            actor,
            team_id,
            mode,
            watched_only,
            before_id,
            limit,
        )
    }

    fn mark_seen(&self, ids: &[u64]) -> Result<(), EngineError> {
        gitmon::Engine::mark_seen(self, ids)
    }

    fn unseen_count(&self) -> u64 {
        gitmon::Engine::unseen_count(self)
    }

    fn get_thread(&self, repo_id: u64, number: u64) -> Result<Thread, EngineError> {
        gitmon::Engine::get_thread(self, repo_id, number)
    }

    fn get_commit(&self, repo_id: u64, sha: &str) -> Result<CommitDetail, EngineError> {
        gitmon::Engine::get_commit(self, repo_id, sha)
    }

    fn get_pull_files(&self, repo_id: u64, number: u64) -> Result<Vec<CommitFile>, EngineError> {
        gitmon::Engine::get_pull_files(self, repo_id, number)
    }

    fn list_watches(&self) -> Vec<Watch> {
        gitmon::Engine::list_watches(self)
    }

    fn watch_thread(&self, repo_id: u64, number: u64) -> Result<Watch, EngineError> {
        gitmon::Engine::watch_thread(self, repo_id, number)
    }

    fn unwatch_thread(&self, repo_id: u64, number: u64) -> Result<(), EngineError> {
        gitmon::Engine::unwatch_thread(self, repo_id, number)
    }

    fn clear_closed_watches(&self) -> u64 {
        gitmon::Engine::clear_closed_watches(self)
    }

    fn digest(
        &self,
        start: i64,
        end: i64,
        team_id: Option<u64>,
        actor: Option<&str>,
        tz_offset_secs: i32,
    ) -> Result<Digest, EngineError> {
        gitmon::Engine::digest(self, start, end, team_id, actor, tz_offset_secs)
    }

    fn open_pulls(
        &self,
        team_id: Option<u64>,
        actor: Option<&str>,
        reviewer: Option<&str>,
    ) -> Result<Vec<OpenPull>, EngineError> {
        gitmon::Engine::open_pulls(self, team_id, actor, reviewer)
    }

    fn get_settings(&self) -> Settings {
        gitmon::Engine::get_settings(self)
    }

    fn set_settings(&self, s: Settings) -> Result<(), EngineError> {
        gitmon::Engine::set_settings(self, s)
    }
}
