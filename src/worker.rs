//! Jobs that run on the tokio runtime and report back to the window through a channel.
//!
//! Trust differs from the CLI on purpose: the CLI trusts the first key it sees, the window asks.
//! A job that meets an unknown key (fresh install) or a changed one stops with
//! [`Msg::Trust`]; if the user accepts, the window runs the job again with that key pinned.
//! Keys the user accepted before ([`Request::trusted_keys`]) are taken without asking, so
//! switching an install between servers doesn't ask about the same key again.

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc::Sender},
    time::Duration,
};

use eframe::egui;
use serde::Deserialize;
use lmoxide_patch::{
    Error, Info, Manifest, Platform, SignedManifest,
    client::{Client, Trust, stored_key},
    manifest::{parse_public_key, public_key_hex},
    release::{Release, compare_versions},
    selfupdate,
    sync::{self, Options, Plan, Progress, Report},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// Server info, and whether the install is current.
    Check,
    /// Install or update; `full` rehashes every file (repair).
    Sync { full: bool },
    /// Compare the install with the manifest, change nothing.
    Verify { full: bool },
}

#[derive(Debug, Clone)]
pub struct Request {
    pub job: Job,
    pub server: String,
    pub dir: PathBuf,
    pub platform: Platform,
    pub jobs: usize,
    /// A key the user accepted (hex), pinned instead of the stored one.
    pub trust: Option<String>,
    /// Keys (hex) the user trusted before, for any server: taken without asking.
    pub trusted_keys: Vec<String>,
    /// Set by the window to stop hashing and downloads (the task is aborted too).
    pub cancel: Arc<AtomicBool>,
    /// Look for a newer build of this patcher on the server (checks only).
    pub patcher_updates: bool,
    /// The game mode to install (its LocalConfig.xml; `Manifest::for_mode`); `None`: the
    /// server's default.
    pub mode: Option<String>,
}

/// This program's name in the server's patcher releases.
pub const PATCHER_NAME: &str = env!("CARGO_PKG_NAME");
/// This build's version.
pub const PATCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The game server as the server's API (`Info::api_url`) reports it.
#[derive(Debug, Clone)]
pub struct GameStatus {
    /// `GET /v1/status`; `None` if the API didn't answer (or keeps its status private).
    pub status: Option<ApiStatus>,
    /// `GET /v1/config`; `None` for APIs without it.
    pub registration_open: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiStatus {
    pub game_server_online: bool,
    pub players_online: Option<u32>,
    pub maintenance: Option<bool>,
}

/// How the install compares with the server.
#[derive(Debug, Clone)]
pub enum Status {
    NotInstalled { bytes: u64 },
    UpToDate,
    UpdateAvailable,
    /// The manifest isn't signed by the key this install trusts.
    KeyChanged,
    /// The server is still hashing its client files.
    Indexing,
}

pub enum Msg {
    Info(Info),
    Game(GameStatus),
    /// The server offers a newer build of this patcher, signed with the install's key.
    PatcherUpdate(Release),
    /// That build replaced this program (`version`); a restart runs it.
    PatcherUpdated(String),
    Status(Status),
    /// Waiting for the server to finish indexing.
    Waiting,
    Progress(Progress),
    /// The manifest's key needs the user's decision (`expected`: the key trusted so far).
    Trust { req: Request, got: String, expected: Option<String> },
    Verified(Plan),
    Synced { plan: Plan, report: Report, manifest: Manifest },
    Failed(String),
    /// A note for the activity log.
    Log(String),
    PickedDir(PathBuf),
    /// A saved password, read from the credential store (`None`: none saved).
    Password { server: String, username: String, result: Result<Option<String>, String> },
    /// A password was saved (`true`) or forgotten (`false`).
    PasswordStored { server: String, username: String, result: Result<bool, String> },
}

/// Sends messages to the window, tagged with the job they belong to (messages of a cancelled
/// job may still be queued when the next one starts).
#[derive(Clone)]
pub struct Outbox {
    tx: Sender<(u64, Msg)>,
    ctx: egui::Context,
    job_id: u64,
}

impl Outbox {
    pub fn new(tx: Sender<(u64, Msg)>, ctx: egui::Context) -> Self {
        Self { tx, ctx, job_id: 0 }
    }

    pub fn for_job(&self, job_id: u64) -> Self {
        Self { job_id, ..self.clone() }
    }

    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send((self.job_id, msg));
        self.ctx.request_repaint();
    }
}

impl Request {
    /// Whether the user trusted `key` (hex) before.
    fn knows(&self, key: &str) -> bool {
        self.trusted_keys.iter().any(|k| k.eq_ignore_ascii_case(key.trim()))
    }
}

pub async fn run(req: Request, out: Outbox) {
    let result = match req.job {
        Job::Check => check(&req, &out).await,
        Job::Sync { .. } | Job::Verify { .. } => sync_job(&req, &out).await,
    };
    if let Err(e) = result {
        out.send(Msg::Failed(e));
    }
}

fn describe(client: &Client, platform: Platform, e: Error) -> String {
    match e {
        Error::Status { status: 404, url } if url.contains("/manifest/") => {
            format!("{} doesn't offer a {platform} client", client.base_url())
        }
        Error::Status { status: 404, url } if url.contains("/files/") => {
            format!("{} is missing one of its client files ({url}); ask its admin to restart it", client.base_url())
        }
        e => e.to_string(),
    }
}

/// The install's saved manifest for `platform`, if it verifies with the trusted key.
pub fn installed(dir: &std::path::Path, platform: Platform) -> Option<Manifest> {
    sync::load_manifest(dir).ok().flatten().filter(|m| m.platform == platform)
}

async fn check(req: &Request, out: &Outbox) -> Result<(), String> {
    let client = Client::new(&req.server).map_err(|e| e.to_string())?;
    let err = |e| describe(&client, req.platform, e);
    let info = client.info().await.map_err(err)?;
    out.send(Msg::Info(info.clone()));
    if let Some(api) = info.api_url.clone() {
        let out = out.clone();
        tokio::spawn(async move { out.send(Msg::Game(game_status(&api).await)) });
    }
    if req.patcher_updates {
        patcher_update(&client, &info, req, out).await;
    }
    let installed = installed(&req.dir, req.platform);
    // a game mode chosen that the install isn't for: an update (it changes LocalConfig.xml)
    if installed.is_some() && sync::load_mode(&req.dir) != req.mode {
        out.send(Msg::Status(Status::UpdateAvailable));
        return Ok(());
    }
    // The server's hints answer most checks without downloading the manifest.
    match (&installed, info.revisions.get(&req.platform)) {
        (Some(i), Some(rev)) if *rev == i.revision() => {
            out.send(Msg::Status(Status::UpToDate));
            return Ok(());
        }
        (None, _) if stored_key(&req.dir).map_err(err)?.is_none() => {
            if let Some(&bytes) = info.install_bytes.get(&req.platform) {
                out.send(Msg::Status(Status::NotInstalled { bytes }));
                return Ok(());
            }
        }
        _ => {}
    }
    let fetched = match &installed {
        Some(i) => client.signed_manifest_if_changed(req.platform, &i.revision()).await,
        None => client.signed_manifest(req.platform).await.map(Some),
    };
    let signed = match fetched {
        Err(Error::NotReady { .. }) => {
            out.send(Msg::Status(Status::Indexing));
            return Ok(());
        }
        Ok(None) => {
            out.send(Msg::Status(Status::UpToDate));
            return Ok(());
        }
        r => r.map_err(err)?.unwrap(),
    };
    let status = match stored_key(&req.dir).map_err(err)? {
        // Nothing trusted yet: the size is only for display, the install verifies.
        None => {
            let bytes = serde_json::from_str::<Manifest>(&signed.manifest)
                .map(|m| m.files.iter().map(|f| f.size).sum())
                .unwrap_or(0);
            Status::NotInstalled { bytes }
        }
        Some(key) => match signed.verify_for(&key, req.platform) {
            Ok(m) => match installed {
                Some(i) if i.revision() == m.revision() => Status::UpToDate,
                Some(_) => Status::UpdateAvailable,
                None => Status::NotInstalled { bytes: m.files.iter().map(|f| f.size).sum() },
            },
            // a key trusted before: the update takes it without asking
            Err(Error::KeyMismatch { got, .. }) if req.knows(&got) => Status::UpdateAvailable,
            Err(Error::KeyMismatch { .. }) => Status::KeyChanged,
            Err(e) => return Err(err(e)),
        },
    };
    out.send(Msg::Status(status));
    Ok(())
}

/// Offers a newer build of this patcher if `info` lists one and it verifies with the key the
/// install trusts (no trusted key yet: nothing is offered; best effort, failures are logged).
async fn patcher_update(client: &Client, info: &Info, req: &Request, out: &Outbox) {
    // servers offer builds for the client platforms (windows, linux); `Platform::host()` is
    // windows on macOS (the client runs under Wine there), whose build would replace this one
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let platform = Platform::host();
    let listed = info.patchers.get(PATCHER_NAME).and_then(|b| b.get(&platform));
    if !listed.is_some_and(|v| compare_versions(v, PATCHER_VERSION).is_gt()) {
        return;
    }
    let Ok(Some(key)) = stored_key(&req.dir) else { return };
    match selfupdate::check(client, PATCHER_NAME, platform, PATCHER_VERSION, &key).await {
        Ok(Some(r)) => out.send(Msg::PatcherUpdate(r)),
        Ok(None) => {}
        Err(e) => out.send(Msg::Log(format!("patcher update check: {}", describe(client, platform, e)))),
    }
}

/// Replaces this program (`exe`) with `release` from `server`.
pub async fn install_patcher(server: String, release: Release, exe: PathBuf, out: Outbox) {
    let result = async {
        let client = Client::new(&server)?;
        selfupdate::install(&client, &release, &exe, &sync::no_progress()).await
    }
    .await;
    match result {
        Ok(()) => out.send(Msg::PatcherUpdated(release.version)),
        Err(e) => out.send(Msg::Failed(format!("updating the patcher: {e}"))),
    }
}

/// Asks the server's API about the game server (best effort: errors leave fields empty).
async fn game_status(api: &str) -> GameStatus {
    let base = api.trim_end_matches('/');
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build();
    let get = async |path: &str| -> Option<reqwest::Response> {
        let r = http.as_ref().ok()?.get(format!("{base}{path}")).send().await.ok()?;
        r.status().is_success().then_some(r)
    };
    #[derive(Deserialize)]
    struct Config {
        registration_open: bool,
    }
    let status = match get("/v1/status").await {
        Some(r) => r.json::<ApiStatus>().await.ok(),
        None => None,
    };
    let registration_open = match get("/v1/config").await {
        Some(r) => r.json::<Config>().await.ok().map(|c| c.registration_open),
        None => None,
    };
    GameStatus { status, registration_open }
}

/// The signed manifest, waiting while the server indexes.
async fn fetch(client: &Client, platform: Platform, out: &Outbox) -> lmoxide_patch::Result<SignedManifest> {
    let mut waited = false;
    loop {
        match client.signed_manifest(platform).await {
            Err(Error::NotReady { retry_after }) => {
                if !waited {
                    out.send(Msg::Waiting);
                    waited = true;
                }
                tokio::time::sleep(Duration::from_secs(retry_after.clamp(1, 60))).await;
            }
            r => return r,
        }
    }
}

async fn sync_job(req: &Request, out: &Outbox) -> Result<(), String> {
    let client = Client::new(&req.server).map_err(|e| e.to_string())?;
    let err = |e| describe(&client, req.platform, e);
    let signed = fetch(&client, req.platform, out).await.map_err(err)?;
    let stored = stored_key(&req.dir).map_err(err)?;
    let remembered = req.trust.is_none()
        && req.knows(&signed.key)
        && stored.is_none_or(|k| !public_key_hex(&k).eq_ignore_ascii_case(signed.key.trim()));
    if remembered {
        out.send(Msg::Log("the server signs with a key you trusted before".into()));
    }
    let pinned = req.trust.as_ref().or(remembered.then_some(&signed.key));
    let trust = match (pinned, stored) {
        (Some(k), _) => Trust::Key(parse_public_key(k).map_err(err)?),
        (None, Some(_)) => Trust::FirstUse,
        (None, None) => {
            out.send(Msg::Trust { req: req.clone(), got: signed.key.clone(), expected: None });
            return Ok(());
        }
    };
    let manifest = match trust.verify(&req.dir, &signed, req.platform) {
        Err(Error::KeyMismatch { expected, got }) => {
            out.send(Msg::Trust { req: req.clone(), got, expected: Some(expected) });
            return Ok(());
        }
        r => r.map_err(err)?,
    };

    let progress: sync::ProgressFn = {
        let out = out.clone();
        std::sync::Arc::new(move |p| out.send(Msg::Progress(p)))
    };
    match req.job {
        Job::Sync { full } => {
            let opts = Options { jobs: req.jobs.max(1), full_check: full, cancel: Some(req.cancel.clone()), mode: req.mode.clone(), ..Default::default() };
            let (plan, report) = sync::sync(&client, &req.dir, &manifest, &opts, &progress).await.map_err(err)?;
            sync::save_manifest(&req.dir, &signed).map_err(err)?;
            sync::save_mode(&req.dir, req.mode.as_deref()).map_err(err)?;
            out.send(Msg::Synced { plan, report, manifest });
        }
        Job::Verify { full } => {
            let opts = Options { jobs: req.jobs.max(1), full_check: full, cancel: Some(req.cancel.clone()), mode: req.mode.clone(), ..Default::default() };
            let plan = sync::plan(&req.dir, &manifest, &opts, &progress).await.map_err(err)?;
            out.send(Msg::Verified(plan));
        }
        Job::Check => unreachable!(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `job` against `server` and returns what it reported.
    async fn sync_msgs(server: &str, dir: &std::path::Path, trust: Option<String>, trusted_keys: &[String]) -> Vec<Msg> {
        let (tx, rx) = std::sync::mpsc::channel();
        let req = Request {
            job: Job::Sync { full: false },
            server: server.into(),
            dir: dir.into(),
            platform: Platform::Linux,
            jobs: 1,
            trust,
            trusted_keys: trusted_keys.to_vec(),
            cancel: Arc::new(AtomicBool::new(false)),
            patcher_updates: false,
            mode: None,
        };
        run(req, Outbox::new(tx, egui::Context::default())).await;
        rx.try_iter().map(|(_, m)| m).collect()
    }

    fn asks(msgs: &[Msg]) -> Option<String> {
        msgs.iter().find_map(|m| match m {
            Msg::Trust { got, .. } => Some(got.clone()),
            _ => None,
        })
    }

    fn synced(msgs: &[Msg]) -> bool {
        msgs.iter().any(|m| matches!(m, Msg::Synced { .. }))
    }

    /// Switching one install between two servers asks about each key once.
    /// Needs two patch servers with different keys serving a Linux client, e.g.
    /// `LMOXIDE_TEST_SERVERS=http://127.0.0.1:18181,http://127.0.0.1:18182`.
    #[tokio::test]
    #[ignore]
    async fn switching_servers_remembers_keys() {
        let servers = std::env::var("LMOXIDE_TEST_SERVERS").expect("LMOXIDE_TEST_SERVERS");
        let (a, b) = servers.split_once(',').unwrap();
        let dir = std::env::temp_dir().join(format!("lmoxide-patcher-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut known = Vec::new();
        for server in [a, b] {
            let key = asks(&sync_msgs(server, &dir, None, &known).await).expect("a new key is asked about");
            assert!(synced(&sync_msgs(server, &dir, Some(key.clone()), &known).await));
            known.push(key);
        }
        for server in [a, b, a] {
            let msgs = sync_msgs(server, &dir, None, &known).await;
            assert!(asks(&msgs).is_none() && synced(&msgs), "{server} asked again");
        }
        // a key nobody accepted still asks
        assert!(asks(&sync_msgs(b, &dir, None, &known[..1]).await).is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
