//! Jobs that run on the tokio runtime and report back to the window through a channel.
//!
//! Trust differs from the CLI on purpose: the CLI trusts the first key it sees, the window asks.
//! A job that meets an unknown key (fresh install) or a changed one stops with
//! [`Msg::Trust`]; if the user accepts, the window runs the job again with that key pinned.

use std::{path::PathBuf, sync::mpsc::Sender, time::Duration};

use eframe::egui;
use lmoxide_patch::{
    Error, Info, Manifest, Platform, SignedManifest,
    client::{Client, Trust, stored_key},
    manifest::parse_public_key,
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
    Status(Status),
    /// Waiting for the server to finish indexing.
    Waiting,
    Progress(Progress),
    /// The manifest's key needs the user's decision (`expected`: the key trusted so far).
    Trust { req: Request, got: String, expected: Option<String> },
    Verified(Plan),
    Synced { plan: Plan, report: Report, manifest: Manifest },
    Failed(String),
    PickedDir(PathBuf),
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
        Error::Status { status: 404, .. } => format!("{} doesn't offer a {platform} client", client.base_url()),
        Error::Http(e) if e.is_connect() || e.is_timeout() => format!("can't reach {}: {e}", client.base_url()),
        e => e.to_string(),
    }
}

async fn check(req: &Request, out: &Outbox) -> Result<(), String> {
    let client = Client::new(&req.server).map_err(|e| e.to_string())?;
    let err = |e| describe(&client, req.platform, e);
    out.send(Msg::Info(client.info().await.map_err(err)?));
    let signed = match client.signed_manifest(req.platform).await {
        Err(Error::NotReady { .. }) => {
            out.send(Msg::Status(Status::Indexing));
            return Ok(());
        }
        r => r.map_err(err)?,
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
            Ok(m) => match crate::install::installed(&req.dir) {
                Some(i) if i.platform == req.platform && i.version == m.version => Status::UpToDate,
                Some(_) => Status::UpdateAvailable,
                None => Status::NotInstalled { bytes: m.files.iter().map(|f| f.size).sum() },
            },
            Err(Error::KeyMismatch { .. }) => Status::KeyChanged,
            Err(e) => return Err(err(e)),
        },
    };
    out.send(Msg::Status(status));
    Ok(())
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
    let trust = match (&req.trust, stored_key(&req.dir).map_err(err)?) {
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
            let opts = Options { jobs: req.jobs.max(1), full_check: full, ..Default::default() };
            let (plan, report) = sync::sync(&client, &req.dir, &manifest, &opts, &progress).await.map_err(err)?;
            sync::save_manifest(&req.dir, &signed).map_err(err)?;
            out.send(Msg::Synced { plan, report, manifest });
        }
        Job::Verify { full } => {
            let opts = Options { jobs: req.jobs.max(1), full_check: full, ..Default::default() };
            let plan = sync::plan(&req.dir, &manifest, &opts, &progress).await.map_err(err)?;
            out.send(Msg::Verified(plan));
        }
        Job::Check => unreachable!(),
    }
    Ok(())
}
