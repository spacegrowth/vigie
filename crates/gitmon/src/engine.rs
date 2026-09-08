//! The public engine: a mutex-guarded store plus a GitHub client.
//!
//! Every method is synchronous and the type is `Send + Sync`, so the app can
//! hold one instance for the process. Network calls always happen *outside* the
//! store lock: fetch first, then lock to insert.

use crate::error::{EngineError, Result};
use crate::github::models::{
    AccessTokenResponse, Commit, DiffEntry, Issue, IssueComment, OAuthResult, PullRequest,
    RepoResponse, Review, ReviewComment, SearchIssuesResponse, User, UserRepo,
};
use crate::github::{GitHubClient, RateLimitReading};
use crate::poll::{
    self, PullSeen, RepoPoll, ThreadSeen, Window, BACKFILL_MAX_SPAN_SECS,
    BACKFILL_MIN_SPAN_SECS,
    OVERLAP_SECS,
};
use crate::store::{ActorTally, DigestActorRow, DigestRows, Store, UNCLAIMED_ACCOUNT};
use crate::types::{
    Account, BackfillResult, CommitDetail, CommitFile, DayCounts, DeviceLogin, DeviceLoginStatus,
    Digest, DigestPerson,
    DigestRepo, DigestThread, Event, EventKind, FilterMode, KindCounts, OpenPull, PersonSource,
    PersonSuggestion,
    PollOptions, PollResult, PrTiming, Repo, RepoBackfill, RepoError, RepoSuggestion, Settings,
    Team, Thread,
    ThreadItem,
    ThreadItemKind, ThreadKind, Watch, WatchSource, DIGEST_MAX_SERIES_DAYS, DIGEST_PEOPLE_LIMIT,
    DIGEST_THREAD_LIMIT,
    MIN_POLL_INTERVAL_SECS, UNSAVED_TEAM_ID, WATCH_STATE_OPEN,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// How long org membership lists are reused before refetching.
const ORG_CACHE_TTL: Duration = Duration::from_secs(3600);
/// The recency window for repo suggestions.
const SUGGEST_REPO_DAYS: i64 = 90;
/// Space-delimited scopes the device login asks for, per the contract.
const DEVICE_SCOPES: &str = "repo read:org read:user";
/// The only grant type GitHub's device endpoint accepts.
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// How long a fetched thread or commit is reused before refetching. The
/// contract fixes this at 60 seconds.
const READ_CACHE_TTL: Duration = Duration::from_secs(60);

/// The key the legacy single token is remembered under in `Engine::tokens`.
///
/// A GitHub login is never empty, so it cannot collide with a real account —
/// and it is the same string an unclaimed repo carries in `account_login`, so a
/// pre-v5 install's repos look the legacy token up by simply being unclaimed.
const LEGACY_TOKEN_KEY: &str = UNCLAIMED_ACCOUNT;

/// What a repo's `last_error` says when its account has no token this session.
/// The repo is skipped, never dropped, and polls again the moment a token
/// arrives.
const SIGNED_OUT: &str = "signed out";

/// What a `RepoBackfill` reports for a repo that has never polled successfully.
/// It has no `backfilled_to`, so there is no floor to step down from — and
/// guessing one would claim coverage of a stretch nothing ever fetched. The
/// first poll seeds the floor; backfill works from there.
const NOT_POLLED_YET: &str = "not polled yet";

/// What a `RepoBackfill` reports for a repo the user has hidden. A hidden repo
/// costs no requests at all, and reaching further back into its history is a
/// request like any other — so an explicit `backfill(Some(id))` on one says so
/// rather than quietly spending the budget hiding was meant to save.
const HIDDEN: &str = "hidden";

/// What a `RepoBackfill` reports when a listing hit the page cap with more
/// pages still on offer. The window is incomplete, but the cursor still moves:
/// re-reading the same busy window would hit the same cap forever and the feed
/// would never get past it.
const WINDOW_TRUNCATED: &str = "window truncated";

/// One of the 60-second read caches: the call's arguments to the value it
/// returned, stamped with when it was fetched.
type ReadCache<K, V> = Mutex<HashMap<K, (Instant, V)>>;

pub struct Engine {
    store: Mutex<Store>,
    client: GitHubClient,
    org_members: RwLock<HashMap<String, (Instant, Vec<User>)>>,
    /// The login the legacy single token belongs to, once a `/user` call has
    /// actually resolved one. In memory only, beside the credential and never
    /// with it. Superseded by `accounts` for every install that has one, and
    /// read only through [`signed_in_logins`](Self::signed_in_logins), which
    /// yields nothing at all when no login is known rather than guessing.
    login: RwLock<Option<String>>,
    /// Every account's token, keyed by lowercased login, plus the legacy single
    /// token under [`LEGACY_TOKEN_KEY`]. In memory only for the life of the
    /// process: the contract forbids this crate from persisting a credential,
    /// and the app re-supplies these from the keychain at every launch.
    tokens: RwLock<HashMap<String, String>>,
    /// 60-second read-through caches for in-app reading, keyed by the call's
    /// arguments. Held only across the lookup, never across a network call.
    threads: ReadCache<(u64, u64), Thread>,
    commits: ReadCache<(u64, String), CommitDetail>,
    pull_files: ReadCache<(u64, u64), Vec<CommitFile>>,
}

impl Engine {
    /// Opens (creating if needed) `data_dir/gitmon.db`.
    pub fn open(data_dir: &Path) -> Result<Engine> {
        Engine::open_with_base_url(data_dir, crate::github::DEFAULT_BASE_URL)
    }

    /// Same as [`open`](Self::open) but points the client at `base_url`.
    /// Tests use this to aim the engine at a mock server.
    pub fn open_with_base_url(data_dir: &Path, base_url: &str) -> Result<Engine> {
        Engine::open_with_base_urls(data_dir, base_url, crate::github::DEFAULT_OAUTH_BASE_URL)
    }

    /// Same again, but with the OAuth device-flow root overridden too. Tests
    /// point both at one mock server.
    pub fn open_with_base_urls(
        data_dir: &Path,
        base_url: &str,
        oauth_base_url: &str,
    ) -> Result<Engine> {
        std::fs::create_dir_all(data_dir).map_err(|e| {
            EngineError::storage(format!("could not create {}: {e}", data_dir.display()))
        })?;
        let store = Store::open(&data_dir.join("gitmon.db"))?;
        Ok(Engine {
            store: Mutex::new(store),
            client: GitHubClient::with_base_urls(base_url, oauth_base_url),
            org_members: RwLock::new(HashMap::new()),
            login: RwLock::new(None),
            tokens: RwLock::new(HashMap::new()),
            threads: Mutex::new(HashMap::new()),
            commits: Mutex::new(HashMap::new()),
            pull_files: Mutex::new(HashMap::new()),
        })
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        // A panic while holding the lock would otherwise poison the engine for
        // the rest of the process; the store has no cross-call invariants.
        self.store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---- credential -----------------------------------------------------

    /// In-memory only; never persisted by this crate.
    ///
    /// Signing out (`None`) also forgets the login, so auto-watch stops the
    /// moment the credential does. A *new* token leaves the remembered login
    /// alone: every path that installs one — the device flow, and the app's
    /// keychain load — verifies it straight afterwards, and it is that
    /// verification, not this call, that says who is signed in.
    pub fn set_token(&self, token: Option<String>) {
        if token.is_none() {
            self.set_login(None);
        }
        // Remembered twice on purpose: on the client, which is what
        // `verify_token` and the device flow authenticate with, and in `tokens`
        // under the legacy key, which is what a repo falls back to when its own
        // account has no token this session.
        self.remember_token(LEGACY_TOKEN_KEY, token.as_deref());
        self.client.set_token(token);
    }

    /// Returns the authenticated login, and remembers it for auto-watch.
    pub fn verify_token(&self) -> Result<String> {
        if !self.client.has_token() {
            return Err(EngineError::auth("no GitHub credential is set"));
        }
        let user: User = self.client.get("/user")?;
        self.set_login(Some(user.login.clone()));
        Ok(user.login)
    }

    /// The first account's login, lowercased, or `None` when nobody is signed
    /// in at all.
    ///
    /// Public because the app needs the same answer for its own "who am I"
    /// (the contract's `current_login` command) and must not have to spend a
    /// request on `/user` to get what this process already knows. With several
    /// accounts this is only the first of them; [`signed_in_logins`] is the
    /// whole answer, and auto-watch uses that one.
    ///
    /// [`signed_in_logins`]: Self::signed_in_logins
    pub fn signed_in_login(&self) -> Option<String> {
        self.signed_in_logins().into_iter().next()
    }

    /// Every login the user is signed in as, lowercased, in account order.
    ///
    /// These are the logins auto-watch matches a PR's author and requested
    /// reviewers against: with two accounts, a PR either of them wrote is the
    /// user's own PR. The legacy single-token login — the one `verify_token`
    /// resolved, before any account exists — comes last, and is the only entry
    /// on a pre-v5 install.
    pub fn signed_in_logins(&self) -> Vec<String> {
        let mut logins: Vec<String> = self
            .list_accounts()
            .into_iter()
            .map(|account| account.login.trim().to_lowercase())
            .filter(|login| !login.is_empty())
            .collect();
        if let Some(legacy) = self.login.read().ok().and_then(|slot| slot.clone()) {
            if !logins.contains(&legacy) {
                logins.push(legacy);
            }
        }
        logins
    }

    fn set_login(&self, login: Option<String>) {
        if let Ok(mut slot) = self.login.write() {
            *slot = login.map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty());
        }
    }

    /// Stores or forgets one token. A blank token forgets rather than storing an
    /// empty `Bearer`, so "set it to nothing" and "sign out" are one path.
    fn remember_token(&self, key: &str, token: Option<&str>) {
        let Ok(mut tokens) = self.tokens.write() else { return };
        match token.map(str::trim).filter(|t| !t.is_empty()) {
            Some(token) => {
                tokens.insert(key.to_lowercase(), token.to_string());
            }
            None => {
                tokens.remove(&key.to_lowercase());
            }
        }
    }

    /// Which entry of `tokens` authenticates a repo owned by `account_login`:
    /// that account's own token, or the legacy single one when the account has
    /// none. `None` when neither exists — the contract's "signed out" repo.
    fn credential_key(&self, account_login: &str) -> Option<String> {
        let tokens = self.tokens.read().ok()?;
        let key = account_login.trim().to_lowercase();
        if !key.is_empty() && tokens.contains_key(&key) {
            return Some(key);
        }
        tokens.contains_key(LEGACY_TOKEN_KEY).then(|| LEGACY_TOKEN_KEY.to_string())
    }

    /// A client carrying the token `key` names. One per account per poll, so
    /// two accounts never overwrite each other's rate-limit reading.
    fn client_for_key(&self, key: &str) -> GitHubClient {
        self.client.with_token(self.tokens.read().ok().and_then(|t| t.get(key).cloned()))
    }

    /// A client that authenticates as the account owning `account_login`, or
    /// `None` when nothing can.
    fn client_for(&self, account_login: &str) -> Option<GitHubClient> {
        Some(self.client_for_key(&self.credential_key(account_login)?))
    }

    // ---- accounts -------------------------------------------------------

    /// Every account, in the order they were added.
    pub fn list_accounts(&self) -> Vec<Account> {
        self.store().list_accounts().unwrap_or_default()
    }

    /// Verifies a token, records the account it belongs to, and remembers the
    /// token for the session.
    ///
    /// Adding an account that is already there replaces its token and returns
    /// the existing row, keeping its `added_at` — re-authenticating must not
    /// reorder the account list.
    ///
    /// Every repo no account has claimed becomes this account's. That is the
    /// pre-v5 install's upgrade path, and it only ever fires once: after the
    /// first account there are no unclaimed repos left for a second to take.
    pub fn add_account(&self, token: &str) -> Result<Account> {
        let token = token.trim();
        if token.is_empty() {
            return Err(EngineError::invalid("a token is required"));
        }
        // Verified before anything is written: an account row for a token
        // GitHub rejects would be an account the app could never poll with.
        let user: User = self.client.with_token(Some(token.to_string())).get("/user")?;
        let account = self.store().upsert_account(
            &user.login,
            user.avatar_url.as_deref(),
            now_secs(),
        )?;
        self.remember_token(&account.login, Some(token));
        let claimed = self.store().claim_orphan_repos(&account.login)?;
        if claimed > 0 {
            log::info!("{} claimed {claimed} previously unowned repos", account.login);
        }
        Ok(account)
    }

    /// Hands the engine a token the app read back from the keychain. In memory
    /// only, and deliberately unverified: startup must not spend a request per
    /// account, and the first poll reports a stale token as that repo's error.
    pub fn set_account_token(&self, login: &str, token: &str) {
        self.remember_token(login, Some(token));
    }

    /// "Sign out" (docs/CONTRACT.md "Accounts"): drops the account's token
    /// from memory for this session only, touching neither its row nor its
    /// repos, events or watches — unlike `remove_account`, which deletes all
    /// of those. Its repos poll-skip as "signed out" (see `SIGNED_OUT`)
    /// until the account signs back in.
    pub fn forget_account_token(&self, login: &str) {
        self.remember_token(login, None);
    }

    /// Forgets an account, its repos, and their events and watches.
    ///
    /// `not_found` when there is no such account, so the app can tell a stale
    /// button from a successful one.
    pub fn remove_account(&self, login: &str) -> Result<()> {
        let login = login.trim();
        if !self.store().delete_account(login)? {
            return Err(EngineError::not_found(format!("no account {login:?}")));
        }
        self.remember_token(login, None);
        // The legacy remembered login is the same person when it matches, and
        // auto-watch must stop treating their PRs as the user's own.
        if self.login.read().ok().and_then(|slot| slot.clone()).as_deref()
            == Some(login.to_lowercase().as_str())
        {
            self.set_login(None);
        }
        Ok(())
    }

    /// The account `add_repo` should file a repo under.
    ///
    /// A named account must actually exist: filing a repo under a login nobody
    /// is signed in as would leave it permanently unpollable. Unnamed, the only
    /// account is the obvious answer and several accounts is `invalid` — the
    /// engine will not guess which one a repo belongs to. With no accounts at
    /// all the repo is left unclaimed, exactly as a pre-v5 install's are, and
    /// the first `add_account` picks it up.
    fn resolve_add_account(&self, requested: Option<&str>) -> Result<String> {
        let accounts = self.list_accounts();
        match requested.map(str::trim).filter(|login| !login.is_empty()) {
            Some(login) => accounts
                .iter()
                .find(|account| account.login.eq_ignore_ascii_case(login))
                .map(|account| account.login.clone())
                .ok_or_else(|| {
                    EngineError::invalid(format!("no account {login:?} is signed in"))
                }),
            None => match accounts.len() {
                0 => Ok(UNCLAIMED_ACCOUNT.to_string()),
                1 => Ok(accounts[0].login.clone()),
                n => Err(EngineError::invalid(format!(
                    "add_repo needs an account_login: {n} accounts are signed in"
                ))),
            },
        }
    }

    // ---- device flow ----------------------------------------------------

    /// Starts the OAuth device flow: the app shows the returned `user_code` at
    /// `verification_uri`, then polls [`poll_device_login`](Self::poll_device_login).
    ///
    /// Nothing here is persisted or logged — not the client ID, not the device
    /// code, not the token that eventually comes back.
    pub fn start_device_login(&self, client_id: &str) -> Result<DeviceLogin> {
        let client_id = client_id.trim();
        if client_id.is_empty() {
            return Err(EngineError::invalid("client_id is required"));
        }
        let url = self.client.oauth_url("/login/device/code");
        let fields = [("client_id", client_id), ("scope", DEVICE_SCOPES)];
        match self.client.post_form(&url, &fields)? {
            OAuthResult::Ok(login) => Ok(login),
            OAuthResult::Error(e) => Err(EngineError::auth(e.message())),
        }
    }

    /// Polls one device login. The engine never sleeps: `authorization_pending`
    /// and `slow_down` both return [`DeviceLoginStatus::Pending`] and it is the
    /// caller that waits `interval` seconds — plus 5 more after a `slow_down`.
    pub fn poll_device_login(
        &self,
        client_id: &str,
        device_code: &str,
    ) -> Result<DeviceLoginStatus> {
        let client_id = client_id.trim();
        let device_code = device_code.trim();
        if client_id.is_empty() || device_code.is_empty() {
            return Err(EngineError::invalid("client_id and device_code are both required"));
        }
        let url = self.client.oauth_url("/login/oauth/access_token");
        let fields = [
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", DEVICE_GRANT_TYPE),
        ];
        let result: OAuthResult<AccessTokenResponse> = self.client.post_form(&url, &fields)?;
        match result {
            OAuthResult::Error(e) => match e.error.as_str() {
                "authorization_pending" | "slow_down" => Ok(DeviceLoginStatus::Pending),
                // `expired_token` and `access_denied` end the flow, and so does
                // anything else GitHub can say here (a disabled device flow, a
                // wrong client ID); all of them are auth failures to the app.
                _ => Err(EngineError::auth(e.message())),
            },
            OAuthResult::Ok(granted) => {
                self.set_token(Some(granted.access_token.clone()));
                let login = self.verify_token()?;
                Ok(DeviceLoginStatus::Ok { token: granted.access_token, login })
            }
        }
    }

    // ---- teams ----------------------------------------------------------

    /// Every team, in the order the app displays them.
    pub fn list_teams(&self) -> Vec<Team> {
        self.store().list_teams().unwrap_or_default()
    }

    /// Appends a team after the existing ones. An empty login list is allowed —
    /// the app creates a team first and fills it in afterwards.
    pub fn create_team(&self, name: &str, logins: &[String]) -> Result<Team> {
        let name = validate_team_name(name)?;
        let logins = normalize_logins(logins.iter().map(String::as_str));
        self.store().create_team(&name, &logins)
    }

    /// Replaces one team's name and logins wholesale, by id.
    pub fn update_team(&self, team: Team) -> Result<()> {
        let name = validate_team_name(&team.name)?;
        let logins = normalize_logins(team.logins.iter().map(String::as_str));
        self.store().update_team(team.id, &name, &logins)
    }

    /// Events are untouched: they are not owned by teams, and `team_ids` is
    /// resolved on read, so the feed simply stops attributing them to this team.
    pub fn delete_team(&self, team_id: u64) -> Result<()> {
        self.store().delete_team(team_id)
    }

    /// Rewrites display order. `ids` must be a permutation of every team id —
    /// anything missing, extra, or repeated is `invalid`, because a partial
    /// order would silently leave some teams where they were.
    pub fn reorder_teams(&self, ids: &[u64]) -> Result<()> {
        let store = self.store();
        let existing: HashSet<u64> = store.team_ids()?.into_iter().collect();
        let given: HashSet<u64> = ids.iter().copied().collect();
        if given.len() != ids.len() || given != existing {
            return Err(EngineError::invalid(format!(
                "reorder_teams needs every team id exactly once ({} teams exist)",
                existing.len()
            )));
        }
        store.reorder_teams(ids)
    }

    /// Fetches an org team's members. Does not store them, so the team comes
    /// back with id [`UNSAVED_TEAM_ID`]; the app passes it to `create_team`.
    pub fn import_org_team(&self, org: &str, team_slug: &str) -> Result<Team> {
        let org = org.trim();
        let team_slug = team_slug.trim();
        if org.is_empty() || team_slug.is_empty() {
            return Err(EngineError::invalid("org and team_slug are both required"));
        }
        let members: Vec<User> = self
            .client
            .get_paged(&format!("/orgs/{org}/teams/{team_slug}/members?per_page=100"))?;
        Ok(Team {
            id: UNSAVED_TEAM_ID,
            name: team_slug.to_string(),
            logins: normalize_logins(members.iter().map(|u| u.login.as_str())),
        })
    }

    // ---- repos ----------------------------------------------------------

    /// Accepts `owner/name`, any github.com URL (with or without `.git` and with
    /// or without a trailing path), and `git@github.com:owner/name.git`.
    ///
    /// The repo is filed under `account_login` and polled with that account's
    /// token from then on. With one account the login may be omitted; with
    /// several it is required, and with none the repo is left unclaimed for the
    /// first `add_account` to take.
    pub fn add_repo(&self, spec: &str, account_login: Option<&str>) -> Result<Repo> {
        let (owner, name) = parse_repo_spec(spec)?;
        let account = self.resolve_add_account(account_login)?;
        let client = self
            .client_for(&account)
            .ok_or_else(|| EngineError::auth("no GitHub credential is set"))?;
        // Validates the repo exists — as that account, so a private repo only
        // one account can see is checked with the token that can see it — and
        // gives us the canonical casing.
        let remote: RepoResponse = client.get(&format!("/repos/{owner}/{name}"))?;
        self.store().insert_repo(
            &remote.owner.login,
            &remote.name,
            &remote.html_url,
            &account,
            &remote.default_branch,
        )
    }

    /// Deletes the repo and everything collected for it — events, pulls and
    /// watches all cascade. Not what [`set_repo_hidden`](Self::set_repo_hidden)
    /// does, and deliberately unchanged by it.
    pub fn remove_repo(&self, repo_id: u64) -> Result<()> {
        self.store().remove_repo(repo_id)
    }

    /// The repos the app shows and the poller spends requests on: hidden ones
    /// are not here.
    ///
    /// That is what keeps a hidden repo free — `poll_now_with` and
    /// `backfill(None)` both iterate this list, so a hidden repo is never
    /// reached, never asked about, and never charged for. Anything that has to
    /// reason about *every* repo watched, hidden included (whether a repo is
    /// already added, say), reads the store's own `list_repos` instead.
    pub fn list_repos(&self) -> Vec<Repo> {
        self.store().list_visible_repos().unwrap_or_default()
    }

    /// The hidden repos, for the one place that still shows them: the Repos
    /// view that manages them.
    pub fn list_hidden_repos(&self) -> Vec<Repo> {
        self.store().list_hidden_repos().unwrap_or_default()
    }

    /// Hides or unhides a repo — the reversible middle state between watching
    /// one and removing it.
    ///
    /// Hidden, it leaves every view and every poll and so costs nothing; its
    /// stored history is untouched throughout, and unhiding brings all of it
    /// back with no refetch. See `Store::set_repo_hidden` for what unhiding
    /// does to the watermark, and why.
    pub fn set_repo_hidden(&self, repo_id: u64, hidden: bool) -> Result<()> {
        self.store().set_repo_hidden(repo_id, hidden, now_secs())
    }

    // ---- polling --------------------------------------------------------

    /// Polls every repo. Never panics; a repo's failure is isolated to its own
    /// entry in `errors` and leaves its watermark untouched.
    ///
    /// Stores every event it fetches, from every actor — `Settings.filter_mode`
    /// no longer touches ingestion (see `list_events`'s `mode`, which is where
    /// that scoping lives now, applied at read time and so instantly
    /// reversible). The set of watched `(repo_id, number)` keys is still read
    /// once, before the repo loop, and still grows as auto-watch adds rows
    /// during the same poll — not because storage needs a bypass any more, but
    /// because `Event.watched` (computed at read time) must reflect a thread
    /// this very poll started watching.
    pub fn poll_now(&self) -> PollResult {
        self.poll_now_with(PollOptions::default())
    }

    /// The same poll, under explicit [`PollOptions`].
    ///
    /// `force` is the only thing they carry today: it ignores each repo's
    /// `X-Poll-Interval` hint, which is what a hand-driven "Poll now" wants and
    /// what the background timer must not do.
    pub fn poll_now_with(&self, options: PollOptions) -> PollResult {
        let now = now_secs();
        // Visible repos only (see `list_repos`): a hidden repo is not polled
        // at all, so it costs no requests and its watermark simply stops
        // advancing until it is unhidden.
        let repos = self.list_repos();
        let settings = self.get_settings();
        // GitHub's own per-repo hint, read once for the whole poll.
        let intervals = self.store().poll_intervals().unwrap_or_default();
        let mut watched: HashSet<(u64, u64)> = self.store().watched_keys().unwrap_or_default();
        // Empty when nobody has signed in this process: auto-watch then does
        // nothing rather than matching every PR or none.
        let logins = self.signed_in_logins();

        let mut new_events: Vec<Event> = Vec::new();
        let mut errors: Vec<RepoError> = Vec::new();
        let mut skipped: Vec<u64> = Vec::new();
        // One client per credential, built as the repos ask for it, so every
        // repo of one account shares a connection and a rate-limit reading and
        // two accounts keep theirs apart.
        let mut clients: HashMap<String, GitHubClient> = HashMap::new();

        for repo in repos {
            // GitHub's hint first, before anything is spent on this repo: it
            // says "do not ask about me again yet", and a skip is not a failure
            // — the watermark and `last_error` are both left where they are.
            if !options.force && !due(&repo, &intervals, now) {
                skipped.push(repo.id);
                continue;
            }
            let Some(key) = self.credential_key(&repo.account_login) else {
                // Signed out: skipped, not dropped, and the watermark stays put
                // so the window it missed is still fetched once a token arrives.
                if let Err(e) = self.store().mark_repo_error(repo.id, SIGNED_OUT) {
                    log::warn!("could not record repo error: {e}");
                }
                continue;
            };
            let client = clients
                .entry(key.clone())
                .or_insert_with(|| self.client_for_key(&key));
            let since = poll::window_start(repo.last_polled_at, now);
            // Conditional requests only in the steady state. The ETag cache is
            // keyed by a URL with no `since=` on it (see `poll::Lists`), and
            // that key only describes the response when `since` is the ordinary
            // watermark-minus-overlap. A first poll, or the first poll after a
            // rescan, reaches back 24 hours instead and asks the endpoints a
            // different question, so it sends no tags and stores fresh ones.
            let steady = repo.last_polled_at.is_some_and(|last| last - OVERLAP_SECS == since);
            let prefix = self.etag_prefix(&repo);
            let known: HashMap<String, String> = if steady {
                self.store().etags_with_prefix(&prefix).unwrap_or_default()
            } else {
                HashMap::new()
            };
            // Network first, with no lock held.
            match poll::poll_repo(client, &repo, Window::from(since), steady.then_some(&known)) {
                Ok(RepoPoll { events, threads, pulls, etags, poll_interval, .. }) => {
                    let store = self.store();
                    if let Err(e) = store.put_etags(&etags, now) {
                        log::warn!("could not cache etags for repo {}: {e}", repo.id);
                    }
                    if let Some(secs) = poll_interval {
                        if let Err(e) = store.set_poll_interval(repo.id, secs) {
                            log::warn!("could not record poll interval for repo {}: {e}", repo.id);
                        }
                    }
                    update_watches(
                        &store,
                        repo.id,
                        &threads,
                        settings.auto_watch,
                        &logins,
                        now,
                        &mut watched,
                    );
                    // Every fetched event is stored, whoever the actor is: the
                    // ingestion filter this once ran here is gone (packet
                    // gm-feed-r1) — see `list_events`'s `mode` for where that
                    // scoping lives now.
                    for event in &events {
                        match store.insert_event(repo.id, event) {
                            // `None` means the dedupe index already had it.
                            Ok(Some(stored)) => new_events.push(stored),
                            Ok(None) => {}
                            Err(e) => log::warn!("could not store event: {e}"),
                        }
                    }
                    // After the events, never before: a PR row's
                    // `review_decision` and `last_activity_at` are read back
                    // off the stored events, so the review that arrived with
                    // this poll has to be in the table first.
                    upsert_pulls(&store, repo.id, &pulls);
                    if let Err(e) = store.mark_repo_polled(repo.id, now, since) {
                        log::warn!("could not advance watermark for repo {}: {e}", repo.id);
                    }
                }
                Err(e) => {
                    let message = e.to_string();
                    // `EngineError::reset_at` is only ever `Some` for
                    // `rate_limited`, so carrying it straight through gives
                    // the caller the refill time for a rate limit and `None`
                    // for every other kind, with no kind test needed here.
                    let reset_at = e.reset_at;
                    if let Err(store_err) = self.store().mark_repo_error(repo.id, &message) {
                        log::warn!("could not record repo error: {store_err}");
                    }
                    errors.push(RepoError { repo_id: repo.id, message, reset_at });
                }
            }
        }

        new_events.sort_by(|a, b| {
            b.occurred_at.cmp(&a.occurred_at).then_with(|| b.id.cmp(&a.id))
        });
        // Neither `team_ids` nor `watched` is stored, so freshly inserted events
        // arrive without them; fill both in exactly as a read would.
        let store = self.store();
        if let Err(e) = store.resolve_team_ids(&mut new_events) {
            log::warn!("could not resolve team ids for new events: {e}");
        }
        if let Err(e) = store.resolve_watched(&mut new_events) {
            log::warn!("could not resolve watched flags for new events: {e}");
        }
        drop(store);
        // Rate limits are budgeted per token, so the reading the app shows is
        // the tightest of the accounts this poll actually used — remaining,
        // limit and reset time all read off that one account's own last
        // response, never mixed across accounts. `None` when this poll used
        // no account — no repos, or every one of them signed out.
        let tightest: Option<RateLimitReading> =
            clients.values().filter_map(GitHubClient::rate_limit).min_by_key(|r| r.remaining);
        PollResult {
            new_events,
            errors,
            rate_limit_remaining: tightest.map(|r| r.remaining),
            rate_limit_reset_at: tightest.and_then(|r| r.reset_at),
            rate_limit_limit: tightest.and_then(|r| r.limit),
            skipped,
        }
    }

    /// The absolute URL prefix every request for one repo sits under, and so
    /// the prefix its cached ETags are filed under.
    fn etag_prefix(&self, repo: &Repo) -> String {
        self.client.url_for(&format!("/repos/{}/{}/", repo.owner, repo.name))
    }

    /// Clears the polling watermark so the next [`poll_now`](Self::poll_now)
    /// refetches the last 24 hours instead of only what changed since the last
    /// poll.
    ///
    /// `None` rescans every repo; `Some(id)` rescans one and is `not_found`
    /// when there is no such repo. `last_error` is cleared alongside the
    /// watermark: a rescan is the user asking for a clean retry, so a stale
    /// failure from the previous attempt must not stay on the row.
    ///
    /// Nothing is deleted and nothing is duplicated. The window simply widens,
    /// and the UNIQUE dedupe index on `(repo_id, kind, external_id)` turns
    /// every re-fetched event that is already stored into a no-op insert — so
    /// the app can call this as freely as the contract says it does: after
    /// onboarding, after the filter or a team changes, and from "Refetch
    /// last 24 h". What it does surface is everything the *old* filter dropped
    /// but the new one keeps, which is the whole point of calling it.
    pub fn rescan(&self, repo_id: Option<u64>) -> Result<()> {
        // The cached ETags go too. A rescan exists to refetch, and a tag GitHub
        // still matches turns that refetch into a 304 with an empty body — the
        // one case where the free answer is the wrong one.
        let prefix = match repo_id {
            Some(id) => {
                let repo = self.store().find_repo_by_id(id)?.ok_or_else(|| {
                    EngineError::not_found(format!("no repo with id {id}"))
                })?;
                Some(self.etag_prefix(&repo))
            }
            None => None,
        };
        let store = self.store();
        store.clear_watermark(repo_id)?;
        store.clear_etags(prefix.as_deref())
    }

    /// Fetches the next older window of history, so the feed can keep going
    /// into the past instead of stopping at whatever the first poll reached.
    ///
    /// Each repo carries a floor, `backfilled_to`: the oldest instant its
    /// stored history covers. One call steps that floor down by `span_secs` —
    /// fetching `[floor - span, floor)` and, on success, recording the new
    /// floor — so calling it repeatedly walks steadily backwards with no gaps
    /// and no re-reading of ground already covered. `span_secs` is clamped to
    /// [`BACKFILL_MIN_SPAN_SECS`]..=[`BACKFILL_MAX_SPAN_SECS`] rather than
    /// rejected: a caller that wants more history calls again.
    ///
    /// `None` backfills every repo; `Some(id)` backfills one and is `not_found`
    /// when there is no such repo. That is the only way the call itself fails —
    /// per-repo trouble lands in that repo's `error` and never stops the others,
    /// exactly as a poll isolates its failures.
    ///
    /// Three things it deliberately does *not* do:
    ///
    /// - it never touches `last_polled_at`. Reading the past says nothing about
    ///   how current a repo is, and moving the watermark would make the next
    ///   poll skip the window between the last poll and now;
    /// - its events are stored already seen, so history the user scrolled back
    ///   for cannot inflate the unread badge or fire a notification about
    ///   something that happened weeks ago;
    /// - it does not run auto-watch. Auto-watch reacts to a PR arriving; a PR
    ///   surfacing from last month is not an arrival, and watching it would
    ///   quietly fill the Watched view with archaeology.
    ///
    /// Stores every event it fetches, from every actor, exactly as a poll
    /// does: there is no ingestion filter left to apply (see `list_events`'s
    /// `mode` for where that scoping lives now, applied at read time).
    pub fn backfill(&self, repo_id: Option<u64>, span_secs: i64) -> Result<BackfillResult> {
        let span = span_secs.clamp(BACKFILL_MIN_SPAN_SECS, BACKFILL_MAX_SPAN_SECS);
        let repos = match repo_id {
            Some(id) => vec![self
                .store()
                .find_repo_by_id(id)?
                .ok_or_else(|| EngineError::not_found(format!("no repo with id {id}")))?],
            None => self.list_repos(),
        };

        let mut out: Vec<RepoBackfill> = Vec::new();
        // One client per credential, shared across that account's repos, so the
        // rate-limit reading below is per token — as the budget itself is.
        let mut clients: HashMap<String, GitHubClient> = HashMap::new();

        for repo in repos {
            // Only reachable through an explicit `repo_id` — `list_repos`
            // above has already left every hidden repo out of the sweep.
            if repo.hidden {
                out.push(RepoBackfill {
                    repo_id: repo.id,
                    from: 0,
                    to: 0,
                    inserted: 0,
                    error: Some(HIDDEN.to_string()),
                });
                continue;
            }
            // No floor means no first poll yet. Reported, not guessed at.
            let Some(to) = repo.backfilled_to else {
                out.push(RepoBackfill {
                    repo_id: repo.id,
                    from: 0,
                    to: 0,
                    inserted: 0,
                    error: Some(NOT_POLLED_YET.to_string()),
                });
                continue;
            };
            let from = to - span;
            let Some(key) = self.credential_key(&repo.account_login) else {
                out.push(RepoBackfill {
                    repo_id: repo.id,
                    from,
                    to,
                    inserted: 0,
                    error: Some(SIGNED_OUT.to_string()),
                });
                continue;
            };
            let client = clients.entry(key.clone()).or_insert_with(|| self.client_for_key(&key));
            // Network first, with no lock held.
            // Never conditional: a backfill window is nothing like the window
            // whose ETag is on file, so a tag could only mislead.
            match poll::poll_repo(client, &repo, Window::between(from, to), None) {
                Ok(RepoPoll { events, pulls, truncated, .. }) => {
                    let store = self.store();
                    let mut inserted = 0u64;
                    for event in events.iter() {
                        match store.insert_event_as(repo.id, event, true) {
                            // `None` means the dedupe index already had it —
                            // what an overlapping window produces, and why
                            // re-running one is free rather than duplicating.
                            Ok(Some(_)) => inserted += 1,
                            Ok(None) => {}
                            Err(e) => log::warn!("could not store backfilled event: {e}"),
                        }
                    }
                    // Same rule as a poll's: events first, then the PR rows
                    // that are derived from them. A backfill only ever adds
                    // older history, so a PR it sees is a PR whose current
                    // state is worth recording just the same.
                    upsert_pulls(&store, repo.id, &pulls);
                    let mut error = truncated.then(|| WINDOW_TRUNCATED.to_string());
                    // The cursor moves even on a truncated window; only a
                    // failure to record it leaves the floor where it was.
                    if let Err(e) = store.set_backfilled_to(repo.id, from) {
                        log::warn!("could not lower the backfill floor for repo {}: {e}", repo.id);
                        error = Some(e.to_string());
                    }
                    out.push(RepoBackfill { repo_id: repo.id, from, to, inserted, error });
                }
                Err(e) => {
                    // The floor stays put, so the same window is retried next
                    // time rather than skipped.
                    out.push(RepoBackfill {
                        repo_id: repo.id,
                        from,
                        to,
                        inserted: 0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        // The tightest reading across the accounts this call actually used,
        // like a poll's — see the comment in `poll_now_with`.
        let tightest: Option<RateLimitReading> =
            clients.values().filter_map(GitHubClient::rate_limit).min_by_key(|r| r.remaining);
        Ok(BackfillResult {
            repos: out,
            rate_limit_remaining: tightest.map(|r| r.remaining),
            rate_limit_reset_at: tightest.and_then(|r| r.reset_at),
            rate_limit_limit: tightest.and_then(|r| r.limit),
        })
    }

    // ---- watches --------------------------------------------------------

    /// Every watched thread, newest `since` first.
    pub fn list_watches(&self) -> Vec<Watch> {
        self.store().list_watches().unwrap_or_default()
    }

    /// Watches one thread by hand.
    ///
    /// Idempotent: an already-watched thread comes back unchanged, keeping the
    /// `source` and `since` it already had — re-watching an auto-watched PR
    /// must not relabel it `manual` or move its `since`.
    ///
    /// `kind`, `title` and `state` come from the events already stored for the
    /// thread, and only from a live `get_thread` when there are none: watching
    /// something in the feed is the common case, and it should not cost a
    /// request.
    pub fn watch_thread(&self, repo_id: u64, number: u64) -> Result<Watch> {
        if let Some(existing) = self.store().get_watch(repo_id, number)? {
            return Ok(existing);
        }

        let stored = self.store().thread_events(repo_id, number)?;
        let (kind, title, state) = match thread_facts(&stored) {
            Some(facts) => facts,
            None => {
                // Nothing stored — the app is watching something it has only
                // seen on GitHub. One live fetch, which also validates the repo.
                let thread = self.get_thread(repo_id, number)?;
                (thread.kind, thread.title, thread.state)
            }
        };

        let since = now_secs();
        let store = self.store();
        store.insert_watch_if_absent(
            repo_id,
            number,
            kind,
            &title,
            &state,
            WatchSource::Manual,
            since,
        )?;
        // Read back rather than returning what was written: if something else
        // inserted the row first, the caller must see that row, not this one.
        store
            .get_watch(repo_id, number)?
            .ok_or_else(|| EngineError::storage("watch vanished immediately after insert"))
    }

    /// Stops watching. `not_found` when the thread was not watched, so the app
    /// can tell a stale button from a successful one.
    pub fn unwatch_thread(&self, repo_id: u64, number: u64) -> Result<()> {
        if self.store().delete_watch(repo_id, number)? {
            return Ok(());
        }
        Err(EngineError::not_found(format!("{repo_id}#{number} is not watched")))
    }

    /// Unwatches every thread that is no longer open — the app's "Clear
    /// closed". Returns how many went.
    pub fn clear_closed_watches(&self) -> u64 {
        self.store().clear_closed_watches().unwrap_or(0)
    }

    // ---- events ---------------------------------------------------------

    /// Newest first, clamped to [`MAX_EVENT_LIMIT`](crate::MAX_EVENT_LIMIT).
    /// `team_id` keeps events whose actor is currently in that team; an id that
    /// matches no team yields an empty page rather than an error. `repo_id`
    /// keeps events in that repo; an id that matches no repo likewise yields an
    /// empty page. `watched_only` keeps events on currently watched threads,
    /// which never includes a commit — it has no `number`, so it belongs to no
    /// thread.
    ///
    /// `mode` is what `Settings.filter_mode` used to decide at ingestion
    /// (packet gm-feed-r1 moved it here): `All` applies no further scoping;
    /// `Team` keeps only actors currently on *some* team (the union, same as
    /// `team_id`'s per-team check but across every team) or a signed-in
    /// account — your own activity is never somebody else's to filter out.
    /// With no team having a single member yet, `Team` behaves exactly like
    /// `All`: the same fresh-install guard the old ingestion filter had, so an
    /// install that has not been configured yet is not read as one configured
    /// to exclude everybody. Unlike the old ingestion filter, this is applied
    /// on every read: nothing is ever dropped from storage, so narrowing and
    /// widening this are both instant and lose nothing either way.
    #[allow(clippy::too_many_arguments)] // five independent, composable filters over one page of events; a struct would only rename this list, not shorten it
    pub fn list_events(
        &self,
        repo_id: Option<u64>,
        actor: Option<&str>,
        team_id: Option<u64>,
        mode: FilterMode,
        watched_only: bool,
        before_id: Option<u64>,
        limit: u32,
    ) -> Vec<Event> {
        let restrict_to: Option<HashSet<String>> = match mode {
            FilterMode::All => None,
            FilterMode::Team => {
                let members = self.store().member_logins().unwrap_or_default();
                if members.is_empty() {
                    None
                } else {
                    let mut set = members;
                    // `signed_in_logins`, not the stored `accounts` table:
                    // the same source the old ingestion filter read, so the
                    // legacy single-token path (no `accounts` row at all)
                    // still exempts its own login exactly as it always did.
                    set.extend(
                        self.signed_in_logins().into_iter().map(|l| l.to_lowercase()),
                    );
                    Some(set)
                }
            }
        };
        self.store()
            .list_events(repo_id, actor, team_id, restrict_to.as_ref(), watched_only, before_id, limit)
            .unwrap_or_default()
    }

    pub fn mark_seen(&self, ids: &[u64]) -> Result<()> {
        self.store().mark_seen(ids)
    }

    pub fn unseen_count(&self) -> u64 {
        self.store().unseen_count().unwrap_or(0)
    }

    // ---- digests --------------------------------------------------------

    /// Summarises the events already stored for the half-open window
    /// `[start, end)` — an event exactly at `start` is in, one exactly at `end`
    /// is out.
    ///
    /// Read from the store and nothing else: no request goes to GitHub, so a
    /// digest reports what Vigie watched, not what happened. `team_id` keeps
    /// events whose actor is *currently* in that team and `actor` keeps one
    /// login; given together they intersect.
    ///
    /// A `team_id` that matches no team yields an empty digest with zeroed
    /// totals rather than an error — it is the same answer a real but empty
    /// team gives, and the app holding a stale id after a delete should see a
    /// quiet nothing, not a failure. That is deliberately unlike
    /// `suggest_people`, where an unknown id excludes nobody: there the id
    /// narrows a list of candidates, here it *is* the population.
    /// `tz_offset_secs` is the caller's own UTC offset, and it decides one
    /// thing only: where a *day* starts, and therefore which bucket of `series`
    /// and which cell of `hours` an event falls in. Nothing else in the digest
    /// moves with it — the window is still the caller's own `[start, end)` in
    /// unix seconds, and every other count is unchanged.
    pub fn digest(
        &self,
        start: i64,
        end: i64,
        team_id: Option<u64>,
        actor: Option<&str>,
        tz_offset_secs: i32,
    ) -> Result<Digest> {
        if end <= start {
            return Err(EngineError::invalid(format!(
                "digest needs end after start (start {start}, end {end})"
            )));
        }

        let store = self.store();
        // Resolved once here rather than per event row inside the query.
        let logins: Option<HashSet<String>> = match team_id {
            Some(id) => Some(store.team_logins(id)?.unwrap_or_default().into_iter().collect()),
            None => None,
        };
        let rows = store.digest_rows(start, end, logins.as_ref(), actor)?;
        let buckets =
            store.digest_hour_buckets(start, end, logins.as_ref(), actor, tz_offset_secs)?;
        let (ttm, ttfr, samples) =
            store.pr_timing_samples(start, end, logins.as_ref(), actor)?;
        let unreviewed_merges = store.unreviewed_merges(start, end, logins.as_ref(), actor)?;
        let (authored, review_queue) = store.open_pull_person_counts()?;
        drop(store);

        Ok(build_digest(DigestInputs {
            start,
            end,
            team_id,
            actor,
            tz_offset_secs,
            rows,
            buckets,
            ttm,
            ttfr,
            samples,
            unreviewed_merges,
            authored,
            review_queue,
        }))
    }

    /// Every pull request that is open right now, oldest first.
    ///
    /// Read from the `pulls` table the poller fills in from the PR listing it
    /// already fetches, so this makes no GitHub request and, like the digest,
    /// reports only what Vigie watches. `team_id` keeps PRs whose *author* is
    /// currently in that team — the same membership rule
    /// [`digest`](Self::digest) applies to an actor — `actor` keeps one
    /// author, and `reviewer` keeps the PRs that currently request one login's
    /// review. Given together they intersect.
    ///
    /// A `team_id` that matches no team yields an empty list, for the reason
    /// spelled out on `digest`.
    pub fn open_pulls(
        &self,
        team_id: Option<u64>,
        actor: Option<&str>,
        reviewer: Option<&str>,
    ) -> Result<Vec<OpenPull>> {
        let store = self.store();
        let logins: Option<HashSet<String>> = match team_id {
            Some(id) => Some(store.team_logins(id)?.unwrap_or_default().into_iter().collect()),
            None => None,
        };
        let mut pulls = store.open_pulls(logins.as_ref(), actor)?;
        drop(store);
        // Applied here rather than in SQL: `requested_reviewers` is a JSON
        // array, and the list this filters is already only the open PRs.
        if let Some(reviewer) = reviewer {
            pulls.retain(|pull| {
                pull.requested_reviewers.iter().any(|r| r.eq_ignore_ascii_case(reviewer))
            });
        }
        Ok(pulls)
    }

    // ---- reading in-app -------------------------------------------------

    /// One PR or issue with its whole conversation, fetched live and merged
    /// into a single chronological `items` list.
    ///
    /// `/pulls/{number}` is tried first; a 404 there means the number is a
    /// plain issue, which is the only way to tell the two apart by number.
    /// Cached for 60 seconds, so paging back and forth in the app is free.
    pub fn get_thread(&self, repo_id: u64, number: u64) -> Result<Thread> {
        if let Some(hit) = cache_get(&self.threads, &(repo_id, number)) {
            return Ok(hit);
        }
        let (slug, client) = self.repo_target(repo_id)?;

        // The PR shape and the issue shape differ enough that they are fetched
        // as their own types rather than one lenient struct.
        let thread = match client.get::<PullRequest>(&format!("/repos/{slug}/pulls/{number}")) {
            Ok(pr) => self.pull_thread(repo_id, &slug, &client, pr)?,
            Err(e) if e.kind == crate::error::ErrorKind::NotFound => {
                let issue: Issue = client.get(&format!("/repos/{slug}/issues/{number}"))?;
                self.issue_thread(repo_id, &slug, &client, issue)?
            }
            Err(e) => return Err(e),
        };

        cache_put(&self.threads, (repo_id, number), thread.clone());
        Ok(thread)
    }

    /// A pull request plus its comments, reviews, review comments and commits.
    fn pull_thread(
        &self,
        repo_id: u64,
        slug: &str,
        client: &GitHubClient,
        pr: PullRequest,
    ) -> Result<Thread> {
        let number = pr.number;
        let mut items = self.comment_items(slug, client, number)?;

        let reviews: Vec<Review> =
            client.get_paged(&format!("/repos/{slug}/pulls/{number}/reviews?per_page=100"))?;
        for review in &reviews {
            // A pending review has no submission time and is visible to nobody.
            let Some(at) = review.submitted_at.as_deref().and_then(poll::parse_ts) else {
                continue;
            };
            let (actor_login, actor_avatar_url) = actor_of(review.user.as_ref());
            items.push(ThreadItem {
                kind: ThreadItemKind::Review,
                actor_login,
                actor_avatar_url,
                body: review.body.clone().filter(|b| !b.trim().is_empty()),
                // Unlike the event feed, a thread shows every submitted review
                // verbatim — including DISMISSED — because it is a reading view,
                // not a notification.
                state: Some(review.state.clone()),
                path: None,
                line: None,
                url: review.html_url.clone(),
                at,
                sha: None,
            });
        }

        let review_comments: Vec<ReviewComment> =
            client.get_paged(&format!("/repos/{slug}/pulls/{number}/comments?per_page=100"))?;
        for comment in &review_comments {
            let Some(at) = poll::parse_ts(&comment.created_at) else { continue };
            let (actor_login, actor_avatar_url) = actor_of(comment.user.as_ref());
            items.push(ThreadItem {
                kind: ThreadItemKind::ReviewComment,
                actor_login,
                actor_avatar_url,
                body: comment.body.clone(),
                state: None,
                path: comment.path.clone(),
                // `line` is null once a comment goes outdated; `original_line`
                // still says which line it was written against.
                line: comment.line.or(comment.original_line),
                url: comment.html_url.clone(),
                at,
                sha: None,
            });
        }

        let commits: Vec<Commit> =
            client.get_paged(&format!("/repos/{slug}/pulls/{number}/commits?per_page=100"))?;
        for commit in &commits {
            let Some(at) = poll::commit_time(commit) else { continue };
            let (actor_login, actor_avatar_url) = poll::commit_actor(commit);
            items.push(ThreadItem {
                kind: ThreadItemKind::Commit,
                actor_login,
                actor_avatar_url,
                body: poll::non_empty(&commit.commit.message),
                state: None,
                path: None,
                line: None,
                url: commit.html_url.clone(),
                // The one item kind that carries a sha: it is what the app
                // hands back to `get_commit` when the reader opens a commit.
                sha: Some(commit.sha.clone()),
                at,
            });
        }

        // Stable, so items sharing a timestamp keep the order their sources were
        // read in rather than shuffling between calls.
        items.sort_by_key(|item| item.at);

        let (author_login, author_avatar_url) = actor_of(pr.user.as_ref());
        Ok(Thread {
            repo_id,
            number,
            kind: ThreadKind::Pull,
            title: pr.title.clone(),
            state: pr.state.clone().unwrap_or_else(|| "open".to_string()),
            author_login,
            author_avatar_url,
            body: pr.body.clone(),
            url: pr.html_url.clone(),
            created_at: poll::parse_ts(&pr.created_at).unwrap_or(0),
            head_ref: pr.head.as_ref().and_then(|r| r.ref_name.clone()),
            base_ref: pr.base.as_ref().and_then(|r| r.ref_name.clone()),
            additions: pr.additions,
            deletions: pr.deletions,
            changed_files: pr.changed_files,
            items,
        })
    }

    /// An issue plus its comments. An issue has no branches, no diff stat and
    /// no reviews, so those five fields stay null per the contract.
    fn issue_thread(
        &self,
        repo_id: u64,
        slug: &str,
        client: &GitHubClient,
        issue: Issue,
    ) -> Result<Thread> {
        let mut items = self.comment_items(slug, client, issue.number)?;
        items.sort_by_key(|item| item.at);
        let (author_login, author_avatar_url) = actor_of(issue.user.as_ref());
        Ok(Thread {
            repo_id,
            number: issue.number,
            kind: ThreadKind::Issue,
            title: issue.title.clone(),
            state: issue.state.clone().unwrap_or_else(|| "open".to_string()),
            author_login,
            author_avatar_url,
            body: issue.body.clone(),
            url: issue.html_url.clone(),
            created_at: poll::parse_ts(&issue.created_at).unwrap_or(0),
            head_ref: None,
            base_ref: None,
            additions: None,
            deletions: None,
            changed_files: None,
            items,
        })
    }

    /// `/issues/{n}/comments` — the conversation timeline both PRs and issues
    /// share.
    fn comment_items(
        &self,
        slug: &str,
        client: &GitHubClient,
        number: u64,
    ) -> Result<Vec<ThreadItem>> {
        let comments: Vec<IssueComment> =
            client.get_paged(&format!("/repos/{slug}/issues/{number}/comments?per_page=100"))?;
        let mut items = Vec::new();
        for comment in &comments {
            let Some(at) = poll::parse_ts(&comment.created_at) else { continue };
            let (actor_login, actor_avatar_url) = actor_of(comment.user.as_ref());
            items.push(ThreadItem {
                kind: ThreadItemKind::Comment,
                actor_login,
                actor_avatar_url,
                body: comment.body.clone(),
                state: None,
                path: None,
                line: None,
                url: comment.html_url.clone(),
                at,
                sha: None,
            });
        }
        Ok(items)
    }

    /// One commit with its per-file diff. Cached for 60 seconds.
    pub fn get_commit(&self, repo_id: u64, sha: &str) -> Result<CommitDetail> {
        let sha = sha.trim();
        if sha.is_empty() {
            return Err(EngineError::invalid("sha is required"));
        }
        let key = (repo_id, sha.to_string());
        if let Some(hit) = cache_get(&self.commits, &key) {
            return Ok(hit);
        }
        let (slug, client) = self.repo_target(repo_id)?;
        let commit: Commit = client.get(&format!("/repos/{slug}/commits/{sha}"))?;

        let (author_login, author_avatar_url) = poll::commit_actor(&commit);
        let stats = commit.stats.as_ref();
        let detail = CommitDetail {
            repo_id,
            sha: commit.sha.clone(),
            message: commit.commit.message.clone(),
            author_login,
            author_avatar_url,
            url: commit.html_url.clone(),
            at: poll::commit_time(&commit).unwrap_or(0),
            additions: stats.and_then(|s| s.additions).unwrap_or(0),
            deletions: stats.and_then(|s| s.deletions).unwrap_or(0),
            files: commit.files.unwrap_or_default().into_iter().map(diff_entry_to_file).collect(),
        };
        cache_put(&self.commits, key, detail.clone());
        Ok(detail)
    }

    /// Every file a pull request changes, with its unified diff.
    ///
    /// `Link` pagination up to ten pages, like every other list endpoint, and
    /// cached for 60 seconds keyed by `(repo_id, number)` so flipping between
    /// the Conversation and Files tabs costs nothing. `patch` is null for
    /// binary and very large files, which the app renders as "too large to show
    /// here" rather than as an empty diff.
    pub fn get_pull_files(&self, repo_id: u64, number: u64) -> Result<Vec<CommitFile>> {
        let key = (repo_id, number);
        if let Some(hit) = cache_get(&self.pull_files, &key) {
            return Ok(hit);
        }
        let (slug, client) = self.repo_target(repo_id)?;
        let entries: Vec<DiffEntry> =
            client.get_paged(&format!("/repos/{slug}/pulls/{number}/files?per_page=100"))?;
        let files: Vec<CommitFile> = entries.into_iter().map(diff_entry_to_file).collect();
        cache_put(&self.pull_files, key, files.clone());
        Ok(files)
    }

    /// `owner/name` for a watched repo id.
    fn repo_slug(&self, repo_id: u64) -> Result<String> {
        let repo = self
            .store()
            .find_repo_by_id(repo_id)?
            .ok_or_else(|| EngineError::not_found(format!("no repo with id {repo_id}")))?;
        Ok(format!("{}/{}", repo.owner, repo.name))
    }

    /// A client authenticating as the account that owns a watched repo.
    ///
    /// Every live read goes through here, so reading a thread, a commit or a
    /// pull request's files uses the same token that polls the repo — a work
    /// account's private repo is never fetched with a personal token that
    /// cannot see it. `auth` when that account has no token this session, which
    /// is the reading equivalent of the poll's "signed out".
    fn repo_client(&self, repo_id: u64) -> Result<GitHubClient> {
        let account = self
            .store()
            .repo_account(repo_id)?
            .ok_or_else(|| EngineError::not_found(format!("no repo with id {repo_id}")))?;
        self.client_for(&account)
            .ok_or_else(|| EngineError::auth(format!("{SIGNED_OUT}: {account:?}")))
    }

    /// Both of the above, for the three live reads that need each.
    fn repo_target(&self, repo_id: u64) -> Result<(String, GitHubClient)> {
        Ok((self.repo_slug(repo_id)?, self.repo_client(repo_id)?))
    }

    // ---- settings -------------------------------------------------------

    pub fn get_settings(&self) -> Settings {
        self.store().get_settings().unwrap_or_default()
    }

    pub fn set_settings(&self, settings: Settings) -> Result<()> {
        if settings.poll_interval_secs < MIN_POLL_INTERVAL_SECS {
            return Err(EngineError::invalid(format!(
                "poll_interval_secs must be at least {MIN_POLL_INTERVAL_SECS}"
            )));
        }
        self.store().set_settings(&settings)
    }

    // ---- suggestions ----------------------------------------------------

    /// Stored contributors first (ranked by event count), then org members, then
    /// bots. With `team_id`, logins already in that team are excluded — the app
    /// is filling that team in; without it nothing is excluded.
    ///
    /// This is the team-*building* flow (Teams' "add a member" search). For
    /// scoping a search to a team instead — "only these people" — see
    /// `suggest_active_people`, which has the opposite `team_id` sense.
    pub fn suggest_people(
        &self,
        query: &str,
        team_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PersonSuggestion>> {
        // An unknown id excludes nobody, matching `list_events`: a stale team in
        // the app's hands narrows nothing rather than failing the call.
        let team: HashSet<String> = match team_id {
            Some(id) => {
                self.store().team_logins(id)?.unwrap_or_default().into_iter().collect()
            }
            None => HashSet::new(),
        };
        self.suggest_people_filtered(query, limit, move |login_lower| !team.contains(login_lower))
    }

    /// Like `suggest_people`, but for narrowing a search *to* a team rather
    /// than building one: with `team_id`, only logins already in that team
    /// match — the same "narrow to these people" sense `digest`'s `team_id`
    /// has (see its doc comment above `digest`) — and an id matching no team,
    /// or a team with no members, yields no results rather than an
    /// unrestricted search. Without a `team_id` nothing is restricted, same
    /// as `suggest_people`.
    ///
    /// This is what a page-level team scope should pass when searching people
    /// *within* that scope (e.g. Summary's person filter) — passing it to
    /// `suggest_people` instead hides exactly the people being searched for,
    /// since that function's `team_id` means "exclude", written for the
    /// add-a-member flow.
    pub fn suggest_active_people(
        &self,
        query: &str,
        team_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PersonSuggestion>> {
        let team: Option<HashSet<String>> = match team_id {
            Some(id) => {
                Some(self.store().team_logins(id)?.unwrap_or_default().into_iter().collect())
            }
            None => None,
        };
        self.suggest_people_filtered(query, limit, move |login_lower| {
            team.as_ref().is_none_or(|t| t.contains(login_lower))
        })
    }

    /// Shared body of `suggest_people` and `suggest_active_people`: stored
    /// contributors first (ranked by event count), then org members, then
    /// bots last. `keep_team` is the only thing that differs between the two
    /// callers — it decides whether a login (already lowercased) passes the
    /// caller's team constraint; the query and dedup logic are identical.
    fn suggest_people_filtered(
        &self,
        query: &str,
        limit: u32,
        keep_team: impl Fn(&str) -> bool,
    ) -> Result<Vec<PersonSuggestion>> {
        let query = query.trim().to_lowercase();
        let repo_names: HashMap<u64, String> = self
            .list_repos()
            .iter()
            .map(|r| (r.id, format!("{}/{}", r.owner, r.name)))
            .collect();

        let rollup = self.store().actor_rollup().unwrap_or_default();
        let mut by_login: HashMap<String, Contributor> = HashMap::new();
        for ActorTally { login, kind, repo_id, count, avatar_url } in rollup {
            let entry = by_login.entry(login.to_lowercase()).or_insert_with(|| Contributor {
                login: login.clone(),
                avatar: None,
                total: 0,
                kinds: HashMap::new(),
                repos: HashMap::new(),
            });
            entry.total += count;
            *entry.kinds.entry(kind).or_insert(0) += count;
            *entry.repos.entry(repo_id).or_insert(0) += count;
            if entry.avatar.is_none() {
                entry.avatar = avatar_url;
            }
        }

        let mut contributors: Vec<Contributor> = by_login.into_values().collect();
        contributors.sort_by(|a, b| {
            b.total.cmp(&a.total).then_with(|| a.login.to_lowercase().cmp(&b.login.to_lowercase()))
        });

        let mut suggestions: Vec<PersonSuggestion> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for contributor in contributors {
            let login_lower = contributor.login.to_lowercase();
            if !keep_team(&login_lower) || !matches_query(&login_lower, &query) {
                continue;
            }
            if !seen.insert(login_lower.clone()) {
                continue;
            }
            suggestions.push(PersonSuggestion {
                source: source_for(&contributor.login, PersonSource::Contributor),
                why: contributor.why(&repo_names),
                login: contributor.login,
                avatar_url: contributor.avatar,
            });
        }

        // Org members of every watched repo owner, cached for an hour, each
        // read with the token of an account that actually watches that owner —
        // a work org is invisible to a personal token.
        let owners: Vec<(String, String)> = {
            let mut owners: Vec<(String, String)> =
                self.list_repos().into_iter().map(|r| (r.owner, r.account_login)).collect();
            owners.sort_by_key(|(owner, _)| owner.to_lowercase());
            owners.dedup_by_key(|(owner, _)| owner.to_lowercase());
            owners
        };
        for (owner, account_login) in owners {
            // No token for that account this session: its orgs are simply not
            // searched, rather than searched with somebody else's credential.
            let Some(client) = self.client_for(&account_login) else { continue };
            for member in self.org_members(&owner, &client) {
                let login_lower = member.login.to_lowercase();
                if !keep_team(&login_lower) || !matches_query(&login_lower, &query) {
                    continue;
                }
                if !seen.insert(login_lower) {
                    continue;
                }
                suggestions.push(PersonSuggestion {
                    source: source_for(&member.login, PersonSource::OrgMember),
                    why: format!("member of {owner}"),
                    login: member.login.clone(),
                    avatar_url: member.avatar_url.clone(),
                });
            }
        }

        // Prefix matches beat substring matches; bots always sort last.
        suggestions.sort_by_key(|s| {
            let login = s.login.to_lowercase();
            let is_bot = s.source == PersonSource::Bot;
            let rank = if query.is_empty() || login.starts_with(&query) { 0 } else { 1 };
            (is_bot, rank)
        });
        suggestions.truncate(limit as usize);
        Ok(suggestions)
    }

    /// Repos an account pushed to, or has an open PR in, in the last 90 days.
    /// Repos already watched are left out.
    ///
    /// With no `account_login` every account is asked and the answers are
    /// merged, deduped by slug in account order — the app's "add a repo" list
    /// should not make the user pick an account first. An account with no token
    /// this session contributes nothing rather than failing the whole call; it
    /// is only `auth` when *nothing* can be asked.
    pub fn suggest_repos(&self, account_login: Option<&str>) -> Result<Vec<RepoSuggestion>> {
        let now = now_secs();
        // Every repo, hidden ones included: a hidden repo is still watched,
        // and suggesting it back as something to add would be a lie that ends
        // in a confusing no-op (`insert_repo` returns the existing, still
        // hidden, row).
        let watched: HashSet<String> = self
            .store()
            .list_repos()
            .unwrap_or_default()
            .iter()
            .map(|r| format!("{}/{}", r.owner, r.name).to_lowercase())
            .collect();

        let mut out: Vec<RepoSuggestion> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (login, client) in self.suggestion_targets(account_login)? {
            self.suggest_repos_for(&login, &client, now, &watched, &mut seen, &mut out)?;
        }
        Ok(out)
    }

    /// The `(login, client)` pairs `suggest_repos` should ask.
    ///
    /// With no accounts at all this is the legacy single-token install, and the
    /// one question worth asking is `/user`: who does this token belong to.
    fn suggestion_targets(
        &self,
        account_login: Option<&str>,
    ) -> Result<Vec<(String, GitHubClient)>> {
        let accounts = self.list_accounts();
        if accounts.is_empty() {
            let login = self.verify_token()?;
            let client = self
                .client_for(UNCLAIMED_ACCOUNT)
                .ok_or_else(|| EngineError::auth("no GitHub credential is set"))?;
            return Ok(vec![(login, client)]);
        }

        let wanted = account_login.map(str::trim).filter(|login| !login.is_empty());
        if let Some(login) = wanted {
            if !accounts.iter().any(|a| a.login.eq_ignore_ascii_case(login)) {
                return Err(EngineError::invalid(format!("no account {login:?} is signed in")));
            }
        }
        let targets: Vec<(String, GitHubClient)> = accounts
            .into_iter()
            .filter(|a| wanted.is_none_or(|login| a.login.eq_ignore_ascii_case(login)))
            .filter_map(|a| Some((a.login.clone(), self.client_for(&a.login)?)))
            .collect();
        if targets.is_empty() {
            return Err(EngineError::auth("no signed-in account has a token this session"));
        }
        Ok(targets)
    }

    /// One account's share of the repo suggestions, appended to `out`.
    /// `seen` spans every account, so a repo two of them can both see is
    /// suggested once.
    fn suggest_repos_for(
        &self,
        login: &str,
        client: &GitHubClient,
        now: i64,
        watched: &HashSet<String>,
        seen: &mut HashSet<String>,
        out: &mut Vec<RepoSuggestion>,
    ) -> Result<()> {
        let repos: Vec<UserRepo> = client.get_paged(
            "/user/repos?sort=pushed&direction=desc&per_page=100\
             &affiliation=owner,collaborator,organization_member",
        )?;
        for repo in repos {
            let Some(pushed) = repo.pushed_at.as_deref().and_then(poll::parse_ts) else {
                continue;
            };
            let days = (now - pushed) / 86_400;
            if days > SUGGEST_REPO_DAYS {
                continue;
            }
            let slug = format!("{}/{}", repo.owner.login, repo.name).to_lowercase();
            if watched.contains(&slug) || !seen.insert(slug) {
                continue;
            }
            out.push(RepoSuggestion {
                owner: repo.owner.login.clone(),
                name: repo.name.clone(),
                why: match days {
                    0 => "you pushed today".to_string(),
                    1 => "you pushed 1 day ago".to_string(),
                    n => format!("you pushed {n} days ago"),
                },
            });
        }

        // Repos where the user has an open PR but may not have push access.
        let search: SearchIssuesResponse =
            client.get(&format!("/search/issues?q=is:open+is:pr+author:{login}&per_page=100"))?;
        let mut open_prs: HashMap<String, u64> = HashMap::new();
        for item in &search.items {
            if let Some(slug) = item.repository_url.as_deref().and_then(repo_slug_from_api_url) {
                *open_prs.entry(slug).or_insert(0) += 1;
            }
        }
        let mut extra: Vec<(String, u64)> = open_prs.into_iter().collect();
        extra.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (slug, count) in extra {
            let key = slug.to_lowercase();
            if watched.contains(&key) || !seen.insert(key) {
                continue;
            }
            let Some((owner, name)) = slug.split_once('/') else { continue };
            out.push(RepoSuggestion {
                owner: owner.to_string(),
                name: name.to_string(),
                why: match count {
                    1 => "1 open PR".to_string(),
                    n => format!("{n} open PRs"),
                },
            });
        }
        Ok(())
    }

    /// Cached org member lookup, made with one repo's account. A user account
    /// (404) or a credential without `read:org` (403) yields an empty list
    /// rather than an error.
    ///
    /// The cache is keyed on the org alone: two accounts that both watch an org
    /// see the same members, and the first answer is good for the hour.
    fn org_members(&self, org: &str, client: &GitHubClient) -> Vec<User> {
        let key = org.to_lowercase();
        let cached = self.org_members.read().ok().and_then(|cache| {
            cache
                .get(&key)
                .filter(|(fetched_at, _)| fetched_at.elapsed() < ORG_CACHE_TTL)
                .map(|(_, members)| members.clone())
        });
        if let Some(members) = cached {
            return members;
        }
        let members: Vec<User> = client
            .get_paged(&format!("/orgs/{org}/members?per_page=100"))
            .unwrap_or_else(|e| {
                log::debug!("no org members for {org}: {e}");
                Vec::new()
            });
        if let Ok(mut cache) = self.org_members.write() {
            cache.insert(key, (Instant::now(), members.clone()));
        }
        members
    }
}

struct Contributor {
    login: String,
    avatar: Option<String>,
    total: u64,
    kinds: HashMap<EventKind, u64>,
    repos: HashMap<u64, u64>,
}

impl Contributor {
    /// e.g. "41 commits · acme/platform"
    fn why(&self, repo_names: &HashMap<u64, String>) -> String {
        let top_kind = self
            .kinds
            .iter()
            .max_by_key(|(kind, count)| (**count, std::cmp::Reverse(kind.as_str())))
            .map(|(kind, _)| *kind);
        let top_repo = self
            .repos
            .iter()
            .max_by_key(|(repo_id, count)| (**count, **repo_id))
            .and_then(|(repo_id, _)| repo_names.get(repo_id).cloned());
        match (top_kind, top_repo) {
            (Some(kind), Some(repo)) => format!("{} {} · {repo}", self.total, kind_label(kind)),
            (Some(kind), None) => format!("{} {}", self.total, kind_label(kind)),
            _ => format!("{} events", self.total),
        }
    }
}

/// Everything `build_digest` needs, gathered under one lock and handed over in
/// one piece so the ranking below stays a pure function with no database in
/// sight.
struct DigestInputs<'a> {
    start: i64,
    end: i64,
    team_id: Option<u64>,
    actor: Option<&'a str>,
    tz_offset_secs: i32,
    rows: DigestRows,
    /// `(local hour bucket, kind, count)` from `Store::digest_hour_buckets`.
    buckets: Vec<(i64, EventKind, u64)>,
    ttm: Vec<i64>,
    ttfr: Vec<i64>,
    samples: u64,
    unreviewed_merges: u64,
    /// Open PRs authored / awaiting review, by lowercased login.
    authored: HashMap<String, u64>,
    review_queue: HashMap<String, u64>,
}

/// Ranks and truncates the store's grouped rows into the contract's `Digest`.
///
/// Kept out of [`Engine::digest`] so the ordering rules are one pure function
/// over rows, with no lock and no database in sight.
fn build_digest(input: DigestInputs<'_>) -> Digest {
    let DigestInputs {
        start,
        end,
        team_id,
        actor,
        tz_offset_secs,
        rows,
        buckets,
        mut ttm,
        mut ttfr,
        samples,
        unreviewed_merges,
        authored,
        review_queue,
    } = input;

    // `totals` counts the whole window, so it is summed before the top-20 cut:
    // the numbers at the top of a digest must not shrink because a 21st person
    // was dropped from the list below them.
    let mut totals = KindCounts::default();
    let mut by_login: HashMap<String, DigestPerson> = HashMap::new();
    for DigestActorRow { login, kind, count, avatar_url, last_at } in rows.actors {
        totals.add(kind, count);
        let key = login.to_lowercase();
        let entry = by_login.entry(key.clone()).or_insert_with(|| DigestPerson {
            login: login.clone(),
            avatar_url: None,
            total: 0,
            counts: KindCounts::default(),
            // Both are current state rather than window state, so they are
            // looked up once per person here and never accumulated.
            open_prs: authored.get(&key).copied().unwrap_or(0),
            review_queue: review_queue.get(&key).copied().unwrap_or(0),
            last_at: i64::MIN,
        });
        entry.total += count;
        entry.counts.add(kind, count);
        entry.last_at = entry.last_at.max(last_at);
        if entry.avatar_url.is_none() {
            entry.avatar_url = avatar_url;
        }
    }

    // Counted before the cut, like `totals`: "20 of 34" is only honest if the
    // 34 was taken from the whole window.
    let people_total = by_login.len() as u64;
    let mut people: Vec<DigestPerson> = by_login.into_values().collect();
    people.sort_by(|a, b| {
        b.total.cmp(&a.total).then_with(|| a.login.to_lowercase().cmp(&b.login.to_lowercase()))
    });
    people.truncate(DIGEST_PEOPLE_LIMIT);

    let mut threads: Vec<DigestThread> = rows
        .threads
        .into_iter()
        // A commit belongs to no thread; one carrying a number would be a
        // storage bug, and it is left out rather than guessed at.
        .filter_map(|row| {
            Some(DigestThread {
                repo_id: row.repo_id,
                number: row.number,
                kind: row.last_kind.thread_kind()?,
                title: row.title,
                url: row.url,
                events: row.events,
                last_at: row.last_at,
            })
        })
        .collect();
    threads.sort_by(|a, b| {
        b.events
            .cmp(&a.events)
            .then_with(|| b.last_at.cmp(&a.last_at))
            // The contract's two keys can still tie; a third keeps the answer
            // stable between calls instead of leaving it to hash order.
            .then_with(|| (a.repo_id, a.number).cmp(&(b.repo_id, b.number)))
    });
    threads.truncate(DIGEST_THREAD_LIMIT);

    // Who wrote each repo's commits, folded per repo before the repos are
    // built so a repo carries its own concentration and nothing else's.
    let mut commit_authors: HashMap<u64, Vec<(String, u64)>> = HashMap::new();
    for (repo_id, login, commits) in rows.repo_commit_authors {
        commit_authors.entry(repo_id).or_default().push((login, commits));
    }

    // The store only counts repos that have events in the window, so the
    // contract's "every watched repo with a non-zero total" is already what
    // came back; the filter guards the shape rather than trimming anything.
    let mut repos: Vec<DigestRepo> = rows
        .repos
        .into_iter()
        .filter(|(_, total)| *total > 0)
        .map(|(repo_id, total)| {
            let (top_author_login, top_author_share) =
                top_commit_author(commit_authors.get(&repo_id).map(Vec::as_slice).unwrap_or(&[]));
            DigestRepo { repo_id, total, top_author_login, top_author_share }
        })
        .collect();
    repos.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.repo_id.cmp(&b.repo_id)));

    let (series, hours) = fold_buckets(start, end, tz_offset_secs, &buckets);

    // Sorted once here, so the median and the p90 below are demonstrably two
    // reads of the *same* ordered samples rather than two sorts that could
    // drift apart.
    ttm.sort_unstable();
    ttfr.sort_unstable();

    Digest {
        start,
        end,
        team_id,
        // Echoed exactly as the caller passed it: the app labels the summary
        // with what it asked for, and the match itself is case-insensitive.
        actor: actor.map(str::to_string),
        totals,
        people,
        people_total,
        threads,
        repos,
        series,
        hours,
        pr_timing: PrTiming {
            median_ttm_secs: median(&ttm),
            median_ttfr_secs: median(&ttfr),
            p90_ttm_secs: percentile_90(&ttm),
            p90_ttfr_secs: percentile_90(&ttfr),
            samples,
        },
        unreviewed_merges,
    }
}

/// The busiest commit author in one repo's window and their share of it, as
/// `(login, share)` — `(None, 0.0)` when the repo has no commits at all.
///
/// `authors` is `(login, commits)` with each login appearing once, as the
/// store's `GROUP BY repo_id, actor_login COLLATE NOCASE` produces. The most
/// commits wins; a tie goes to the lowest login, compared case-insensitively so
/// the winner does not depend on how GitHub happened to capitalise it.
fn top_commit_author(authors: &[(String, u64)]) -> (Option<String>, f32) {
    let total: u64 = authors.iter().map(|(_, commits)| *commits).sum();
    if total == 0 {
        return (None, 0.0);
    }
    let top = authors
        .iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.to_lowercase().cmp(&a.0.to_lowercase())));
    match top {
        Some((login, commits)) => (Some(login.clone()), *commits as f32 / total as f32),
        None => (None, 0.0),
    }
}

/// Turns the store's local-hour buckets into the digest's two time views: one
/// gapless entry per local day in the window, and the 7x24 weekday-by-hour grid.
///
/// Both are folded here, from the same buckets, so they cannot disagree about
/// which local day an event fell on. A bucket is a whole number of hours from
/// the unix epoch *in local time*, so dividing it by 24 gives the local day and
/// the remainder gives the local hour — with `div_euclid`/`rem_euclid`, which
/// floor rather than truncate and so stay right for a pre-1970 timestamp.
///
/// The unix epoch was a Thursday, which is why the weekday is `day + 3` with
/// Monday at 0.
fn fold_buckets(
    start: i64,
    end: i64,
    tz_offset_secs: i32,
    buckets: &[(i64, EventKind, u64)],
) -> (Vec<DayCounts>, Vec<Vec<u64>>) {
    const HOURS_PER_DAY: i64 = 24;
    let tz = tz_offset_secs as i64;

    // The window is half-open, so its last instant is `end - 1`: a window
    // ending exactly at local midnight covers the day before, not the day that
    // is about to start.
    let first_day = (start + tz).div_euclid(86_400);
    let last_day = (end - 1 + tz).div_euclid(86_400);
    let span = (last_day - first_day + 1).clamp(0, DIGEST_MAX_SERIES_DAYS as i64) as usize;

    let mut series: Vec<DayCounts> = (0..span as i64)
        .map(|offset| DayCounts {
            // Back to the unix second that local day began.
            day: (first_day + offset) * 86_400 - tz,
            counts: KindCounts::default(),
        })
        .collect();
    let mut hours: Vec<Vec<u64>> = vec![vec![0; 24]; 7];

    for (bucket, kind, count) in buckets {
        let day = bucket.div_euclid(HOURS_PER_DAY);
        let hour = bucket.rem_euclid(HOURS_PER_DAY) as usize;
        let weekday = (day + 3).rem_euclid(7) as usize;
        hours[weekday][hour] += count;
        // An event outside `[first_day, last_day]` cannot exist — the store
        // already filtered by the window — but a truncated series can end
        // before the window does, and that day simply has nowhere to go.
        if let Ok(index) = usize::try_from(day - first_day) {
            if let Some(entry) = series.get_mut(index) {
                entry.counts.add(*kind, *count);
            }
        }
    }

    (series, hours)
}

/// The median of an **ascending** set of samples, or `None` when there are
/// none. An even count averages the two middle values, rounding toward zero.
fn median(samples: &[i64]) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    let mid = samples.len() / 2;
    if samples.len() % 2 == 1 {
        Some(samples[mid])
    } else {
        // Summed as `i128` so two very large gaps cannot overflow on the way
        // to their own average.
        Some(((samples[mid - 1] as i128 + samples[mid] as i128) / 2) as i64)
    }
}

/// The 90th percentile of an **ascending** set of samples, or `None` when
/// there are none — see [`PrTiming`] for the rule and why it is that one.
///
/// R-7: the answer sits at rank `0.9 * (n - 1)` counting from zero, and is
/// interpolated linearly when that lands between two samples. Held in tenths
/// as `9 * (n - 1)`, so the whole computation is integer arithmetic and no
/// float rounding can creep into a second count; `i128` because the gap
/// between two samples multiplied by nine must not overflow.
///
/// The interpolation rounds **up**. That keeps `p90 >= median` true for every
/// sample set — including the negative ones a duration can never be, where the
/// median's own round-toward-zero moves the other way — rather than true only
/// for the inputs the store happens to produce.
fn percentile_90(samples: &[i64]) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    let position = 9 * (samples.len() as i128 - 1);
    let index = (position / 10) as usize;
    let remainder = position % 10;
    let lower = samples[index] as i128;
    if remainder == 0 {
        // The rank is exact: one sample, eleven samples, twenty-one...
        return Some(lower as i64);
    }
    // `index` is at most `n - 2` whenever the rank is inexact, so this is in
    // bounds; and the samples ascend, so the gap cannot be negative and
    // `(x + 9) / 10` really is a ceiling.
    let gap = samples[index + 1] as i128 - lower;
    Some((lower + (remainder * gap + 9) / 10) as i64)
}

/// Human plural for a kind, used in suggestion `why` strings.
fn kind_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Commit => "commits",
        EventKind::PrOpened => "PRs opened",
        EventKind::PrMerged => "PRs merged",
        EventKind::PrClosed => "PRs closed",
        EventKind::PrReviewed => "reviews",
        EventKind::PrCommented => "PR comments",
        EventKind::IssueOpened => "issues opened",
        EventKind::IssueCommented => "issue comments",
    }
}

/// A cache hit that is still inside the TTL, cloned out so the lock is released
/// before the caller does anything with it.
fn cache_get<K, V>(cache: &ReadCache<K, V>, key: &K) -> Option<V>
where
    K: std::hash::Hash + Eq,
    V: Clone,
{
    let guard = cache.lock().ok()?;
    let (fetched_at, value) = guard.get(key)?;
    if fetched_at.elapsed() >= READ_CACHE_TTL {
        return None;
    }
    Some(value.clone())
}

/// Stores a fresh value, dropping every entry that has already expired so the
/// map cannot grow without bound across a long-running session.
fn cache_put<K, V>(cache: &ReadCache<K, V>, key: K, value: V)
where
    K: std::hash::Hash + Eq,
{
    if let Ok(mut guard) = cache.lock() {
        guard.retain(|_, (fetched_at, _)| fetched_at.elapsed() < READ_CACHE_TTL);
        guard.insert(key, (Instant::now(), value));
    }
}

/// The actor of a comment/review/PR, with the contract's `ghost` fallback for a
/// deleted account.
fn actor_of(user: Option<&User>) -> (String, Option<String>) {
    match user {
        Some(user) => (user.login.clone(), user.avatar_url.clone()),
        None => ("ghost".to_string(), None),
    }
}

fn source_for(login: &str, default: PersonSource) -> PersonSource {
    if login.to_lowercase().ends_with("[bot]") {
        PersonSource::Bot
    } else {
        default
    }
}

fn matches_query(login_lower: &str, query: &str) -> bool {
    query.is_empty() || login_lower.contains(query)
}

/// GitHub's `files[]` entry as the contract's `CommitFile`. Shared by
/// `get_commit` and `get_pull_files` so a changed file reads identically
/// whichever tab the app opened it from.
fn diff_entry_to_file(entry: DiffEntry) -> CommitFile {
    CommitFile {
        path: entry.filename,
        status: entry.status,
        additions: entry.additions,
        deletions: entry.deletions,
        // Absent for binary and very large files; null, not empty.
        patch: entry.patch,
    }
}

/// The `kind`, `title` and `state` a watch takes from a thread's stored events,
/// or `None` when nothing is stored for it.
///
/// `events` is newest first. The newest one fixes the kind and the state — a
/// `pr_merged` means merged, a `pr_closed` means closed, and anything else
/// means the thread is still open, which is the only reading the stored events
/// support. The *title* deliberately skips reviews: every other kind carries
/// the thread's own title, while `pr_reviewed` carries "Review: approved" and
/// friends, which would label the watch with the last thing that happened to it
/// instead of what it is.
fn thread_facts(events: &[(EventKind, String)]) -> Option<(ThreadKind, String, String)> {
    let (newest_kind, newest_title) = events.first()?;
    let kind = events.iter().find_map(|(kind, _)| kind.thread_kind())?;
    let state = match newest_kind {
        EventKind::PrMerged => "merged",
        EventKind::PrClosed => "closed",
        _ => WATCH_STATE_OPEN,
    };
    let title = events
        .iter()
        .find(|(kind, _)| *kind != EventKind::PrReviewed)
        .map(|(_, title)| title)
        .unwrap_or(newest_title);
    Some((kind, title.clone(), state.to_string()))
}

/// Auto-watches the signed-in user's pull requests and refreshes every watched
/// row this poll saw, adding whatever it wrote to `watched`.
///
/// The refresh runs for every thread seen: a thread that is not watched matches
/// no row and the update is a no-op, which is cheaper than asking first.
/// Auto-watch inserts only when absent, so a manual watch keeps its own
/// `source` and `since`.
///
/// A free function rather than a method: it needs the store the caller is
/// already holding the lock on, and nothing else off the engine.
fn update_watches(
    store: &Store,
    repo_id: u64,
    threads: &[ThreadSeen],
    auto_watch: bool,
    logins: &[String],
    now: i64,
    watched: &mut HashSet<(u64, u64)>,
) {
    for thread in threads {
        if auto_watch {
            if let Some(source) = auto_watch_source(logins, thread) {
                match store.insert_watch_if_absent(
                    repo_id,
                    thread.number,
                    thread.kind,
                    &thread.title,
                    &thread.state,
                    source,
                    now,
                ) {
                    Ok(true) => {
                        watched.insert((repo_id, thread.number));
                    }
                    Ok(false) => {}
                    Err(e) => log::warn!("could not auto-watch a thread: {e}"),
                }
            }
        }
        if let Err(e) = store.refresh_watch(repo_id, thread.number, &thread.title, &thread.state) {
            log::warn!("could not refresh watch {repo_id}#{}: {e}", thread.number);
        }
    }
}

/// Records every pull request one fetch saw, current state and all.
///
/// A failure is logged and skipped rather than failing the poll: `pulls` holds
/// state the next poll will simply re-derive, so one lost row costs an interval
/// and nothing more — unlike an event, which is only ever offered once.
///
/// A free function rather than a method, for the same reason
/// [`update_watches`] is one: it needs the store the caller already holds the
/// lock on, and nothing else off the engine.
fn upsert_pulls(store: &Store, repo_id: u64, pulls: &[PullSeen]) {
    for pull in pulls {
        if let Err(e) = store.upsert_pull(repo_id, pull) {
            log::warn!("could not record pull {repo_id}#{}: {e}", pull.number);
        }
    }
}

/// Which auto-watch source, if any, one seen thread earns for the signed-in
/// user. `None` with no login known, for an issue (only PRs auto-watch), and
/// for a PR none of the user's accounts wrote or was asked to review.
///
/// Any account matching is enough: a PR the work account wrote is still the
/// user's own PR. Authorship is checked across every login before review
/// requests are, so a PR one account wrote and another was asked to review is
/// `author` rather than depending on which account is listed first.
fn auto_watch_source(logins: &[String], thread: &ThreadSeen) -> Option<WatchSource> {
    if logins.is_empty() || thread.kind != ThreadKind::Pull {
        return None;
    }
    if logins.iter().any(|login| thread.author_login.eq_ignore_ascii_case(login)) {
        return Some(WatchSource::Author);
    }
    if thread
        .requested_reviewers
        .iter()
        .any(|r| logins.iter().any(|login| r.eq_ignore_ascii_case(login)))
    {
        return Some(WatchSource::Reviewer);
    }
    None
}

/// A team must be named. Blank is `invalid` rather than silently renamed: the
/// app owns the default, and quietly inventing a name hides a UI bug.
fn validate_team_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(EngineError::invalid("a team needs a name"));
    }
    Ok(trimmed.to_string())
}

/// Lowercases, drops blanks, and dedupes while preserving first-seen order.
fn normalize_logins<'a>(logins: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for login in logins {
        let normalized = login.trim().trim_start_matches('@').to_lowercase();
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            continue;
        }
        out.push(normalized);
    }
    out
}

fn repo_slug_from_api_url(url: &str) -> Option<String> {
    let (_, tail) = url.split_once("/repos/")?;
    let mut parts = tail.trim_end_matches('/').split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// Accepts `owner/name` and every github.com URL form.
pub(crate) fn parse_repo_spec(spec: &str) -> Result<(String, String)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(EngineError::invalid("repository spec is empty"));
    }

    // scp-style remote: git@github.com:owner/name.git
    let path = if let Some((_, tail)) = spec.split_once("github.com:") {
        tail.to_string()
    } else if spec.contains("://") {
        let parsed = url::Url::parse(spec)
            .map_err(|e| EngineError::invalid(format!("could not read {spec:?}: {e}")))?;
        match parsed.host_str() {
            Some(host) if host.eq_ignore_ascii_case("github.com") => {}
            Some(host) if host.eq_ignore_ascii_case("www.github.com") => {}
            Some(host) => {
                return Err(EngineError::invalid(format!("{host} is not github.com")));
            }
            None => return Err(EngineError::invalid(format!("no host in {spec:?}"))),
        }
        parsed.path().to_string()
    } else if let Some(tail) = strip_bare_host(spec) {
        tail.to_string()
    } else {
        spec.to_string()
    };

    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let owner = segments.next().unwrap_or_default();
    // A trailing path like /pulls or /issues/3 is ignored.
    let name = segments.next().unwrap_or_default().trim_end_matches(".git");
    if !is_valid_segment(owner) || !is_valid_segment(name) {
        return Err(EngineError::invalid(format!(
            "{spec:?} is not a repository; expected owner/name or a github.com URL"
        )));
    }
    Ok((owner.to_string(), name.to_string()))
}

fn strip_bare_host(spec: &str) -> Option<&str> {
    for prefix in ["github.com/", "www.github.com/"] {
        if spec.len() > prefix.len() && spec[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return Some(&spec[prefix.len()..]);
        }
    }
    None
}

fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether a repo may be fetched now, given GitHub's own `X-Poll-Interval`
/// hint for it.
///
/// A repo with no hint, or one that has never polled, is always due: the hint
/// is advice about how soon it is worth asking *again*.
fn due(repo: &Repo, intervals: &HashMap<u64, i64>, now: i64) -> bool {
    let (Some(last), Some(interval)) = (repo.last_polled_at, intervals.get(&repo.id)) else {
        return true;
    };
    last + interval <= now
}

pub(crate) fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_forms_all_parse() {
        let expected = ("octocat".to_string(), "hello-world".to_string());
        for spec in [
            "octocat/hello-world",
            "  octocat/hello-world  ",
            "https://github.com/octocat/hello-world",
            "https://github.com/octocat/hello-world.git",
            "https://github.com/octocat/hello-world/pulls",
            "https://github.com/octocat/hello-world/issues/3",
            "https://www.github.com/octocat/hello-world",
            "github.com/octocat/hello-world",
            "git@github.com:octocat/hello-world.git",
        ] {
            assert_eq!(parse_repo_spec(spec).unwrap(), expected, "spec was {spec:?}");
        }
    }

    #[test]
    fn rubbish_specs_are_invalid() {
        for spec in ["", "   ", "octocat", "https://gitlab.com/a/b", "a//"] {
            let err = parse_repo_spec(spec).unwrap_err();
            assert_eq!(err.kind, crate::error::ErrorKind::Invalid, "spec was {spec:?}");
        }
    }

    #[test]
    fn logins_are_lowercased_deduped_and_ordered() {
        let logins = normalize_logins(
            ["Bob", "alice", "  BOB ", "@Carol", "", "ALICE"].into_iter(),
        );
        assert_eq!(logins, vec!["bob", "alice", "carol"]);
    }

    /// No samples means no percentile — the same `None` the median gives,
    /// never a zero that would read as "instant".
    #[test]
    fn the_p90_of_no_samples_is_none() {
        assert_eq!(percentile_90(&[]), None);
        assert_eq!(median(&[]), None);
    }

    /// One sample is its own p90: the rank `0.9 * (n - 1)` is zero, so there
    /// is nothing to interpolate towards.
    #[test]
    fn the_p90_of_one_sample_is_that_sample() {
        assert_eq!(percentile_90(&[7_200]), Some(7_200));
        assert_eq!(percentile_90(&[0]), Some(0));
    }

    /// R-7: the answer sits at rank `0.9 * (n - 1)` and is interpolated when
    /// that lands between two samples — which is the whole point. Nearest-rank
    /// would return the largest sample for every one of the small sets below,
    /// and a p90 that is always the maximum says nothing the maximum did not.
    #[test]
    fn the_p90_interpolates_between_the_two_closest_ranks() {
        // Two samples: nine tenths of the way from the faster to the slower.
        assert_eq!(percentile_90(&[100, 200]), Some(190));
        // Three: 2 + 0.8 * 8 = 8.4, rounded up off the whole second.
        assert_eq!(percentile_90(&[1, 2, 10]), Some(9));
        // Five (odd): rank 3.6, so 40 + 0.6 * 10.
        assert_eq!(percentile_90(&[10, 20, 30, 40, 50]), Some(46));
        // Ten (even): rank 8.1, so 90 + 0.1 * 10 — and emphatically not the
        // hundred that a nearest-rank p90 would have handed back.
        let ten: Vec<i64> = (1..=10).map(|n| n * 10).collect();
        assert_eq!(percentile_90(&ten), Some(91));
        assert_ne!(percentile_90(&ten), ten.last().copied());
        // Eleven: rank 9 exactly, so no interpolation at all.
        let eleven: Vec<i64> = (0..=10).map(|n| n * 10).collect();
        assert_eq!(percentile_90(&eleven), Some(90));

        // A flat set has a flat tail, whatever the count.
        for n in 1..=12 {
            let flat = vec![42i64; n];
            assert_eq!(percentile_90(&flat), Some(42), "{n} identical samples");
        }
    }

    /// The p90 can never sit below the median it is printed beside. The two
    /// are one rule at two probabilities, and the p90's round-up keeps the
    /// promise even where the median's round-toward-zero moves the other way.
    #[test]
    fn the_p90_is_never_below_the_median() {
        // A small deterministic generator, so the property is checked over
        // hundreds of shapes rather than the handful anyone would write out.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..500 {
            let n = (next() % 24) as usize + 1;
            let mut samples: Vec<i64> =
                (0..n).map(|_| (next() % 200_000) as i64 - 100_000).collect();
            samples.sort_unstable();
            let median = median(&samples).expect("non-empty");
            let p90 = percentile_90(&samples).expect("non-empty");
            assert!(p90 >= median, "p90 {p90} below median {median} for {samples:?}");
            assert!(p90 >= samples[0] && p90 <= samples[n - 1], "{p90} outside {samples:?}");
        }
    }

    /// The busiest commit author wins; a tie goes to the lower login, compared
    /// without regard to casing, so the answer never depends on the order the
    /// rows arrived in.
    #[test]
    fn the_top_commit_author_is_the_busiest_and_ties_go_to_the_lower_login() {
        let (login, share) = top_commit_author(&[("bob".into(), 1), ("alice".into(), 4)]);
        assert_eq!(login.as_deref(), Some("alice"));
        assert_eq!(share, 0.8);

        // A tie, given both ways round: the lower login wins either time.
        for authors in [
            vec![("bob".to_string(), 2), ("alice".to_string(), 2)],
            vec![("alice".to_string(), 2), ("bob".to_string(), 2)],
        ] {
            let (login, share) = top_commit_author(&authors);
            assert_eq!(login.as_deref(), Some("alice"));
            assert_eq!(share, 0.5);
        }
        // Casing does not decide a tie — "Bob" must not beat "alice" merely
        // because a capital B sorts before a lower-case a.
        let (login, _) = top_commit_author(&[("Bob".into(), 2), ("alice".into(), 2)]);
        assert_eq!(login.as_deref(), Some("alice"));

        // One author owns the repo outright.
        let (login, share) = top_commit_author(&[("solo".into(), 9)]);
        assert_eq!(login.as_deref(), Some("solo"));
        assert_eq!(share, 1.0);
    }

    /// A repo with no commits in the window has no busiest author, and says so
    /// with `None` and a zero share rather than naming a reviewer.
    #[test]
    fn the_top_commit_author_of_no_commits_is_none() {
        assert_eq!(top_commit_author(&[]), (None, 0.0));
        // A row that somehow counted zero commits is the same nothing.
        assert_eq!(top_commit_author(&[("alice".into(), 0)]), (None, 0.0));
    }

    #[test]
    fn repo_slug_parses_from_api_url() {
        assert_eq!(
            repo_slug_from_api_url("https://api.github.com/repos/acme/platform").unwrap(),
            "acme/platform"
        );
        assert_eq!(repo_slug_from_api_url("https://example.com/x"), None);
    }
}
