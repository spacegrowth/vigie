//! The Vigie polling engine.
//!
//! Implements `docs/CONTRACT.md`: a synchronous, `Send + Sync` [`Engine`] that
//! polls GitHub repositories, filters events to the people on your teams, and
//! stores them in SQLite. It has no async runtime, no Tauri dependency, and no
//! global state.
//!
//! ```no_run
//! use gitmon::Engine;
//! let engine = Engine::open(std::path::Path::new("/tmp/vigie")).unwrap();
//! engine.set_token(Some("ghp_example".to_string()));
//! engine.add_repo("octocat/hello-world", None).unwrap();
//! let result = engine.poll_now();
//! println!("{} new events", result.new_events.len());
//! ```

mod engine;
mod error;
pub mod github;
mod poll;
mod store;
pub mod types;

pub use engine::Engine;
pub use error::{EngineError, ErrorKind, Result};
pub use github::GitHubClient;
pub use poll::{
    BACKFILL_DEFAULT_SPAN_SECS, BACKFILL_MAX_SPAN_SECS, BACKFILL_MIN_SPAN_SECS,
    FIRST_POLL_LOOKBACK_SECS, OVERLAP_SECS,
};
pub use store::MAX_EVENT_LIMIT;
pub use types::{
    Account, BackfillResult, CommitDetail, CommitFile, DayCounts, DeviceLogin, DeviceLoginStatus,
    Digest, DigestPerson,
    DigestRepo, DigestThread, Event, EventKind, FilterMode, KindCounts, OpenPull, PersonSource,
    PersonSuggestion,
    PollOptions, PollResult, PrTiming, QuietHours, Repo, RepoBackfill, RepoError, RepoSuggestion,
    Settings,
    Team, Thread,
    ThreadItem,
    ThreadItemKind, ThreadKind, Watch, WatchSource, DIGEST_MAX_SERIES_DAYS, DIGEST_PEOPLE_LIMIT,
    DIGEST_THREAD_LIMIT,
    MIN_POLL_INTERVAL_SECS, PULL_STATE_OPEN, REVIEW_APPROVED, REVIEW_CHANGES_REQUESTED,
    REVIEW_REQUIRED, UNSAVED_TEAM_ID, WATCH_STATE_OPEN,
};

#[cfg(test)]
mod contract_assertions {
    /// The app holds one engine for the process and calls it from its poll
    /// timer, so this bound is part of the contract.
    #[test]
    fn engine_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::Engine>();
    }
}
