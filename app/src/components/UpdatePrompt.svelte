<script lang="ts">
  // Shown wherever an `Update` was already found — App.svelte's quiet check
  // on launch, and Settings.svelte's explicit "Check for updates…" button,
  // both call `check()` from `@tauri-apps/plugin-updater` themselves and
  // hand the result here; this component only ever asks what to do with an
  // update it's already been given; it never checks on its own.
  import { relaunch } from "@tauri-apps/plugin-process";
  import type { Update } from "@tauri-apps/plugin-updater";

  let { update, onDismiss }: { update: Update; onDismiss: () => void } = $props();

  let installing = $state(false);
  let downloadedBytes = $state(0);
  let totalBytes = $state<number | null>(null);
  let error = $state<string | null>(null);

  let percent = $derived(totalBytes ? Math.min(100, Math.round((downloadedBytes / totalBytes) * 100)) : null);

  // "Never install without the user's say-so": this only ever runs from the
  // Install button below. Downloads, then installs, then relaunches so the
  // new version is what's actually running — matching every other Tauri
  // updater flow (macOS needs the relaunch; nothing here plays that ready
  // to skip on the theory a given install did the swap in place).
  async function install() {
    installing = true;
    error = null;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          downloadedBytes = 0;
          totalBytes = event.data.contentLength ?? null;
        } else if (event.event === "Progress") {
          downloadedBytes += event.data.chunkLength;
        }
      });
      await relaunch();
    } catch (e) {
      installing = false;
      error = e instanceof Error ? e.message : "Couldn't install the update.";
    }
  }

  function dismiss() {
    // `Update` holds a Rust-side resource handle (it extends `Resource`);
    // closing it here (best-effort — nothing to do if it's already gone)
    // frees that handle rather than leaving it for garbage collection to
    // eventually reclaim.
    update.close().catch(() => {});
    onDismiss();
  }
</script>

<div class="update-card" role="status">
  <div class="update-row">
    <div class="update-text">
      <span class="update-title">Vigie {update.version} is available</span>
      {#if update.body}<p class="update-notes">{update.body}</p>{/if}
    </div>
    {#if installing}
      <span class="update-status">{percent !== null ? `Downloading… ${percent}%` : "Downloading…"}</span>
    {:else}
      <div class="update-actions">
        <button class="update-btn" onclick={install}>Install and relaunch</button>
        <button class="update-dismiss" onclick={dismiss} aria-label="Dismiss">&times;</button>
      </div>
    {/if}
  </div>
  {#if error}<p class="update-error">{error}</p>{/if}
</div>

<style>
  .update-card {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 10px 16px;
    background: var(--surface-sunken);
    border-bottom: 1px solid var(--border);
    color: var(--text);
    font-size: 12px;
  }
  .update-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .update-text {
    flex-grow: 1;
    min-width: 0;
  }
  .update-title {
    font-weight: 500;
  }
  .update-notes {
    margin: 2px 0 0;
    color: var(--muted);
    white-space: pre-wrap;
  }
  .update-status {
    color: var(--muted);
    flex-shrink: 0;
  }
  .update-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }
  .update-btn {
    border: none;
    background: var(--accent);
    color: #fff;
    font-size: 12px;
    font-weight: 500;
    font-family: inherit;
    padding: 5px 12px;
    border-radius: 6px;
    cursor: pointer;
  }
  .update-btn:hover {
    background: var(--accent-strong);
  }
  .update-dismiss {
    border: 0;
    background: none;
    color: var(--muted);
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
    padding: 0 2px;
    border-radius: 4px;
    transition: background 0.12s, color 0.12s;
  }
  .update-dismiss:hover {
    color: var(--text);
    background: var(--surface-active);
  }
  .update-error {
    margin: 0;
    color: var(--danger);
  }
</style>
