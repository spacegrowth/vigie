// Typed wrappers over invoke(). This is the ONLY file allowed to call
// @tauri-apps/api's invoke — every other module goes through here.
//
// Arg keys are snake_case: the Rust commands are annotated
// `rename_all = "snake_case"` (see src-tauri/src/commands.rs), matching the
// snake_case wire format the engine types use everywhere else.
import { invoke } from "@tauri-apps/api/core";
import type {
  Account,
  BackfillResult,
  CommitDetail,
  CommitFile,
  DeviceLogin,
  DeviceLoginStatus,
  Digest,
  Event,
  EngineError,
  FilterMode,
  OpenPull,
  PersonSuggestion,
  PollResult,
  Repo,
  RepoSuggestion,
  Settings,
  Team,
  Thread,
  Watch,
} from "./types";

export type { EngineError };

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(cmd, args);
}

export const api = {
  // GitHub OAuth device flow — the only sign-in path (docs/CONTRACT.md
  // "Sign-in"). There is no token-paste command anywhere in this file.
  githubClientIdReady(): Promise<boolean> {
    return call<boolean>("github_client_id_ready");
  },
  startDeviceLogin(): Promise<DeviceLogin> {
    return call<DeviceLogin>("start_device_login");
  },
  openGithubSigninWindow(url: string): Promise<void> {
    return call<void>("open_github_signin_window", { url });
  },
  /** The "Use Safari instead" escape hatch: opens the same verification URL
   * in the default browser, for what the in-app window can't do (passkeys).
   * The device-flow poll loop is unaffected — it watches GitHub, not the
   * window — so either window can finish the sign-in. */
  openSigninInBrowser(url: string): Promise<void> {
    return call<void>("open_signin_in_browser", { url });
  },
  pollDeviceLogin(deviceCode: string): Promise<DeviceLoginStatus> {
    return call<DeviceLoginStatus>("poll_device_login", { device_code: deviceCode });
  },
  signOut(): Promise<void> {
    return call<void>("sign_out");
  },
  loadToken(): Promise<string | null> {
    return call<string | null>("load_token");
  },
  /** Who the engine has signed in, or null. Replaces the old localStorage
   * cache: the engine remembers the login from `verify_token` and the
   * device flow, so there is nothing left to cache. */
  currentLogin(): Promise<string | null> {
    return call<string | null>("current_login");
  },

  // Accounts (docs/CONTRACT.md "Accounts") — an install may have several,
  // each signed in via the same device flow as `pollDeviceLogin` above.
  listAccounts(): Promise<Account[]> {
    return call<Account[]>("list_accounts");
  },
  /** Registers the account a device-flow token belongs to and stores it in
   * the keychain — the same last step `pollDeviceLogin` itself ends with,
   * exposed here for completeness (see commands.rs). */
  addAccountToken(token: string): Promise<Account> {
    return call<Account>("add_account_token", { token });
  },
  /** Forgets the account, its repos, and their events and watches. */
  removeAccount(login: string): Promise<void> {
    return call<void>("remove_account", { login });
  },
  /** "Sign out": deletes that account's keychain item and drops its token
   * from the engine for this session. The account row and its repos are
   * untouched — unlike `removeAccount`, above. */
  signOutAccount(login: string): Promise<void> {
    return call<void>("sign_out_account", { login });
  },

  listTeams(): Promise<Team[]> {
    return call<Team[]>("list_teams");
  },
  createTeam(name: string, logins: string[]): Promise<Team> {
    return call<Team>("create_team", { name, logins });
  },
  updateTeam(team: Team): Promise<void> {
    return call<void>("update_team", { team });
  },
  deleteTeam(teamId: number): Promise<void> {
    return call<void>("delete_team", { team_id: teamId });
  },
  reorderTeams(ids: number[]): Promise<void> {
    return call<void>("reorder_teams", { ids });
  },
  importOrgTeam(org: string, teamSlug: string): Promise<Team> {
    return call<Team>("import_org_team", { org, team_slug: teamSlug });
  },

  suggestPeople(query: string, limit: number, teamId?: number | null): Promise<PersonSuggestion[]> {
    return call<PersonSuggestion[]>("suggest_people", { query, team_id: teamId ?? null, limit });
  },
  /** `suggestPeople`'s mirror image: `teamId`, when given, keeps only that
   * team's members instead of excluding them — for narrowing a search *to*
   * a team scope (e.g. Summary's person filter) rather than filling one in
   * (Teams' "add a member" search, which stays on `suggestPeople`). */
  suggestActivePeople(
    query: string,
    limit: number,
    teamId?: number | null,
  ): Promise<PersonSuggestion[]> {
    return call<PersonSuggestion[]>("suggest_active_people", {
      query,
      team_id: teamId ?? null,
      limit,
    });
  },
  suggestRepos(): Promise<RepoSuggestion[]> {
    return call<RepoSuggestion[]>("suggest_repos");
  },

  addRepo(spec: string, accountLogin?: string | null): Promise<Repo> {
    return call<Repo>("add_repo", { spec, account_login: accountLogin ?? null });
  },
  removeRepo(repoId: number): Promise<void> {
    return call<void>("remove_repo", { repo_id: repoId });
  },
  listRepos(): Promise<Repo[]> {
    return call<Repo[]>("list_repos");
  },
  /** Only the hidden repos — everything else in the app reads `listRepos`,
   * which leaves them out. Repos.svelte's collapsed "Hidden" section is the
   * one place they are still shown, because it is the page that unhides
   * them. */
  listHiddenRepos(): Promise<Repo[]> {
    return call<Repo[]>("list_hidden_repos");
  },
  /** Hides or unhides a repo (docs/CONTRACT.md "Hiding a repo"): it leaves
   * every view and stops being polled, and its stored history stays exactly
   * where it is. `removeRepo` above is the destructive one. */
  setRepoHidden(repoId: number, hidden: boolean): Promise<void> {
    return call<void>("set_repo_hidden", { repo_id: repoId, hidden });
  },

  pollNow(): Promise<PollResult> {
    return call<PollResult>("poll_now");
  },
  /** Loads the next older window of history — what the feed calls when it
   * reaches the end of what's stored, and what Repos.svelte calls right
   * after adding a repo so it isn't left looking empty. `repoId` null steps
   * every repo back; `spanSecs` defaults to a week and is clamped by the
   * engine either way. */
  backfill(repoId: number | null, spanSecs?: number): Promise<BackfillResult> {
    return call<BackfillResult>("backfill", { repo_id: repoId, span_secs: spanSecs ?? null });
  },
  listEvents(opts: {
    repoId?: number | null;
    actor?: string | null;
    teamId?: number | null;
    /** The app-wide "My team / Everyone" view filter (docs/CONTRACT.md
     * "Filter"). Applied on every read, so switching it is instant and never
     * loses anything — unlike `Settings.filter_mode`'s old role gating
     * ingestion.
     *
     * Every caller of `listEvents` now passes this explicitly (packet
     * gm-scope-r1's "scope is always explicit at the call site rather than
     * silently assumed") — Feed, Comments, Commits, Issues, People,
     * PullRequests and the tray popover all resolve it from
     * `feed-filters.ts`'s `feedModeStore` the same way Feed.svelte
     * originally did. The `"team"` default below is kept for exactly one
     * remaining caller: `stores.ts`'s `badgeStore.refresh()` (the sidebar's
     * unseen counts), which gm-scope-r1's boundary marks do-not-touch — it
     * predates this packet, like every other caller once did, and until a
     * packet that's allowed to edit `stores.ts` wires it through, it keeps
     * the same "team" behavior it always had rather than silently widening
     * to "all" or breaking on a now-missing argument. */
    mode?: FilterMode;
    /** Only events on watched threads — the Feed/Pull requests/Comments
     * "Watched" chip. */
    watchedOnly?: boolean;
    beforeId?: number | null;
    limit: number;
  }): Promise<Event[]> {
    return call<Event[]>("list_events", {
      repo_id: opts.repoId ?? null,
      actor: opts.actor ?? null,
      team_id: opts.teamId ?? null,
      mode: opts.mode ?? "team",
      watched_only: opts.watchedOnly ?? false,
      before_id: opts.beforeId ?? null,
      limit: opts.limit,
    });
  },
  markSeen(ids: number[]): Promise<void> {
    return call<void>("mark_seen", { ids });
  },
  unseenCount(): Promise<number> {
    return call<number>("unseen_count");
  },
  getThread(repoId: number, number: number): Promise<Thread> {
    return call<Thread>("get_thread", { repo_id: repoId, number });
  },
  getCommit(repoId: number, sha: string): Promise<CommitDetail> {
    return call<CommitDetail>("get_commit", { repo_id: repoId, sha });
  },
  /** The detail pane's Files tab (docs/CONTRACT.md "Reading in-app"). */
  getPullFiles(repoId: number, number: number): Promise<CommitFile[]> {
    return call<CommitFile[]>("get_pull_files", { repo_id: repoId, number });
  },

  listWatches(): Promise<Watch[]> {
    return call<Watch[]>("list_watches");
  },
  watchThread(repoId: number, number: number): Promise<Watch> {
    return call<Watch>("watch_thread", { repo_id: repoId, number });
  },
  unwatchThread(repoId: number, number: number): Promise<void> {
    return call<void>("unwatch_thread", { repo_id: repoId, number });
  },
  /** Unwatches every watch whose state is not `open`; returns how many. */
  clearClosedWatches(): Promise<number> {
    return call<number>("clear_closed_watches");
  },

  /** A period summary of stored events (docs/CONTRACT.md "Digests"). `start`
   * and `end` are unix seconds, half-open `[start, end)` — the Summary
   * view resolves its period control to these before calling. `tzOffsetSecs`
   * is `-new Date().getTimezoneOffset()*60` — how the engine buckets
   * `series`/`hours` into local days/hours. */
  digest(
    start: number,
    end: number,
    teamId: number | null | undefined,
    actor: string | null | undefined,
    tzOffsetSecs: number,
  ): Promise<Digest> {
    return call<Digest>("digest", {
      start,
      end,
      team_id: teamId ?? null,
      actor: actor ?? null,
      tz_offset_secs: tzOffsetSecs,
    });
  },
  /** Every currently-open pull request in scope (docs/CONTRACT.md "Pulls") —
   * a live snapshot, not scoped to any window. `actor` narrows to PRs they
   * authored, `reviewer` to PRs where they're a requested reviewer; both may
   * be given together with `teamId`. */
  openPulls(
    teamId?: number | null,
    actor?: string | null,
    reviewer?: string | null,
  ): Promise<OpenPull[]> {
    return call<OpenPull[]>("open_pulls", {
      team_id: teamId ?? null,
      actor: actor ?? null,
      reviewer: reviewer ?? null,
    });
  },

  getSettings(): Promise<Settings> {
    return call<Settings>("get_settings");
  },
  setSettings(settings: Settings): Promise<void> {
    return call<void>("set_settings", { settings });
  },
} as const;

export type FilterModeOption = FilterMode;
