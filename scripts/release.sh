#!/usr/bin/env bash
#
# Builds, signs, notarizes and staples Vigie for direct distribution
# (outside the Mac App Store), then proves the result to Gatekeeper.
#
#   scripts/release.sh
#
# The *app* is signed by Tauri as it bundles, using the identity below
# passed through as `APPLE_SIGNING_IDENTITY` (tauri.conf.json's
# `bundle.macOS` carries only `hardenedRuntime` — no identity, so the public
# source names no one), so this script never signs the app itself — one
# place decides how the app is signed. The script does sign the disk image,
# because the disk image is something this script creates and Tauri never
# sees (see the hdiutil block below).
#
# Requirements:
#   * the "Developer ID Application: …" certificate in the login keychain
#   * a notarytool keychain profile, default `vigie-notary`, created once with
#       xcrun notarytool store-credentials vigie-notary \
#         --apple-id <apple-id> --team-id <team-id> --password <app-specific-password>
#   * VIGIE_GITHUB_CLIENT_ID exported (the OAuth App's device-flow client id).
#     It is compiled in via option_env! and is deliberately not in any
#     committed file. Instead of exporting it, it can live in
#     app/.env.local (gitignored) — this script loads it from there.
#   * VIGIE_SIGNING_IDENTITY exported — the full "Developer ID Application:
#     Name (TEAMID)" string from the certificate above. Deliberately not in
#     any committed file, same reasoning as VIGIE_GITHUB_CLIENT_ID; it can
#     also live in app/.env.local, loaded the same way.
#   * The updater's minisign private key at ~/Documents/Vigie-Signing-Backup/updater.key (0600,
#     never in this repo — *.key is gitignored, belt and braces). Generated
#     once with:
#       npm run tauri signer generate --ci -w ~/Documents/Vigie-Signing-Backup/updater.key
#     which also prints the PUBLIC key — that half goes into
#     app/src-tauri/tauri.conf.json's plugins.updater.pubkey and is meant to
#     be committed. This script reads the private key from that path (or
#     from an already-exported TAURI_SIGNING_PRIVATE_KEY, which wins if set,
#     same precedence as VIGIE_SIGNING_IDENTITY above) and hands it to
#     `tauri build` as TAURI_SIGNING_PRIVATE_KEY — that's what actually signs
#     the `.app.tar.gz` updater artifact `createUpdaterArtifacts: true` in
#     tauri.conf.json makes the build also produce. If the key is ever lost,
#     there is no recovery: generate a new pair, put the new public key in a
#     new app version, and publish it — installs older than that version
#     cannot auto-update to it (see docs/RELEASING.md).
#
# Everything is checked before the long build starts, so a missing
# credential fails in seconds rather than after a release build.

set -euo pipefail

readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly APP_DIR="$REPO_ROOT/app"
readonly BUNDLE_DIR="$APP_DIR/src-tauri/target/release/bundle"
readonly APP_PATH="$BUNDLE_DIR/macos/Vigie.app"
readonly DMG_DIR="$BUNDLE_DIR/dmg"
readonly SITE_DIR="$REPO_ROOT/docs"
readonly KEYCHAIN_PROFILE="${VIGIE_NOTARY_PROFILE:-vigie-notary}"
readonly UPDATER_KEY_PATH="${VIGIE_UPDATER_KEY_PATH:-$HOME/Documents/Vigie-Signing-Backup/updater.key}"
# The signing identity is deliberately not written down in this repository.
# It is taken from VIGIE_SIGNING_IDENTITY when set; otherwise the one
# Developer ID Application certificate in the login keychain is used, which
# is what a machine set up to sign this app already has. Only if there is
# none, or several, does this ask for the variable.
detect_signing_identity() {
  local found
  found="$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p')"
  [[ -z "$found" ]] && return 1
  [[ "$(printf '%s\n' "$found" | wc -l | tr -d ' ')" != "1" ]] && return 2
  printf '%s' "$found"
}
if [[ -n "${VIGIE_SIGNING_IDENTITY:-}" ]]; then
  readonly SIGNING_IDENTITY="$VIGIE_SIGNING_IDENTITY"
else
  if ! SIGNING_IDENTITY="$(detect_signing_identity)"; then
    case $? in
      1) printf '\nerror: no "Developer ID Application" certificate in the login keychain. Set VIGIE_SIGNING_IDENTITY, or install the certificate.\n' >&2 ;;
      2) printf '\nerror: several "Developer ID Application" certificates found. Set VIGIE_SIGNING_IDENTITY to the one to use.\n' >&2 ;;
    esac
    exit 1
  fi
  readonly SIGNING_IDENTITY
fi

say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
die() { printf '\nerror: %s\n' "$*" >&2; exit 1; }

# The client id can also live in app/.env.local (gitignored), the same file
# build.rs reads — an explicitly exported VIGIE_GITHUB_CLIENT_ID still wins.
if [[ -z "${VIGIE_GITHUB_CLIENT_ID:-}" && -f "$APP_DIR/.env.local" ]]; then
  export VIGIE_GITHUB_CLIENT_ID="$(sed -n 's/^VIGIE_GITHUB_CLIENT_ID=//p' "$APP_DIR/.env.local" | head -1)"
fi

# ---- preflight -------------------------------------------------------------

[[ -n "${VIGIE_GITHUB_CLIENT_ID:-}" ]] ||
  die "VIGIE_GITHUB_CLIENT_ID is not set — the build would ship a Sign in button that cannot work."

# The updater signing key: an already-exported TAURI_SIGNING_PRIVATE_KEY
# wins (same precedence as VIGIE_SIGNING_IDENTITY above); otherwise it's
# read from disk. Checked here, before the build, and exported for the
# `tauri build` invocation below to actually sign the updater artifact with
# — fail loudly now rather than ship an unsigned (and therefore unusable)
# update feed.
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  [[ -f "$UPDATER_KEY_PATH" ]] ||
    die "no updater signing key at $UPDATER_KEY_PATH and TAURI_SIGNING_PRIVATE_KEY is not set.
     Generate one once with:
       npm run tauri signer generate --ci -w $UPDATER_KEY_PATH
     then put the PUBLIC key it prints into app/src-tauri/tauri.conf.json's plugins.updater.pubkey."
  export TAURI_SIGNING_PRIVATE_KEY="$(cat "$UPDATER_KEY_PATH")"
fi

command -v jq >/dev/null 2>&1 || die "jq is required to write docs/latest.json (brew install jq)."

security find-identity -v -p codesigning | grep -qF "$SIGNING_IDENTITY" ||
  die "no \"$SIGNING_IDENTITY\" certificate in the keychain."

if ! xcrun notarytool history --keychain-profile "$KEYCHAIN_PROFILE" >/dev/null 2>&1; then
  die "notarytool keychain profile \"$KEYCHAIN_PROFILE\" is missing or unusable.
     Create it with:
       xcrun notarytool store-credentials $KEYCHAIN_PROFILE \\
         --apple-id <apple-id> --team-id <team-id> --password <app-specific-password>"
fi

# ---- build (Tauri signs as it bundles) -------------------------------------
#
# tauri.conf.json's `bundle.macOS` carries no `signingIdentity` (deliberately
# — that would put the identity back in tracked source), so Tauri is told it
# here instead, the way its CLI documents for exactly this case.
say "Building and signing"
( cd "$APP_DIR" && APPLE_SIGNING_IDENTITY="$SIGNING_IDENTITY" npm run tauri build )

[[ -d "$APP_PATH" ]] || die "no app bundle at $APP_PATH"

# ---- disk image ------------------------------------------------------------
#
# Built here with hdiutil rather than by Tauri's `dmg` bundle target, which
# drives Finder over AppleScript to lay the window out and therefore fails
# on any machine that has not granted this terminal Automation access to
# Finder (`AppleEvent timed out (-1712)`) — CI, ssh, and a fresh Mac all
# qualify. This image is plain: an app and an Applications symlink, no
# background art and no icon placement.
#
# SHORTCUT: no styled DMG window. Fine for a direct download; if the layout
# is ever wanted, add "dmg" back to bundle.targets in tauri.conf.json and
# grant Terminal → Automation → Finder, then delete this block.
readonly VERSION="$(/usr/bin/plutil -extract CFBundleShortVersionString raw "$APP_PATH/Contents/Info.plist")"
readonly DMG_PATH="$DMG_DIR/Vigie_${VERSION}_$(/usr/bin/uname -m).dmg"

say "Building $(basename "$DMG_PATH")"
STAGE="$(/usr/bin/mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
# ditto, not cp: it is the copier that keeps a signed bundle intact.
/usr/bin/ditto "$APP_PATH" "$STAGE/$(basename "$APP_PATH")"
/bin/ln -s /Applications "$STAGE/Applications"
/bin/mkdir -p "$DMG_DIR"
/bin/rm -f "$DMG_PATH"
hdiutil create -volname Vigie -srcfolder "$STAGE" -ov -format UDZO "$DMG_PATH" >/dev/null

# The image itself has to be signed, and before notarizing: hdiutil leaves it
# unsigned, and an unsigned image is one Gatekeeper rejects out of hand
# ("source=no usable signature") no matter how well notarized the app inside
# it is. Signing after notarizing would instead invalidate the stapled ticket,
# so the order here — create, sign, notarize, staple — is the only one that
# leaves both the image and the app it carries acceptable. `--timestamp`
# because notarization requires a secure timestamp.
codesign --force --sign "$SIGNING_IDENTITY" --timestamp "$DMG_PATH"

say "Signature"
codesign -dv --verbose=2 "$APP_PATH" 2>&1 | grep -E 'Authority=|flags=|Identifier=' || true

# ---- notarize --------------------------------------------------------------

say "Notarizing $(basename "$DMG_PATH")"
xcrun notarytool submit "$DMG_PATH" --keychain-profile "$KEYCHAIN_PROFILE" --wait

# ---- staple ----------------------------------------------------------------
#
# Both, and in this order: the .app inside the .dmg must carry its own
# ticket, or copying it out of the image loses the offline proof.
say "Stapling"
xcrun stapler staple "$APP_PATH"
xcrun stapler staple "$DMG_PATH"

# ---- prove it --------------------------------------------------------------
#
# The acceptance criterion: "accepted" and "source=Notarized Developer ID" —
# for the app, and for the image a user actually downloads. Both, because the
# ticket stapled to one says nothing about the other: an unsigned image can
# carry a perfectly notarized app and still be turned away at the door.
say "Gatekeeper assessment"
spctl -a -vv -t install "$APP_PATH"
spctl -a -vv -t install "$DMG_PATH"

# ---- update docs/latest.json -----------------------------------------------
#
# The updater's own feed (app/src-tauri/tauri.conf.json's
# plugins.updater.endpoints points at its published URL). Hosted the
# simplest way for a GitHub Pages site: both binaries copied straight into
# docs/ alongside it, versioned by filename so a release is a distinct URL
# (no stale-cache risk from reusing one filename across releases) — the
# GitHub Release asset alternative was passed over because it needs a
# remote and `gh` set up, which this repo doesn't have yet. Nothing here
# commits or pushes: docs/ is now updated on disk, same as every other file
# this script doesn't touch — see docs/RELEASING.md for publishing it.
say "Updating docs/latest.json"

readonly UPDATER_ARCHIVE="$(find "$BUNDLE_DIR/macos" -maxdepth 1 -name '*.app.tar.gz' -print -quit)"
[[ -n "$UPDATER_ARCHIVE" && -f "$UPDATER_ARCHIVE.sig" ]] ||
  die "no updater artifact (*.app.tar.gz + .sig) in $BUNDLE_DIR/macos.
     Check bundle.createUpdaterArtifacts is true in app/src-tauri/tauri.conf.json."

# arm64 -> aarch64 is Tauri's own target-triple spelling for the "platforms"
# key (tauri-plugin-updater's Updater::check, arch field); x86_64 stays as
# printed. This machine's own architecture, same as the DMG name above —
# only ever the one this script actually just built and signed.
readonly HOST_ARCH="$(/usr/bin/uname -m)"
if [[ "$HOST_ARCH" == "arm64" ]]; then
  readonly PLATFORM_KEY="darwin-aarch64"
else
  readonly PLATFORM_KEY="darwin-$HOST_ARCH"
fi

readonly UPDATER_ASSET_NAME="Vigie-${VERSION}.app.tar.gz"
readonly DMG_ASSET_NAME="Vigie-${VERSION}.dmg"
readonly DOWNLOAD_URL="https://spacegrowth.github.io/vigie/${UPDATER_ASSET_NAME}"

/bin/mkdir -p "$SITE_DIR"
/usr/bin/ditto "$UPDATER_ARCHIVE" "$SITE_DIR/$UPDATER_ASSET_NAME"
/usr/bin/ditto "$DMG_PATH" "$SITE_DIR/$DMG_ASSET_NAME"

# SHORTCUT: this writes only the platform this machine just built — it does
# not merge in a previous run's entry for a *different* architecture, so a
# release cut only on Apple silicon (the only Mac this ships from today)
# simply has no "darwin-x86_64" key; tauri-plugin-updater's own docs treat a
# missing platform key as "no update available" for that platform, which is
# safe, just not a real update path for Intel Macs. Fine at today's scale
# (one release machine, one architecture); if an Intel build is ever added,
# merge this run's platform entry into the existing file's "platforms"
# instead of overwriting it wholesale.
jq -n \
  --arg version "$VERSION" \
  --arg notes "Vigie ${VERSION}. ${VIGIE_RELEASE_NOTES:-See the source repository for what changed.}" \
  --arg pub_date "$(/bin/date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg platform "$PLATFORM_KEY" \
  --arg url "$DOWNLOAD_URL" \
  --arg signature "$(cat "$UPDATER_ARCHIVE.sig")" \
  '{
     version: $version,
     notes: $notes,
     pub_date: $pub_date,
     platforms: { ($platform): { signature: $signature, url: $url } }
   }' > "$SITE_DIR/latest.json.tmp"
/bin/mv "$SITE_DIR/latest.json.tmp" "$SITE_DIR/latest.json"

say "Done — $DMG_PATH"
echo "  docs/latest.json → $PLATFORM_KEY, v$VERSION"
echo "  docs/$UPDATER_ASSET_NAME (updater payload)"
echo "  docs/$DMG_ASSET_NAME (first-time install, linked from docs/index.html)"
echo "  Not committed or pushed — see docs/RELEASING.md to publish."
