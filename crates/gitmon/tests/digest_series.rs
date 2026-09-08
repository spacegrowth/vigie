//! The Summary-v2 halves of a digest: the per-local-day `series`, the 7x24
//! `hours` heatmap, the `pr_timing` medians and p90s, and the per-person numbers that
//! come off the `pulls` table rather than off the window.
//!
//! Like `digest.rs`, every event is planted through a mock GitHub server and a
//! real poll, so what is being summarised is what the store actually holds.

mod common;

use common::{engine_for, iso8601, mount, server, RepoMock, Resp};
use chrono::{DateTime, Datelike, Timelike};
use gitmon::{EventKind, FilterMode, Settings};

const PLATFORM: &str = "acme/platform";

fn json_array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

fn user(login: &str) -> String {
    format!(r#"{{"login":"{login}","avatar_url":null}}"#)
}

fn commit(sha: &str, actor: &str, at: i64) -> String {
    format!(
        r#"{{"sha":"{sha}",
             "html_url":"https://github.com/{PLATFORM}/commit/{sha}",
             "commit":{{"message":"Commit {sha}",
                        "author":{{"name":"{actor}","date":"{date}"}},
                        "committer":{{"name":"{actor}","date":"{date}"}}}},
             "author":{who},"committer":{who}}}"#,
        date = iso8601(at),
        who = user(actor),
    )
}

fn pull(
    number: u64,
    author: &str,
    created_at: i64,
    merged_at: Option<i64>,
    reviewers: &[&str],
) -> String {
    let stamp = |at: Option<i64>| match at {
        Some(at) => format!("\"{}\"", iso8601(at)),
        None => "null".to_string(),
    };
    let requested: Vec<String> = reviewers.iter().map(|r| user(r)).collect();
    let updated = merged_at.unwrap_or(created_at);
    format!(
        r#"{{"number":{number},"title":"PR {number}",
             "html_url":"https://github.com/{PLATFORM}/pull/{number}",
             "user":{who},"body":null,"state":"open","draft":false,
             "created_at":"{created}","updated_at":"{updated}",
             "closed_at":{closed},"merged_at":{merged},
             "merged_by":null,"requested_reviewers":[{requested}],
             "head":null,"base":null,
             "additions":null,"deletions":null,"changed_files":null}}"#,
        who = user(author),
        created = iso8601(created_at),
        updated = iso8601(updated),
        closed = stamp(merged_at),
        merged = stamp(merged_at),
        requested = requested.join(","),
    )
}

fn review(number: u64, id: u64, actor: &str, at: i64) -> String {
    format!(
        r#"{{"id":{id},
             "html_url":"https://github.com/{PLATFORM}/pull/{number}#pullrequestreview-{id}",
             "body":null,"user":{who},"state":"APPROVED","submitted_at":"{at}"}}"#,
        who = user(actor),
        at = iso8601(at),
    )
}

/// An engine with one repo, polled once against `platform`.
fn polled(platform: RepoMock) -> (gitmon::Engine, Keep) {
    let mut srv = server();
    let mocks = mount(&mut srv, &platform);
    let (engine, dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo(PLATFORM, None).unwrap();
    let polled = engine.poll_now();
    assert!(polled.errors.is_empty(), "the fixture server misfired: {:?}", polled.errors);
    (engine, Keep { _server: srv, _mocks: mocks, _dir: dir })
}

/// Everything that has to outlive the test body.
struct Keep {
    _server: mockito::ServerGuard,
    _mocks: Vec<mockito::Mock>,
    _dir: tempfile::TempDir,
}

/// The local weekday (Monday = 0) and hour of an instant under `tz`, computed
/// with chrono rather than with the engine's own arithmetic — so the heatmap
/// assertions below are a second opinion, not an echo.
fn local_cell(at: i64, tz: i32) -> (usize, usize) {
    let local = DateTime::from_timestamp(at + tz as i64, 0).expect("valid timestamp");
    (local.weekday().num_days_from_monday() as usize, local.hour() as usize)
}

/// `series` is one entry per **local** day the window touches — the offset
/// decides where a day starts, quiet days are present and zeroed, and the day
/// an event lands on is the local one, not the UTC one.
#[test]
fn the_series_buckets_events_by_local_day_and_keeps_the_empty_ones() {
    // Twenty hours ago, comfortably inside the 24 h first-poll window.
    let base = chrono::Utc::now().timestamp() - 20 * 3600;
    // The suite runs at an arbitrary instant, so rather than hoping a real
    // timezone's midnight falls somewhere useful, the offset is *chosen* to put
    // one exactly six hours after `base`. Any offset the app could pass is a
    // whole number of seconds, and the engine treats them all alike.
    let boundary = base + 6 * 3600;
    let tz = (-boundary).rem_euclid(86_400) as i32;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(&[
        // Local day D-1, at 22:00 and 23:00 local.
        commit("aaa1", "alice", boundary - 2 * 3600),
        commit("aaa2", "alice", boundary - 3600),
        // Local day D, at 01:00 and 02:00 local.
        commit("bbb1", "bob", boundary + 3600),
        commit("bbb2", "bob", boundary + 2 * 3600),
    ]));
    let (engine, _keep) = polled(platform);

    // The window opens six hours before the boundary and closes thirty hours
    // after it, so it touches three local days: D-1, D, and D+1 — and nothing
    // at all happened on D+1.
    let start = boundary - 6 * 3600;
    let end = boundary + 30 * 3600;
    let digest = engine.digest(start, end, None, None, tz).unwrap();

    assert_eq!(digest.series.len(), 3, "three local days: {:?}", digest.series);
    // Each `day` is the unix second that local day began, so they are exactly
    // 86 400 apart and the middle one is the boundary itself.
    assert_eq!(digest.series[0].day, boundary - 86_400);
    assert_eq!(digest.series[1].day, boundary);
    assert_eq!(digest.series[2].day, boundary + 86_400);

    assert_eq!(digest.series[0].counts.get(EventKind::Commit), 2, "D-1 has alice's two");
    assert_eq!(digest.series[1].counts.get(EventKind::Commit), 2, "D has bob's two");
    assert_eq!(digest.series[2].counts.total(), 0, "D+1 is present and empty");
    assert_eq!(
        digest.series.iter().map(|d| d.counts.total()).sum::<u64>(),
        digest.totals.total(),
        "the series has to add up to the window it summarises"
    );

    // Under UTC the same four commits fall differently, which is the whole
    // point of passing an offset at all.
    let utc = engine.digest(start, end, None, None, 0).unwrap();
    assert_ne!(
        utc.series.iter().map(|d| d.counts.total()).collect::<Vec<_>>(),
        digest.series.iter().map(|d| d.counts.total()).collect::<Vec<_>>(),
        "a six-hour shift must move at least one commit to another day"
    );
    assert_eq!(
        utc.series.iter().map(|d| d.counts.total()).sum::<u64>(),
        utc.totals.total(),
        "and it still adds up"
    );
}

/// A window that closes exactly at a local midnight covers the day *before* it,
/// not the day about to start: the window is half-open, and so is the series.
#[test]
fn a_window_ending_at_local_midnight_stops_at_the_day_before() {
    let base = chrono::Utc::now().timestamp() - 20 * 3600;
    let boundary = base + 6 * 3600;
    let tz = (-boundary).rem_euclid(86_400) as i32;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(&[commit("aaa1", "alice", boundary - 3600)]));
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(boundary - 86_400, boundary, None, None, tz).unwrap();
    assert_eq!(digest.series.len(), 1, "one day, not two: {:?}", digest.series);
    assert_eq!(digest.series[0].day, boundary - 86_400);
    assert_eq!(digest.series[0].counts.get(EventKind::Commit), 1);
}

/// `hours` is always seven rows of twenty-four, and each event lands in the
/// cell its own local weekday and hour name.
#[test]
fn the_heatmap_is_always_seven_by_twenty_four() {
    let base = chrono::Utc::now().timestamp() - 20 * 3600;
    let boundary = base + 6 * 3600;
    let tz = (-boundary).rem_euclid(86_400) as i32;

    let times = [boundary - 2 * 3600, boundary - 3600, boundary + 3600, boundary + 2 * 3600];
    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(
        &times
            .iter()
            .enumerate()
            .map(|(i, at)| commit(&format!("sha{i}"), "alice", *at))
            .collect::<Vec<_>>(),
    ));
    let (engine, _keep) = polled(platform);

    let digest = engine
        .digest(boundary - 6 * 3600, boundary + 30 * 3600, None, None, tz)
        .unwrap();

    assert_eq!(digest.hours.len(), 7);
    assert!(digest.hours.iter().all(|row| row.len() == 24), "every row is 24 wide");
    assert_eq!(
        digest.hours.iter().flatten().sum::<u64>(),
        digest.totals.total(),
        "the grid has to add up to the window it summarises"
    );

    // Exactly the four cells chrono names, one event each.
    let mut expected: Vec<(usize, usize)> =
        times.iter().map(|at| local_cell(*at, tz)).collect();
    expected.sort_unstable();
    let mut got: Vec<(usize, usize)> = Vec::new();
    for (weekday, row) in digest.hours.iter().enumerate() {
        for (hour, count) in row.iter().enumerate() {
            for _ in 0..*count {
                got.push((weekday, hour));
            }
        }
    }
    got.sort_unstable();
    assert_eq!(got, expected);
    // The chosen offset puts local midnight at `boundary`, so the two commits
    // before it are the last hours of one day and the two after it the first
    // hours of the next.
    let mut hours: Vec<usize> = expected.iter().map(|(_, hour)| *hour).collect();
    hours.sort_unstable();
    assert_eq!(hours, vec![1, 2, 22, 23]);
    // ... and they really do straddle a weekday change.
    let weekdays: Vec<usize> = expected.iter().map(|(weekday, _)| *weekday).collect();
    assert_eq!(
        weekdays.iter().collect::<std::collections::HashSet<_>>().len(),
        2,
        "two local days means two rows of the grid: {expected:?}"
    );
}

/// Three PRs opened, merged and reviewed at known gaps: an odd number of
/// time-to-merge samples and an even number of time-to-first-review ones, so
/// both halves of the median rule are exercised at once.
#[test]
fn the_pr_timings_are_medians_over_what_closed_inside_the_window() {
    // Far enough back that everything below is in the past and inside the 24 h
    // first-poll window.
    let base = chrono::Utc::now().timestamp() - 20 * 3600;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.pulls = Resp::ok(json_array(&[
        pull(101, "alice", base + 100, Some(base + 100 + 3_600), &[]),
        pull(102, "alice", base + 200, Some(base + 200 + 7_200), &[]),
        pull(103, "bob", base + 300, Some(base + 300 + 10_800), &[]),
    ]));
    platform.reviews = vec![
        (
            101,
            Resp::ok(json_array(&[
                review(101, 9001, "bob", base + 100 + 1_800),
                // A later review must not displace the first one.
                review(101, 9002, "carol", base + 100 + 3_000),
            ])),
        ),
        (102, Resp::ok(json_array(&[review(102, 9003, "bob", base + 200 + 5_400)]))),
        (103, Resp::json_array()),
    ];
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(base, base + 20_000, None, None, 0).unwrap();
    assert_eq!(
        digest.pr_timing.median_ttm_secs,
        Some(7_200),
        "three merges of 3600/7200/10800 — the middle one"
    );
    assert_eq!(
        digest.pr_timing.median_ttfr_secs,
        Some(3_600),
        "two first-reviews of 1800 and 5400 — their average"
    );
    assert_eq!(digest.pr_timing.samples, 3, "three distinct PRs contributed something");

    // Scoped to one author, only her PRs count: two merges (3600, 7200) and
    // two first-reviews (1800, 5400).
    let hers = engine.digest(base, base + 20_000, None, Some("alice"), 0).unwrap();
    assert_eq!(hers.pr_timing.median_ttm_secs, Some(5_400));
    assert_eq!(hers.pr_timing.median_ttfr_secs, Some(3_600));
    assert_eq!(hers.pr_timing.samples, 2);

    // A window that opens after everything closed measures nothing, and says
    // so with nulls rather than with zeros.
    let quiet = engine.digest(base + 19_000, base + 20_000, None, None, 0).unwrap();
    assert_eq!(quiet.pr_timing.median_ttm_secs, None);
    assert_eq!(quiet.pr_timing.median_ttfr_secs, None);
    assert_eq!(quiet.pr_timing.samples, 0);

    // A PR merged inside the window but opened before it still counts: the
    // window bounds what *finished*, not what started.
    let late = engine.digest(base + 5_000, base + 20_000, None, None, 0).unwrap();
    assert_eq!(
        late.pr_timing.median_ttm_secs,
        Some(9_000),
        "only #102 (7200) and #103 (10800) merged in this window; an even count averages"
    );
    assert_eq!(late.pr_timing.samples, 2);
}

/// Beside each median sits a p90 over the very same samples: five merges with
/// one slow tail, so the two numbers are visibly different and the interpolated
/// answer is not simply the slowest PR.
#[test]
fn the_pr_timings_carry_a_p90_over_the_same_samples() {
    let base = chrono::Utc::now().timestamp() - 20 * 3600;
    // Merge gaps 1000/2000/3000/4000/20000 and first-review gaps
    // 500/600/700/800/9000: an odd count each, with a fifth PR far out in the
    // tail where a p90 is supposed to notice it and a median is not.
    let merges = [1_000, 2_000, 3_000, 4_000, 20_000];
    let reviews = [500, 600, 700, 800, 9_000];

    let mut platform = RepoMock::quiet("acme", "platform");
    // Newest-updated first, which is the order the poller pages `pulls` in.
    platform.pulls = Resp::ok(json_array(
        &(0..5)
            .rev()
            .map(|i| {
                pull(201 + i as u64, "alice", base + 100, Some(base + 100 + merges[i]), &[])
            })
            .collect::<Vec<_>>(),
    ));
    platform.reviews = (0..5)
        .map(|i| {
            (
                201 + i as u64,
                Resp::ok(json_array(&[review(
                    201 + i as u64,
                    9_100 + i as u64,
                    "bob",
                    base + 100 + reviews[i],
                )])),
            )
        })
        .collect();
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(base, base + 30_000, None, None, 0).unwrap();
    assert_eq!(digest.pr_timing.samples, 5);
    assert_eq!(digest.pr_timing.median_ttm_secs, Some(3_000), "the middle of five merges");
    assert_eq!(
        digest.pr_timing.p90_ttm_secs,
        Some(13_600),
        "rank 3.6 of five: 4000 + 0.6 * (20000 - 4000)"
    );
    assert_eq!(digest.pr_timing.median_ttfr_secs, Some(700));
    assert_eq!(
        digest.pr_timing.p90_ttfr_secs,
        Some(5_720),
        "rank 3.6 again: 800 + 0.6 * (9000 - 800)"
    );
    // Interpolated, so the tail shows without the p90 collapsing onto the
    // slowest sample — which is what a nearest-rank p90 would have reported.
    assert!(digest.pr_timing.p90_ttm_secs < Some(base + 100 + merges[4] - (base + 100)));
    assert!(digest.pr_timing.p90_ttm_secs > digest.pr_timing.median_ttm_secs);

    // A window in which exactly one PR closed: with one sample the p90 and the
    // median are the same number, because there is nothing to interpolate.
    let single = engine
        .digest(base + 100 + 19_000, base + 30_000, None, None, 0)
        .unwrap();
    assert_eq!(single.pr_timing.samples, 1);
    assert_eq!(single.pr_timing.median_ttm_secs, Some(20_000));
    assert_eq!(single.pr_timing.p90_ttm_secs, Some(20_000));
    assert_eq!(single.pr_timing.median_ttfr_secs, None);
    assert_eq!(single.pr_timing.p90_ttfr_secs, None);
}

/// The per-person extras: `last_at` is the window's, while `open_prs` and
/// `review_queue` are read off the `pulls` table as it stands now — and
/// `people_total` counts everyone, not just the twenty who are listed.
#[test]
fn people_carry_their_last_activity_and_their_current_pull_request_load() {
    let base = chrono::Utc::now().timestamp() - 20 * 3600;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(&[
        commit("aaa1", "alice", base + 100),
        commit("aaa2", "alice", base + 900),
        commit("bbb1", "bob", base + 500),
    ]));
    // Two open PRs by alice, one asking bob to review; one merged PR by bob,
    // which must not count towards anybody's open total.
    platform.pulls = Resp::ok(json_array(&[
        pull(101, "alice", base + 50, None, &["bob"]),
        pull(102, "alice", base + 60, None, &[]),
        pull(103, "bob", base + 70, Some(base + 800), &["alice"]),
    ]));
    platform.reviews = vec![
        (101, Resp::json_array()),
        (102, Resp::json_array()),
        (103, Resp::json_array()),
    ];
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(base, base + 20_000, None, None, 0).unwrap();
    assert_eq!(digest.people_total, 2, "alice and bob, counted before the top-20 cut");

    let alice = digest.people.iter().find(|p| p.login == "alice").expect("alice is here");
    assert_eq!(alice.last_at, base + 900, "her newest event in the window");
    assert_eq!(alice.open_prs, 2, "#101 and #102; the merged #103 is not open");
    assert_eq!(alice.review_queue, 0, "the PR asking her to review is merged");

    let bob = digest.people.iter().find(|p| p.login == "bob").expect("bob is here");
    assert_eq!(bob.last_at, base + 800, "his merge is newer than his commit");
    assert_eq!(bob.open_prs, 0);
    assert_eq!(bob.review_queue, 1, "#101 is waiting on him");

    // The load is current state, so a window in which nobody did anything still
    // reports it — but nobody is listed, because the list is of the window.
    let quiet = engine.digest(base - 10_000, base - 9_000, None, None, 0).unwrap();
    assert!(quiet.people.is_empty());
    assert_eq!(quiet.people_total, 0);
    assert!(quiet.series.iter().all(|d| d.counts.total() == 0));
    assert_eq!(quiet.hours.iter().flatten().sum::<u64>(), 0);
}
