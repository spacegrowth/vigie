<script lang="ts">
  // GitHub OAuth device flow UI (docs/CONTRACT.md "Sign-in"), shared by
  // Onboarding's first sign-in and Settings' "Add account" — the packet's
  // "sign-in and 'Add account' are the same flow", including the code-box
  // UI itself: this is the one place it's built, so the two never drift.
  import { onDestroy, onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { api } from "../lib/api";
  import { isEngineError, engineErrorMessage, type DeviceLogin } from "../lib/types";
  import Icon from "./Icon.svelte";

  let {
    onSignedIn,
    buttonLabel = "Sign in with GitHub",
  }: {
    /** Called once with the login the device flow resolved to — after
     * `poll_device_login` succeeds, the account is already registered and
     * its token already stored (commands.rs `add_account_and_store`), so
     * there is nothing left for the caller to do but react. */
    onSignedIn: (login: string) => void;
    buttonLabel?: string;
  } = $props();

  let clientIdChecked = $state(false);
  let clientIdReady = $state(false);
  let starting = $state(false);
  let deviceLogin = $state<DeviceLogin | null>(null);
  let signedInLogin = $state<string | null>(null);
  let authError = $state<string | null>(null);
  let copied = $state(false);

  let cancelled = false;
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  // The keychain save failure from `poll_device_login` (commands.rs): the
  // sign-in succeeds for the session, this explains what won't survive it.
  let saveWarning = $state<string | null>(null);
  let unlistenTokenSaveFailed: (() => void) | undefined;
  // Set when the in-app GitHub window navigated to a passkey page and
  // handed it to the default browser (commands.rs `passkey-handoff`).
  // WKWebView exposes the WebAuthn API but has no authenticator behind it,
  // so a passkey can only be finished in a real browser.
  let passkeyHandoff = $state(false);
  let unlistenPasskeyHandoff: (() => void) | undefined;
  let openingBrowser = $state(false);

  onMount(async () => {
    unlistenTokenSaveFailed = await listen<{ message: string }>("token-save-failed", (event) => {
      saveWarning = event.payload.message;
    });
    unlistenPasskeyHandoff = await listen("passkey-handoff", () => {
      passkeyHandoff = true;
    });
    try {
      clientIdReady = await api.githubClientIdReady();
    } catch {
      clientIdReady = false;
    } finally {
      clientIdChecked = true;
    }
  });

  onDestroy(() => {
    cancelled = true;
    if (pollTimer) clearTimeout(pollTimer);
    unlistenTokenSaveFailed?.();
    unlistenPasskeyHandoff?.();
  });

  /** A non-EngineError rejection (a Tauri IPC string, a JS Error, …) — the
   * raw text is shown so failures are diagnosable instead of generic. */
  function rawErrorText(e: unknown): string {
    if (typeof e === "string") return e;
    if (e instanceof Error) return e.message;
    try {
      return JSON.stringify(e);
    } catch {
      return String(e);
    }
  }

  async function startSignIn() {
    if (starting || deviceLogin) return;
    starting = true;
    authError = null;
    passkeyHandoff = false;
    try {
      const login = await api.startDeviceLogin();
      deviceLogin = login;
      await api.openGithubSigninWindow(verificationUrl(login));
      schedulePoll(login.device_code, login.interval);
    } catch (e) {
      deviceLogin = null;
      authError = isEngineError(e)
        ? engineErrorMessage(e)
        : `Couldn't start sign-in: ${rawErrorText(e)}`;
    } finally {
      starting = false;
    }
  }

  function schedulePoll(deviceCode: string, intervalSecs: number) {
    pollTimer = setTimeout(() => void pollOnce(deviceCode, intervalSecs), intervalSecs * 1000);
  }

  async function pollOnce(deviceCode: string, intervalSecs: number) {
    if (cancelled) return;
    try {
      const status = await api.pollDeviceLogin(deviceCode);
      if (cancelled) return;
      if (status.status === "ok") {
        deviceLogin = null;
        // The engine resolved and registered this account as part of the
        // flow (commands.rs `poll_device_login` -> `add_account_and_store`),
        // so there is nothing to save app-side — just tell the caller.
        signedInLogin = status.login;
        onSignedIn(status.login);
      } else {
        // "pending" covers both authorization_pending and slow_down (see
        // docs/CONTRACT.md "Sign-in") — nothing in the returned status
        // tells them apart, so this always waits the flow's own
        // `interval` rather than the slow_down+5 the contract mentions.
        schedulePoll(deviceCode, intervalSecs);
      }
    } catch (e) {
      if (cancelled) return;
      authError = isEngineError(e) ? engineErrorMessage(e) : `Sign-in failed: ${rawErrorText(e)}`;
    }
  }

  /** The verification URL for the current device code — what both the
   * in-app window and the browser fallback open. */
  function verificationUrl(login: DeviceLogin): string {
    return login.verification_uri_complete ?? login.verification_uri;
  }

  /** "Use Safari instead": hands the same URL to the default browser. The
   * poll loop keeps running untouched — it is watching GitHub for the
   * approval, so whichever window the user finishes in, the next poll sees
   * it and the in-app window is closed from the Rust side. */
  async function openInBrowser() {
    if (!deviceLogin || openingBrowser) return;
    openingBrowser = true;
    try {
      await api.openSigninInBrowser(verificationUrl(deviceLogin));
    } catch (e) {
      authError = isEngineError(e)
        ? engineErrorMessage(e)
        : `Couldn't open your browser: ${rawErrorText(e)}`;
    } finally {
      openingBrowser = false;
    }
  }

  function retrySignIn() {
    authError = null;
    deviceLogin = null;
    startSignIn();
  }

  async function copyCode() {
    if (!deviceLogin) return;
    try {
      await navigator.clipboard.writeText(deviceLogin.user_code);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      // Clipboard access can be denied in some sandboxes; the code is
      // still shown on screen to copy by hand either way.
    }
  }
</script>

{#if signedInLogin}
  <p class="ok-text">✓ signed in as {signedInLogin}</p>
  {#if saveWarning}
    <p class="error-text">Signed in for this session only — couldn't save the token to the keychain ({saveWarning}). You'll need to sign in again next launch.</p>
  {/if}
{:else if deviceLogin}
  <div class="device-code">
    <span class="muted">Enter this code on GitHub</span>
    <div class="code-row">
      <span class="code">{deviceLogin.user_code}</span>
      <button class="btn-text" onclick={copyCode}>{copied ? "Copied" : "Copy code"}</button>
      <button class="btn-text" onclick={openInBrowser} disabled={openingBrowser}>Use Safari instead</button>
    </div>
    {#if passkeyHandoff}
      <p class="hint">Passkeys need Safari; finish signing in there.</p>
    {/if}
    {#if authError}
      <p class="error-text">{authError}</p>
      <button class="btn-primary" onclick={retrySignIn}>Try again</button>
    {:else}
      <span class="muted">Waiting for approval…</span>
    {/if}
  </div>
{:else}
  <button class="btn-primary btn-github" onclick={startSignIn} disabled={starting || (clientIdChecked && !clientIdReady)}>
    <Icon name="github" size={14} />
    {starting ? "Starting…" : buttonLabel}
  </button>
  {#if clientIdChecked && !clientIdReady}
    <p class="error-text">Build is missing a GitHub client ID.</p>
  {:else}
    <p class="hint">Opens a Vigie window, not your browser. Nothing to paste or copy.</p>
  {/if}
  {#if authError}
    <p class="error-text">{authError}</p>
  {/if}
{/if}

<style>
  .btn-github {
    align-self: flex-start;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .hint {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
  }
  .device-code {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
  }
  .code-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .code {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 22px;
    font-weight: 600;
    letter-spacing: 0.08em;
  }
</style>
