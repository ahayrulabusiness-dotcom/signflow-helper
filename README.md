# SignFlow Helper

A small Windows 11 tray app (and macOS menu-bar app) that opens a SignFlow
file in Flexi, or on a Mac in whatever app the Mac uses for the file, and saves
it back. SignFlow's "Open in Flexi" link (`signflow-edit://open?pass=…`) starts
it; it downloads that one file, opens it with the program Windows uses for
`.fs` files, and uploads each save as the file's new version.

Nothing in this repository is a secret. The helper holds no password, key or
storage token: each edit starts from a one-time pass in the link, which the
helper swaps for a short-lived key that it keeps in memory only. SignFlow's
address is built in and never read from a link.

## How it works

| Piece | File |
|---|---|
| The link: only `signflow-edit://open?pass=<43 characters>`, nothing else | `src-tauri/src/link.rs` |
| SignFlow's helper API (`/api/helper/…`): pass → key, download link, upload token, replace, heartbeat, done | `src-tauri/src/api.rs` |
| Downloads and uploads (streamed; parts above 10MB; at most 500MB) | `src-tauri/src/blob.rs` |
| Watching for saves: waits until the file stops changing and Flexi lets go of it; skips saves that changed nothing | `src-tauri/src/watcher.rs` |
| Her copy on this computer and what's been saved | `src-tauri/src/store.rs` |
| One edit, start to Done, including the "someone saved a newer version" question | `src-tauri/src/session.rs` |
| The tray, the stale window, links arriving | `src-tauri/src/app.rs`, `ui/stale.*` |

Files live in `%LOCALAPPDATA%\SignFlow Helper Files\files\<file id>\` on
Windows and `~/Library/Application Support/SignFlow Helper Files/files/<file id>/`
on a Mac. The helper never deletes them; uninstalling leaves them alone. Its
log is `helper.log` in the same folder. On Windows the app installs, for this
user only and with no admin prompt, into `%LOCALAPPDATA%\SignFlow Helper\`; on
a Mac it is dragged from the .dmg into /Applications. The Mac app is ad-hoc
signed (no Apple developer account, not notarized), so macOS asks once before
it first opens.

## Builds and tests (GitHub Actions, Windows)

- **CI** (every push, on Windows and on macOS): the unit tests, an end-to-end
  run, and a build of the release installer (.exe; universal .dmg). The end-to-end run installs an e2e build silently and
  points it at a throwaway stand-in for SignFlow and its file store on the
  runner (`e2e/standin/server.mjs`). It then fires links, saves the file the
  ways Flexi might, answers the stale question (once by clicking the real
  window), and uploads 500MB while measuring the helper's memory. The e2e
  build is the only one that may talk to `http://127.0.0.1`; it is never
  released.
- **Release**: push a tag `v<version>` (the version in
  `src-tauri/tauri.conf.json`), or use "Run workflow". It makes a **draft**
  release with the Windows installer (`SignFlow-Helper_<version>_x64-setup.exe`),
  the Mac app (`SignFlow-Helper_<version>_universal.dmg`), each platform's
  signed update and one `latest.json` that serves both. A draft isn't public until someone
  publishes it; a version that is already published is never touched.

## Update signing

Updates are signed with a key whose private half lives only in this
repository's secrets (`TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`). The public half is in
`src-tauri/tauri.conf.json` (`plugins.updater.pubkey`). Installed copies only
accept updates signed by that key, so losing it means reinstalling by hand.
