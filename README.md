# Vigie

Pronounced *vee-zhee*. French for the lookout in a ship's crow's nest: the one whose
only job is to watch and call out what's coming.

Vigie is a native macOS app that watches GitHub repositories for what your team is
doing — commits, pull requests, reviews, issues and comments — in a window and in
your menu bar, with notifications when you want them. No local clone required, no
account, no server: your token talks to GitHub and everything is stored on your Mac.

**[Download for macOS](https://spacegrowth.github.io/vigie/)** · Apple silicon ·
macOS 13 or later · signed and notarized.

## Installing

1. Download the `.dmg` from the [website](https://spacegrowth.github.io/vigie/).
2. Open it and drag **Vigie** to your Applications folder.
3. Launch it. It is signed with a Developer ID and notarized by Apple, so it opens
   without a warning and without right-click-Open tricks.

Updates install themselves. Vigie checks quietly on launch, and there is an explicit
**Check for updates…** in Settings. When one is found it tells you the version and
installs it on relaunch, only after you say so. Every update is signed; anything that
does not verify is refused.

Uninstalling is dragging it to the Trash. Its data lives in
`~/Library/Application Support/dev.vigie.app` and its GitHub tokens in your login
keychain; delete both if you want it gone completely.

## Setting it up

1. **Sign in.** Vigie uses GitHub's device flow: it shows a code, you paste it into
   your browser once. The token is stored in your keychain, never on disk in the
   clear. Several accounts are supported — personal and work at the same time —
   each with its own credentials.
2. **Add repositories.** Paste `owner/name` or a GitHub URL in *Repos*. Adding one
   fetches its last seven days so it is not empty while you wait.
3. **Optionally define teams** in *Teams*, so you can filter the feed to the people
   you work with rather than everyone in a busy repository.

Polling is cached with ETags, so unchanged repositories cost nothing against
GitHub's hourly allowance, and the status bar shows what is left of it.

## Building from source

Requires Rust, Node 20+, and Xcode command line tools.

```
cd app && npm install
npm run tauri dev     # run it
npm run tauri build   # build it
```

A signed, notarized release — DMG plus the update feed — is `bash scripts/release.sh`;
see [`docs/RELEASING.md`](docs/RELEASING.md) for what it needs.

## How it is put together

A Rust engine (`crates/gitmon`) does the polling and stores events in SQLite, behind a
Tauri 2 shell with a Svelte 5 interface (`app/`). [`docs/CONTRACT.md`](docs/CONTRACT.md)
is the interface both sides code against; `docs/design/` holds the agreed screens.

## Licence

Not yet chosen — the repository is public to read, but no licence is granted until
one is added here.
