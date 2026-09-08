<script lang="ts">
  // Settings (docs/design/Settings.dc.html): every control saves on
  // change, no Save button anywhere. GitHub account is sign-in-only
  // (docs/CONTRACT.md "Sign-in") — no token field anywhere in the app.
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import { isEnabled as autostartEnabled, enable as autostartEnable, disable as autostartDisable } from "@tauri-apps/plugin-autostart";
  import { check as checkForUpdate, type Update } from "@tauri-apps/plugin-updater";
  import { accountsStore, pollStore, reposStore, settingsStore } from "../lib/stores";
  import { isEngineError, engineErrorMessage, ALL_EVENT_KINDS, type EventKind } from "../lib/types";
  import { scroller } from "../lib/scroller";
  import Avatar from "../components/Avatar.svelte";
  import DeviceSignIn from "../components/DeviceSignIn.svelte";
  import UpdatePrompt from "../components/UpdatePrompt.svelte";

  let { onSignedOut }: { onSignedOut: () => void } = $props();

  const { settings } = settingsStore;
  const { items: accounts, signedOutLogins } = accountsStore;

  let pollIntervalSecs = $state(120);
  // No control writes this any more (packet gm-feed-r1): read once on mount
  // and carried unchanged through every `persist()` so the value survives —
  // the Feed's "My team / Everyone" row is what a fresh window seeds from it
  // now, and is where switching actually happens.
  let filterMode = $state<"all" | "team">("team");
  let notificationsEnabled = $state(true);
  let notifyKinds = $state<Set<EventKind>>(new Set(ALL_EVENT_KINDS));
  let quietEnabled = $state(false);
  let autoWatch = $state(true);
  let quietStart = $state("22:00");
  let quietEnd = $state("08:00");
  let openAtLogin = $state(false);

  // --- Accounts (docs/CONTRACT.md "Accounts") ---
  const { items: repos } = reposStore;
  /** "Sign out" (drop the token, keep the account and its repos) and "Remove
   * account" (delete the account, its repos and their history) are two
   * different actions on a row, one reversible and one not — hence the
   * separate confirm/busy state for each below. */
  let removeConfirmFor = $state<string | null>(null);
  let signingOutLogin = $state<string | null>(null);
  let removingLogin = $state<string | null>(null);
  let signingInLogin = $state<string | null>(null);
  let accountError = $state<string | null>(null);
  let addingAccount = $state(false);

  function repoCountFor(login: string): number {
    return $repos.filter((r) => r.account_login === login).length;
  }

  let saveError = $state<string | null>(null);
  let ready = false;

  // --- Updates (explicit "Check for updates…"; the quiet on-launch check
  // is App.svelte's own — this is only the manual path) ---
  let updateStatus = $state<"idle" | "checking" | "none" | "error">("idle");
  let foundUpdate = $state<Update | null>(null);

  async function checkForUpdates() {
    updateStatus = "checking";
    try {
      const update = await checkForUpdate();
      if (update) {
        foundUpdate = update;
        updateStatus = "idle";
      } else {
        updateStatus = "none";
      }
    } catch {
      updateStatus = "error";
    }
  }

  function dismissFoundUpdate() {
    foundUpdate = null;
  }

  const KIND_LABELS: Record<EventKind, string> = {
    commit: "Commits",
    pr_opened: "PRs opened",
    pr_merged: "PRs merged",
    pr_closed: "PRs closed",
    pr_reviewed: "Reviews",
    pr_commented: "PR comments",
    issue_opened: "Issues opened",
    issue_commented: "Issue comments",
  };

  function minuteToTime(m: number): string {
    const h = Math.floor(m / 60).toString().padStart(2, "0");
    const mi = (m % 60).toString().padStart(2, "0");
    return `${h}:${mi}`;
  }
  function timeToMinute(t: string): number {
    const [h, m] = t.split(":").map(Number);
    return (h || 0) * 60 + (m || 0);
  }

  onMount(async () => {
    await settingsStore.refresh();
    const s = $settings;
    pollIntervalSecs = s.poll_interval_secs;
    filterMode = s.filter_mode;
    notificationsEnabled = s.notifications_enabled;
    notifyKinds = new Set(s.notify_kinds);
    autoWatch = s.auto_watch;
    quietEnabled = s.quiet_hours !== null;
    if (s.quiet_hours) {
      quietStart = minuteToTime(s.quiet_hours.start_minute);
      quietEnd = minuteToTime(s.quiet_hours.end_minute);
    }
    try {
      await accountsStore.refresh();
    } catch {
      // The list stays empty; the section shows "No accounts signed in."
    }
    try {
      openAtLogin = await autostartEnabled();
    } catch {
      // autostart isn't available in every environment (e.g. some sandboxed
      // dev runs) — leave the toggle at its default rather than fail Settings.
    }
    ready = true;
  });

  async function persist() {
    if (!ready) return;
    saveError = null;
    try {
      await settingsStore.save({
        poll_interval_secs: Math.max(30, pollIntervalSecs),
        filter_mode: filterMode,
        notifications_enabled: notificationsEnabled,
        notify_kinds: [...notifyKinds],
        quiet_hours: quietEnabled ? { start_minute: timeToMinute(quietStart), end_minute: timeToMinute(quietEnd) } : null,
        auto_watch: autoWatch,
      });
    } catch (e) {
      saveError = isEngineError(e) ? engineErrorMessage(e) : "Couldn't save settings.";
    }
  }

  function toggleKind(k: EventKind) {
    const next = new Set(notifyKinds);
    if (next.has(k)) next.delete(k);
    else next.add(k);
    notifyKinds = next;
    persist();
  }

  async function toggleOpenAtLogin() {
    const next = !openAtLogin;
    openAtLogin = next;
    try {
      if (next) await autostartEnable();
      else await autostartDisable();
    } catch (e) {
      openAtLogin = !next;
      saveError = e instanceof Error ? e.message : "Couldn't change Open at login.";
    }
  }

  /** "Sign out" (docs/CONTRACT.md "Accounts"): drops the keychain item and
   * the engine's in-memory token for this account, but keeps its row and
   * repos — unlike "Remove account…" below. Never routes to Onboarding,
   * even for the last account: the row is still there to sign back in on.
   * Goes through `accountsStore.signOut` (not `api.signOutAccount`
   * directly) so its optimistic overlay is the one and only "signed out"
   * source both this view and the sidebar's account switcher read. */
  async function doSignOut(login: string) {
    signingOutLogin = login;
    accountError = null;
    try {
      await accountsStore.signOut(login);
    } catch (e) {
      accountError = isEngineError(e) ? engineErrorMessage(e) : "Couldn't sign out.";
    } finally {
      signingOutLogin = null;
    }
  }

  /** The row's inline re-sign-in (device flow) resolved: this account has a
   * token again, so `accountsStore.noteSignedIn` drops it from the shared
   * optimistic signed-out set and the repo list is refreshed so its "signed
   * out" `last_error` clears too. */
  async function handleReSignedIn(login: string) {
    signingInLogin = null;
    accountsStore.noteSignedIn(login);
    await accountsStore.refresh();
    await reposStore.refresh();
    pollStore.pollNow().catch(() => {
      // Best-effort: the scheduled poller will pick this account's repos
      // back up on its own next tick either way.
    });
  }

  /** "Remove account…" (docs/CONTRACT.md "Accounts"): `remove_account`
   * cascades — the account, its repos and their events and watches are all
   * gone, which is why this path alone gets a confirm sentence. Routes back
   * to Onboarding when that was the last account, the same way the old
   * single-account sign-out did. */
  async function confirmRemove(login: string) {
    removingLogin = login;
    accountError = null;
    try {
      await accountsStore.remove(login);
      await reposStore.refresh();
      if (get(accountsStore.items).length === 0) onSignedOut();
    } catch (e) {
      accountError = isEngineError(e) ? engineErrorMessage(e) : "Couldn't remove the account.";
    } finally {
      removingLogin = null;
      removeConfirmFor = null;
    }
  }

  /** The device flow (DeviceSignIn.svelte) already registered the account
   * and stored its token by the time this fires — nothing left to do but
   * refresh and poll its repos immediately (packet's "rescan after
   * sign-in"; `reposStore` too, in case this was the first account and it
   * just claimed some previously-unclaimed repos). */
  async function handleAccountAdded() {
    addingAccount = false;
    await accountsStore.refresh();
    await reposStore.refresh();
    pollStore.pollNow().catch(() => {
      // Best-effort: the scheduled poller will pick the new account's
      // repos up on its own next tick either way.
    });
  }
</script>

<div class="head">
  <h1>Settings</h1>
  <span class="muted">changes save as you go</span>
</div>

<div class="body" use:scroller>
  <div class="columns">
    <div class="col">
      <section class="field">
        <span class="label">Accounts</span>
        <div class="account-list">
          {#each $accounts as account (account.login)}
            <div class="account-row">
              <Avatar login={account.login} avatarUrl={account.avatar_url} size={24} />
              <span class="account-login">{account.login}</span>
              {#if $signedOutLogins.has(account.login)}
                <span class="tag-muted">signed out</span>
              {/if}
              <span class="spacer"></span>
              {#if $signedOutLogins.has(account.login)}
                <button class="btn-text" onclick={() => (signingInLogin = signingInLogin === account.login ? null : account.login)}>
                  Sign in
                </button>
              {:else}
                <button
                  class="btn-text"
                  onclick={() => doSignOut(account.login)}
                  disabled={signingOutLogin === account.login}
                >
                  {signingOutLogin === account.login ? "Signing out…" : "Sign out"}
                </button>
              {/if}
              <button
                class="btn-text danger subtle"
                onclick={() => (removeConfirmFor = removeConfirmFor === account.login ? null : account.login)}
              >
                Remove…
              </button>
            </div>
            {#if signingInLogin === account.login}
              <div class="row-detail">
                <DeviceSignIn onSignedIn={() => handleReSignedIn(account.login)} buttonLabel="Sign in" />
              </div>
            {/if}
            {#if removeConfirmFor === account.login}
              <div class="row-detail">
                <p class="hint">
                  Removes the account, its {repoCountFor(account.login)} repos and their history from Vigie. Tokens are deleted from the keychain.
                </p>
                <button
                  class="btn-text danger"
                  onclick={() => confirmRemove(account.login)}
                  disabled={removingLogin === account.login}
                >
                  {removingLogin === account.login ? "Removing…" : "Remove account"}
                </button>
                <button class="btn-text" onclick={() => (removeConfirmFor = null)}>Cancel</button>
              </div>
            {/if}
          {:else}
            <span class="muted">No accounts signed in.</span>
          {/each}
        </div>
        {#if accountError}<p class="error-text">{accountError}</p>{/if}

        {#if addingAccount}
          <DeviceSignIn onSignedIn={handleAccountAdded} buttonLabel="Add account" />
        {:else}
          <button class="btn-text" onclick={() => (addingAccount = true)}>+ Add account</button>
        {/if}
        <p class="hint">Sign-in is stored in your Keychain. Nothing leaves this Mac except calls to GitHub.</p>
      </section>

      <section class="field">
        <span class="label">Poll every</span>
        <div class="row">
          <input type="number" min="30" bind:value={pollIntervalSecs} onchange={persist} style="width:80px" />
          <span>seconds</span>
        </div>
      </section>

      <section class="field">
        <span class="label">Show activity from</span>
        <p class="hint">
          Moved to the Feed's filter row as "My team / Everyone" — it changes what you see right
          away now, rather than what gets stored.
        </p>
      </section>

      <div class="switch-row">
        <span class="switch-label">Open at login</span>
        <button
          class="switch"
          class:on={openAtLogin}
          onclick={toggleOpenAtLogin}
          role="switch"
          aria-checked={openAtLogin}
          aria-label="Open at login"
        >
          <span class="knob"></span>
        </button>
      </div>

      <div class="field">
        <div class="switch-row">
          <span class="switch-label">Auto-watch my PRs</span>
          <button
            class="switch"
            class:on={autoWatch}
            onclick={() => {
              autoWatch = !autoWatch;
              persist();
            }}
            role="switch"
            aria-checked={autoWatch}
            aria-label="Auto-watch my PRs"
          >
            <span class="knob"></span>
          </button>
        </div>
        <p class="hint">Watches PRs you open and PRs you're asked to review.</p>
      </div>
    </div>

    <div class="col">
      <div class="switch-row">
        <span class="switch-label">Notifications</span>
        <button
          class="switch"
          class:on={notificationsEnabled}
          onclick={() => {
            notificationsEnabled = !notificationsEnabled;
            persist();
          }}
          role="switch"
          aria-checked={notificationsEnabled}
          aria-label="Notifications"
        >
          <span class="knob"></span>
        </button>
      </div>

      <section class="field" class:disabled={!notificationsEnabled}>
        <span class="label">Notify me about</span>
        <div class="kind-grid">
          {#each ALL_EVENT_KINDS as k (k)}
            <label class="checkbox-row">
              <input type="checkbox" checked={notifyKinds.has(k)} onchange={() => toggleKind(k)} disabled={!notificationsEnabled} />
              <span>{KIND_LABELS[k]}</span>
            </label>
          {/each}
        </div>
      </section>

      <section class="field" class:disabled={!notificationsEnabled}>
        <span class="label">Quiet hours</span>
        <div class="row">
          <input type="time" bind:value={quietStart} disabled={!notificationsEnabled} onchange={persist} />
          <span class="muted">to</span>
          <input type="time" bind:value={quietEnd} disabled={!notificationsEnabled} onchange={persist} />
        </div>
        <label class="checkbox-row">
          <input type="checkbox" bind:checked={quietEnabled} disabled={!notificationsEnabled} onchange={persist} />
          <span>Enabled</span>
        </label>
      </section>
    </div>
  </div>

  {#if saveError}<p class="error-text">{saveError}</p>{/if}

  <section class="field">
    <span class="label">Updates</span>
    {#if foundUpdate}
      <div class="update-embed">
        <UpdatePrompt update={foundUpdate} onDismiss={dismissFoundUpdate} />
      </div>
    {:else}
      <div class="row">
        <button class="btn-text" onclick={checkForUpdates} disabled={updateStatus === "checking"}>
          {updateStatus === "checking" ? "Checking…" : "Check for updates…"}
        </button>
        {#if updateStatus === "none"}<span class="muted">You're up to date.</span>{/if}
        {#if updateStatus === "error"}<span class="error-text">Couldn't check for updates.</span>{/if}
      </div>
    {/if}
  </section>

  <div class="footer muted">Vigie 0.1 · pronounced vee-zhee · French for the lookout in a ship's crow's nest</div>
</div>

<style>
  .head {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 12px;
    padding: 18px 24px 12px 24px;
  }
  .head h1 {
    font-size: 17px;
    font-weight: 600;
    margin: 0;
  }
  .body {
    padding: 0 24px 24px 24px;
    overflow-y: auto;
    flex-grow: 1;
    display: flex;
    flex-direction: column;
  }
  .columns {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 28px;
    padding-top: 4px;
    max-width: 720px;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .field.disabled {
    opacity: 0.55;
  }
  .label {
    font-size: 12px;
    color: var(--label);
    font-weight: 500;
  }
  .hint {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
  }
  .row {
    display: flex;
    gap: 10px;
    align-items: center;
  }
  .account-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .account-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 0;
  }
  .account-login {
    font-size: 13px;
    font-weight: 500;
  }
  .spacer {
    flex-grow: 1;
  }
  .danger {
    color: var(--danger);
  }
  .subtle {
    opacity: 0.55;
  }
  .subtle:hover {
    opacity: 1;
  }
  .tag-muted {
    font-size: 11px;
    font-weight: 500;
    color: var(--muted);
    background: var(--checkbox-border-off);
    border-radius: 4px;
    padding: 1px 6px;
  }
  .row-detail {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    padding: 2px 0 8px 34px;
  }
  .checkbox-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    cursor: pointer;
  }
  .kind-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 6px 16px;
  }
  .switch-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .switch-label {
    font-size: 13px;
    font-weight: 500;
  }
  .switch {
    width: 34px;
    height: 20px;
    border-radius: 10px;
    background: var(--checkbox-border-off);
    position: relative;
    border: none;
    cursor: pointer;
    flex-shrink: 0;
    padding: 0;
  }
  .switch.on {
    background: var(--accent);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 8px;
    background: var(--surface-card);
    display: block;
    transition: left 0.12s ease;
  }
  .switch.on .knob {
    left: 16px;
  }
  .footer {
    margin-top: auto;
    padding-top: 16px;
  }
  /* UpdatePrompt.svelte is styled as a full-bleed banner (it's also used
     that way at the top of App.svelte); embedded in a field here instead,
     it gets its own rounded, bordered box rather than bleeding to the
     window's edges. */
  .update-embed {
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
</style>
