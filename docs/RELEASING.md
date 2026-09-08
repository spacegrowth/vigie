# Releasing Vigie

How to cut a release, and how updates reach installs that already exist.

## How auto-update works

Vigie ships with [`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/),
pointed at a static feed:

```
https://spacegrowth.github.io/vigie/latest.json
```

served by GitHub Pages out of this repo's `docs/` folder (same shape as the
maintainer's other app, Jay, whose `site/appcast.xml` does the equivalent job
for Sparkle). `docs/index.html` is a plain landing page that reads the same
feed to fill in its download link — release day never has to touch it.

The app checks that feed quietly on launch (App.svelte) and again whenever
someone picks Settings → **Check for updates…**. A machine offline, or simply
nothing new, looks identical either way — no dialog, no error. Finding an
update shows a small prompt (version, release notes, Install/Dismiss); only
clicking Install downloads, installs, and relaunches into the new version.
Nothing here ever installs without that click.

**These URLs only resolve once `spacegrowth/vigie` exists as this repo's
remote and GitHub Pages is enabled on it (Settings → Pages → deploy from the
`main` branch's `/site` folder, or a small Pages Actions workflow — either
works, since `docs/` is already a complete static site).** Until then,
`docs/latest.json` sits in the repo unpublished and every install stays on
whatever version it already has.

## The signing key

Updates are signed with a minisign keypair `tauri-plugin-updater` itself
generates and checks — this is separate from the Apple Developer ID
certificate that signs the app and the notarization credential; it exists so
a compromised or spoofed feed can't get an install to run arbitrary code.

- **Public key**: committed, in `app/src-tauri/tauri.conf.json`'s
  `plugins.updater.pubkey`. Anyone can see it; that's the point — it's what
  every install actually checks a downloaded update against.
- **Private key**: lives at `~/Documents/Vigie-Signing-Backup/updater.key` (0600) on the machine that
  runs releases, and nowhere else — never in this repo (`*.key` is
  gitignored, plus an explicit `updater.key`/`updater.key.pub` line, belt and
  braces). `scripts/release.sh` reads it from that path, or from an
  already-exported `TAURI_SIGNING_PRIVATE_KEY` if set (that wins, same
  precedence the script already gives `VIGIE_SIGNING_IDENTITY`). Missing
  either way, the script fails before the build starts rather than shipping
  an update feed nothing can verify.

Generated once, with:

```
cd app && npm run tauri signer generate --ci -w ~/Documents/Vigie-Signing-Backup/updater.key
```

which prints the public key to paste into `tauri.conf.json` and writes the
private half straight to that path (`--ci` skips the password prompt — this
key has none; add `-p` if a password is ever wanted, and export
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` alongside it).

**If the private key is ever lost** (the machine dies, the file is deleted):
there is no recovery — minisign has no key-escrow story. Generate a new
keypair the same way, put the new public key in `tauri.conf.json` under a new
app version, and release that version through whatever channel still
reaches existing installs (the DMG link on `docs/index.html`, at minimum).
Every install *older* than that version cannot auto-update past it — its old
public key will never verify anything signed by the new private key. This is
inherent to how the scheme works, not a bug to route around.

## Cutting a release

1. Bump the version in `app/src-tauri/tauri.conf.json` (`"version"`) — this
   is the single source of truth `scripts/release.sh` reads back out of the
   built `Info.plist`.
2. Run `scripts/release.sh` (see its header for the full credential list:
   the Developer ID certificate, the notarytool profile, the GitHub OAuth
   client id, and the updater key above). It builds, signs, notarizes,
   staples, and — new since this packet — also:
   - copies the signed `.app.tar.gz` updater payload and the notarized
     `.dmg` into `docs/`, named `Vigie-<version>.app.tar.gz` /
     `Vigie-<version>.dmg` (versioned so a release is a distinct URL — no
     stale-CDN-cache risk from reusing one filename release after release);
   - regenerates `docs/latest.json` from that build's actual version,
     signature (the `.sig` file `createUpdaterArtifacts: true` makes Tauri
     produce alongside the archive) and download URL.
3. The script does **not** commit or push — `docs/` is left updated on disk,
   same as any other file it touches. Review the diff, then:
   ```
   git add docs/
   git commit -m "Release <version>"
   git push
   ```
   Once Pages is enabled (see above), that push is what actually publishes
   the update to every existing install.

### A note on hosting the binaries

`docs/` hosting both the updater payload and the DMG (rather than a GitHub
Release asset, which is what Jay's own script uses for its Sparkle
enclosure) was picked because it's the simpler of the two for this repo
today: no remote exists yet for `gh release create` to target, and Pages
already serves whatever sits in `docs/` with nothing else to configure.
Every release does add a new binary blob to the repo's git history this way
— fine at the release cadence of a small team's internal tool, worth
revisiting (Git LFS, or a GitHub Release after all) if the repo's size ever
becomes a real problem.

### A note on architectures

`scripts/release.sh` writes `docs/latest.json` with only the platform key
for the machine that ran it (`darwin-aarch64` on Apple silicon,
`darwin-x86_64` on Intel) — it does not merge in a previous run's entry for
the *other* architecture. Today that's fine: every release ships from one
Apple-silicon Mac. A client on a platform the feed has no entry for simply
sees "no update available", which is safe, just not a real update path. If
an Intel build is ever added, the script's own comment (search
`SHORTCUT:` in it) marks where to change one-shot-overwrite into a
merge-in-the-other-platform's-existing-entry instead.
