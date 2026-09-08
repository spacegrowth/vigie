//! SQLite persistence: schema, migrations, and every query the engine runs.
//!
//! Nothing secret is ever written here. The GitHub credential lives only in
//! `GitHubClient`, in memory, and this module has no way to reach it.

use crate::error::{EngineError, Result};
use crate::poll::{NewEvent, PullSeen, FIRST_POLL_LOOKBACK_SECS};
use crate::types::{
    Account, Event, EventKind, OpenPull, Repo, Settings, Team, ThreadKind, Watch, WatchSource,
    PULL_STATE_OPEN, REVIEW_APPROVED, REVIEW_CHANGES_REQUESTED, REVIEW_REQUIRED,
    WATCH_STATE_OPEN,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Current schema version, recorded in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 9;

/// What `repos.account_login` holds for a repo no account has claimed yet: every
/// repo a pre-v5 install added, until the first `add_account` claims them.
pub const UNCLAIMED_ACCOUNT: &str = "";

/// The name the single pre-v3 team carried when the user never renamed it. A v2
/// database still holding it, with no logins, is treated as "no team at all"
/// rather than migrated into an empty team the user never made.
const LEGACY_DEFAULT_TEAM_NAME: &str = "Team";

/// The v1 schema, verbatim. Frozen: v2 and later arrive as `ALTER`s below, so a
/// database created by an older build migrates down exactly the same path a
/// fresh one takes. The migration test applies this directly to build a real v1
/// database rather than a hand-made approximation of one.
const V1_SCHEMA: &str = r#"
                CREATE TABLE IF NOT EXISTS repos (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    owner           TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    url             TEXT NOT NULL,
                    default_branch  TEXT NOT NULL,
                    last_polled_at  INTEGER,
                    last_error      TEXT,
                    UNIQUE (owner, name) ON CONFLICT IGNORE
                );

                CREATE TABLE IF NOT EXISTS events (
                    id               INTEGER PRIMARY KEY AUTOINCREMENT,
                    repo_id          INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
                    kind             TEXT NOT NULL,
                    external_id      TEXT NOT NULL,
                    actor_login      TEXT NOT NULL,
                    actor_avatar_url TEXT,
                    title            TEXT NOT NULL,
                    body_preview     TEXT,
                    url              TEXT NOT NULL,
                    number           INTEGER,
                    occurred_at      INTEGER NOT NULL,
                    seen             INTEGER NOT NULL DEFAULT 0
                );

                CREATE UNIQUE INDEX IF NOT EXISTS events_dedupe
                    ON events (repo_id, kind, external_id);
                CREATE INDEX IF NOT EXISTS events_recent
                    ON events (occurred_at DESC, id DESC);
                CREATE INDEX IF NOT EXISTS events_actor
                    ON events (actor_login COLLATE NOCASE);

                CREATE TABLE IF NOT EXISTS team_logins (
                    position INTEGER PRIMARY KEY,
                    login    TEXT NOT NULL UNIQUE
                );

                CREATE TABLE IF NOT EXISTS settings (
                    id        INTEGER PRIMARY KEY CHECK (id = 1),
                    json      TEXT NOT NULL,
                    team_name TEXT NOT NULL
                );
"#;

/// Every column [`row_to_repo`] reads, in its order. Shared by the four repo
/// reads below so a column added here cannot reach one of them and miss
/// another.
const REPO_COLUMNS: &str = "id, owner, name, url, account_login, default_branch,
     last_polled_at, backfilled_to, last_error, hidden";

/// The `AND` clause that keeps a read out of hidden repos, written against
/// whichever column holds the repo id (`repo_id`, or an alias like
/// `m.repo_id`).
///
/// Every read the app makes goes through this: a hidden repo contributes no
/// events, no pulls, no watches and no unseen count until it is unhidden, and
/// its rows are meanwhile left exactly where they are. It carries no bound
/// arguments, so it can be appended to any query without disturbing the
/// numbering of the ones already there.
fn not_hidden(repo_id_column: &str) -> String {
    format!(" AND {repo_id_column} NOT IN (SELECT id FROM repos WHERE hidden = 1)")
}

/// The contract clamps `list_events` to this many rows.
pub const MAX_EVENT_LIMIT: u32 = 500;

/// The two review-event titles `review_decision` reads. The poller writes them
/// (see `poll::review_title`) and `docs/CONTRACT.md` fixes them, because a
/// review's state is carried by the event's title and by no column of its own.
const REVIEW_APPROVED_TITLE: &str = "Review: approved";
const REVIEW_CHANGES_REQUESTED_TITLE: &str = "Review: changes requested";

/// Added to every timestamp before SQLite divides it into hour buckets.
///
/// SQLite's integer division truncates toward zero rather than flooring, so a
/// negative `occurred_at + tz_offset` would bucket the wrong way and put an
/// event on the following day. Four hundred years of slack puts the floor
/// somewhere in the sixteenth century and the ceiling nowhere near `i64`, and
/// because it is a whole number of hours the bias subtracts back out exactly.
const HOUR_BUCKET_BIAS_SECS: i64 = 400 * 365 * 24 * 60 * 60;

/// How long a cached ETag is kept. Rows older than this are swept at open: a
/// URL nobody has asked about in a month belongs to a repo that was removed or
/// an endpoint that moved, and its row is dead weight either way.
pub const ETAG_TTL_SECS: i64 = 30 * 24 * 60 * 60;

pub struct Store {
    conn: Connection,
    /// True when the dead `settings.team_name` column could not be dropped
    /// (SQLite older than 3.35 has no `DROP COLUMN`). It is `NOT NULL` with no
    /// default, so inserts must still hand it a value even though nothing reads
    /// it. False on every build that bundles its own SQLite.
    legacy_team_name_column: bool,
}

/// The three aggregations one digest needs, read in one pass each rather than
/// by pulling every matching event into memory as an `Event`.
pub struct DigestRows {
    /// One row per (actor, kind) that occurs in the window.
    pub actors: Vec<DigestActorRow>,
    /// One row per PR/issue thread touched in the window.
    pub threads: Vec<DigestThreadRow>,
    /// `(repo_id, total)` for every repo with an event in the window.
    pub repos: Vec<(u64, u64)>,
    /// `(repo_id, login, commits)` — one row per repo per commit author in the
    /// window, for the per-repo author concentration. Only `commit` events, so
    /// a repo with none contributes no rows at all.
    pub repo_commit_authors: Vec<(u64, String, u64)>,
}

/// How often one actor did one kind of thing inside a digest window, and when
/// they last did it.
pub struct DigestActorRow {
    pub login: String,
    pub kind: EventKind,
    pub count: u64,
    pub avatar_url: Option<String>,
    /// The newest event of this (actor, kind) inside the window.
    pub last_at: i64,
}

/// One thread's activity inside a digest window, carrying the newest event's
/// kind, title, url and time — the row the contract labels the thread with.
pub struct DigestThreadRow {
    pub repo_id: u64,
    pub number: u64,
    pub last_kind: EventKind,
    pub title: String,
    pub url: String,
    pub events: u64,
    pub last_at: i64,
}

/// How often one actor did one kind of thing in one repo.
pub struct ActorTally {
    pub login: String,
    pub kind: EventKind,
    pub repo_id: u64,
    pub count: u64,
    pub avatar_url: Option<String>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;
        Store::from_connection(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Store> {
        Store::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Store { conn, legacy_team_name_column: false };
        store.migrate()?;
        store.legacy_team_name_column =
            column_exists(&store.conn, "settings", "team_name")?;
        // The ETag cache is capped here rather than on every write: it is a
        // cache, so a failed sweep is worth a log line and nothing more.
        if let Err(e) = store.prune_etags(crate::engine::now_secs() - ETAG_TTL_SECS) {
            log::warn!("could not sweep the etag cache: {e}");
        }
        Ok(store)
    }

    /// Applies every migration the database has not seen yet, all of it in one
    /// transaction — every step *and* the version stamp.
    ///
    /// SQLite gives DDL the same atomicity as any other statement, so an error
    /// or a crash part-way through leaves the database exactly as it was rather
    /// than half-migrated. The v3 step is why that matters: it drops
    /// `team_logins` before rebuilding and refilling it, and its re-run guard
    /// keys on the *rebuilt* shape. Interrupted between the drop and the
    /// refill, an unwrapped migration would lose the legacy team and then skip
    /// the step forever after, making the loss permanent and silent.
    fn migrate(&self) -> Result<()> {
        // `unchecked_transaction` rather than `transaction` because this has
        // only `&self`. Nothing else can be mid-transaction on this connection:
        // migrate runs once, from `from_connection`, before the store is shared.
        let tx = self.conn.unchecked_transaction()?;
        let version: i64 =
            tx.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap_or(0);
        if version < 1 {
            tx.execute_batch(V1_SCHEMA)?;
        }
        if version < 2 {
            // Rows written by v1 keep `body` NULL: the full text was never
            // fetched for them, and back-filling would mean refetching every
            // event from GitHub. The app renders `body_preview` when `body` is
            // null, and any re-poll of a still-live item fills it in.
            add_column_if_missing(&tx, "events", "body", "TEXT")?;
        }
        if version < 3 {
            migrate_to_v3(&tx)?;
        }
        if version < 4 {
            migrate_to_v4(&tx)?;
        }
        if version < 5 {
            migrate_to_v5(&tx)?;
        }
        if version < 6 {
            migrate_to_v6(&tx)?;
        }
        if version < 7 {
            migrate_to_v7(&tx)?;
        }
        if version < 8 {
            migrate_to_v8(&tx)?;
        }
        if version < 9 {
            migrate_to_v9(&tx)?;
        }
        if version != SCHEMA_VERSION {
            // `PRAGMA user_version` is transactional too, so the stamp lands
            // with the steps it claims to describe, never ahead of them.
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        // Anything that returned early above dropped `tx`, which rolls back.
        tx.commit()?;
        Ok(())
    }

    /// Only the migration tests read this back.
    #[cfg(test)]
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    // ---- accounts -------------------------------------------------------

    /// Every account, in the order they were added.
    ///
    /// `added_at` has one-second resolution and two accounts can be added inside
    /// one second, so `rowid` breaks the tie: the order is the insertion order,
    /// not whatever the table scan happens to produce.
    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT login, avatar_url, added_at FROM accounts ORDER BY added_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map([], row_to_account)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn find_account(&self, login: &str) -> Result<Option<Account>> {
        self.conn
            .query_row(
                "SELECT login, avatar_url, added_at FROM accounts WHERE login = ?1",
                params![login],
                row_to_account,
            )
            .optional()
            .map_err(EngineError::from)
    }

    /// Adds the account, or refreshes the avatar of one that is already there.
    ///
    /// Re-adding keeps the original `added_at`, so the display order does not
    /// jump when the app hands the engine a fresh token for an account the user
    /// signed into months ago. The primary key is `COLLATE NOCASE`, so GitHub's
    /// casing changing between two `/user` calls updates one row rather than
    /// making a second.
    pub fn upsert_account(
        &self,
        login: &str,
        avatar_url: Option<&str>,
        added_at: i64,
    ) -> Result<Account> {
        self.conn.execute(
            "INSERT INTO accounts (login, avatar_url, added_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (login) DO UPDATE SET avatar_url = excluded.avatar_url",
            params![login, avatar_url, added_at],
        )?;
        self.find_account(login)?
            .ok_or_else(|| EngineError::storage("account vanished immediately after upsert"))
    }

    /// Removes an account and every repo it owns, returning whether there was
    /// one to remove.
    ///
    /// The repos go first and explicitly: `repos.account_login` is a plain
    /// column, not a foreign key, so nothing would cascade on its own. Their
    /// events and watches then cascade through the `repos(id)` foreign key that
    /// is already there.
    pub fn delete_account(&self, login: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let removed =
            tx.execute("DELETE FROM accounts WHERE login = ?1", params![login])?;
        if removed == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM repos WHERE account_login = ?1 COLLATE NOCASE",
            params![login],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Gives every unclaimed repo to `login`, returning how many it took.
    ///
    /// This is the pre-v5 install's upgrade path: its repos were added before
    /// accounts existed, so they carry no owner until the first account arrives.
    pub fn claim_orphan_repos(&self, login: &str) -> Result<u64> {
        let claimed = self.conn.execute(
            "UPDATE repos SET account_login = ?1 WHERE account_login = ?2",
            params![login, UNCLAIMED_ACCOUNT],
        )?;
        Ok(claimed as u64)
    }

    /// Which account owns one repo, or `None` when there is no such repo. An
    /// unclaimed repo answers with the empty string, not with `None`.
    pub fn repo_account(&self, repo_id: u64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT account_login FROM repos WHERE id = ?1",
                params![repo_id as i64],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(EngineError::from)
    }

    // ---- repos ----------------------------------------------------------

    pub fn find_repo(&self, owner: &str, name: &str) -> Result<Option<Repo>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {REPO_COLUMNS}
                     FROM repos WHERE owner = ?1 COLLATE NOCASE AND name = ?2 COLLATE NOCASE"
                ),
                params![owner, name],
                row_to_repo,
            )
            .optional()
            .map_err(EngineError::from)
    }

    /// Inserts the repo, or returns the existing row when it is already watched.
    ///
    /// An existing row comes back untouched, `account_login` included: a repo
    /// already owned by one account is not silently reassigned to another just
    /// because it was added again.
    pub fn insert_repo(
        &self,
        owner: &str,
        name: &str,
        url: &str,
        account_login: &str,
        default_branch: &str,
    ) -> Result<Repo> {
        if let Some(existing) = self.find_repo(owner, name)? {
            return Ok(existing);
        }
        self.conn.execute(
            "INSERT INTO repos (owner, name, url, account_login, default_branch)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![owner, name, url, account_login, default_branch],
        )?;
        self.find_repo(owner, name)?
            .ok_or_else(|| EngineError::storage("repo vanished immediately after insert"))
    }

    pub fn find_repo_by_id(&self, id: u64) -> Result<Option<Repo>> {
        self.conn
            .query_row(
                &format!("SELECT {REPO_COLUMNS} FROM repos WHERE id = ?1"),
                params![id as i64],
                row_to_repo,
            )
            .optional()
            .map_err(EngineError::from)
    }

    /// Every repo, hidden ones included, in display order.
    ///
    /// The full list is what "is this repo already watched?" has to be asked
    /// against — a hidden repo is still watched — so `suggest_repos` reads
    /// this one. Everything the app *shows* or *polls* reads
    /// [`list_visible_repos`](Self::list_visible_repos) instead.
    pub fn list_repos(&self) -> Result<Vec<Repo>> {
        self.repos_where("")
    }

    /// The repos that are not hidden — what the app lists and what the poller
    /// spends its request budget on.
    pub fn list_visible_repos(&self) -> Result<Vec<Repo>> {
        self.repos_where(" WHERE hidden = 0")
    }

    /// The hidden ones, for the Repos view's own "Hidden" section — the one
    /// place they are still shown, because it is the page that manages them.
    pub fn list_hidden_repos(&self) -> Result<Vec<Repo>> {
        self.repos_where(" WHERE hidden = 1")
    }

    fn repos_where(&self, filter: &str) -> Result<Vec<Repo>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {REPO_COLUMNS} FROM repos{filter}
             ORDER BY owner COLLATE NOCASE, name COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map([], row_to_repo)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Removes the repo and everything that hangs off it: its events, pulls
    /// and watches all cascade through `repos(id)`. Destructive, and
    /// deliberately unchanged by hiding — [`set_repo_hidden`](Self::set_repo_hidden)
    /// is the reversible middle state, this one is not.
    pub fn remove_repo(&self, id: u64) -> Result<()> {
        let removed =
            self.conn.execute("DELETE FROM repos WHERE id = ?1", params![id as i64])?;
        if removed == 0 {
            return Err(EngineError::not_found(format!("no repo with id {id}")));
        }
        Ok(())
    }

    /// Hides or unhides one repo. Nothing is deleted either way: the flag is
    /// the whole of it, and every event, pull and watch stays exactly where it
    /// is, so unhiding brings all of it straight back with no refetch.
    ///
    /// Unhiding also decides what the *next* poll costs, and that is why `now`
    /// is passed in. While a repo is hidden nothing polls it, so its
    /// `last_polled_at` simply stops advancing; left alone, the first poll
    /// after unhiding would fetch `now - watermark` of history in one go — a
    /// window that grows without bound the longer the repo stayed hidden, and
    /// whose cost is not one request but one *per pull request updated in it*
    /// (see `poll::poll_repo`, which asks for each PR's reviews separately).
    /// A repo hidden for a month could therefore cost hundreds of requests the
    /// moment it came back, which is the opposite of what hiding is for.
    ///
    /// So a watermark older than [`FIRST_POLL_LOOKBACK_SECS`] is cleared,
    /// along with the backfill floor and the stale `last_error` — exactly what
    /// `rescan` does, and for the same reason. The next poll then costs one
    /// ordinary first-poll window (24h) whatever the gap was, the dedupe index
    /// keeps the re-fetched overlap from duplicating anything, and the cleared
    /// floor lets `backfill` walk back down into the gap deliberately, a step
    /// at a time, instead of paying for all of it unasked. A repo unhidden
    /// within the day keeps its watermark untouched and just resumes.
    ///
    /// `not_found` when no repo has that id.
    pub fn set_repo_hidden(&self, id: u64, hidden: bool, now: i64) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE repos SET hidden = ?2 WHERE id = ?1",
            params![id as i64, i64::from(hidden)],
        )?;
        if updated == 0 {
            return Err(EngineError::not_found(format!("no repo with id {id}")));
        }
        if !hidden {
            self.conn.execute(
                "UPDATE repos
                    SET last_polled_at = NULL, backfilled_to = NULL, last_error = NULL
                  WHERE id = ?1 AND last_polled_at < ?2",
                params![id as i64, now - FIRST_POLL_LOOKBACK_SECS],
            )?;
        }
        Ok(())
    }

    /// Advances the watermark and clears the last error. Only called after a
    /// fully successful poll of that repo.
    ///
    /// `window_start` is the bottom of the window that poll just fetched, and it
    /// seeds `backfilled_to` — the oldest instant this repo is known to cover —
    /// the *first* time the repo polls successfully. `COALESCE` is what makes
    /// that "first time only": every later poll fetches a window that starts
    /// above the floor already recorded, so letting it overwrite the cursor
    /// would throw away everything backfill has reached and hand the user the
    /// same week of history over and over.
    pub fn mark_repo_polled(&self, id: u64, at: i64, window_start: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE repos
                SET last_polled_at = ?2,
                    last_error = NULL,
                    backfilled_to = COALESCE(backfilled_to, ?3)
              WHERE id = ?1",
            params![id as i64, at, window_start],
        )?;
        Ok(())
    }

    /// Moves the backfill floor down after a window below it was fetched and
    /// stored. Unlike [`mark_repo_polled`](Self::mark_repo_polled) this always
    /// writes: reaching further back is exactly what it records.
    ///
    /// `last_polled_at` is deliberately untouched. A backfill reads the past;
    /// it says nothing about how current the repo is, and moving the watermark
    /// would make the next poll skip the window between the last poll and now.
    pub fn set_backfilled_to(&self, id: u64, floor: i64) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE repos SET backfilled_to = ?2 WHERE id = ?1",
            params![id as i64, floor],
        )?;
        if updated == 0 {
            return Err(EngineError::not_found(format!("no repo with id {id}")));
        }
        Ok(())
    }

    /// Clears the watermark for one repo, or for every repo when `id` is
    /// `None`, so the next poll's `window_start` falls back to the 24-hour
    /// first-poll window. `last_error` goes with it: a rescan is a fresh start,
    /// not a retry of the failure that was recorded last time.
    ///
    /// `backfilled_to` is cleared alongside it. The floor's meaning is "the
    /// oldest instant covered by a *contiguous* run of windows down from the
    /// watermark", and a rescan restarts that run from a fresh 24-hour window —
    /// so a floor left behind would claim coverage of a stretch this repo is
    /// about to stop tracking. The next successful poll re-seeds it, and the
    /// dedupe index means the history already stored is not lost, only
    /// re-reachable.
    ///
    /// `not_found` when a given id matches no repo. The `None` form has nothing
    /// to be missing, so an install with no repos yet clears nothing and
    /// succeeds.
    pub fn clear_watermark(&self, id: Option<u64>) -> Result<()> {
        let Some(id) = id else {
            self.conn.execute(
                "UPDATE repos
                    SET last_polled_at = NULL, backfilled_to = NULL, last_error = NULL",
                [],
            )?;
            return Ok(());
        };
        let cleared = self.conn.execute(
            "UPDATE repos
                SET last_polled_at = NULL, backfilled_to = NULL, last_error = NULL
              WHERE id = ?1",
            params![id as i64],
        )?;
        if cleared == 0 {
            return Err(EngineError::not_found(format!("no repo with id {id}")));
        }
        Ok(())
    }

    /// Records GitHub's `X-Poll-Interval` hint for one repo, in seconds.
    ///
    /// Written only when a poll actually saw a hint, so a poll answered
    /// entirely out of 304s (which carry no body and need no hint) leaves the
    /// last one standing rather than forgetting it.
    ///
    /// The value written is the largest hint that poll saw across the repo's
    /// endpoints, and it *replaces* whatever was there. A hint is advice about
    /// now: keeping the high-water mark of every hint ever seen would leave one
    /// transient spike throttling that repo for good, with nothing to lower it.
    pub fn set_poll_interval(&self, id: u64, secs: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE repos SET poll_interval_secs = ?2 WHERE id = ?1",
            params![id as i64, secs as i64],
        )?;
        Ok(())
    }

    /// Every repo that has a hint on it, as `repo id -> seconds`.
    ///
    /// Deliberately not a column on [`Repo`](crate::types::Repo): it is the
    /// poller's own bookkeeping about how often GitHub wants to be asked, and
    /// nothing that reads a repo has any use for it.
    pub fn poll_intervals(&self) -> Result<HashMap<u64, i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, poll_interval_secs FROM repos WHERE poll_interval_secs IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    // ---- etags ----------------------------------------------------------

    /// Every cached ETag whose URL starts with `prefix`, as `url -> etag`.
    ///
    /// One query per repo per poll rather than one per endpoint: the poller
    /// asks for its repo's whole prefix and looks up inside the map.
    pub fn etags_with_prefix(&self, prefix: &str) -> Result<HashMap<String, String>> {
        let mut stmt =
            self.conn.prepare("SELECT url, etag FROM etags WHERE url LIKE ?1 ESCAPE '\\'")?;
        let rows = stmt.query_map(params![like_prefix(prefix)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    /// Stores what a fetch learned, replacing any tag already held for the same
    /// URL. One transaction, so a poll's tags land together or not at all.
    pub fn put_etags(&self, entries: &[(String, String)], at: i64) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        // `unchecked_transaction` because this has only `&self`; the engine
        // holds the store behind a mutex, so nothing else is mid-transaction.
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO etags (url, etag, fetched_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(url) DO UPDATE
                    SET etag = excluded.etag, fetched_at = excluded.fetched_at",
            )?;
            for (url, etag) in entries {
                stmt.execute(params![url, etag, at])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Forgets cached tags: everything under `prefix`, or the whole cache when
    /// `prefix` is `None`.
    ///
    /// A rescan calls this. Its whole purpose is to refetch, and a tag GitHub
    /// would match turns that refetch into a 304 with an empty body — the one
    /// case where a free answer is the wrong answer.
    pub fn clear_etags(&self, prefix: Option<&str>) -> Result<()> {
        match prefix {
            Some(prefix) => self.conn.execute(
                "DELETE FROM etags WHERE url LIKE ?1 ESCAPE '\\'",
                params![like_prefix(prefix)],
            )?,
            None => self.conn.execute("DELETE FROM etags", [])?,
        };
        Ok(())
    }

    /// Drops every tag fetched before `before`. Returns how many went.
    pub fn prune_etags(&self, before: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM etags WHERE fetched_at < ?1", params![before])?)
    }

    /// Records the failure and deliberately leaves `last_polled_at` untouched,
    /// so the next poll retries the same window.
    pub fn mark_repo_error(&self, id: u64, message: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE repos SET last_error = ?2 WHERE id = ?1",
            params![id as i64, message],
        )?;
        Ok(())
    }

    // ---- events ---------------------------------------------------------

    /// Inserts one unseen event, returning `None` when the UNIQUE dedupe index
    /// rejects it as already stored.
    pub fn insert_event(&self, repo_id: u64, event: &NewEvent) -> Result<Option<Event>> {
        self.insert_event_as(repo_id, event, false)
    }

    /// The same insert, but stating up front whether the row arrives already
    /// seen.
    ///
    /// A poll inserts unseen — that is what the badge counts and what raises a
    /// notification. A backfill inserts seen: it fetches history the user
    /// deliberately scrolled back for, which was never "new" and must not light
    /// up the badge or fire a notification for something that happened weeks
    /// ago.
    pub fn insert_event_as(
        &self,
        repo_id: u64,
        event: &NewEvent,
        seen: bool,
    ) -> Result<Option<Event>> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO events
                (repo_id, kind, external_id, actor_login, actor_avatar_url,
                 title, body_preview, body, url, number, occurred_at, seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                repo_id as i64,
                event.kind.as_str(),
                event.external_id,
                event.actor_login,
                event.actor_avatar_url,
                event.title,
                event.body_preview,
                event.body,
                event.url,
                event.number.map(|n| n as i64),
                event.occurred_at,
                seen as i64,
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let id = self.conn.last_insert_rowid() as u64;
        Ok(Some(Event {
            id,
            repo_id,
            kind: event.kind,
            actor_login: event.actor_login.clone(),
            actor_avatar_url: event.actor_avatar_url.clone(),
            title: event.title.clone(),
            body_preview: event.body_preview.clone(),
            body: event.body.clone(),
            url: event.url.clone(),
            number: event.number,
            occurred_at: event.occurred_at,
            seen,
            // Both resolved by the caller once per batch, not once per row.
            team_ids: Vec::new(),
            watched: false,
        }))
    }

    /// Newest first by `(occurred_at, id)`, keyset-paginated with `before_id`.
    ///
    /// `team_id` keeps events whose actor is a *current* member of that team,
    /// and `watched_only` keeps events on a *currently* watched thread. Both
    /// tests are part of the `WHERE`, so `limit` counts rows that survived them
    /// rather than rows read before them. An unknown team id matches nobody and
    /// yields an empty page — not an error.
    ///
    /// `restrict_to`, when `Some`, keeps only events whose actor (case
    /// insensitively) is in that set — the engine's `list_events(mode)` builds
    /// it from every team's members plus every signed-in account, or passes
    /// `None` for `FilterMode::All` and for the fresh-install case where no
    /// team has a member yet. An empty set is treated the same as `None`
    /// (nothing to narrow by) rather than emitted as a SQL `IN ()`, which
    /// would match nothing for the wrong reason.
    #[allow(clippy::too_many_arguments)] // five independent, composable filters over one page of events; a struct would only rename this list, not shorten it
    pub fn list_events(
        &self,
        repo_id: Option<u64>,
        actor: Option<&str>,
        team_id: Option<u64>,
        restrict_to: Option<&HashSet<String>>,
        watched_only: bool,
        before_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let limit = limit.min(MAX_EVENT_LIMIT);
        if limit == 0 {
            return Ok(Vec::new());
        }

        // Anchor the keyset on the cursor row's sort position.
        let anchor: Option<(i64, i64)> = match before_id {
            Some(id) => self
                .conn
                .query_row(
                    "SELECT occurred_at, id FROM events WHERE id = ?1",
                    params![id as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?,
            None => None,
        };
        // A cursor pointing at a row that no longer exists yields nothing rather
        // than silently restarting from the newest event.
        if before_id.is_some() && anchor.is_none() {
            return Ok(Vec::new());
        }

        let mut sql = String::from(
            "SELECT id, repo_id, kind, actor_login, actor_avatar_url, title,
                    body_preview, body, url, number, occurred_at, seen
             FROM events WHERE 1 = 1",
        );
        sql.push_str(&not_hidden("repo_id"));
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(id) = repo_id {
            sql.push_str(" AND repo_id = ?");
            args.push(Box::new(id as i64));
        }
        if let Some(actor) = actor {
            sql.push_str(" AND actor_login = ? COLLATE NOCASE");
            args.push(Box::new(actor.to_string()));
        }
        if let Some(team_id) = team_id {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM team_logins tl
                              WHERE tl.team_id = ?
                                AND tl.login = events.actor_login COLLATE NOCASE)",
            );
            args.push(Box::new(team_id as i64));
        }
        if let Some(logins) = restrict_to.filter(|set| !set.is_empty()) {
            // `LOWER(...)` rather than `COLLATE NOCASE` because the operand
            // here is a placeholder list, not one comparison the collation of
            // a single argument can carry; `logins` is already lowercased by
            // every caller.
            sql.push_str(" AND LOWER(actor_login) IN (");
            for (i, login) in logins.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('?');
                args.push(Box::new(login.to_lowercase()));
            }
            sql.push(')');
        }
        if watched_only {
            // A commit's `number` is NULL and never equals `w.number`, so the
            // contract's "commits are never watched" falls out of the join
            // rather than needing a clause of its own.
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM watches w
                              WHERE w.repo_id = events.repo_id
                                AND w.number = events.number)",
            );
        }
        if let Some((occurred_at, id)) = anchor {
            sql.push_str(" AND (occurred_at < ? OR (occurred_at = ? AND id < ?))");
            args.push(Box::new(occurred_at));
            args.push(Box::new(occurred_at));
            args.push(Box::new(id));
        }
        sql.push_str(" ORDER BY occurred_at DESC, id DESC LIMIT ?");
        args.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), row_to_event)?;
        let mut events = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        self.resolve_team_ids(&mut events)?;
        self.resolve_watched(&mut events)?;
        Ok(events)
    }

    pub fn mark_seen(&self, ids: &[u64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut stmt = self.conn.prepare("UPDATE events SET seen = 1 WHERE id = ?1")?;
        for id in ids {
            stmt.execute(params![*id as i64])?;
        }
        Ok(())
    }

    pub fn unseen_count(&self) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM events WHERE seen = 0{}", not_hidden("repo_id")),
            [],
            |r| r.get(0),
        )?;
        Ok(count as u64)
    }

    /// Per-actor rollup used to rank people suggestions.
    pub fn actor_rollup(&self) -> Result<Vec<ActorTally>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT MIN(actor_login), kind, repo_id, COUNT(*), MAX(actor_avatar_url)
             FROM events WHERE 1 = 1{}
             GROUP BY actor_login COLLATE NOCASE, kind, repo_id",
            not_hidden("repo_id")
        ))?;
        let rows = stmt.query_map([], |row| {
            let login: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let repo_id: i64 = row.get(2)?;
            let count: i64 = row.get(3)?;
            let avatar: Option<String> = row.get(4)?;
            Ok((login, kind, repo_id as u64, count as u64, avatar))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (login, kind, repo_id, count, avatar_url) = row?;
            // A kind this build does not know about is skipped, not fatal.
            if let Ok(kind) = kind.parse::<EventKind>() {
                out.push(ActorTally { login, kind, repo_id, count, avatar_url });
            }
        }
        Ok(out)
    }

    // ---- watches --------------------------------------------------------

    /// Every watch, newest `since` first.
    ///
    /// `(repo_id, number)` breaks a tie the contract's single key leaves open —
    /// several rows written by one auto-watch pass share a `since` to the
    /// second — so the order is stable between calls rather than left to
    /// whatever the table scan happens to produce.
    pub fn list_watches(&self) -> Result<Vec<Watch>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT repo_id, number, kind, title, state, source, since
             FROM watches WHERE 1 = 1{}
             ORDER BY since DESC, repo_id ASC, number ASC",
            not_hidden("repo_id")
        ))?;
        let rows = stmt.query_map([], row_to_watch)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_watch(&self, repo_id: u64, number: u64) -> Result<Option<Watch>> {
        self.conn
            .query_row(
                "SELECT repo_id, number, kind, title, state, source, since
                 FROM watches WHERE repo_id = ?1 AND number = ?2",
                params![repo_id as i64, number as i64],
                row_to_watch,
            )
            .optional()
            .map_err(EngineError::from)
    }

    /// Writes the watch unless one is already there, returning whether it was
    /// written.
    ///
    /// `INSERT OR IGNORE` on the `(repo_id, number)` primary key, so an existing
    /// row keeps its own `source` and `since`: auto-watch must never overwrite
    /// a manual watch, and re-watching must never move the `since`.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_watch_if_absent(
        &self,
        repo_id: u64,
        number: u64,
        kind: ThreadKind,
        title: &str,
        state: &str,
        source: WatchSource,
        since: i64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO watches
                (repo_id, number, kind, title, state, source, since)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                repo_id as i64,
                number as i64,
                kind.as_str(),
                title,
                state,
                source.as_str(),
                since,
            ],
        )?;
        Ok(changed > 0)
    }

    /// Updates one watch's `title` and `state` to what the poll just saw.
    /// A thread that is not watched matches no row and is silently a no-op,
    /// which is what lets the caller offer every thread it saw.
    pub fn refresh_watch(
        &self,
        repo_id: u64,
        number: u64,
        title: &str,
        state: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE watches SET title = ?3, state = ?4 WHERE repo_id = ?1 AND number = ?2",
            params![repo_id as i64, number as i64, title, state],
        )?;
        Ok(())
    }

    /// Removes one watch, returning whether there was one to remove. The caller
    /// turns `false` into `not_found`, because it owns the message.
    pub fn delete_watch(&self, repo_id: u64, number: u64) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM watches WHERE repo_id = ?1 AND number = ?2",
            params![repo_id as i64, number as i64],
        )?;
        Ok(removed > 0)
    }

    /// One thread's stored events, newest first by `(occurred_at, id)`, as
    /// `(kind, title)` pairs.
    ///
    /// The caller turns these into a watch's `kind`, `title` and `state`; the
    /// ordering is the whole point, so it is fixed here rather than left to
    /// each caller to re-impose.
    pub fn thread_events(&self, repo_id: u64, number: u64) -> Result<Vec<(EventKind, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, title FROM events
             WHERE repo_id = ?1 AND number = ?2
             ORDER BY occurred_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![repo_id as i64, number as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (kind, title) = row?;
            // A kind this build does not know about is skipped, not fatal.
            if let Ok(kind) = kind.parse::<EventKind>() {
                out.push((kind, title));
            }
        }
        Ok(out)
    }

    /// Every watched `(repo_id, number)`. Read once per poll and once per read
    /// of the feed, never once per event.
    pub fn watched_keys(&self) -> Result<HashSet<(u64, u64)>> {
        let mut stmt = self.conn.prepare("SELECT repo_id, number FROM watches")?;
        let keys = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(keys)
    }

    /// Fills in `watched` on events that were read or inserted with it false.
    ///
    /// One read of the whole (small) watch set, exactly as `resolve_team_ids`
    /// does one lookup per distinct actor: nothing here is per-row work.
    pub fn resolve_watched(&self, events: &mut [Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let keys = self.watched_keys()?;
        for event in events {
            // A commit has no `number` and so belongs to no thread.
            event.watched =
                event.number.is_some_and(|number| keys.contains(&(event.repo_id, number)));
        }
        Ok(())
    }

    /// Unwatches every thread that is no longer open — the app's "Clear closed".
    /// Returns how many rows went.
    pub fn clear_closed_watches(&self) -> Result<u64> {
        let removed = self
            .conn
            .execute("DELETE FROM watches WHERE state <> ?1", params![WATCH_STATE_OPEN])?;
        Ok(removed as u64)
    }

    // ---- digests --------------------------------------------------------

    /// Everything a digest needs for `[start, end)`, in three grouped reads.
    ///
    /// `logins` is a team's membership, already resolved and lowercased by the
    /// caller so the set is looked up once instead of as a correlated subquery
    /// per event row; `None` means "no team scoping". An *empty* set matches
    /// nobody — SQLite accepts an empty `IN ()` list and reads it as always
    /// false — which is what an unknown or empty team must yield.
    ///
    /// No `Event` is ever materialised here: the counting happens in SQLite and
    /// only the grouped rows cross into Rust, so a digest over a year of events
    /// costs the same handful of rows as one over a day.
    pub fn digest_rows(
        &self,
        start: i64,
        end: i64,
        logins: Option<&HashSet<String>>,
        actor: Option<&str>,
    ) -> Result<DigestRows> {
        let (filter, args) = digest_filter(start, end, logins, actor);
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();

        // Per actor per kind. `MIN(actor_login)` picks one canonical casing for
        // a login stored with several, exactly as `actor_rollup` does.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT MIN(actor_login), kind, COUNT(*), MAX(actor_avatar_url), MAX(occurred_at)
             FROM events{filter}
             GROUP BY actor_login COLLATE NOCASE, kind"
        ))?;
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut actors = Vec::new();
        for row in rows {
            let (login, kind, count, avatar_url, last_at) = row?;
            // A kind this build does not know about is skipped, not fatal.
            if let Ok(kind) = kind.parse::<EventKind>() {
                actors.push(DigestActorRow {
                    login,
                    kind,
                    count: count as u64,
                    avatar_url,
                    last_at,
                });
            }
        }
        drop(stmt);

        // Per thread. The window functions do both jobs in one scan: `COUNT(*)`
        // over the partition is the thread's event count, and `rn = 1` keeps
        // the newest row, whose own `kind`, `title`, `url` and `occurred_at`
        // are therefore the newest event's. Ordering the row number by
        // `(occurred_at, id)` makes the pick deterministic when two events in
        // a thread share a timestamp.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT repo_id, number, kind, title, url, occurred_at, events FROM (
                 SELECT repo_id, number, kind, title, url, occurred_at,
                        COUNT(*) OVER (PARTITION BY repo_id, number) AS events,
                        ROW_NUMBER() OVER (PARTITION BY repo_id, number
                                           ORDER BY occurred_at DESC, id DESC) AS rn
                 FROM events{filter} AND number IS NOT NULL
             ) WHERE rn = 1"
        ))?;
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)? as u64,
            ))
        })?;
        let mut threads = Vec::new();
        for row in rows {
            let (repo_id, number, kind, title, url, last_at, events) = row?;
            if let Ok(last_kind) = kind.parse::<EventKind>() {
                threads.push(DigestThreadRow {
                    repo_id,
                    number,
                    last_kind,
                    title,
                    url,
                    events,
                    last_at,
                });
            }
        }
        drop(stmt);

        // Per repo. Events cascade with their repo, so every id here is watched.
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT repo_id, COUNT(*) FROM events{filter} GROUP BY repo_id"))?;
        let repos = stmt
            .query_map(refs.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Per repo per commit author. Commits only: the concentration is a
        // statement about who *writes* a repo, and a reviewer or a commenter
        // is not an author of it. `MIN(actor_login)` picks the canonical
        // casing exactly as the per-actor read above does.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT repo_id, MIN(actor_login), COUNT(*)
             FROM events{filter} AND kind = 'commit'
             GROUP BY repo_id, actor_login COLLATE NOCASE"
        ))?;
        let repo_commit_authors = stmt
            .query_map(refs.as_slice(), |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as u64,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(DigestRows { actors, threads, repos, repo_commit_authors })
    }

    /// One local hour's worth of one kind, for the series and the heatmap.
    ///
    /// The bucket is the number of whole hours from the biased epoch to the
    /// event's *local* clock — see [`HOUR_BUCKET_BIAS_SECS`] — and both the
    /// per-day series and the 7x24 grid are folded out of it in Rust, so the
    /// two can never disagree about which day an event fell on. Grouping in
    /// SQLite keeps this the same handful of rows a `digest_rows` read is: at
    /// most one row per hour per kind in the window.
    pub fn digest_hour_buckets(
        &self,
        start: i64,
        end: i64,
        logins: Option<&HashSet<String>>,
        actor: Option<&str>,
        tz_offset_secs: i32,
    ) -> Result<Vec<(i64, EventKind, u64)>> {
        let (filter, mut args) = digest_filter(start, end, logins, actor);
        // The shift goes in front of the filter's own arguments because it is
        // bound in the SELECT list, which SQLite numbers before the WHERE.
        let shift = tz_offset_secs as i64 + HOUR_BUCKET_BIAS_SECS;
        args.insert(0, Box::new(shift));
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();

        let mut stmt = self.conn.prepare(&format!(
            "SELECT (occurred_at + ?) / 3600 AS bucket, kind, COUNT(*)
             FROM events{filter}
             GROUP BY bucket, kind"
        ))?;
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (bucket, kind, count) = row?;
            // A kind this build does not know about is skipped, not fatal —
            // the same rule `digest_rows` applies.
            if let Ok(kind) = kind.parse::<EventKind>() {
                out.push((bucket - HOUR_BUCKET_BIAS_SECS / 3600, kind, count as u64));
            }
        }
        Ok(out)
    }

    /// Every pull-request timing measurable in `[start, end)`, as
    /// `(time-to-merge samples, time-to-first-review samples, PRs measured)`.
    ///
    /// Both are joined to the PR's own `pr_opened` event, and both are scoped
    /// by the *author* — the actor on that `pr_opened` — rather than by whoever
    /// merged or reviewed. "How long did this team's PRs take" is a question
    /// about the team's PRs, so a review or a merge by an outsider still counts
    /// towards the author's numbers.
    ///
    /// The window bounds the *closing* event: the merge for TTM, and the
    /// earliest review for TTFR. A PR opened long before the window and merged
    /// inside it counts, which is what "what finished this week" means; one
    /// opened inside it and neither merged nor reviewed counts towards neither.
    ///
    /// The dedupe index makes `pr_opened` and `pr_merged` at most one row each
    /// per `(repo_id, number)` — their `external_id`s are `<n>:opened` and
    /// `<n>:merged` — so neither join can multiply one PR into several samples.
    /// A PR whose own `pr_opened` was never stored (the repo was added after it
    /// was opened) is measured by neither: the join drops it rather than
    /// inventing a start.
    pub fn pr_timing_samples(
        &self,
        start: i64,
        end: i64,
        logins: Option<&HashSet<String>>,
        actor: Option<&str>,
    ) -> Result<(Vec<i64>, Vec<i64>, u64)> {
        let (author_filter, author_args) = author_filter(logins, actor);
        let bind = |values: &[String]| -> Vec<Box<dyn rusqlite::ToSql>> {
            let mut args: Vec<Box<dyn rusqlite::ToSql>> =
                vec![Box::new(start), Box::new(end)];
            for value in values {
                args.push(Box::new(value.clone()));
            }
            args
        };
        let mut measured: HashSet<(u64, u64)> = HashSet::new();

        // Time to merge.
        let args = bind(&author_args);
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT m.repo_id, m.number, m.occurred_at - o.occurred_at
             FROM events m
             JOIN events o
               ON o.repo_id = m.repo_id AND o.number = m.number AND o.kind = 'pr_opened'
             WHERE m.kind = 'pr_merged'
               AND m.occurred_at >= ? AND m.occurred_at < ?
               AND m.occurred_at >= o.occurred_at{author_filter}{visible}",
            visible = not_hidden("m.repo_id")
        ))?;
        let mut ttm = Vec::new();
        let rows = stmt.query_map(refs.as_slice(), timing_row)?;
        for row in rows {
            let (repo_id, number, secs) = row?;
            measured.insert((repo_id, number));
            ttm.push(secs);
        }
        drop(stmt);

        // Time to first review. The inner grouping picks each PR's *earliest*
        // review over all of history, and only then is it asked whether that
        // one landed in the window — a PR first reviewed last month has
        // nothing to say about this week, however often it was reviewed since.
        let args = bind(&author_args);
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT f.repo_id, f.number, f.first_at - o.occurred_at
             FROM (SELECT repo_id, number, MIN(occurred_at) AS first_at
                   FROM events WHERE kind = 'pr_reviewed' AND number IS NOT NULL
                   GROUP BY repo_id, number) f
             JOIN events o
               ON o.repo_id = f.repo_id AND o.number = f.number AND o.kind = 'pr_opened'
             WHERE f.first_at >= ? AND f.first_at < ?
               AND f.first_at >= o.occurred_at{author_filter}{visible}",
            visible = not_hidden("f.repo_id")
        ))?;
        let mut ttfr = Vec::new();
        let rows = stmt.query_map(refs.as_slice(), timing_row)?;
        for row in rows {
            let (repo_id, number, secs) = row?;
            measured.insert((repo_id, number));
            ttfr.push(secs);
        }
        drop(stmt);

        Ok((ttm, ttfr, measured.len() as u64))
    }

    /// How many pull requests merged inside `[start, end)` have **no** stored
    /// `pr_reviewed` event at any time up to their merge.
    ///
    /// Scoped by the PR's *author*, the same rule [`Self::pr_timing_samples`]
    /// follows and for the same reason: "did this team ship without review" is
    /// a question about the team's pull requests, not about whoever pressed
    /// merge. The `pr_opened` join is therefore only added when there is a
    /// scope to apply — unscoped, every merge in the window counts, including
    /// one whose opening was never stored; scoped, such a merge is dropped,
    /// because nothing in the store names its author.
    ///
    /// The `NOT EXISTS` looks at *all* of history up to the merge instant, not
    /// just the window, so a PR reviewed last month and merged this week is
    /// reviewed. It can still only see what Vigie stored: a repo added after a
    /// review happened holds no `pr_reviewed` for it, and that PR's merge reads
    /// as unreviewed here — the same limitation `review_decision` carries.
    pub fn unreviewed_merges(
        &self,
        start: i64,
        end: i64,
        logins: Option<&HashSet<String>>,
        actor: Option<&str>,
    ) -> Result<u64> {
        let (author_filter, author_args) = author_filter(logins, actor);
        // Only a scoped query needs the author, and only a scoped query should
        // pay for the join — or drop a merge for want of an opening.
        let join = if author_filter.is_empty() {
            ""
        } else {
            "JOIN events o
               ON o.repo_id = m.repo_id AND o.number = m.number AND o.kind = 'pr_opened'"
        };
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(start), Box::new(end)];
        for value in &author_args {
            args.push(Box::new(value.clone()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();

        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*)
                 FROM events m
                 {join}
                 WHERE m.kind = 'pr_merged' AND m.number IS NOT NULL
                   AND m.occurred_at >= ? AND m.occurred_at < ?
                   AND NOT EXISTS (
                       SELECT 1 FROM events r
                       WHERE r.kind = 'pr_reviewed'
                         AND r.repo_id = m.repo_id AND r.number = m.number
                         AND r.occurred_at <= m.occurred_at
                   ){author_filter}{visible}",
                visible = not_hidden("m.repo_id")
            ),
            refs.as_slice(),
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    // ---- pulls ----------------------------------------------------------

    /// Records one pull request exactly as the last poll saw it.
    ///
    /// `review_decision` and `last_activity_at` are derived here rather than
    /// passed in, because both are answers about the *stored* events for this
    /// PR and this is the only place holding the lock over them. Everything
    /// else is GitHub's own answer, replaced wholesale: a PR row is current
    /// state, not history, so a second poll simply overwrites the first.
    pub fn upsert_pull(&self, repo_id: u64, pull: &PullSeen) -> Result<()> {
        let decision = self.review_decision(repo_id, pull)?;
        let newest_event: Option<i64> = self.conn.query_row(
            "SELECT MAX(occurred_at) FROM events WHERE repo_id = ?1 AND number = ?2",
            params![repo_id as i64, pull.number as i64],
            |row| row.get(0),
        )?;
        let last_activity_at = pull.updated_at.max(newest_event.unwrap_or(i64::MIN));
        let reviewers = serde_json::to_string(&pull.requested_reviewers)
            .map_err(|e| EngineError::storage(format!("could not encode reviewers: {e}")))?;

        self.conn.execute(
            "INSERT INTO pulls (repo_id, number, title, url, author_login, author_avatar_url,
                                state, draft, created_at, updated_at, merged_at, closed_at,
                                additions, deletions, requested_reviewers, review_decision,
                                last_activity_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT (repo_id, number) DO UPDATE SET
                 title = excluded.title,
                 url = excluded.url,
                 author_login = excluded.author_login,
                 author_avatar_url = excluded.author_avatar_url,
                 state = excluded.state,
                 draft = excluded.draft,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at,
                 merged_at = excluded.merged_at,
                 closed_at = excluded.closed_at,
                 -- The list shape omits both, so a poll must not wipe the
                 -- numbers a single-PR fetch put there.
                 additions = COALESCE(excluded.additions, pulls.additions),
                 deletions = COALESCE(excluded.deletions, pulls.deletions),
                 requested_reviewers = excluded.requested_reviewers,
                 review_decision = excluded.review_decision,
                 last_activity_at = excluded.last_activity_at",
            params![
                repo_id as i64,
                pull.number as i64,
                pull.title,
                pull.url,
                pull.author_login,
                pull.author_avatar_url,
                pull.state,
                pull.draft as i64,
                pull.created_at,
                pull.updated_at,
                pull.merged_at,
                pull.closed_at,
                pull.additions,
                pull.deletions,
                reviewers,
                decision,
                last_activity_at,
            ],
        )?;
        Ok(())
    }

    /// Where one PR's review stands, from the `pr_reviewed` events already
    /// stored for it: the newest review *per reviewer* decides that reviewer's
    /// position, and one "changes requested" outranks any number of approvals.
    ///
    /// A review's state is not stored as a column — the poller folds it into
    /// the event `title`, which the contract fixes at `Review: approved` /
    /// `Review: changes requested` / `Review: commented` — so that is what this
    /// reads. A plain comment is neither an approval nor a block and leaves the
    /// decision to fall through to `review_required`.
    fn review_decision(&self, repo_id: u64, pull: &PullSeen) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT title FROM (
                 SELECT title,
                        ROW_NUMBER() OVER (PARTITION BY actor_login COLLATE NOCASE
                                           ORDER BY occurred_at DESC, id DESC) AS rn
                 FROM events
                 WHERE repo_id = ?1 AND number = ?2 AND kind = 'pr_reviewed'
             ) WHERE rn = 1",
        )?;
        let titles = stmt
            .query_map(params![repo_id as i64, pull.number as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if titles.iter().any(|t| t.eq_ignore_ascii_case(REVIEW_CHANGES_REQUESTED_TITLE)) {
            return Ok(Some(REVIEW_CHANGES_REQUESTED.to_string()));
        }
        if titles.iter().any(|t| t.eq_ignore_ascii_case(REVIEW_APPROVED_TITLE)) {
            return Ok(Some(REVIEW_APPROVED.to_string()));
        }
        if pull.requested_reviewers.is_empty() {
            Ok(None)
        } else {
            Ok(Some(REVIEW_REQUIRED.to_string()))
        }
    }

    /// Every open pull request, oldest first, optionally narrowed to a team's
    /// membership or to one author.
    ///
    /// The reviewer filter is *not* here: `requested_reviewers` is a JSON
    /// array, and the caller applies that one in Rust over what this returns —
    /// open PRs are few, and a `json_each` join would cost more to read than it
    /// saves.
    pub fn open_pulls(
        &self,
        logins: Option<&HashSet<String>>,
        actor: Option<&str>,
    ) -> Result<Vec<OpenPull>> {
        let mut sql = String::from(
            "SELECT repo_id, number, title, url, author_login, author_avatar_url, draft,
                    created_at, updated_at, last_activity_at, additions, deletions,
                    requested_reviewers, review_decision
             FROM pulls WHERE state = ?",
        );
        sql.push_str(&not_hidden("repo_id"));
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(PULL_STATE_OPEN)];
        if let Some(actor) = actor {
            sql.push_str(" AND author_login = ? COLLATE NOCASE");
            args.push(Box::new(actor.to_string()));
        }
        if let Some(logins) = logins {
            // Sorted for the same reason `digest_filter` sorts: one membership
            // always produces one SQL string, which keeps the statement cache
            // useful.
            let mut sorted: Vec<&str> = logins.iter().map(String::as_str).collect();
            sorted.sort_unstable();
            sql.push_str(" AND author_login COLLATE NOCASE IN (");
            for (i, login) in sorted.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('?');
                args.push(Box::new(login.to_string()));
            }
            sql.push(')');
        }
        sql.push_str(" ORDER BY created_at ASC, repo_id ASC, number ASC");

        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(refs.as_slice(), row_to_open_pull)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// How many open PRs each login authored, and how many are waiting on each
    /// login's review, both keyed by the lowercased login.
    ///
    /// Current state, not window-scoped: see [`crate::types::DigestPerson`].
    pub fn open_pull_person_counts(&self) -> Result<(HashMap<String, u64>, HashMap<String, u64>)> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT author_login, requested_reviewers FROM pulls WHERE state = ?1{}",
                not_hidden("repo_id")
            ))?;
        let rows = stmt.query_map(params![PULL_STATE_OPEN], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut authored: HashMap<String, u64> = HashMap::new();
        let mut queued: HashMap<String, u64> = HashMap::new();
        for row in rows {
            let (author, reviewers) = row?;
            *authored.entry(author.to_lowercase()).or_insert(0) += 1;
            // One PR can only be in a reviewer's queue once, however many times
            // GitHub listed them.
            let mut seen: HashSet<String> = HashSet::new();
            for reviewer in decode_reviewers(&reviewers) {
                if seen.insert(reviewer.to_lowercase()) {
                    *queued.entry(reviewer.to_lowercase()).or_insert(0) += 1;
                }
            }
        }
        Ok((authored, queued))
    }

    // ---- teams ----------------------------------------------------------

    /// Every team in display order, each with its logins in their own order.
    ///
    /// Two queries rather than a join: the logins come back already grouped and
    /// ordered, so a team with no logins is not lost the way an inner join
    /// would lose it.
    pub fn list_teams(&self) -> Result<Vec<Team>> {
        let mut stmt =
            self.conn.prepare("SELECT id, name FROM teams ORDER BY position ASC, id ASC")?;
        let heads = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut logins_by_team = self.logins_by_team()?;
        Ok(heads
            .into_iter()
            .map(|(id, name)| Team {
                id,
                name,
                logins: logins_by_team.remove(&id).unwrap_or_default(),
            })
            .collect())
    }

    /// One team's logins in order, or `None` when there is no such team. The
    /// `None` distinguishes an unknown id from a team that simply has nobody in
    /// it, which callers need in order to raise `not_found`.
    pub fn team_logins(&self, id: u64) -> Result<Option<Vec<String>>> {
        let exists: Option<i64> = self
            .conn
            .query_row("SELECT id FROM teams WHERE id = ?1", params![id as i64], |r| r.get(0))
            .optional()?;
        if exists.is_none() {
            return Ok(None);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT login FROM team_logins WHERE team_id = ?1 ORDER BY position ASC")?;
        let logins = stmt
            .query_map(params![id as i64], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(logins))
    }

    /// Appends a team after the last one. `logins` must already be normalized.
    pub fn create_team(&self, name: &str, logins: &[String]) -> Result<Team> {
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM teams",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO teams (name, position) VALUES (?1, ?2)",
            params![name, position],
        )?;
        let id = self.conn.last_insert_rowid() as u64;
        self.replace_team_logins(id, logins)?;
        Ok(Team { id, name: name.to_string(), logins: logins.to_vec() })
    }

    /// Replaces one team's name and logins wholesale. `not_found` if unknown.
    pub fn update_team(&self, id: u64, name: &str, logins: &[String]) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE teams SET name = ?2 WHERE id = ?1",
            params![id as i64, name],
        )?;
        if changed == 0 {
            return Err(EngineError::not_found(format!("no team with id {id}")));
        }
        self.replace_team_logins(id, logins)
    }

    /// Events are left alone: they are not owned by teams. The team's logins go
    /// with it through `ON DELETE CASCADE`.
    pub fn delete_team(&self, id: u64) -> Result<()> {
        let removed =
            self.conn.execute("DELETE FROM teams WHERE id = ?1", params![id as i64])?;
        if removed == 0 {
            return Err(EngineError::not_found(format!("no team with id {id}")));
        }
        Ok(())
    }

    /// Rewrites display order. `ids` must be a permutation of every team id;
    /// the caller checks that, because it owns the error message.
    pub fn reorder_teams(&self, ids: &[u64]) -> Result<()> {
        let mut stmt = self.conn.prepare("UPDATE teams SET position = ?2 WHERE id = ?1")?;
        for (position, id) in ids.iter().enumerate() {
            stmt.execute(params![*id as i64, position as i64])?;
        }
        Ok(())
    }

    /// Every team id, in display order. Used to validate a reorder.
    pub fn team_ids(&self) -> Result<Vec<u64>> {
        let mut stmt = self.conn.prepare("SELECT id FROM teams ORDER BY position ASC, id ASC")?;
        let ids = stmt
            .query_map([], |row| Ok(row.get::<_, i64>(0)? as u64))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Which teams one login belongs to, in team display order.
    pub fn team_ids_for_login(&self, login: &str) -> Result<Vec<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT tl.team_id FROM team_logins tl
             JOIN teams t ON t.id = tl.team_id
             WHERE tl.login = ?1 COLLATE NOCASE
             ORDER BY t.position ASC, t.id ASC",
        )?;
        let ids = stmt
            .query_map(params![login], |row| Ok(row.get::<_, i64>(0)? as u64))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// The union of every team's logins — what the `team` ingestion filter
    /// admits. Resolved once per poll, never per event.
    pub fn member_logins(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT login FROM team_logins")?;
        let logins = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(logins)
    }

    /// Fills in `team_ids` on events that were read or inserted with it empty.
    ///
    /// Looked up once per *distinct actor* rather than once per event: a page of
    /// 500 events is written by a handful of people, so this is a few indexed
    /// lookups, not 500.
    pub fn resolve_team_ids(&self, events: &mut [Event]) -> Result<()> {
        let mut by_login: HashMap<String, Vec<u64>> = HashMap::new();
        for event in &*events {
            let login = event.actor_login.to_lowercase();
            if !by_login.contains_key(&login) {
                by_login.insert(login.clone(), self.team_ids_for_login(&login)?);
            }
        }
        for event in events {
            let login = event.actor_login.to_lowercase();
            event.team_ids = by_login.get(&login).cloned().unwrap_or_default();
        }
        Ok(())
    }

    fn logins_by_team(&self) -> Result<HashMap<u64, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT team_id, login FROM team_logins ORDER BY team_id ASC, position ASC")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out: HashMap<u64, Vec<String>> = HashMap::new();
        for (team_id, login) in rows {
            out.entry(team_id).or_default().push(login);
        }
        Ok(out)
    }

    fn replace_team_logins(&self, id: u64, logins: &[String]) -> Result<()> {
        self.conn
            .execute("DELETE FROM team_logins WHERE team_id = ?1", params![id as i64])?;
        let mut stmt = self
            .conn
            .prepare("INSERT INTO team_logins (team_id, login, position) VALUES (?1, ?2, ?3)")?;
        for (position, login) in logins.iter().enumerate() {
            stmt.execute(params![id as i64, login, position as i64])?;
        }
        Ok(())
    }

    // ---- settings -------------------------------------------------------

    fn ensure_settings_row(&self) -> Result<()> {
        let defaults = serde_json::to_string(&Settings::default())
            .map_err(|e| EngineError::storage(e.to_string()))?;
        if self.legacy_team_name_column {
            // The v3 migration could not drop the column on this SQLite, and it
            // is `NOT NULL` with no default, so the insert still has to name it.
            // Nothing ever reads it back.
            self.conn.execute(
                "INSERT OR IGNORE INTO settings (id, json, team_name) VALUES (1, ?1, '')",
                params![defaults],
            )?;
        } else {
            self.conn.execute(
                "INSERT OR IGNORE INTO settings (id, json) VALUES (1, ?1)",
                params![defaults],
            )?;
        }
        Ok(())
    }

    pub fn get_settings(&self) -> Result<Settings> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT json FROM settings WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        match raw {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(Settings::default()),
        }
    }

    pub fn set_settings(&self, settings: &Settings) -> Result<()> {
        let json = serde_json::to_string(settings)
            .map_err(|e| EngineError::storage(e.to_string()))?;
        self.ensure_settings_row()?;
        self.conn
            .execute("UPDATE settings SET json = ?1 WHERE id = 1", params![json])?;
        Ok(())
    }
}

/// Schema v3: the single team becomes many.
///
/// The old `team_logins (position PRIMARY KEY, login UNIQUE)` held one unnamed
/// list and `settings.team_name` held its name. v3 introduces `teams` and
/// re-points `team_logins` at it. Whatever the v2 database held becomes team
/// id 1 at position 0 — but only if it held something: a database with no
/// logins whose name is still the stock "Team" is a user who never set a team
/// up, and gets none, so the app's onboarding can ask.
///
/// Safe to re-run. Every step is guarded, so a database a newer build already
/// migrated passes through untouched rather than losing its teams.
fn migrate_to_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS teams (
            id       INTEGER PRIMARY KEY,
            name     TEXT NOT NULL,
            position INTEGER NOT NULL
        );
        "#,
    )?;

    // A `team_id` column means some earlier run already rebuilt this table;
    // rebuilding it again would throw away the teams it now holds.
    if !column_exists(conn, "team_logins", "team_id")? {
        let legacy = read_legacy_team(conn)?;
        conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS team_logins;
            CREATE TABLE team_logins (
                team_id  INTEGER NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
                login    TEXT NOT NULL,
                position INTEGER NOT NULL,
                UNIQUE (team_id, login)
            );
            "#,
        )?;
        if let Some((name, logins)) = legacy {
            conn.execute(
                "INSERT INTO teams (id, name, position) VALUES (1, ?1, 0)",
                params![name],
            )?;
            let mut stmt = conn
                .prepare("INSERT INTO team_logins (team_id, login, position) VALUES (1, ?1, ?2)")?;
            for (position, login) in logins.iter().enumerate() {
                stmt.execute(params![login, position as i64])?;
            }
        }
    }

    // `settings.team_name` is dead once the name lives on the team row.
    // `DROP COLUMN` landed in SQLite 3.35; this crate bundles a newer one, so
    // the failure branch is only reachable against an older system library, and
    // then the column simply stays behind unread.
    if column_exists(conn, "settings", "team_name")?
        && conn.execute("ALTER TABLE settings DROP COLUMN team_name", []).is_err()
    {
        log::debug!("this SQLite cannot DROP COLUMN; settings.team_name stays, unused");
    }
    Ok(())
}

/// Schema v4: watched threads.
///
/// Purely additive — one new table, and nothing else in the database changes.
/// `CREATE TABLE IF NOT EXISTS` is its own re-run guard, so a database a newer
/// build already migrated passes through with its watches intact.
///
/// `ON DELETE CASCADE` on `repo_id` is what stops a removed repo leaving
/// watches behind that point at nothing; it works because `from_connection`
/// turns `PRAGMA foreign_keys` on before migrating.
fn migrate_to_v4(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS watches (
            repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
            number  INTEGER NOT NULL,
            kind    TEXT NOT NULL,
            title   TEXT NOT NULL,
            state   TEXT NOT NULL,
            source  TEXT NOT NULL,
            since   INTEGER NOT NULL,
            PRIMARY KEY (repo_id, number)
        );
        "#,
    )?;
    Ok(())
}

/// Schema v5: several GitHub accounts.
///
/// Purely additive — one new table and one new column, both guarded, so a
/// database a newer build already migrated passes through untouched.
///
/// The new column defaults to [`UNCLAIMED_ACCOUNT`] rather than to a guess at
/// which account owns the repos already there: at migration time no account
/// exists yet, and the first `add_account` is what claims them.
///
/// `login` is `COLLATE NOCASE` because GitHub logins are case-insensitive:
/// without it, `Alice` and `alice` would be two accounts holding two tokens for
/// one person.
fn migrate_to_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS accounts (
            login      TEXT PRIMARY KEY COLLATE NOCASE,
            avatar_url TEXT,
            added_at   INTEGER NOT NULL
        );
        "#,
    )?;
    add_column_if_missing(conn, "repos", "account_login", "TEXT NOT NULL DEFAULT ''")?;
    Ok(())
}

/// The pre-v3 single team, or `None` when the database never had one.
/// v6: one nullable column, `repos.backfilled_to`.
///
/// Purely additive and guarded, so a database a newer build already migrated
/// passes through untouched. NULL is the honest value for every repo already
/// there: nothing recorded how far back their first poll reached, and inventing
/// a floor would let `backfill` claim to have covered a stretch it never
/// fetched. The next successful poll of each repo seeds it — see
/// [`Store::mark_repo_polled`].
fn migrate_to_v6(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "repos", "backfilled_to", "INTEGER")?;
    Ok(())
}

/// v7: the conditional-request cache, plus GitHub's own per-repo polling hint.
///
/// `etags` is a pure cache keyed by the absolute URL its tag describes — losing
/// it costs bandwidth and nothing else, which is why it carries no foreign key
/// to `repos` and why `Store::open` is free to sweep it. `fetched_at` exists
/// only for that sweep.
///
/// `repos.poll_interval_secs` is the largest `X-Poll-Interval` GitHub offered
/// for that repo, in seconds. NULL means "no hint yet", which is the honest
/// value for every repo already there and the state in which the poller applies
/// no floor of its own. It is *not* the user's own `Settings.poll_interval_secs`
/// — that is how often the app asks; this is how often GitHub says it is worth
/// asking about one repo.
fn migrate_to_v7(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS etags (
            url        TEXT PRIMARY KEY,
            etag       TEXT NOT NULL,
            fetched_at INTEGER NOT NULL
        );
        "#,
    )?;
    add_column_if_missing(conn, "repos", "poll_interval_secs", "INTEGER")?;
    Ok(())
}

/// v8: `pulls`, one row per pull request Vigie has seen, holding its *current*
/// state rather than a history of it.
///
/// Purely additive — one new table and one new index, both guarded — so a
/// database a newer build already migrated passes through untouched, and there
/// is nothing to back-fill: the next poll of each repo lists its PRs anyway and
/// [`Store::upsert_pull`] fills the table in from that, at no extra request.
///
/// `ON DELETE CASCADE` on `repo_id` is what stops a removed repo (or a removed
/// account, which deletes its repos) leaving open PRs behind that point at
/// nothing; it works because `from_connection` turns `PRAGMA foreign_keys` on
/// before migrating. The index serves the one shape `open_pulls` reads in.
fn migrate_to_v8(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS pulls (
            repo_id             INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
            number              INTEGER NOT NULL,
            title               TEXT NOT NULL,
            url                 TEXT NOT NULL,
            author_login        TEXT NOT NULL,
            author_avatar_url   TEXT,
            state               TEXT NOT NULL CHECK (state IN ('open','closed','merged')),
            draft               INTEGER NOT NULL DEFAULT 0,
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL,
            merged_at           INTEGER,
            closed_at           INTEGER,
            additions           INTEGER,
            deletions           INTEGER,
            requested_reviewers TEXT NOT NULL DEFAULT '[]',
            review_decision     TEXT,
            last_activity_at    INTEGER NOT NULL,
            PRIMARY KEY (repo_id, number)
        );

        CREATE INDEX IF NOT EXISTS pulls_open ON pulls (state, repo_id);
        "#,
    )?;
    Ok(())
}

/// v9: one column, `repos.hidden`, defaulting to 0.
///
/// Additive and idempotent, exactly as v5-v7 are: [`add_column_if_missing`] is
/// its own re-run guard, so a database a newer build already migrated passes
/// through untouched and every existing repo simply reads back visible.
///
/// Nothing is back-filled and nothing is deleted — hiding is a read-and-poll
/// filter over rows that stay exactly where they are.
fn migrate_to_v9(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "repos", "hidden", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}

fn read_legacy_team(conn: &Connection) -> Result<Option<(String, Vec<String>)>> {
    let mut stmt = conn.prepare("SELECT login FROM team_logins ORDER BY position ASC")?;
    let logins = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let name: Option<String> = if column_exists(conn, "settings", "team_name")? {
        conn.query_row("SELECT team_name FROM settings WHERE id = 1", [], |r| r.get(0))
            .optional()?
    } else {
        None
    };
    let named = name.filter(|n| n != LEGACY_DEFAULT_TEAM_NAME && !n.trim().is_empty());

    if logins.is_empty() && named.is_none() {
        return Ok(None);
    }
    Ok(Some((named.unwrap_or_else(|| LEGACY_DEFAULT_TEAM_NAME.to_string()), logins)))
}

/// Whether `table` currently has `column`. Used to make each migration step
/// safe to re-run.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(existing.iter().any(|name| name == column))
}

/// Turns a literal URL prefix into a `LIKE` pattern.
///
/// `%` and `_` are `LIKE` wildcards and both are legal in a GitHub repo name —
/// `_` especially so. Escaped, `acme/my_repo` cannot match `acme/myXrepo` and
/// clear another repo's cached tags.
fn like_prefix(prefix: &str) -> String {
    format!("{}%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// `ALTER TABLE ... ADD COLUMN`, skipped when the column is already there.
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, and a migration must be safe to
/// re-run against a database that a newer build already touched.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<()> {
    if column_exists(conn, table, column)? {
        return Ok(());
    }
    conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    Ok(())
}

/// The shared `WHERE` tail of the three digest queries, plus its bound
/// arguments in order. The same clause and the same arguments drive all three,
/// so any one of them can be read on its own without the filters drifting apart.
///
/// The window is half-open: an event exactly at `start` is in, one exactly at
/// `end` is out.
fn digest_filter(
    start: i64,
    end: i64,
    logins: Option<&HashSet<String>>,
    actor: Option<&str>,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut sql = String::from(" WHERE occurred_at >= ? AND occurred_at < ?");
    sql.push_str(&not_hidden("repo_id"));
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(start), Box::new(end)];

    if let Some(actor) = actor {
        sql.push_str(" AND actor_login = ? COLLATE NOCASE");
        args.push(Box::new(actor.to_string()));
    }

    if let Some(logins) = logins {
        // Sorted so the SQL text for one membership is always the same string,
        // which keeps SQLite's prepared-statement cache useful across calls.
        let mut sorted: Vec<&str> = logins.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sql.push_str(" AND actor_login COLLATE NOCASE IN (");
        for (i, login) in sorted.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push('?');
            args.push(Box::new(login.to_string()));
        }
        sql.push(')');
    }

    (sql, args)
}

/// The `AND` tail scoping a timing query to the PR *author*, plus its bound
/// logins in order. Written against the `o` alias every timing query gives the
/// `pr_opened` row, so the clause reads the same in both of them.
///
/// An empty membership matches nobody, exactly as `digest_filter`'s does.
fn author_filter(
    logins: Option<&HashSet<String>>,
    actor: Option<&str>,
) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut args: Vec<String> = Vec::new();
    if let Some(actor) = actor {
        sql.push_str(" AND o.actor_login = ? COLLATE NOCASE");
        args.push(actor.to_string());
    }
    if let Some(logins) = logins {
        let mut sorted: Vec<&str> = logins.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sql.push_str(" AND o.actor_login COLLATE NOCASE IN (");
        for (i, login) in sorted.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push('?');
            args.push((*login).to_string());
        }
        sql.push(')');
    }
    (sql, args)
}

/// `(repo_id, number, seconds)` — the shape both timing queries select.
fn timing_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(u64, u64, i64)> {
    Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)?))
}

/// The logins in a stored `requested_reviewers` array. A row this build cannot
/// parse reads as "nobody was asked" rather than failing the whole query: the
/// column is written by [`Store::upsert_pull`] and the next poll rewrites it.
fn decode_reviewers(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

fn row_to_open_pull(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpenPull> {
    let reviewers: String = row.get(12)?;
    Ok(OpenPull {
        repo_id: row.get::<_, i64>(0)? as u64,
        number: row.get::<_, i64>(1)? as u64,
        title: row.get(2)?,
        url: row.get(3)?,
        author_login: row.get(4)?,
        author_avatar_url: row.get(5)?,
        draft: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        last_activity_at: row.get(9)?,
        additions: row.get(10)?,
        deletions: row.get(11)?,
        requested_reviewers: decode_reviewers(&reviewers),
        review_decision: row.get(13)?,
    })
}

fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account { login: row.get(0)?, avatar_url: row.get(1)?, added_at: row.get(2)? })
}

fn row_to_repo(row: &rusqlite::Row<'_>) -> rusqlite::Result<Repo> {
    Ok(Repo {
        id: row.get::<_, i64>(0)? as u64,
        owner: row.get(1)?,
        name: row.get(2)?,
        url: row.get(3)?,
        account_login: row.get(4)?,
        default_branch: row.get(5)?,
        last_polled_at: row.get(6)?,
        backfilled_to: row.get(7)?,
        last_error: row.get(8)?,
        hidden: row.get::<_, i64>(9)? != 0,
    })
}

/// A stored watch. `kind` and `source` were written from their own wire names,
/// so an unreadable one means the row was written by a build this one does not
/// know; it degrades to the commonest reading rather than failing the list.
fn row_to_watch(row: &rusqlite::Row<'_>) -> rusqlite::Result<Watch> {
    let kind: String = row.get(2)?;
    let source: String = row.get(5)?;
    Ok(Watch {
        repo_id: row.get::<_, i64>(0)? as u64,
        number: row.get::<_, i64>(1)? as u64,
        kind: kind.parse().unwrap_or(ThreadKind::Pull),
        title: row.get(3)?,
        state: row.get(4)?,
        source: source.parse().unwrap_or(WatchSource::Manual),
        since: row.get(6)?,
    })
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let kind: String = row.get(2)?;
    Ok(Event {
        id: row.get::<_, i64>(0)? as u64,
        repo_id: row.get::<_, i64>(1)? as u64,
        kind: kind.parse().unwrap_or(EventKind::Commit),
        actor_login: row.get(3)?,
        actor_avatar_url: row.get(4)?,
        title: row.get(5)?,
        body_preview: row.get(6)?,
        body: row.get(7)?,
        url: row.get(8)?,
        number: row.get::<_, Option<i64>>(9)?.map(|n| n as u64),
        occurred_at: row.get(10)?,
        seen: row.get::<_, i64>(11)? != 0,
        // Neither is ever stored; `list_events` fills both in from the current
        // team membership and the current watches.
        team_ids: Vec::new(),
        watched: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: EventKind, external_id: &str, actor: &str, occurred_at: i64) -> NewEvent {
        NewEvent {
            kind,
            external_id: external_id.to_string(),
            actor_login: actor.to_string(),
            actor_avatar_url: None,
            title: "t".to_string(),
            body_preview: None,
            body: None,
            url: "https://example.invalid".to_string(),
            number: None,
            occurred_at,
        }
    }

    fn store_with_repo() -> (Store, u64) {
        let store = Store::open_in_memory().unwrap();
        let repo = store.insert_repo("acme", "platform", "https://x", "", "main").unwrap();
        (store, repo.id)
    }

    #[test]
    fn migration_stamps_the_schema_version() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    /// A real v1 database — built from the frozen v1 DDL, with a row written
    /// through the v1 column list — migrates to v2 with its data intact and its
    /// pre-existing `body` NULL.
    #[test]
    fn a_v1_database_migrates_to_v2_keeping_its_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO repos (owner, name, url, default_branch)
             VALUES ('acme', 'platform', 'https://x', 'main')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
                (repo_id, kind, external_id, actor_login, title, body_preview, url, occurred_at)
             VALUES (1, 'commit', 'sha1', 'alice', 'Old title', 'old preview', 'https://y', 42)",
            [],
        )
        .unwrap();
        // v1 genuinely has no `body` column: writing one must fail here, or the
        // rest of this test would be proving nothing.
        assert!(
            conn.execute("UPDATE events SET body = 'x'", []).is_err(),
            "the v1 fixture already had a body column"
        );

        let store = Store::from_connection(conn).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let events = store.list_events(None, None, None, None, false, None, 10).unwrap();
        assert_eq!(events.len(), 1, "the v1 row survived the migration");
        assert_eq!(events[0].title, "Old title");
        assert_eq!(events[0].body_preview.as_deref(), Some("old preview"));
        assert_eq!(events[0].body, None, "a pre-v2 row has no stored body");
        assert_eq!(store.list_repos().unwrap().len(), 1);

        // And the new column is writable through the normal insert path.
        let mut fresh = event(EventKind::Commit, "sha2", "bob", 43);
        fresh.body = Some("full\n\nbody".to_string());
        let stored = store.insert_event(1, &fresh).unwrap().unwrap();
        assert_eq!(stored.body.as_deref(), Some("full\n\nbody"));
        let reread = store.list_events(None, None, None, None, false, None, 10).unwrap();
        assert_eq!(reread[0].body.as_deref(), Some("full\n\nbody"), "body round-trips");
    }

    /// A real v2 database: the frozen v1 DDL plus the v2 `body` column, which is
    /// exactly what `migrate` builds for a v1 database. Nothing here is a
    /// hand-made approximation of the old shape.
    fn v2_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        add_column_if_missing(&conn, "events", "body", "TEXT").unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn
    }

    /// The single v2 team becomes team id 1, keeping its name and the order its
    /// logins were stored in.
    #[test]
    fn a_v2_team_migrates_into_team_one_in_its_original_order() {
        let conn = v2_database();
        conn.execute(
            "INSERT INTO settings (id, json, team_name) VALUES (1, ?1, 'Platform')",
            params![serde_json::to_string(&Settings::default()).unwrap()],
        )
        .unwrap();
        // Deliberately not alphabetical, and not in rowid order either, so the
        // assertion below can only pass by honouring `position`.
        for (position, login) in [(0, "carol"), (1, "alice"), (2, "bob")] {
            conn.execute(
                "INSERT INTO team_logins (position, login) VALUES (?1, ?2)",
                params![position as i64, login],
            )
            .unwrap();
        }

        let store = Store::from_connection(conn).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let teams = store.list_teams().unwrap();
        assert_eq!(teams.len(), 1, "one team, not one per login: {teams:?}");
        assert_eq!(teams[0].id, 1);
        assert_eq!(teams[0].name, "Platform");
        assert_eq!(teams[0].logins, vec!["carol", "alice", "bob"], "original order kept");
        // And it is a working team, not just rows: the filter union sees it.
        assert_eq!(
            store.member_logins().unwrap(),
            ["carol", "alice", "bob"].iter().map(|s| s.to_string()).collect()
        );
        assert_eq!(store.team_ids_for_login("Alice").unwrap(), vec![1]);
    }

    /// A v2 database nobody ever set a team up in gets no team, so the app's
    /// onboarding can ask instead of inheriting an empty one.
    #[test]
    fn an_empty_v2_database_migrates_to_no_teams() {
        let store = Store::from_connection(v2_database()).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(store.list_teams().unwrap().is_empty());
        assert!(store.member_logins().unwrap().is_empty());
    }

    /// The stock name with no logins is also "no team": that is a v2 database
    /// whose settings row was written by `ensure_settings_row` and never edited.
    #[test]
    fn the_untouched_default_v2_team_is_not_migrated() {
        let conn = v2_database();
        conn.execute(
            "INSERT INTO settings (id, json, team_name) VALUES (1, ?1, 'Team')",
            params![serde_json::to_string(&Settings::default()).unwrap()],
        )
        .unwrap();
        let store = Store::from_connection(conn).unwrap();
        assert!(store.list_teams().unwrap().is_empty());
    }

    /// A renamed team with no logins yet is a real team and survives.
    #[test]
    fn a_named_but_empty_v2_team_still_migrates() {
        let conn = v2_database();
        conn.execute(
            "INSERT INTO settings (id, json, team_name) VALUES (1, ?1, 'Platform')",
            params![serde_json::to_string(&Settings::default()).unwrap()],
        )
        .unwrap();
        let store = Store::from_connection(conn).unwrap();
        let teams = store.list_teams().unwrap();
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].name, "Platform");
        assert!(teams[0].logins.is_empty());
    }

    /// Re-running the v3 step must not wipe teams created since. This drives
    /// `migrate_to_v3` a second time directly, which is the only way to reach
    /// the guard: the version stamp would otherwise skip it.
    #[test]
    fn re_running_the_v3_migration_is_a_no_op() {
        let store = Store::open_in_memory().unwrap();
        let platform = store.create_team("Platform", &["alice".to_string()]).unwrap();
        let infra = store.create_team("Infra", &["bob".to_string()]).unwrap();

        migrate_to_v3(&store.conn).unwrap();

        assert_eq!(
            store.list_teams().unwrap(),
            vec![platform, infra],
            "a second v3 run rebuilt the tables and lost the teams"
        );
    }

    /// A migration that fails part-way must leave the database exactly as it
    /// was. v3 is the step that makes this matter: it drops `team_logins`
    /// before rebuilding and refilling it, and its re-run guard keys on the
    /// *rebuilt* shape — so a half-applied v3 would lose the legacy team and
    /// then skip the step on every later open, silently and permanently.
    ///
    /// The failure is injected by pre-creating `teams` with an extra NOT NULL
    /// column: `CREATE TABLE IF NOT EXISTS teams` leaves that table alone, and
    /// the migration's `INSERT INTO teams` then trips the constraint — which
    /// happens *after* the drop, inside the window that matters.
    ///
    /// File-backed on purpose. Rollback has to be observable after the failed
    /// `Store` is gone, and `from_connection` consumes its connection; an
    /// in-memory database would vanish with it and could prove nothing.
    #[test]
    fn a_failed_migration_rolls_the_whole_thing_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gitmon.db");

        // A real v2 database with a team in it.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(V1_SCHEMA).unwrap();
            add_column_if_missing(&conn, "events", "body", "TEXT").unwrap();
            conn.execute(
                "INSERT INTO settings (id, json, team_name) VALUES (1, ?1, 'Platform')",
                params![serde_json::to_string(&Settings::default()).unwrap()],
            )
            .unwrap();
            for (position, login) in [(0, "carol"), (1, "alice"), (2, "bob")] {
                conn.execute(
                    "INSERT INTO team_logins (position, login) VALUES (?1, ?2)",
                    params![position as i64, login],
                )
                .unwrap();
            }
            conn.execute_batch(
                "CREATE TABLE teams (
                     id          INTEGER PRIMARY KEY,
                     name        TEXT NOT NULL,
                     position    INTEGER NOT NULL,
                     must_be_set TEXT NOT NULL
                 );",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }

        let failed = Store::from_connection(Connection::open(&path).unwrap());
        assert!(failed.is_err(), "the poisoned v3 step must fail the open");

        // A second connection, so this is what actually reached the disk.
        let after = Connection::open(&path).unwrap();
        assert_eq!(
            after.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0)).unwrap(),
            2,
            "the version stamp committed without the steps it describes"
        );
        assert!(
            !column_exists(&after, "team_logins", "team_id").unwrap(),
            "the rebuilt team_logins survived the rollback"
        );

        let mut stmt =
            after.prepare("SELECT login FROM team_logins ORDER BY position ASC").unwrap();
        let logins = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(logins, vec!["carol", "alice", "bob"], "the legacy team was lost");
        drop(stmt);

        let name: String = after
            .query_row("SELECT team_name FROM settings WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Platform", "settings.team_name was dropped and not restored");

        // And the database is still openable and migratable once the cause is
        // removed: the failure cost nothing but the attempt.
        after.execute_batch("DROP TABLE teams;").unwrap();
        drop(after);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let teams = store.list_teams().unwrap();
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].name, "Platform");
        assert_eq!(teams[0].logins, vec!["carol", "alice", "bob"]);
    }

    /// A real v3 database: everything `migrate` builds up to and including the
    /// v3 step, stamped at 3, with a team, a repo and an event in it. Nothing
    /// here is a hand-made approximation of the old shape.
    fn v3_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        add_column_if_missing(&conn, "events", "body", "TEXT").unwrap();
        migrate_to_v3(&conn).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn
    }

    /// v3 -> v4 is additive: the teams and events already there survive, and the
    /// new table arrives empty rather than pre-populated with guesses.
    #[test]
    fn a_v3_database_migrates_to_v4_keeping_its_teams_and_events() {
        let conn = v3_database();
        conn.execute(
            "INSERT INTO teams (id, name, position) VALUES (1, 'Platform', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO team_logins (team_id, login, position) VALUES (1, 'alice', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repos (owner, name, url, default_branch)
             VALUES ('acme', 'platform', 'https://x', 'main')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
                (repo_id, kind, external_id, actor_login, title, url, number, occurred_at)
             VALUES (1, 'pr_opened', '101:opened', 'alice', 'Split the poller',
                     'https://y', 101, 42)",
            [],
        )
        .unwrap();
        // v3 genuinely has no `watches` table: writing one must fail here, or
        // the rest of this test would be proving nothing.
        assert!(
            conn.execute("SELECT 1 FROM watches", []).is_err(),
            "the v3 fixture already had a watches table"
        );

        let store = Store::from_connection(conn).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let teams = store.list_teams().unwrap();
        assert_eq!(teams.len(), 1, "the v3 team survived");
        assert_eq!(teams[0].logins, vec!["alice"]);
        let events = store.list_events(None, None, None, None, false, None, 10).unwrap();
        assert_eq!(events.len(), 1, "the v3 event survived");
        assert_eq!(events[0].title, "Split the poller");
        assert!(!events[0].watched, "nothing is watched in a freshly migrated database");

        // The new table is there, empty, and writable through the normal path.
        assert!(store.list_watches().unwrap().is_empty());
        assert!(store.watched_keys().unwrap().is_empty());
        assert!(store
            .insert_watch_if_absent(
                1,
                101,
                ThreadKind::Pull,
                "Split the poller",
                "open",
                WatchSource::Manual,
                77,
            )
            .unwrap());
        assert_eq!(
            store.list_watches().unwrap(),
            vec![Watch {
                repo_id: 1,
                number: 101,
                kind: ThreadKind::Pull,
                title: "Split the poller".to_string(),
                state: "open".to_string(),
                source: WatchSource::Manual,
                since: 77,
            }]
        );
        // And the read-time flag now sees it, without the event row changing.
        let events = store.list_events(None, None, None, None, false, None, 10).unwrap();
        assert!(events[0].watched);
    }

    /// Re-running the v4 step must not wipe watches created since. This drives
    /// `migrate_to_v4` a second time directly, which is the only way to reach
    /// the `IF NOT EXISTS` guard: the version stamp would otherwise skip it.
    #[test]
    fn re_running_the_v4_migration_is_a_no_op() {
        let (store, repo_id) = store_with_repo();
        store
            .insert_watch_if_absent(
                repo_id,
                101,
                ThreadKind::Pull,
                "Split the poller",
                "open",
                WatchSource::Manual,
                5,
            )
            .unwrap();
        let before = store.list_watches().unwrap();

        migrate_to_v4(&store.conn).unwrap();

        assert_eq!(
            store.list_watches().unwrap(),
            before,
            "a second v4 run rebuilt the table and lost the watches"
        );
    }

    /// The `ON DELETE CASCADE` on `watches.repo_id`: removing a repo must not
    /// leave watches behind pointing at a repo that is gone.
    #[test]
    fn removing_a_repo_cascades_its_watches() {
        let (store, repo_id) = store_with_repo();
        let other = store.insert_repo("acme", "web", "https://x", "", "main").unwrap();
        for (repo, number) in [(repo_id, 101), (repo_id, 5), (other.id, 202)] {
            store
                .insert_watch_if_absent(
                    repo,
                    number,
                    ThreadKind::Pull,
                    "t",
                    "open",
                    WatchSource::Manual,
                    1,
                )
                .unwrap();
        }
        assert_eq!(store.list_watches().unwrap().len(), 3);

        store.remove_repo(repo_id).unwrap();

        let left = store.list_watches().unwrap();
        assert_eq!(left.len(), 1, "only the other repo's watch survives: {left:?}");
        assert_eq!(left[0].repo_id, other.id);
        assert_eq!(left[0].number, 202);
    }

    /// A real v4 database: everything `migrate` builds up to and including the
    /// v4 step, stamped at 4. Nothing here is a hand-made approximation of the
    /// old shape.
    fn v4_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        add_column_if_missing(&conn, "events", "body", "TEXT").unwrap();
        migrate_to_v3(&conn).unwrap();
        migrate_to_v4(&conn).unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn
    }

    /// v4 -> v5 is additive: the teams, repos, events and watches already there
    /// all survive, and every repo comes through *unclaimed* rather than
    /// guessed into an account that does not exist yet.
    #[test]
    fn a_v4_database_migrates_to_v5_keeping_everything_with_no_account() {
        let conn = v4_database();
        conn.execute("INSERT INTO teams (id, name, position) VALUES (1, 'Platform', 0)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO team_logins (team_id, login, position) VALUES (1, 'alice', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repos (owner, name, url, default_branch)
             VALUES ('acme', 'platform', 'https://x', 'main')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
                (repo_id, kind, external_id, actor_login, title, url, number, occurred_at)
             VALUES (1, 'pr_opened', '101:opened', 'alice', 'Split the poller',
                     'https://y', 101, 42)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO watches (repo_id, number, kind, title, state, source, since)
             VALUES (1, 101, 'pull', 'Split the poller', 'open', 'manual', 77)",
            [],
        )
        .unwrap();
        // v4 genuinely has neither the table nor the column: both must fail
        // here, or the rest of this test would be proving nothing.
        assert!(
            conn.execute("SELECT 1 FROM accounts", []).is_err(),
            "the v4 fixture already had an accounts table"
        );
        assert!(
            !column_exists(&conn, "repos", "account_login").unwrap(),
            "the v4 fixture already had repos.account_login"
        );

        let store = Store::from_connection(conn).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.list_teams().unwrap()[0].logins, vec!["alice"], "the v4 team survived");
        let events = store.list_events(None, None, None, None, false, None, 10).unwrap();
        assert_eq!(events.len(), 1, "the v4 event survived");
        assert_eq!(events[0].title, "Split the poller");
        assert!(events[0].watched, "the v4 watch survived and still marks its event");
        assert_eq!(store.list_watches().unwrap().len(), 1);

        let repos = store.list_repos().unwrap();
        assert_eq!(repos.len(), 1, "the v4 repo survived");
        assert_eq!(repos[0].account_login, UNCLAIMED_ACCOUNT, "a migrated repo is unclaimed");
        assert_eq!(store.repo_account(repos[0].id).unwrap().as_deref(), Some(UNCLAIMED_ACCOUNT));

        // The new table is there, empty, and the migrated repo is claimable.
        assert!(store.list_accounts().unwrap().is_empty());
        let alice = store.upsert_account("alice", Some("https://avatar"), 100).unwrap();
        assert_eq!(
            alice,
            Account {
                login: "alice".to_string(),
                avatar_url: Some("https://avatar".to_string()),
                added_at: 100,
            }
        );
        assert_eq!(store.claim_orphan_repos("alice").unwrap(), 1);
        assert_eq!(store.list_repos().unwrap()[0].account_login, "alice");
        assert_eq!(store.claim_orphan_repos("alice").unwrap(), 0, "nothing left to claim");
    }

    /// Re-running the v5 step must not wipe accounts added since, nor reset the
    /// `account_login` of repos already claimed. This drives `migrate_to_v5` a
    /// second time directly, which is the only way to reach its guards: the
    /// version stamp would otherwise skip it.
    #[test]
    fn re_running_the_v5_migration_is_a_no_op() {
        let (store, repo_id) = store_with_repo();
        store.upsert_account("alice", None, 5).unwrap();
        store.claim_orphan_repos("alice").unwrap();
        let accounts = store.list_accounts().unwrap();

        migrate_to_v5(&store.conn).unwrap();

        assert_eq!(store.list_accounts().unwrap(), accounts, "a second v5 run lost the accounts");
        assert_eq!(
            store.repo_account(repo_id).unwrap().as_deref(),
            Some("alice"),
            "a second v5 run unclaimed the repos"
        );
    }

    fn v5_database() -> Connection {
        let conn = v4_database();
        migrate_to_v5(&conn).unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn
    }

    /// v5 -> v6 is additive: the repo already there survives and comes through
    /// with a NULL floor rather than an invented one. NULL is the point —
    /// nothing recorded how far back that repo's first poll reached, so
    /// `backfill` must decline until a poll seeds the floor honestly.
    #[test]
    fn a_v5_database_migrates_to_v6_with_no_backfill_floor() {
        let conn = v5_database();
        conn.execute(
            "INSERT INTO repos (owner, name, url, account_login, default_branch, last_polled_at)
             VALUES ('acme', 'platform', 'https://x', 'alice', 'main', 4242)",
            [],
        )
        .unwrap();
        // v5 genuinely lacks the column, or the rest of this proves nothing.
        assert!(
            !column_exists(&conn, "repos", "backfilled_to").unwrap(),
            "the v5 fixture already had repos.backfilled_to"
        );

        let store = Store::from_connection(conn).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let repos = store.list_repos().unwrap();
        assert_eq!(repos.len(), 1, "the v5 repo survived");
        assert_eq!(repos[0].last_polled_at, Some(4242), "its watermark survived");
        assert_eq!(repos[0].backfilled_to, None, "a migrated repo has no floor yet");
    }

    fn v6_database() -> Connection {
        let conn = v5_database();
        migrate_to_v6(&conn).unwrap();
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn
    }

    /// A real v7 database: everything `migrate` builds up to and including the
    /// v7 step, stamped at 7. Nothing here is a hand-made approximation of the
    /// old shape.
    fn v7_database() -> Connection {
        let conn = v6_database();
        migrate_to_v7(&conn).unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
        conn
    }

    /// One seeded pull request, so the tests below say only what they are about.
    fn seen_pull(number: u64, state: &str, created_at: i64) -> PullSeen {
        PullSeen {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/acme/platform/pull/{number}"),
            author_login: "alice".to_string(),
            author_avatar_url: None,
            state: state.to_string(),
            draft: false,
            created_at,
            updated_at: created_at,
            merged_at: None,
            closed_at: None,
            additions: None,
            deletions: None,
            requested_reviewers: Vec::new(),
        }
    }

    /// v7 -> v8 is additive: everything already there survives, and `pulls`
    /// arrives empty rather than back-filled with guesses — the next poll of
    /// each repo fills it in from the listing it fetches anyway.
    #[test]
    fn a_v7_database_migrates_to_v8_with_an_empty_pulls_table() {
        let conn = v7_database();
        conn.execute(
            "INSERT INTO repos (owner, name, url, account_login, default_branch)
             VALUES ('acme', 'platform', 'https://x', 'alice', 'main')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
                (repo_id, kind, external_id, actor_login, title, url, number, occurred_at)
             VALUES (1, 'pr_opened', '101:opened', 'alice', 'Split the poller',
                     'https://y', 101, 42)",
            [],
        )
        .unwrap();
        // v7 genuinely has no `pulls` table: reading one must fail here, or the
        // rest of this test would be proving nothing.
        assert!(
            conn.execute("SELECT 1 FROM pulls", []).is_err(),
            "the v7 fixture already had a pulls table"
        );

        let store = Store::from_connection(conn).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.list_repos().unwrap().len(), 1, "the v7 repo survived");
        assert_eq!(
            store.list_events(None, None, None, None, false, None, 10).unwrap().len(),
            1,
            "the v7 event survived"
        );
        assert!(store.open_pulls(None, None).unwrap().is_empty(), "no PR is invented");

        // And the new table is writable through the normal path.
        store.upsert_pull(1, &seen_pull(101, "open", 40)).unwrap();
        let open = store.open_pulls(None, None).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].number, 101);
        assert_eq!(open[0].title, "PR 101");
    }

    // ---- hiding a repo (schema v9) --------------------------------------

    fn v8_database() -> Connection {
        let conn = v7_database();
        migrate_to_v8(&conn).unwrap();
        conn.pragma_update(None, "user_version", 8).unwrap();
        conn
    }

    /// A repo, an event, an open pull and a watch, so the "hidden is not
    /// deleted" tests can each say only what they are about.
    fn store_with_two_repos() -> (Store, u64, u64) {
        let store = Store::open_in_memory().unwrap();
        let kept = store.insert_repo("acme", "platform", "https://x", "", "main").unwrap();
        let hidden = store.insert_repo("acme", "web", "https://y", "", "main").unwrap();
        for id in [kept.id, hidden.id] {
            let mut e = event(EventKind::PrOpened, &format!("pr-{id}"), "alice", 1_000);
            e.number = Some(101);
            store.insert_event(id, &e).unwrap();
            store.upsert_pull(id, &seen_pull(101, "open", 900)).unwrap();
            store
                .insert_watch_if_absent(
                    id,
                    101,
                    ThreadKind::Pull,
                    "PR 101",
                    PULL_STATE_OPEN,
                    WatchSource::Manual,
                    900,
                )
                .unwrap();
        }
        (store, kept.id, hidden.id)
    }

    /// v8 -> v9 is one additive column: the repo already there survives, and
    /// reads back visible rather than hidden.
    #[test]
    fn a_v8_database_migrates_to_v9_with_every_repo_visible() {
        let conn = v8_database();
        conn.execute(
            "INSERT INTO repos (owner, name, url, account_login, default_branch, last_polled_at)
             VALUES ('acme', 'platform', 'https://x', 'alice', 'main', 4242)",
            [],
        )
        .unwrap();
        assert!(
            !column_exists(&conn, "repos", "hidden").unwrap(),
            "the v8 fixture already had repos.hidden"
        );

        let store = Store::from_connection(conn).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let repos = store.list_repos().unwrap();
        assert_eq!(repos.len(), 1, "the v8 repo survived");
        assert_eq!(repos[0].last_polled_at, Some(4242), "its watermark survived");
        assert!(!repos[0].hidden, "a migrated repo is visible");
        assert_eq!(store.list_visible_repos().unwrap().len(), 1);
        assert!(store.list_hidden_repos().unwrap().is_empty());
    }

    /// `add_column_if_missing` is the v9 step's own re-run guard, so a second
    /// run must not reset a repo the user has since hidden.
    #[test]
    fn re_running_the_v9_migration_is_a_no_op() {
        let (store, repo_id) = store_with_repo();
        store.set_repo_hidden(repo_id, true, 10_000).unwrap();

        migrate_to_v9(&store.conn).unwrap();

        assert_eq!(
            store.list_hidden_repos().unwrap().len(),
            1,
            "a second v9 run un-hid the repo"
        );
    }

    /// The heart of it: a hidden repo is absent from every read the app makes,
    /// and everything it collected is still in the database — the visible
    /// repo's own rows are untouched throughout, and unhiding brings the
    /// hidden one's back exactly as they were.
    #[test]
    fn a_hidden_repo_leaves_every_read_and_comes_back_whole() {
        let (store, kept, hidden) = store_with_two_repos();
        let all_events = store.list_events(None, None, None, None, false, None, 50).unwrap();
        let all_pulls = store.open_pulls(None, None).unwrap();
        let all_watches = store.list_watches().unwrap();
        assert_eq!(all_events.len(), 2);
        assert_eq!(all_pulls.len(), 2);
        assert_eq!(all_watches.len(), 2);
        assert_eq!(store.unseen_count().unwrap(), 2);

        store.set_repo_hidden(hidden, true, 10_000).unwrap();

        // Every read now sees the visible repo only.
        let events = store.list_events(None, None, None, None, false, None, 50).unwrap();
        assert_eq!(events.len(), 1, "the hidden repo's events are still listed");
        assert_eq!(events[0].repo_id, kept);
        assert_eq!(store.unseen_count().unwrap(), 1, "the hidden repo still counts as unseen");
        assert_eq!(store.open_pulls(None, None).unwrap().len(), 1);
        assert_eq!(store.list_watches().unwrap().len(), 1);
        assert_eq!(store.actor_rollup().unwrap().len(), 1);
        let (authored, _) = store.open_pull_person_counts().unwrap();
        assert_eq!(authored.get("alice").copied(), Some(1));
        let digest = store.digest_rows(0, 2_000, None, None).unwrap();
        assert_eq!(digest.repos.len(), 1, "the digest counted the hidden repo");
        assert_eq!(digest.repos[0].0, kept);
        // Asking for the hidden repo by id yields nothing rather than a
        // back door into it.
        assert!(store
            .list_events(Some(hidden), None, None, None, false, None, 50)
            .unwrap()
            .is_empty());
        // The repo filter's own options lose it, and the Repos view's Hidden
        // section is the one place it turns up.
        assert_eq!(store.list_visible_repos().unwrap().len(), 1);
        assert_eq!(store.list_hidden_repos().unwrap().len(), 1);
        assert_eq!(store.list_repos().unwrap().len(), 2, "hiding deleted the repo row");

        // Nothing was deleted: unhiding restores every read to what it was.
        store.set_repo_hidden(hidden, false, 10_000).unwrap();
        assert_eq!(
            store.list_events(None, None, None, None, false, None, 50).unwrap(),
            all_events
        );
        assert_eq!(store.open_pulls(None, None).unwrap(), all_pulls);
        assert_eq!(store.list_watches().unwrap(), all_watches);
        assert_eq!(store.unseen_count().unwrap(), 2);
    }

    /// Removing is still the destructive one, hiding still is not — the whole
    /// point of the middle state.
    #[test]
    fn hiding_keeps_the_rows_removing_still_cascades_them() {
        let (store, _kept, hidden) = store_with_two_repos();
        let rows = |store: &Store, id: u64| -> (i64, i64, i64) {
            let events = store
                .conn
                .query_row("SELECT COUNT(*) FROM events WHERE repo_id = ?1", params![id as i64], |r| r.get(0))
                .unwrap();
            let pulls = store
                .conn
                .query_row("SELECT COUNT(*) FROM pulls WHERE repo_id = ?1", params![id as i64], |r| r.get(0))
                .unwrap();
            let watches = store
                .conn
                .query_row("SELECT COUNT(*) FROM watches WHERE repo_id = ?1", params![id as i64], |r| r.get(0))
                .unwrap();
            (events, pulls, watches)
        };
        assert_eq!(rows(&store, hidden), (1, 1, 1));

        store.set_repo_hidden(hidden, true, 10_000).unwrap();
        assert_eq!(rows(&store, hidden), (1, 1, 1), "hiding deleted stored rows");

        store.remove_repo(hidden).unwrap();
        assert_eq!(rows(&store, hidden), (0, 0, 0), "removing left rows behind");
        assert_eq!(store.list_repos().unwrap().len(), 1);
    }

    /// Unhiding a repo that has been hidden longer than one first-poll window
    /// clears its watermark, so the next poll costs one ordinary 24h window
    /// instead of everything since it was hidden.
    #[test]
    fn unhiding_a_long_hidden_repo_resets_its_watermark() {
        let (store, repo_id) = store_with_repo();
        let hidden_at = 1_000_000;
        store.mark_repo_polled(repo_id, hidden_at, hidden_at - 3_600).unwrap();
        store.mark_repo_error(repo_id, "signed out").unwrap();
        store.set_repo_hidden(repo_id, true, hidden_at).unwrap();

        // A month later.
        let now = hidden_at + 30 * 24 * 60 * 60;
        store.set_repo_hidden(repo_id, false, now).unwrap();

        let repo = store.find_repo_by_id(repo_id).unwrap().unwrap();
        assert!(!repo.hidden);
        assert_eq!(repo.last_polled_at, None, "the stale watermark survived");
        assert_eq!(repo.backfilled_to, None, "the floor now claims a gap it does not cover");
        assert_eq!(repo.last_error, None, "a stale error survived the unhide");
    }

    /// The mirror image: hidden and unhidden inside the day, the watermark is
    /// left exactly where it was and the repo simply resumes.
    #[test]
    fn unhiding_a_briefly_hidden_repo_keeps_its_watermark() {
        let (store, repo_id) = store_with_repo();
        let hidden_at = 1_000_000;
        store.mark_repo_polled(repo_id, hidden_at, hidden_at - 3_600).unwrap();
        store.set_repo_hidden(repo_id, true, hidden_at).unwrap();

        store.set_repo_hidden(repo_id, false, hidden_at + 600).unwrap();

        let repo = store.find_repo_by_id(repo_id).unwrap().unwrap();
        assert_eq!(repo.last_polled_at, Some(hidden_at), "a fresh watermark was thrown away");
        assert_eq!(repo.backfilled_to, Some(hidden_at - 3_600), "the floor was thrown away");
    }

    /// A repo that has never polled has no watermark to reset, and unhiding
    /// must not invent one.
    #[test]
    fn hiding_an_unpolled_repo_leaves_its_null_watermark_alone() {
        let (store, repo_id) = store_with_repo();
        store.set_repo_hidden(repo_id, true, 10_000).unwrap();
        store.set_repo_hidden(repo_id, false, 10_000 + 30 * 24 * 60 * 60).unwrap();
        let repo = store.find_repo_by_id(repo_id).unwrap().unwrap();
        assert_eq!(repo.last_polled_at, None);
    }

    #[test]
    fn hiding_an_unknown_repo_is_not_found() {
        let store = Store::open_in_memory().unwrap();
        let err = store.set_repo_hidden(404, true, 0).unwrap_err();
        assert_eq!(err.kind, crate::error::ErrorKind::NotFound);
    }

    /// The v8 step is `CREATE TABLE IF NOT EXISTS` plus one guarded index, so
    /// running it again against a database that already has the table must
    /// leave the rows alone rather than rebuild it.
    #[test]
    fn re_running_the_v8_migration_is_a_no_op() {
        let (store, repo_id) = store_with_repo();
        store.upsert_pull(repo_id, &seen_pull(101, "open", 40)).unwrap();
        let before = store.open_pulls(None, None).unwrap();
        assert_eq!(before.len(), 1);

        migrate_to_v8(&store.conn).unwrap();

        assert_eq!(
            store.open_pulls(None, None).unwrap(),
            before,
            "a second v8 run rebuilt the table and lost the pulls"
        );
    }

    /// A poll re-listing a PR replaces its row rather than adding a second, and
    /// a PR that has since merged leaves `open_pulls` — the state transition
    /// the app's "open PRs" count depends on.
    #[test]
    fn re_upserting_a_pull_moves_it_from_open_to_merged() {
        let (store, repo_id) = store_with_repo();
        let mut pull = seen_pull(101, "open", 40);
        pull.requested_reviewers = vec!["bob".to_string()];
        store.upsert_pull(repo_id, &pull).unwrap();
        let open = store.open_pulls(None, None).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].requested_reviewers, vec!["bob".to_string()]);
        assert_eq!(
            open[0].review_decision.as_deref(),
            Some("review_required"),
            "somebody was asked and nobody has answered"
        );

        pull.state = "merged".to_string();
        pull.merged_at = Some(90);
        pull.closed_at = Some(90);
        pull.updated_at = 90;
        pull.requested_reviewers.clear();
        store.upsert_pull(repo_id, &pull).unwrap();

        assert!(store.open_pulls(None, None).unwrap().is_empty(), "a merged PR is not open");
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM pulls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the second upsert replaced the row rather than adding one");
        let (state, merged_at): (String, Option<i64>) = store
            .conn
            .query_row("SELECT state, merged_at FROM pulls", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(state, "merged");
        assert_eq!(merged_at, Some(90));
    }

    /// The list shape GitHub calls "Pull Request Simple" carries no `additions`
    /// or `deletions`, so a poll that re-lists a PR must not wipe the numbers a
    /// single-PR fetch put there.
    #[test]
    fn re_upserting_keeps_diff_stats_the_list_shape_omits() {
        let (store, repo_id) = store_with_repo();
        let mut pull = seen_pull(101, "open", 40);
        pull.additions = Some(180);
        pull.deletions = Some(24);
        store.upsert_pull(repo_id, &pull).unwrap();

        pull.additions = None;
        pull.deletions = None;
        pull.updated_at = 60;
        store.upsert_pull(repo_id, &pull).unwrap();

        let open = store.open_pulls(None, None).unwrap();
        assert_eq!(open[0].additions, Some(180));
        assert_eq!(open[0].deletions, Some(24));
    }

    /// `last_activity_at` is the later of GitHub's `updated_at` and the newest
    /// stored event on the PR, so a comment that arrived with this poll counts
    /// even when GitHub's own timestamp lags behind it.
    #[test]
    fn last_activity_takes_the_newest_of_github_and_the_stored_events() {
        let (store, repo_id) = store_with_repo();
        let mut comment = event(EventKind::PrCommented, "c1", "bob", 500);
        comment.number = Some(101);
        store.insert_event(repo_id, &comment).unwrap();

        let mut pull = seen_pull(101, "open", 40);
        pull.updated_at = 300;
        store.upsert_pull(repo_id, &pull).unwrap();
        assert_eq!(store.open_pulls(None, None).unwrap()[0].last_activity_at, 500);

        pull.updated_at = 900;
        store.upsert_pull(repo_id, &pull).unwrap();
        assert_eq!(store.open_pulls(None, None).unwrap()[0].last_activity_at, 900);
    }

    /// `review_decision` is read off the stored `pr_reviewed` events: the newest
    /// review per reviewer decides that reviewer's position, one "changes
    /// requested" outranks any number of approvals, and a plain comment is
    /// neither.
    #[test]
    fn the_review_decision_follows_the_newest_review_per_reviewer() {
        let (store, repo_id) = store_with_repo();
        let review = |id: &str, actor: &str, title: &str, at: i64| {
            let mut e = event(EventKind::PrReviewed, id, actor, at);
            e.number = Some(101);
            e.title = title.to_string();
            e
        };
        let mut pull = seen_pull(101, "open", 40);

        // Nobody asked, nobody reviewed.
        store.upsert_pull(repo_id, &pull).unwrap();
        assert_eq!(store.open_pulls(None, None).unwrap()[0].review_decision, None);

        // Asked, unanswered.
        pull.requested_reviewers = vec!["bob".to_string()];
        store.upsert_pull(repo_id, &pull).unwrap();
        assert_eq!(
            store.open_pulls(None, None).unwrap()[0].review_decision.as_deref(),
            Some("review_required")
        );

        // A plain comment is not an answer.
        store.insert_event(repo_id, &review("r0", "bob", "Review: commented", 50)).unwrap();
        store.upsert_pull(repo_id, &pull).unwrap();
        assert_eq!(
            store.open_pulls(None, None).unwrap()[0].review_decision.as_deref(),
            Some("review_required")
        );

        // An approval is.
        store.insert_event(repo_id, &review("r1", "bob", "Review: approved", 60)).unwrap();
        store.upsert_pull(repo_id, &pull).unwrap();
        assert_eq!(
            store.open_pulls(None, None).unwrap()[0].review_decision.as_deref(),
            Some("approved")
        );

        // A second reviewer blocking outranks the first one's approval.
        store
            .insert_event(repo_id, &review("r2", "carol", "Review: changes requested", 70))
            .unwrap();
        store.upsert_pull(repo_id, &pull).unwrap();
        assert_eq!(
            store.open_pulls(None, None).unwrap()[0].review_decision.as_deref(),
            Some("changes_requested")
        );

        // ... until carol herself approves: her newest review is what counts,
        // not the fact that she once asked for changes.
        store.insert_event(repo_id, &review("r3", "carol", "Review: approved", 80)).unwrap();
        store.upsert_pull(repo_id, &pull).unwrap();
        assert_eq!(
            store.open_pulls(None, None).unwrap()[0].review_decision.as_deref(),
            Some("approved")
        );
    }

    /// The `ON DELETE CASCADE` on `pulls.repo_id`: removing a repo must not
    /// leave open PRs behind pointing at a repo that is gone.
    #[test]
    fn removing_a_repo_cascades_its_pulls() {
        let (store, repo_id) = store_with_repo();
        let other = store.insert_repo("acme", "web", "https://x", "", "main").unwrap();
        store.upsert_pull(repo_id, &seen_pull(101, "open", 40)).unwrap();
        store.upsert_pull(repo_id, &seen_pull(102, "open", 41)).unwrap();
        store.upsert_pull(other.id, &seen_pull(202, "open", 42)).unwrap();
        assert_eq!(store.open_pulls(None, None).unwrap().len(), 3);

        store.remove_repo(repo_id).unwrap();

        let left = store.open_pulls(None, None).unwrap();
        assert_eq!(left.len(), 1, "only the other repo's pull survives: {left:?}");
        assert_eq!(left[0].repo_id, other.id);
        assert_eq!(left[0].number, 202);
    }

    /// Removing an *account* deletes its repos, and those repos' pulls have to
    /// go with them — the same cascade, reached by a different door.
    #[test]
    fn deleting_an_account_cascades_its_repos_pulls() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_account("alice", None, 1).unwrap();
        store.upsert_account("bob", None, 2).unwrap();
        let hers = store.insert_repo("acme", "platform", "https://x", "alice", "main").unwrap();
        let his = store.insert_repo("acme", "web", "https://x", "bob", "main").unwrap();
        store.upsert_pull(hers.id, &seen_pull(101, "open", 40)).unwrap();
        store.upsert_pull(his.id, &seen_pull(202, "open", 41)).unwrap();

        assert!(store.delete_account("alice").unwrap());

        let left = store.open_pulls(None, None).unwrap();
        assert_eq!(left.len(), 1, "alice's repo took its pull with it: {left:?}");
        assert_eq!(left[0].repo_id, his.id);
    }

    /// `open_pulls` is ordered oldest-first and scoped by the author, either to
    /// one login or to a team's membership — with the casing of a login never
    /// mattering, exactly as everywhere else.
    #[test]
    fn open_pulls_are_oldest_first_and_scoped_by_author() {
        let (store, repo_id) = store_with_repo();
        let mut newest = seen_pull(103, "open", 300);
        newest.author_login = "Bob".to_string();
        let mut middle = seen_pull(102, "open", 200);
        middle.author_login = "carol".to_string();
        let oldest = seen_pull(101, "open", 100); // alice
        let merged = seen_pull(104, "merged", 50);
        for pull in [&oldest, &middle, &newest, &merged] {
            store.upsert_pull(repo_id, pull).unwrap();
        }

        let all = store.open_pulls(None, None).unwrap();
        assert_eq!(
            all.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![101, 102, 103],
            "oldest first, and the merged one is not open"
        );

        let team: HashSet<String> = ["ALICE".to_string(), "bob".to_string()].into_iter().collect();
        assert_eq!(
            store
                .open_pulls(Some(&team), None)
                .unwrap()
                .iter()
                .map(|p| p.number)
                .collect::<Vec<_>>(),
            vec![101, 103],
            "carol is not on the team"
        );

        assert_eq!(
            store.open_pulls(None, Some("bOb")).unwrap().iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![103],
        );
        // The two intersect, and an empty team matches nobody.
        assert!(store.open_pulls(Some(&team), Some("carol")).unwrap().is_empty());
        assert!(store.open_pulls(Some(&HashSet::new()), None).unwrap().is_empty());
    }

    /// The per-person counts the digest hangs off `pulls`: PRs you authored and
    /// PRs waiting on your review, keyed by lowercased login, counting only the
    /// open ones and never counting one PR twice for one reviewer.
    #[test]
    fn open_pull_person_counts_split_authorship_from_the_review_queue() {
        let (store, repo_id) = store_with_repo();
        let mut first = seen_pull(101, "open", 100);
        first.requested_reviewers = vec!["Bob".to_string(), "bob".to_string()];
        let mut second = seen_pull(102, "open", 200);
        second.author_login = "Bob".to_string();
        second.requested_reviewers = vec!["carol".to_string()];
        let mut done = seen_pull(103, "merged", 300);
        done.requested_reviewers = vec!["carol".to_string()];
        for pull in [&first, &second, &done] {
            store.upsert_pull(repo_id, pull).unwrap();
        }

        let (authored, queued) = store.open_pull_person_counts().unwrap();
        assert_eq!(authored.get("alice"), Some(&1));
        assert_eq!(authored.get("bob"), Some(&1));
        assert_eq!(authored.get("carol"), None);
        assert_eq!(queued.get("bob"), Some(&1), "listed twice on one PR still counts once");
        assert_eq!(queued.get("carol"), Some(&1), "the merged PR's request does not count");
    }

    /// v6 -> v7 is additive too: the repo survives, the ETag cache arrives
    /// empty, and the repo carries no polling hint until GitHub offers one.
    #[test]
    fn a_v6_database_migrates_to_v7_with_an_empty_etag_cache() {
        let conn = v6_database();
        conn.execute(
            "INSERT INTO repos (owner, name, url, account_login, default_branch, last_polled_at)
             VALUES ('acme', 'platform', 'https://x', 'alice', 'main', 4242)",
            [],
        )
        .unwrap();
        // v6 genuinely lacks both, or the rest of this proves nothing.
        assert!(
            !column_exists(&conn, "repos", "poll_interval_secs").unwrap(),
            "the v6 fixture already had repos.poll_interval_secs"
        );

        let store = Store::from_connection(conn).unwrap();

        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let repos = store.list_repos().unwrap();
        assert_eq!(repos.len(), 1, "the v6 repo survived");
        assert_eq!(repos[0].last_polled_at, Some(4242), "its watermark survived");
        assert!(store.poll_intervals().unwrap().is_empty(), "a migrated repo has no hint yet");
        assert!(store.etags_with_prefix("").unwrap().is_empty(), "the cache starts empty");
    }

    /// Re-running the v7 step must not empty a cache filled since. This drives
    /// `migrate_to_v7` a second time directly, which is the only way to reach
    /// its guards.
    #[test]
    fn re_running_the_v7_migration_is_a_no_op() {
        let (store, repo_id) = store_with_repo();
        store.put_etags(&[("https://api/x".to_string(), "\"t\"".to_string())], 10).unwrap();
        store.set_poll_interval(repo_id, 600).unwrap();

        migrate_to_v7(&store.conn).unwrap();

        assert_eq!(store.etags_with_prefix("https://api/").unwrap().len(), 1);
        assert_eq!(store.poll_intervals().unwrap().get(&repo_id), Some(&600));
    }

    /// Tags are written, replaced, read back by prefix, and swept by age.
    #[test]
    fn the_etag_cache_stores_replaces_prunes_and_clears() {
        let store = Store::open_in_memory().unwrap();
        let url = "https://api.github.com/repos/acme/platform/commits?per_page=100";
        store.put_etags(&[(url.to_string(), "\"one\"".to_string())], 100).unwrap();
        store.put_etags(&[(url.to_string(), "\"two\"".to_string())], 200).unwrap();

        let cached = store.etags_with_prefix("https://api.github.com/repos/acme/platform/").unwrap();
        assert_eq!(cached.len(), 1, "one URL keeps one tag");
        assert_eq!(cached[url], "\"two\"", "the later fetch replaced the earlier tag");

        // Written at 200, so a sweep of everything older than 200 spares it and
        // one of everything older than 201 does not.
        assert_eq!(store.prune_etags(200).unwrap(), 0);
        assert_eq!(store.prune_etags(201).unwrap(), 1);
        assert!(store.etags_with_prefix("").unwrap().is_empty());
    }

    /// `_` is a `LIKE` wildcard and a perfectly ordinary character in a repo
    /// name. Unescaped, clearing `acme/my_repo` would also clear `acme/myXrepo`.
    #[test]
    fn clearing_one_repos_tags_leaves_a_similarly_named_repo_alone() {
        let store = Store::open_in_memory().unwrap();
        let mine = "https://api.github.com/repos/acme/my_repo/commits";
        let theirs = "https://api.github.com/repos/acme/myXrepo/commits";
        store
            .put_etags(
                &[(mine.to_string(), "\"a\"".to_string()), (theirs.to_string(), "\"b\"".to_string())],
                1,
            )
            .unwrap();

        store.clear_etags(Some("https://api.github.com/repos/acme/my_repo/")).unwrap();

        let left = store.etags_with_prefix("").unwrap();
        assert_eq!(left.len(), 1, "only the named repo's tags go");
        assert!(left.contains_key(theirs), "acme/myXrepo kept its tag");

        store.clear_etags(None).unwrap();
        assert!(store.etags_with_prefix("").unwrap().is_empty(), "None clears the lot");
    }

    /// Re-running the v6 step must not wipe a floor recorded since. This drives
    /// `migrate_to_v6` a second time directly, which is the only way to reach
    /// its guard: the version stamp would otherwise skip it.
    #[test]
    fn re_running_the_v6_migration_is_a_no_op() {
        let (store, repo_id) = store_with_repo();
        store.mark_repo_polled(repo_id, 1_000, 500).unwrap();

        migrate_to_v6(&store.conn).unwrap();

        let repo = store.find_repo_by_id(repo_id).unwrap().unwrap();
        assert_eq!(repo.backfilled_to, Some(500), "a second v6 run dropped the floor");
    }

    /// The floor is seeded once and then owned by `backfill`. A later poll
    /// fetches a newer, narrower window, so letting it rewrite the cursor would
    /// erase everything backfill had reached.
    #[test]
    fn the_floor_is_seeded_by_the_first_poll_and_never_raised_by_a_later_one() {
        let (store, repo_id) = store_with_repo();
        assert_eq!(store.find_repo_by_id(repo_id).unwrap().unwrap().backfilled_to, None);

        store.mark_repo_polled(repo_id, 100_000, 13_600).unwrap();
        assert_eq!(
            store.find_repo_by_id(repo_id).unwrap().unwrap().backfilled_to,
            Some(13_600),
            "the first poll did not seed the floor"
        );

        store.set_backfilled_to(repo_id, 6_400).unwrap();
        store.mark_repo_polled(repo_id, 200_000, 99_700).unwrap();
        let repo = store.find_repo_by_id(repo_id).unwrap().unwrap();
        assert_eq!(repo.backfilled_to, Some(6_400), "a later poll raised the floor");
        assert_eq!(repo.last_polled_at, Some(200_000), "the watermark still advanced");
    }

    /// A rescan restarts the contiguous run of windows, so the floor goes with
    /// the watermark rather than outliving it.
    #[test]
    fn clearing_the_watermark_clears_the_floor_too() {
        let (store, repo_id) = store_with_repo();
        let other = store.insert_repo("acme", "web", "https://y", "", "main").unwrap();
        store.mark_repo_polled(repo_id, 100_000, 13_600).unwrap();
        store.mark_repo_polled(other.id, 100_000, 13_600).unwrap();

        store.clear_watermark(Some(repo_id)).unwrap();
        let one = store.find_repo_by_id(repo_id).unwrap().unwrap();
        assert_eq!((one.last_polled_at, one.backfilled_to), (None, None));
        assert_eq!(
            store.find_repo_by_id(other.id).unwrap().unwrap().backfilled_to,
            Some(13_600),
            "clearing one repo cleared another's floor"
        );

        store.clear_watermark(None).unwrap();
        for repo in store.list_repos().unwrap() {
            assert_eq!(repo.backfilled_to, None, "a full rescan left a floor behind");
        }
    }

    /// The seen flag is chosen at insert time, and it is what keeps backfilled
    /// history out of the unread count.
    #[test]
    fn an_event_inserted_as_seen_is_seen_and_uncounted() {
        let (store, repo_id) = store_with_repo();
        store.insert_event(repo_id, &event(EventKind::Commit, "a", "alice", 10)).unwrap();
        store
            .insert_event_as(repo_id, &event(EventKind::Commit, "b", "alice", 20), true)
            .unwrap();

        assert_eq!(store.unseen_count().unwrap(), 1, "only the polled event is unread");
        let stored = store.list_events(None, None, None, None, false, None, 10).unwrap();
        let seen: Vec<bool> = stored.iter().map(|e| e.seen).collect();
        assert_eq!(seen, vec![true, false], "newest (the backfilled one) first");
    }

    /// Accounts come back in the order they were added, not alphabetically, and
    /// re-adding one keeps its place and its original `added_at`.
    #[test]
    fn accounts_keep_their_insertion_order_across_an_upsert() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_account("zoe", None, 10).unwrap();
        store.upsert_account("alice", None, 20).unwrap();

        let refreshed = store.upsert_account("ZOE", Some("https://avatar"), 999).unwrap();

        assert_eq!(refreshed.added_at, 10, "an upsert must not restamp added_at");
        assert_eq!(refreshed.login, "zoe", "the stored casing wins over the caller's");
        assert_eq!(refreshed.avatar_url.as_deref(), Some("https://avatar"));
        let logins: Vec<String> =
            store.list_accounts().unwrap().into_iter().map(|a| a.login).collect();
        assert_eq!(logins, vec!["zoe", "alice"], "insertion order, and one row not two");
    }

    /// Deleting an account takes its repos, and their events and watches with
    /// them — and leaves every other account's alone.
    #[test]
    fn deleting_an_account_cascades_only_its_own_repos() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_account("alice", None, 1).unwrap();
        store.upsert_account("bob", None, 2).unwrap();
        let platform =
            store.insert_repo("acme", "platform", "https://x", "alice", "main").unwrap();
        let web = store.insert_repo("acme", "web", "https://x", "bob", "main").unwrap();
        for repo_id in [platform.id, web.id] {
            store.insert_event(repo_id, &event(EventKind::Commit, "sha", "a", 5)).unwrap();
            store
                .insert_watch_if_absent(
                    repo_id,
                    7,
                    ThreadKind::Pull,
                    "t",
                    "open",
                    WatchSource::Manual,
                    1,
                )
                .unwrap();
        }

        assert!(store.delete_account("alice").unwrap());

        let logins: Vec<String> =
            store.list_accounts().unwrap().into_iter().map(|a| a.login).collect();
        assert_eq!(logins, vec!["bob"]);
        let repos = store.list_repos().unwrap();
        assert_eq!(repos.len(), 1, "only alice's repo went: {repos:?}");
        assert_eq!(repos[0].id, web.id);
        let events = store.list_events(None, None, None, None, false, None, 50).unwrap();
        assert_eq!(events.len(), 1, "alice's repo's events cascaded with it");
        assert_eq!(events[0].repo_id, web.id);
        let watches = store.list_watches().unwrap();
        assert_eq!(watches.len(), 1, "and so did its watches");
        assert_eq!(watches[0].repo_id, web.id);

        assert!(!store.delete_account("nobody").unwrap(), "an unknown login removes nothing");
    }

    /// Re-running the v2 step against a database that already has the column is
    /// a no-op, not an error.
    #[test]
    fn migrating_an_already_current_database_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gitmon.db");
        let first = Store::open(&path).unwrap();
        first.insert_repo("acme", "platform", "https://x", "", "main").unwrap();
        drop(first);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(reopened.list_repos().unwrap().len(), 1);
    }

    #[test]
    fn adding_the_same_repo_twice_returns_one_row() {
        let (store, id) = store_with_repo();
        let again = store.insert_repo("acme", "platform", "https://x", "", "main").unwrap();
        assert_eq!(again.id, id);
        assert_eq!(store.list_repos().unwrap().len(), 1);
    }

    #[test]
    fn the_unique_index_rejects_a_second_identical_event() {
        let (store, repo_id) = store_with_repo();
        let first = store.insert_event(repo_id, &event(EventKind::Commit, "sha1", "a", 5)).unwrap();
        let second =
            store.insert_event(repo_id, &event(EventKind::Commit, "sha1", "a", 5)).unwrap();
        assert!(first.is_some());
        assert!(second.is_none(), "re-inserting the same external_id must be ignored");
    }

    #[test]
    fn removing_a_repo_cascades_its_events() {
        let (store, repo_id) = store_with_repo();
        store.insert_event(repo_id, &event(EventKind::Commit, "sha1", "a", 5)).unwrap();
        store.remove_repo(repo_id).unwrap();
        assert!(store.list_events(None, None, None, None, false, None, 50).unwrap().is_empty());
    }

    #[test]
    fn events_come_back_newest_first_and_clamp_to_500() {
        let (store, repo_id) = store_with_repo();
        for i in 0..3 {
            store
                .insert_event(repo_id, &event(EventKind::Commit, &format!("s{i}"), "a", i))
                .unwrap();
        }
        let rows = store.list_events(None, None, None, None, false, None, u32::MAX).unwrap();
        assert_eq!(rows.iter().map(|e| e.occurred_at).collect::<Vec<_>>(), vec![2, 1, 0]);
    }

    #[test]
    fn a_dangling_cursor_returns_nothing() {
        let (store, repo_id) = store_with_repo();
        store.insert_event(repo_id, &event(EventKind::Commit, "s", "a", 1)).unwrap();
        assert!(store.list_events(None, None, None, None, false, Some(9_999), 10).unwrap().is_empty());
    }

    #[test]
    fn actor_filter_ignores_case() {
        let (store, repo_id) = store_with_repo();
        store.insert_event(repo_id, &event(EventKind::Commit, "s", "AlIcE", 1)).unwrap();
        assert_eq!(store.list_events(None, Some("alice"), None, None, false, None, 10).unwrap().len(), 1);
        assert_eq!(store.list_events(None, Some("ALICE"), None, None, false, None, 10).unwrap().len(), 1);
        assert_eq!(store.list_events(None, Some("bob"), None, None, false, None, 10).unwrap().len(), 0);
    }

    #[test]
    fn teams_and_settings_round_trip() {
        let store = Store::open_in_memory().unwrap();
        // A fresh database has no teams at all; onboarding makes the first.
        assert!(store.list_teams().unwrap().is_empty());

        let created =
            store.create_team("Platform", &["alice".to_string(), "bob".to_string()]).unwrap();
        assert_eq!(created.name, "Platform");
        assert_eq!(created.logins, vec!["alice", "bob"]);
        let teams = store.list_teams().unwrap();
        assert_eq!(teams, vec![created.clone()], "what create returned is what is stored");

        assert_eq!(store.get_settings().unwrap(), Settings::default());
        let custom = Settings { poll_interval_secs: 300, ..Settings::default() };
        store.set_settings(&custom).unwrap();
        assert_eq!(store.get_settings().unwrap(), custom);
        // Settings no longer share a row with the team name, and must not have
        // disturbed the teams either way.
        assert_eq!(store.list_teams().unwrap(), vec![created]);
    }

    /// The name used to live in `settings.team_name`; v3 drops that column.
    #[test]
    fn the_dead_settings_team_name_column_is_gone() {
        let store = Store::open_in_memory().unwrap();
        assert!(
            !column_exists(&store.conn, "settings", "team_name").unwrap(),
            "settings.team_name survived the v3 migration"
        );
        assert!(!store.legacy_team_name_column);
        // And the settings row still writes through the narrowed table.
        store.set_settings(&Settings::default()).unwrap();
        assert_eq!(store.get_settings().unwrap(), Settings::default());
    }

    /// The `ON DELETE CASCADE` on `team_logins.team_id` is what stops a deleted
    /// team leaving its members behind in the filter union.
    #[test]
    fn deleting_a_team_takes_only_its_own_logins() {
        let store = Store::open_in_memory().unwrap();
        let platform = store.create_team("Platform", &["alice".to_string()]).unwrap();
        let infra =
            store.create_team("Infra", &["bob".to_string(), "alice".to_string()]).unwrap();

        store.delete_team(platform.id).unwrap();

        assert_eq!(store.team_logins(platform.id).unwrap(), None, "the team is gone");
        assert_eq!(
            store.team_logins(infra.id).unwrap(),
            Some(vec!["bob".to_string(), "alice".to_string()]),
            "the survivor kept its logins, alice included"
        );
        // alice is still a member — through Infra, not through the dead team.
        assert_eq!(store.team_ids_for_login("alice").unwrap(), vec![infra.id]);
        assert_eq!(
            store.member_logins().unwrap(),
            ["alice", "bob"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn seen_flags_drive_the_unseen_count() {
        let (store, repo_id) = store_with_repo();
        let a = store.insert_event(repo_id, &event(EventKind::Commit, "a", "x", 1)).unwrap();
        store.insert_event(repo_id, &event(EventKind::Commit, "b", "x", 2)).unwrap();
        assert_eq!(store.unseen_count().unwrap(), 2);
        store.mark_seen(&[a.unwrap().id]).unwrap();
        assert_eq!(store.unseen_count().unwrap(), 1);
        store.mark_seen(&[]).unwrap();
        assert_eq!(store.unseen_count().unwrap(), 1);
    }
}
