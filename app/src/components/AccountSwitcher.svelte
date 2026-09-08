<script lang="ts">
  // Bottom-of-sidebar account switcher: stacked avatars for who's signed in,
  // opening a menu (same look as People.svelte's "⋯" card menu) that lists
  // every account with its state and lets you sign one in or out, add
  // another, or jump to Settings. Every account is always active — this is
  // about *seeing* who's signed in and acting on it quickly, not a filter
  // (a per-account "show activity from" filter is a separate packet).
  import { onMount, tick } from "svelte";
  import { accountsStore, pollStore, reposStore } from "../lib/stores";
  import { scroller } from "../lib/scroller";
  import type { ViewName } from "./Sidebar.svelte";
  import Avatar from "./Avatar.svelte";
  import DeviceSignIn from "./DeviceSignIn.svelte";
  import Icon from "./Icon.svelte";

  let { onNavigate }: { onNavigate: (v: ViewName) => void } = $props();

  const { items: accounts, signedOutLogins } = accountsStore;

  const STACK_MAX = 3;

  let open = $state(false);
  /** Which row's inline re-sign-in (DeviceSignIn) is expanded, or none. */
  let signingInFor = $state<string | null>(null);
  let signingOutFor = $state<string | null>(null);
  let addingAccount = $state(false);
  let menuError = $state<string | null>(null);

  let wrapperEl: HTMLDivElement | undefined = $state();
  let menuEl: HTMLDivElement | undefined = $state();
  let triggerEl: HTMLButtonElement | undefined = $state();

  let visibleAccounts = $derived($accounts.slice(0, STACK_MAX));
  let overflowCount = $derived(Math.max(0, $accounts.length - STACK_MAX));

  /** Collapsed-trigger label (Work item 3): recognisable as an account
   * switcher without opening it. Two lines — a main line ("N accounts", or
   * the login itself when there's exactly one) and, only when it says
   * something the main line doesn't already, a muted sub-line for the
   * signed-out count. */
  let signedOutCount = $derived($accounts.filter((a) => $signedOutLogins.has(a.login)).length);
  let triggerMain = $derived(
    $accounts.length === 0
      ? "Accounts"
      : $accounts.length === 1
        ? $accounts[0].login
        : `${$accounts.length} accounts`,
  );
  let triggerSub = $derived(
    signedOutCount === 0 ? null : $accounts.length === 1 ? "Signed out" : `${signedOutCount} signed out`,
  );

  onMount(() => {
    // Sidebar (and this switcher with it) is mounted before anyone
    // necessarily visits Settings or Repos, the two views that otherwise
    // populate this store — so it needs its own fetch.
    accountsStore.refresh().catch(() => {
      // The trigger just shows nothing until a later refresh succeeds.
    });
  });

  async function openMenu() {
    open = true;
    menuError = null;
    await tick();
    menuEl?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
  }

  function closeMenu() {
    open = false;
    signingInFor = null;
    addingAccount = false;
  }

  function toggleMenu() {
    if (open) closeMenu();
    else openMenu();
  }

  /** Only ArrowDown opens from the trigger here — Enter/Space are left to
   * the button's own native activation (that already calls `toggleMenu` via
   * `onclick`; handling them here too used to open the menu on keydown and
   * then have the native Space-triggered click toggle it shut again). */
  function handleTriggerKeydown(e: KeyboardEvent) {
    if (!open && e.key === "ArrowDown") {
      e.preventDefault();
      openMenu();
    }
  }

  /** Active only while the menu is open: click-outside and Esc/arrow-key
   * navigation across every focusable row in the popover (rows themselves,
   * plus whatever a DeviceSignIn flow expanded inline).
   *
   * `handleKeydown` is registered on `window` in the *capture* phase (the
   * `true` below) and calls `stopPropagation` for every key it handles —
   * App.svelte's own `window` keydown listener (Esc closes/steps back the
   * detail pane, ArrowUp/Down step it) sits on the same target in the
   * (default) bubble phase, so without both of these this menu's Esc/arrow
   * keys used to leak straight through to it. Capture always runs before
   * bubble on a shared target regardless of registration order, so this
   * runs — and can stop the event — before App's handler ever sees it. */
  $effect(() => {
    if (!open) return;

    function handlePointerDown(e: PointerEvent) {
      if (wrapperEl && !wrapperEl.contains(e.target as Node)) closeMenu();
    }
    function handleKeydown(e: KeyboardEvent) {
      switch (e.key) {
        case "Escape":
          e.preventDefault();
          e.stopPropagation();
          closeMenu();
          triggerEl?.focus();
          break;
        case "ArrowDown":
          e.preventDefault();
          e.stopPropagation();
          moveFocus(1);
          break;
        case "ArrowUp":
          e.preventDefault();
          e.stopPropagation();
          moveFocus(-1);
          break;
        case "Home":
          e.preventDefault();
          e.stopPropagation();
          focusEdge("first");
          break;
        case "End":
          e.preventDefault();
          e.stopPropagation();
          focusEdge("last");
          break;
        case "Enter":
          // preventDefault here also cancels the focused button's own
          // native "Enter activates" default action, so this triggers that
          // activation itself via a plain `.click()` rather than losing it.
          e.preventDefault();
          e.stopPropagation();
          (document.activeElement as HTMLElement | null)?.click();
          break;
      }
    }

    window.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeydown, true);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeydown, true);
    };
  });

  function focusableButtons(): HTMLButtonElement[] {
    if (!menuEl) return [];
    return Array.from(menuEl.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
  }

  function moveFocus(delta: number) {
    const focusables = focusableButtons();
    if (focusables.length === 0) return;
    const current = focusables.indexOf(document.activeElement as HTMLButtonElement);
    const next = current === -1 ? 0 : (current + delta + focusables.length) % focusables.length;
    focusables[next].focus();
  }

  function focusEdge(edge: "first" | "last") {
    const focusables = focusableButtons();
    if (focusables.length === 0) return;
    focusables[edge === "first" ? 0 : focusables.length - 1].focus();
  }

  function toggleSignIn(login: string) {
    signingInFor = signingInFor === login ? null : login;
  }

  async function handleSignOut(login: string) {
    signingOutFor = login;
    menuError = null;
    try {
      await accountsStore.signOut(login);
    } catch {
      menuError = "Couldn't sign out.";
    } finally {
      signingOutFor = null;
    }
  }

  /** Fires once a row's inline DeviceSignIn resolves (re-signing an
   * existing account back in) — the token is already stored by then, same
   * as Settings' `handleReSignedIn`. */
  async function handleSignedIn(login: string) {
    signingInFor = null;
    accountsStore.noteSignedIn(login);
    await accountsStore.refresh();
    await reposStore.refresh();
    pollStore.pollNow().catch(() => {
      // Best-effort: the scheduled poller picks this account's repos back
      // up on its own next tick either way.
    });
  }

  /** Fires once "Add account…"'s DeviceSignIn resolves — mirrors Settings'
   * `handleAccountAdded`. */
  async function handleAccountAdded() {
    addingAccount = false;
    await accountsStore.refresh();
    await reposStore.refresh();
    pollStore.pollNow().catch(() => {
      // Best-effort, as above.
    });
  }

  function goToSettings() {
    closeMenu();
    onNavigate("settings");
  }
</script>

<div class="switcher" bind:this={wrapperEl}>
  {#if open}
    <div class="menu" role="menu" aria-label="Accounts" bind:this={menuEl} use:scroller>
      {#each $accounts as account (account.login)}
        <div class="row">
          <Avatar login={account.login} avatarUrl={account.avatar_url} size={28} />
          <div class="row-main">
            <span class="row-login">{account.login}</span>
            <span class="row-state">{$signedOutLogins.has(account.login) ? "Signed out" : "Signed in"}</span>
          </div>
          {#if $signedOutLogins.has(account.login)}
            <button class="btn-text row-action" role="menuitem" onclick={() => toggleSignIn(account.login)}>
              Sign in
            </button>
          {:else}
            <button
              class="btn-text row-action"
              role="menuitem"
              onclick={() => handleSignOut(account.login)}
              disabled={signingOutFor === account.login}
            >
              {signingOutFor === account.login ? "Signing out…" : "Sign out"}
            </button>
          {/if}
        </div>
        {#if signingInFor === account.login}
          <div class="row-detail">
            <DeviceSignIn onSignedIn={() => handleSignedIn(account.login)} buttonLabel="Sign in" />
          </div>
        {/if}
      {:else}
        <span class="empty muted">No accounts signed in.</span>
      {/each}

      {#if menuError}<p class="error-text">{menuError}</p>{/if}

      <div class="divider"></div>
      {#if addingAccount}
        <div class="row-detail">
          <DeviceSignIn onSignedIn={handleAccountAdded} buttonLabel="Add account" />
        </div>
      {:else}
        <button class="menu-item" role="menuitem" onclick={() => (addingAccount = true)}>Add account…</button>
      {/if}
      <button class="menu-item" role="menuitem" onclick={goToSettings}>Manage accounts…</button>
    </div>
  {/if}

  <button
    class="trigger"
    type="button"
    title="Accounts"
    aria-haspopup="menu"
    aria-expanded={open}
    bind:this={triggerEl}
    onclick={toggleMenu}
    onkeydown={handleTriggerKeydown}
  >
    <span class="avatar-stack">
      {#each visibleAccounts as account (account.login)}
        <span class="stack-slot" class:signed-out={$signedOutLogins.has(account.login)}>
          <Avatar login={account.login} avatarUrl={account.avatar_url} size={22} />
          {#if $signedOutLogins.has(account.login)}<span class="dot"></span>{/if}
        </span>
      {/each}
      {#if overflowCount > 0}
        <span class="stack-slot stack-more">+{overflowCount}</span>
      {/if}
    </span>
    <span class="trigger-label">
      <span class="trigger-main">{triggerMain}</span>
      {#if triggerSub}<span class="trigger-sub">{triggerSub}</span>{/if}
    </span>
    <Icon name="chevron_up" size={12} />
  </button>
</div>

<style>
  .switcher {
    position: relative;
    /* Sits at the bottom of the sidebar, just above `.side-foot` (see
       Sidebar.svelte — its own `margin-top: auto` moved here so the two sit
       flush together instead of splitting the leftover flex space). */
    margin-top: auto;
  }
  .trigger {
    /* Reads like any other nav item on the light column — `--text` for the
       label, `--surface-active` on hover/open, same as `.nav` in
       Sidebar.svelte. */
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 10px;
    border-radius: 6px;
    border: none;
    background: none;
    cursor: pointer;
    font-family: inherit;
    color: var(--text);
    text-align: left;
    transition: background 0.12s;
  }
  .trigger:hover,
  .trigger[aria-expanded="true"] {
    background: var(--surface-active);
  }
  .avatar-stack {
    display: flex;
    align-items: center;
    flex-shrink: 0;
  }
  .stack-slot {
    position: relative;
    display: inline-flex;
  }
  .stack-slot + .stack-slot {
    margin-left: -6px;
  }
  .stack-slot.signed-out :global(img),
  .stack-slot.signed-out :global(.initials) {
    opacity: 0.45;
  }
  .dot {
    position: absolute;
    right: -1px;
    bottom: -1px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
    /* Matches the sidebar's own background so the dot reads as cut into
       the avatar rather than pasted on top of it. */
    border: 1.5px solid var(--surface-sunken);
  }
  .stack-more {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: var(--surface-active);
    color: var(--muted);
    font-size: 10px;
    font-weight: 600;
  }
  .trigger-label {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    line-height: 1.3;
  }
  .trigger-main {
    font-size: 12px;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .trigger-sub {
    font-size: 11px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .trigger :global(svg) {
    flex-shrink: 0;
    opacity: 0.7;
  }

  .menu {
    /* Wide enough that a typical GitHub login (~20 chars) sits on one line
       next to "Sign out" — deliberately wider than the sidebar itself
       (200px), so this floats out over the content pane like any other
       popover; `left: 0` (no `right`) lets it shrink-to-fit within
       min/max instead of stretching to the switcher's own width. */
    position: absolute;
    bottom: 100%;
    left: 0;
    min-width: 280px;
    max-width: 340px;
    margin-bottom: 6px;
    z-index: 20;
    display: flex;
    flex-direction: column;
    padding: 4px;
    background: var(--surface-card);
    border: 1px solid var(--border-card);
    border-radius: 8px;
    box-shadow: 0 4px 16px rgb(0 0 0 / 0.12);
    max-height: 70vh;
    overflow-y: auto;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
  }
  .row-main {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    line-height: 1.3;
  }
  .row-login {
    font-size: 12px;
    font-weight: 600;
    /* The full login must be readable — wrap it rather than truncate, and
       only fall back to an ellipsis if it still doesn't fit in two lines. */
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .row-state {
    font-size: 11px;
    color: var(--muted);
  }
  .row-action {
    flex: none;
    white-space: nowrap;
  }
  .empty {
    padding: 6px 8px;
    font-size: 12px;
  }
  .row-detail {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    padding: 2px 8px 8px 42px;
  }
  .divider {
    height: 1px;
    background: var(--divider);
    margin: 4px 6px;
  }
  .menu-item {
    text-align: left;
    background: none;
    border: none;
    font-size: 12px;
    color: var(--text);
    padding: 6px 8px;
    border-radius: 5px;
    cursor: pointer;
    font-family: inherit;
  }
  .menu-item:hover {
    background: var(--surface-active);
  }
</style>
