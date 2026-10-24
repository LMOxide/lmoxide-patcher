# Proposed changes to lmoxide

Changes to the patch server (`lmoxide-patch-server`) and the patcher library (`lmoxide-patch`)
in the [lmoxide](https://github.com/LMOxide/lmoxide) repo that this GUI would benefit from.
Found while building the first version (2026-10-24).

**Status:** items 1, 2, 4, 5 and 6 landed in lmoxide e3a1a76 (2026-10-24) and the GUI uses them;
item 3 is still open.

## Patch server

### 1. Cheap update checks (done)

The window checks for updates on start and on every Connect. The protocol has no small way to
ask "did the manifest change?", so the check downloads the whole signed manifest (for a real
client ~60k entries, several MB) just to compare `version`. Either:

- add `versions: {platform: version}` and `download_bytes: {platform: bytes}` to
  `GET /lmoxide/v1/info` (a hint; the signed manifest stays authoritative), or
- send `ETag: "<version>"` on `/lmoxide/v1/manifest/{platform}` and answer `If-None-Match` with
  `304`; the patcher sends the installed manifest's version.

GUI: `worker::check` would compare against `info.versions` (or send `If-None-Match`) and only
fetch the manifest to install. The "Not installed (N to download)" size would come from
`download_bytes` instead of an unverified manifest.

**Done:** `Info::revisions` and `Info::install_bytes` (per platform), where a revision is
`Manifest::revision()` (sha256 of the manifest without its build time, so a server restart
doesn't look like an update). The manifest route sends it as `ETag` and answers
`If-None-Match` with 304 (`Client::signed_manifest_if_changed`). `worker::check` answers "up to
date" and "not installed (N)" from `info` alone, and otherwise fetches only if changed (older
servers without these fields still work).

### 2. Where the game server's status lives (done)

`/lmoxide/v1/info` describes the patch server only. The window can't say whether the game server
is up, how many players are online, or link to account registration. Add an optional
`api_url` (the public `lmoxide-api` base URL, whose `GET /v1/status` already has this) and an
optional `website` to `Info`.

GUI: a status line under the server name ("online, 3 players") and a "Create an account" link.

**Done:** `Info::api_url` and `Info::website` (patch server `--api-url`, `--website`). The check
job asks the API's `/v1/status` and `/v1/config` (`worker::game_status`, best effort). The
Server card shows "online, N players", "maintenance" or "offline" next to the game server, a
website link, and "Create an account" (to the website) when the API says registration is open.

### 3. Patcher downloads (later)

A way to distribute and update the patcher itself, e.g. `GET /lmoxide/v1/patcher/{platform}`
listing signed builds, would let a server hand out this window instead of the client kit.
Not needed yet.

## Patcher library (`lmoxide-patch`)

### 4. Cancelling a check (done)

The GUI cancels by aborting the job's task. Downloads stop right away (partial files resume
later), but `sync::plan` hashes in `spawn_blocking` + scoped threads, which run to the end after
the abort: cancelling a Repair or Verify keeps the disk busy until hashing finishes. Add a
cancel flag to `sync::Options` (e.g. `cancel: Option<Arc<AtomicBool>>`) that the hashing loop
and `apply` check, returning a new `Error::Cancelled`.

GUI: set the flag in `PatcherApp::cancel` before aborting.

**Done:** `sync::Options::cancel` and `Error::Cancelled`. Each job gets a flag in
`Request::cancel`; `PatcherApp::cancel` and `start` set it before aborting.

### 5. Shared helpers the CLI and the GUI both need (done)

These exist twice now (CLI and GUI) and could drift:

- `default_install_dir()` (`~/Games/LMO`): `lmoxide-patcher` `SyncArgs::dir`, GUI
  `settings::default_dir`.
- Reading the installed manifest: `sync::save_manifest` writes `.lmoxide/manifest.json`, but
  nothing reads it back. Add `sync::load_manifest(dir) -> Result<Option<Manifest>>` that
  verifies it against `client::stored_key` (GUI: `install::installed`).
- Building the launch command: `Launch` documents `{username}`/`{password}` substitution and
  install-relative env values, and the CLI prints a command line while the GUI spawns one. A
  `Launch::command(dir, login: Option<(&str, &str)>) -> std::process::Command` would keep the
  rules in one place (GUI: `install::launch`).

**Done:** `lmoxide_patch::default_install_dir()`, `sync::load_manifest(dir)` (used by
`worker::installed`), `Launch::command(dir, login, wrapper)`. `install::launch` now only picks
the Windows runner as the wrapper and silences stdio.

### 6. `Error` for user interfaces (done)

`Error::Http` wraps `reqwest::Error`, whose message for a refused connection is long and
technical. The GUI maps connect/timeout errors itself (`worker::describe`); an
`Error::Unreachable { url }` variant (or a helper) would give every patcher the same wording.

**Done:** `Error::Unreachable { url, source }` ("can't reach URL: the server is down or the
address is wrong"); `worker::describe` no longer maps it.

## Not an API change, but worth knowing

`launch.login_args` put the password on the client's command line, where other local users can
read it (`ps`). That's how the stock client takes it; the GUI only passes it when a username
is filled in, and never stores it.
