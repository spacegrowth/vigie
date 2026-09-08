<script lang="ts">
  // Avatar image, or an initials circle in a colour derived from the login
  // when there's no avatar_url (or it fails to load).
  let { login, avatarUrl = null, size = 24 }: { login: string; avatarUrl?: string | null; size?: number } = $props();

  // Identity colours as CSS custom properties (app.css), per the bloat
  // budget's "use nothing else for colour" — matches docs/design/*.dc.html's
  // five people (priya/marcus/lena/tom/aiko) exactly for the first five.
  const PALETTE = [
    "var(--id-1)",
    "var(--id-2)",
    "var(--id-3)",
    "var(--id-4)",
    "var(--id-5)",
    "var(--id-6)",
    "var(--id-7)",
    "var(--id-8)",
  ];

  function colorFor(name: string): string {
    let hash = 0;
    for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
    return PALETTE[hash % PALETTE.length];
  }

  let initial = $derived(login ? login[0]!.toUpperCase() : "?");
  let bg = $derived(colorFor(login || "?"));
  let failed = $state(false);
</script>

{#if avatarUrl && !failed}
  <img
    src={avatarUrl}
    alt={login}
    width={size}
    height={size}
    style="width:{size}px;height:{size}px"
    onerror={() => (failed = true)}
  />
{:else}
  <div
    class="initials"
    style="width:{size}px;height:{size}px;background:{bg};font-size:{Math.round(size * 0.45)}px"
    title={login}
  >
    {initial}
  </div>
{/if}

<style>
  img {
    border-radius: 50%;
    object-fit: cover;
    flex-shrink: 0;
    display: block;
  }
  .initials {
    border-radius: 50%;
    color: var(--id-fg);
    font-weight: 600;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    user-select: none;
  }
</style>
