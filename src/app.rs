//! The window: header, status with progress and the main buttons, account and installation
//! cards, server details, the activity log, and the settings and trust dialogs.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};

use eframe::egui::{self, Align, Color32, Layout, Margin, Rect, RichText, Sense, Vec2, pos2, vec2};
use lmoxide_patch::{
    Info, Manifest, Platform,
    client::stored_key,
    manifest::public_key_hex,
    release::Release,
    sync::{Plan, Progress, Reason},
};
use tokio::{runtime::Runtime, task::JoinHandle};

use crate::{
    install, secrets,
    settings::{self, Settings},
    theme::{self, Kind, icon},
    worker::{self, GameStatus, Job, Msg, Outbox, Request, Status},
};

const LOG_LINES: usize = 500;
/// Tags the messages of a patcher update, which runs beside the other jobs.
const PATCHER_JOB: u64 = u64::MAX;
/// How often to ask again while the server indexes.
const RECHECK: Duration = Duration::from_secs(10);
/// How often an idle window asks whether the server is (still) up.
const PING_EVERY: Duration = Duration::from_secs(15);
/// Pings tag their messages from here up (`PatcherApp::ping_id`), below [`PATCHER_JOB`].
const PING_JOBS: u64 = 1 << 62;

pub struct PatcherApp {
    rt: Runtime,
    settings: Settings,
    rx: Receiver<(u64, Msg)>,
    out: Outbox,
    /// The running job and its cancel flag; `job_id` tags its messages.
    task: Option<(Job, JoinHandle<()>, Arc<AtomicBool>)>,
    job_id: u64,
    run: RunState,
    info: Option<Info>,
    /// The game server, from the server's API (`Info::api_url`).
    game: Option<GameStatus>,
    /// Server warnings already logged.
    warned: Vec<String>,
    status: Option<Status>,
    /// The last job's error, shown under the status.
    error: Option<String>,
    recheck_at: Option<Instant>,
    /// The running ping (`worker::ping`), its message tag and when the next one is due.
    ping: Option<JoinHandle<()>>,
    ping_id: u64,
    ping_at: Instant,
    /// The verified manifest of the current install (for Play).
    installed: Option<Manifest>,
    /// No patcher install, but the client is already in the folder (copied in or set up some
    /// other way): installing just checks it and downloads what differs.
    existing: bool,
    password: String,
    /// The account whose saved password is in `password` (none: typed, or empty).
    password_of: Option<String>,
    /// Reading the saved password of this account.
    loading_password: Option<String>,
    /// Save the password on Play.
    remember: bool,
    /// The credential store's last error, shown in the account card.
    secret_error: Option<String>,
    /// The install directory was edited and not yet checked.
    dir_edited: bool,
    show_settings: bool,
    /// The profile dialog is open.
    show_profile: bool,
    /// A newer build of this patcher the server offers.
    patcher_update: Option<Release>,
    /// Installing it.
    patcher_installing: bool,
    /// Its banner was put off ("Not now"); the header button brings it back.
    patcher_banner_hidden: bool,
    /// Scroll to the banner on the next frame (the header button was clicked).
    reveal_patcher_banner: bool,
    /// Installed (its version): restart to use it.
    patcher_updated: Option<String>,
    /// This program, as found at startup (on Linux `current_exe` names the deleted file once
    /// an update replaced it).
    exe: Option<PathBuf>,
    trust: Option<TrustPrompt>,
    verified: Option<Plan>,
    log: VecDeque<(Level, String)>,
}

struct TrustPrompt {
    req: Request,
    got: String,
    expected: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Default)]
enum Stage {
    #[default]
    Connecting,
    Waiting,
    Checking {
        done: usize,
        total: usize,
    },
    Downloading,
}

/// Progress of the running job.
#[derive(Default)]
struct RunState {
    stage: Stage,
    total_bytes: u64,
    done_bytes: u64,
    files_total: usize,
    files_done: usize,
    /// Files being downloaded.
    current: Vec<String>,
    /// (time, done_bytes) over the last few seconds, for the rate.
    samples: VecDeque<(Instant, u64)>,
}

impl RunState {
    fn rate(&self) -> Option<f64> {
        let (t0, b0) = self.samples.front()?;
        let (t1, b1) = self.samples.back()?;
        let dt = t1.duration_since(*t0).as_secs_f64();
        (dt > 0.5).then(|| (b1 - b0) as f64 / dt)
    }

    fn sample(&mut self) {
        let now = Instant::now();
        self.samples.push_back((now, self.done_bytes));
        while self.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(5)) {
            self.samples.pop_front();
        }
    }

    fn fraction(&self) -> Option<f32> {
        match self.stage {
            Stage::Connecting | Stage::Waiting => None,
            Stage::Checking { done, total } => Some(if total == 0 { 1.0 } else { done as f32 / total as f32 }),
            Stage::Downloading => {
                Some(if self.total_bytes == 0 { 1.0 } else { self.done_bytes as f32 / self.total_bytes as f32 })
            }
        }
    }

    fn describe(&self) -> String {
        match self.stage {
            Stage::Connecting => "Fetching the file list…".into(),
            Stage::Waiting => "Waiting for the server to finish indexing…".into(),
            Stage::Checking { done, total } => format!("Checking files: {done} of {total}"),
            Stage::Downloading => format!("Downloading: {} of {} files", self.files_done, self.files_total),
        }
    }
}

impl PatcherApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Runtime) -> Self {
        theme::install(&cc.egui_ctx);
        let mut settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        settings.normalize();
        let (tx, rx) = channel();
        let mut app = Self {
            rt,
            settings,
            rx,
            out: Outbox::new(tx, cc.egui_ctx.clone()),
            task: None,
            job_id: 0,
            run: RunState::default(),
            info: None,
            game: None,
            warned: Vec::new(),
            status: None,
            error: None,
            recheck_at: None,
            ping: None,
            ping_id: PING_JOBS,
            ping_at: Instant::now() + PING_EVERY,
            installed: None,
            existing: false,
            password: String::new(),
            password_of: None,
            loading_password: None,
            remember: false,
            secret_error: None,
            dir_edited: false,
            show_settings: false,
            show_profile: false,
            patcher_update: None,
            patcher_installing: false,
            patcher_banner_hidden: false,
            reveal_patcher_banner: false,
            patcher_updated: None,
            exe: std::env::current_exe().ok(),
            trust: None,
            verified: None,
            log: VecDeque::new(),
        };
        app.location_changed();
        app.account_changed();
        app
    }

    fn dir(&self) -> PathBuf {
        PathBuf::from(self.settings.profile().dir.trim())
    }

    fn busy(&self) -> Option<Job> {
        self.task.as_ref().map(|(j, ..)| *j)
    }

    /// Installing, updating or verifying (not just checking).
    fn working(&self) -> bool {
        matches!(self.busy(), Some(Job::Sync { .. } | Job::Verify { .. }))
    }

    fn log(&mut self, level: Level, msg: impl Into<String>) {
        if self.log.len() == LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back((level, msg.into()));
    }

    /// Server, directory or platform changed: forget what we knew and check again.
    /// The game mode this profile plays, as the server names it (servers with `modes`).
    fn mode_name(&self, info: &Info) -> Option<String> {
        let current = self.settings.profile().mode.trim();
        info.modes.iter().find(|m| m.id == current).or(info.modes.first()).map(|m| m.name.clone())
    }

    fn location_changed(&mut self) {
        let platform = self.settings.profile().platform;
        // installs from before the list (or trusted by the CLI) count as the user's choice
        if let Ok(Some(key)) = stored_key(&self.dir()) {
            self.settings.trust_key(&public_key_hex(&key));
        }
        self.installed = worker::installed(&self.dir(), platform);
        self.existing = self.installed.is_none() && self.dir().join(executable_name(platform)).is_file();
        self.info = None;
        self.game = None;
        self.status = None;
        self.verified = None;
        self.dir_edited = false;
        self.stop_ping();
        if self.settings.profile().server.trim().is_empty() {
            self.cancel();
            self.error = None;
        } else {
            self.start(Job::Check, None);
        }
    }

    fn start(&mut self, job: Job, trust: Option<String>) {
        if let Some((_, h, cancel)) = self.task.take() {
            cancel.store(true, Ordering::Relaxed);
            h.abort();
        }
        if job != Job::Check {
            self.verified = None;
        }
        self.job_id += 1;
        self.run = RunState::default();
        self.error = None;
        self.recheck_at = None;
        self.ping_at = Instant::now() + PING_EVERY;
        let req = Request {
            job,
            server: self.settings.profile().server.trim().to_string(),
            dir: self.dir(),
            platform: self.settings.profile().platform,
            jobs: self.settings.jobs,
            trust,
            trusted_keys: self.settings.trusted_keys.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            patcher_updates: self.settings.check_patcher_updates && self.patcher_updated.is_none(),
            mode: Some(self.settings.profile().mode.trim().to_string()).filter(|m| !m.is_empty()),
        };
        let cancel = req.cancel.clone();
        self.task = Some((job, self.rt.spawn(worker::run(req, self.out.for_job(self.job_id))), cancel));
    }

    fn cancel(&mut self) {
        if let Some((job, h, cancel)) = self.task.take() {
            // the abort alone would leave hashing threads running to the end
            cancel.store(true, Ordering::Relaxed);
            h.abort();
            self.job_id += 1;
            if matches!(job, Job::Sync { .. }) {
                self.log(Level::Info, "cancelled; the next update resumes the downloads");
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Info(info) => {
                let new: Vec<String> = info.warnings.iter().filter(|w| !self.warned.contains(w)).cloned().collect();
                for w in new {
                    self.log(Level::Warn, format!("server: {w}"));
                    self.warned.push(w);
                }
                self.info = Some(info);
            }
            Msg::Game(g) => self.game = Some(g),
            Msg::PatcherUpdate(r) => {
                if self.patcher_update.as_ref().is_none_or(|old| old.version != r.version) {
                    self.log(Level::Info, format!("the server offers {} {}", r.name, r.version));
                    self.patcher_banner_hidden = false;
                }
                self.patcher_update = Some(r);
            }
            Msg::PatcherUpdated(_) => unreachable!("handled in drain"),
            Msg::Log(line) => self.log(Level::Info, line),
            Msg::Status(s) => {
                if matches!(s, Status::Indexing) {
                    self.recheck_at = Some(Instant::now() + RECHECK);
                }
                self.status = Some(s);
            }
            Msg::Waiting => {
                self.run.stage = Stage::Waiting;
                self.status = Some(Status::Indexing);
            }
            Msg::Progress(p) => self.progress(p),
            Msg::Trust { req, got, expected } => self.trust = Some(TrustPrompt { req, got, expected }),
            Msg::Verified(plan) => {
                self.log(
                    Level::Info,
                    if plan.is_up_to_date() {
                        format!("verified: all {} files are intact", plan.unchanged)
                    } else {
                        format!("verified: {} files differ, {} to remove", plan.download.len() + plan.fix_mode.len(), plan.remove.len())
                    },
                );
                self.verified = Some(plan);
            }
            Msg::Synced { plan, report, manifest } => {
                self.log(
                    Level::Info,
                    if plan.is_up_to_date() {
                        format!("up to date ({} files)", report.unchanged)
                    } else {
                        format!(
                            "done: {} files downloaded ({}), {} removed, {} unchanged",
                            report.downloaded_files,
                            human_bytes(report.downloaded_bytes),
                            report.removed_files,
                            report.unchanged
                        )
                    },
                );
                if manifest.login_public_key.is_none() {
                    self.log(Level::Warn, "the server has no login key: this client can't log in to it");
                }
                self.installed = Some(manifest);
                self.status = Some(Status::UpToDate);
            }
            Msg::Failed(e) => {
                // a check that fails means the server is down: don't keep showing what it said before
                if self.busy() == Some(Job::Check) {
                    self.went_offline();
                }
                self.log(Level::Error, e.clone());
                self.error = Some(e);
            }
            Msg::PickedDir(_) | Msg::Password { .. } | Msg::PasswordStored { .. } => unreachable!("handled in drain"),
        }
    }

    /// Asks whether the server is up, every [`PING_EVERY`] while nothing else runs.
    fn maybe_ping(&mut self) {
        let due = Instant::now() >= self.ping_at
            && self.task.is_none()
            && self.ping.as_ref().is_none_or(|h| h.is_finished())
            && self.trust.is_none()
            && !self.settings.profile().server.trim().is_empty();
        if !due {
            return;
        }
        self.ping_id += 1;
        self.ping_at = Instant::now() + PING_EVERY;
        let server = self.settings.profile().server.trim().to_string();
        self.ping = Some(self.rt.spawn(worker::ping(server, self.out.for_job(self.ping_id))));
    }

    /// Forgets a running ping (the server or install changed).
    fn stop_ping(&mut self) {
        if let Some(h) = self.ping.take() {
            h.abort();
        }
        self.ping_id += 1;
        self.ping_at = Instant::now() + PING_EVERY;
    }

    fn ping_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Info(info) => {
                // back online, or the server's files changed: check the install again
                let changed = self
                    .info
                    .as_ref()
                    .is_none_or(|old| old.revisions != info.revisions || old.manifest_key != info.manifest_key);
                if self.info.is_none() {
                    self.log(Level::Info, "the server is back online");
                }
                self.handle(Msg::Info(info));
                if changed && self.task.is_none() && self.trust.is_none() {
                    self.start(Job::Check, None);
                }
            }
            Msg::Game(g) => self.game = Some(g),
            Msg::Failed(e) if self.info.is_some() => {
                self.log(Level::Warn, format!("lost the server: {e}"));
                self.went_offline();
                self.error.get_or_insert(e);
            }
            _ => {}
        }
    }

    /// The server stopped answering: show it as offline ("Can't check for updates").
    fn went_offline(&mut self) {
        self.info = None;
        self.game = None;
        if !matches!(self.status, Some(Status::Indexing)) {
            self.status = None;
        }
    }

    fn progress(&mut self, p: Progress) {
        let r = &mut self.run;
        match p {
            Progress::Checking { done, total } => r.stage = Stage::Checking { done, total },
            Progress::Planned { download_files, download_bytes, .. } => {
                r.stage = Stage::Downloading;
                r.files_total = download_files;
                r.total_bytes = download_bytes;
            }
            Progress::FileStarted { path, .. } => r.current.push(path),
            Progress::Bytes(n) => r.done_bytes += n,
            Progress::FileDone { path } => {
                r.files_done += 1;
                r.current.retain(|p| *p != path);
            }
            Progress::Removed { path } => self.log(Level::Info, format!("removed {path}")),
            Progress::Done => {}
        }
    }

    fn drain(&mut self) {
        let finished = self.task.as_ref().is_some_and(|(_, h, _)| h.is_finished());
        while let Ok((id, msg)) = self.rx.try_recv() {
            match msg {
                Msg::PickedDir(d) => {
                    self.settings.profile_mut().dir = d.display().to_string();
                    self.location_changed();
                }
                Msg::Password { server, username, result } => self.password_loaded(server, username, result),
                Msg::PasswordStored { server, username, result } => self.password_stored(server, username, result),
                msg if id == PATCHER_JOB => self.patcher_msg(msg),
                msg if id == self.ping_id && self.ping.is_some() => self.ping_msg(msg),
                msg if id == self.job_id => self.handle(msg),
                _ => {}
            }
        }
        if finished {
            self.task = None;
        }
        if self.task.is_none() && self.recheck_at.is_some_and(|t| Instant::now() >= t) {
            self.start(Job::Check, None);
        }
        self.maybe_ping();
    }

    fn patcher_msg(&mut self, msg: Msg) {
        self.patcher_installing = false;
        match msg {
            Msg::PatcherUpdated(version) => {
                self.log(Level::Info, format!("the patcher was updated to {version}; restart to use it"));
                self.patcher_update = None;
                self.patcher_updated = Some(version);
            }
            Msg::Failed(e) => {
                self.log(Level::Error, e.clone());
                self.error = Some(e);
            }
            _ => {}
        }
    }

    fn update_patcher(&mut self) {
        let Some(r) = self.patcher_update.clone() else { return };
        let Some(exe) = self.exe.clone() else {
            self.error = Some("can't tell where this program is, so it can't update itself".into());
            return;
        };
        self.patcher_installing = true;
        self.log(Level::Info, format!("downloading {} {}", r.name, r.version));
        let server = self.settings.profile().server.trim().to_string();
        self.rt.spawn(worker::install_patcher(server, r, exe, self.out.for_job(PATCHER_JOB)));
    }

    /// Starts the new build and closes this window.
    fn restart(&mut self, ctx: &egui::Context) {
        let Some(exe) = &self.exe else { return };
        let started = std::process::Command::new(exe).spawn();
        match started {
            Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => {
                let e = format!("restarting the patcher: {e}");
                self.log(Level::Error, e.clone());
                self.error = Some(e);
            }
        }
    }

    fn switch_profile(&mut self, index: usize) {
        if index == self.settings.current || index >= self.settings.profiles.len() {
            return;
        }
        self.cancel();
        self.settings.current = index;
        self.clear_password();
        self.log(Level::Info, format!("profile {}", self.settings.profile().name));
        self.location_changed();
        self.account_changed();
    }

    /// A new profile like the current one (same server, install and runner), without the account.
    fn new_profile(&mut self) {
        let mut p = self.settings.profile().clone();
        p.name = self.settings.unique_name("New profile");
        p.username.clear();
        p.accounts.clear();
        self.settings.profiles.push(p);
        self.switch_profile(self.settings.profiles.len() - 1);
        self.show_profile = true;
    }

    fn delete_profile(&mut self) {
        if self.settings.profiles.len() < 2 {
            return;
        }
        let gone = self.settings.profiles.remove(self.settings.current);
        self.log(Level::Info, format!("deleted profile {} (its install stays on disk)", gone.name));
        // its saved passwords, unless another profile uses them too
        for a in gone.accounts.iter().filter(|a| a.saved_password) {
            if !self.settings.password_shared(usize::MAX, &gone.server, &a.username) {
                secrets::store(gone.server.clone(), a.username.clone(), None, self.out.clone());
            }
        }
        self.settings.current = self.settings.current.min(self.settings.profiles.len() - 1);
        self.cancel();
        self.clear_password();
        self.location_changed();
        self.account_changed();
    }

    fn clear_password(&mut self) {
        self.password.clear();
        self.password_of = None;
        self.loading_password = None;
        self.secret_error = None;
    }

    /// The username changed (picked, typed, another profile): a password loaded for another
    /// account goes; a known account brings back its checkbox and its saved password.
    fn account_changed(&mut self) {
        let username = self.settings.profile().username.trim().to_string();
        if self.password_of.as_ref().is_some_and(|u| *u != username) {
            self.clear_password();
        }
        let p = self.settings.profile();
        let Some(account) = p.account(&username) else { return };
        self.remember = account.saved_password;
        if account.saved_password && self.password.is_empty() && self.loading_password.as_ref() != Some(&username) {
            self.loading_password = Some(username.clone());
            secrets::load(p.server.clone(), username, self.out.clone());
        }
    }

    /// The saved password arrived (if it's still wanted: same account, nothing typed since).
    fn password_loaded(&mut self, server: String, username: String, result: Result<Option<String>, String>) {
        let p = self.settings.profile();
        let current = secrets::server_key(&p.server) == secrets::server_key(&server) && p.username.trim() == username;
        if self.loading_password.as_ref() == Some(&username) {
            self.loading_password = None;
        }
        if !current {
            return;
        }
        match result {
            Ok(Some(password)) if self.password.is_empty() => {
                self.password = password;
                self.password_of = Some(username);
                self.secret_error = None;
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                // gone from the store (or the server changed)
                if let Some(a) = self.settings.profile_mut().accounts.iter_mut().find(|a| a.username == username) {
                    a.saved_password = false;
                }
                self.remember = false;
                self.log(Level::Warn, format!("no saved password for {username} on this server"));
            }
            Err(e) => {
                self.log(Level::Error, e.clone());
                self.secret_error = Some(e);
            }
        }
    }

    fn password_stored(&mut self, server: String, username: String, result: Result<bool, String>) {
        match result {
            Ok(true) => {
                self.log(Level::Info, format!("saved the password for {username}"));
                self.secret_error = None;
            }
            Ok(false) => self.log(Level::Info, format!("forgot the saved password for {username}")),
            Err(e) => {
                // not saved after all
                let p = self.settings.profile_mut();
                if secrets::server_key(&p.server) == secrets::server_key(&server)
                    && let Some(a) = p.accounts.iter_mut().find(|a| a.username == username)
                {
                    a.saved_password = false;
                }
                self.log(Level::Error, e.clone());
                self.secret_error = Some(e);
            }
        }
    }

    /// After logging in: the account goes to the top of the list, and its password into the
    /// credential store or out of it, as the checkbox says.
    fn remember_login(&mut self) {
        let i = self.settings.current;
        let p = self.settings.profile_mut();
        let username = p.username.trim().to_string();
        if username.is_empty() {
            return;
        }
        let server = p.server.clone();
        let account = p.touch_account(&username);
        let was = account.saved_password;
        account.saved_password = self.remember;
        if self.remember {
            // unless it's the saved one, unchanged
            if !was || self.password_of.as_ref() != Some(&username) {
                secrets::store(server, username.clone(), Some(self.password.clone()), self.out.clone());
                self.password_of = Some(username);
            }
        } else if was {
            self.forget_password(i, server, username);
        }
    }

    /// Takes the account's password out of the store, unless another profile uses it.
    fn forget_password(&mut self, profile: usize, server: String, username: String) {
        if !self.settings.password_shared(profile, &server, &username) {
            secrets::store(server, username, None, self.out.clone());
        }
    }

    /// Removes the account from the profile's list, with its saved password.
    fn forget_account(&mut self, username: &str) {
        let i = self.settings.current;
        let p = self.settings.profile_mut();
        let Some(at) = p.accounts.iter().position(|a| a.username == username) else { return };
        let a = p.accounts.remove(at);
        let server = p.server.clone();
        if p.username.trim() == username {
            p.username.clear();
            self.clear_password();
            self.remember = false;
        }
        if a.saved_password {
            self.forget_password(i, server, a.username);
        }
    }

    fn browse(&self) {
        let out = self.out.clone();
        let start = self.dir();
        self.rt.spawn(async move {
            let mut dialog = rfd::AsyncFileDialog::new().set_title("Install directory");
            if let Some(existing) = start.ancestors().find(|p| p.is_dir()) {
                dialog = dialog.set_directory(existing);
            }
            if let Some(d) = dialog.pick_folder().await {
                out.send(Msg::PickedDir(d.path().to_path_buf()));
            }
        });
    }

    /// A username without a password: the client would finish patching and then hang on its
    /// loading screen without ever contacting the server.
    fn missing_password(&self) -> bool {
        !self.settings.profile().username.trim().is_empty() && self.password.is_empty()
    }

    fn play(&mut self) {
        if self.missing_password() {
            return;
        }
        let Some(m) = &self.installed else { return };
        match install::launch(&self.dir(), m, self.settings.profile(), &self.password) {
            Ok(()) => {
                self.log(Level::Info, format!("started {}", m.launch.executable));
                self.remember_login();
            }
            Err(e) => {
                let e = format!("{e:#}");
                self.log(Level::Error, e.clone());
                self.error = Some(e);
            }
        }
    }

    // ---- drawing ----

    /// The header's connection chip.
    fn connection(&self) -> (Color32, &'static str) {
        if self.settings.profile().server.trim().is_empty() {
            (theme::WEAK, "No server")
        } else if self.info.is_some() {
            (theme::GREEN, "Online")
        } else if self.busy() == Some(Job::Check) {
            (theme::BLUE, "Connecting…")
        } else {
            (theme::RED, "Offline")
        }
    }

    /// The line under the connection chip: players online, when the server's API reports it.
    fn connection_caption(&self) -> Option<String> {
        let s = self.info.as_ref().and(self.game.as_ref())?.status.as_ref()?;
        match s.players_online.filter(|_| s.game_server_online)? {
            1 => Some("1 player online".into()),
            n => Some(format!("{n} players online")),
        }
    }

    fn header_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let painter = ui.painter();
        theme::gradient(painter, rect, Color32::from_rgb(0x13, 0x2c, 0x63), Color32::from_rgb(0x1d, 0x15, 0x40));
        // a LEGO-coloured stripe along the bottom
        let third = rect.width() / 3.0;
        for (i, c) in [theme::ACCENT, theme::RED, theme::BLUE].into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(rect.left() + third * i as f32, rect.bottom() - 3.0), vec2(third + 1.0, 3.0));
            painter.rect_filled(r, 0, c);
        }
        let content = rect.shrink2(vec2(20.0, 0.0)).with_max_y(rect.bottom() - 3.0);
        ui.scope_builder(egui::UiBuilder::new().max_rect(content).layout(Layout::left_to_right(Align::Center)), |ui| {
            let (logo, _) = ui.allocate_exact_size(Vec2::splat(46.0), Sense::hover());
            theme::logo(ui.painter(), logo);
            ui.add_space(8.0);
            // Title and subtitle centred on the logo, painted (a vertical layout would sit at the top).
            let sub = match &self.info {
                Some(i) => match (self.mode_name(i), &i.kind) {
                    (Some(m), _) => format!("LMOxide Patcher  ·  {}  ·  {m}", i.name),
                    (None, Some(k)) => format!("LMOxide Patcher  ·  {}  ·  {}", i.name, kind_label(k).0),
                    (None, None) => format!("LMOxide Patcher  ·  {}", i.name),
                },
                None => "LMOxide Patcher".into(),
            };
            // Leave room for the chip and the header buttons.
            let pending = self.patcher_pending();
            let (color, chip_text) = self.connection();
            let caption = self.connection_caption().map(|text| {
                ui.painter().layout_no_wrap(text, egui::FontId::proportional(11.5), Color32::from_rgb(0xa9, 0xb6, 0xd8))
            });
            // the caption can be wider than the chip (`theme::chip` is its text + 32)
            let chip_w = ui.painter().layout_no_wrap(chip_text.into(), egui::FontId::new(12.5, theme::semibold()), theme::TEXT).size().x + 32.0;
            let extra = caption.as_ref().map_or(0.0, |c| (c.size().x - chip_w).max(0.0));
            let buttons = if pending.is_some() { 246.0 } else { 208.0 } + extra;
            let room = (ui.available_width() - buttons).max(40.0);
            let line = |text: String, font: egui::FontId, color: Color32| {
                let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
                job.wrap = egui::text::TextWrapping::truncate_at_width(room);
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let title = line("Minifigures Online".into(), egui::FontId::new(24.0, theme::display()), theme::TEXT);
            let sub = line(sub, egui::FontId::proportional(14.0), Color32::from_rgb(0xa9, 0xb6, 0xd8));
            let (text, _) = ui.allocate_exact_size(vec2(title.size().x.max(sub.size().x), 46.0), Sense::hover());
            let top = text.center().y - (title.size().y + sub.size().y) / 2.0;
            ui.painter().galley(pos2(text.left(), top), title.clone(), theme::TEXT);
            ui.painter().galley(pos2(text.left(), top + title.size().y), sub, theme::TEXT);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::icon_button(ui, icon::GEAR, "Settings").clicked() {
                    self.show_settings = true;
                }
                if let Some((glyph, tip)) = pending {
                    ui.add_space(4.0);
                    let resp = theme::icon_button(ui, glyph, &tip);
                    // a dot on the corner, ringed in the header's colour
                    let dot = resp.rect.right_top() + vec2(-6.0, 6.0);
                    ui.painter().circle(dot, 4.5, theme::BLUE, egui::Stroke::new(2.0, Color32::from_rgb(0x1d, 0x15, 0x40)));
                    if resp.clicked() {
                        self.patcher_banner_hidden = false;
                        self.reveal_patcher_banner = true;
                    }
                }
                if !self.settings.profile().server.trim().is_empty() {
                    ui.add_space(4.0);
                    let tip = if self.working() { "Check the server (after this finishes)" } else { "Check the server now" };
                    if theme::icon_button(ui, icon::UPDATE, tip).clicked() && !self.working() {
                        self.start(Job::Check, None);
                    }
                }
                ui.add_space(4.0);
                // the chip, with how many are playing under it
                let caption_h = caption.as_ref().map_or(0.0, |c| c.size().y + 2.0);
                let width = chip_w + extra;
                let (block, _) = ui.allocate_exact_size(vec2(width, 26.0 + caption_h), Sense::hover());
                let chip_rect = Rect::from_min_size(pos2(block.right() - chip_w, block.top()), vec2(chip_w, 26.0));
                let chip = ui.scope_builder(egui::UiBuilder::new().max_rect(chip_rect), |ui| theme::chip(ui, color, chip_text)).inner;
                if let Some(e) = self.error.as_deref().filter(|_| self.info.is_none()) {
                    chip.on_hover_text(e);
                }
                if let Some(c) = caption {
                    ui.painter().galley(pos2(block.right() - c.size().x, block.top() + 28.0), c, theme::TEXT);
                }
            });
        });
    }

    /// The status headline: icon, colour, title and a detail line.
    fn headline(&self) -> (&'static str, Color32, String, String) {
        let summary = self.installed.as_ref().map(|m| {
            let version: String = m.version.chars().take(8).collect();
            format!("{} client  ·  version {version}", platform_name(m.platform))
        });
        let summary = || summary.clone().unwrap_or_default();
        match (self.busy(), &self.status) {
            (Some(Job::Sync { full }), _) => {
                let title = if full {
                    "Repairing…"
                } else if self.installed.is_some() || self.existing {
                    "Updating…"
                } else {
                    "Installing…"
                };
                (icon::DOWNLOAD, theme::ACCENT, title.into(), self.run.describe())
            }
            (Some(Job::Verify { .. }), _) => (icon::SHIELD_CHECK, theme::BLUE, "Verifying…".into(), self.run.describe()),
            (_, Some(Status::Indexing)) => (
                icon::HOURGLASS,
                theme::AMBER,
                "Server is getting ready".into(),
                "It's still indexing its client files. This updates by itself when it's done.".into(),
            ),
            (Some(Job::Check), None) => {
                (icon::PULSE, theme::BLUE, "Checking for updates…".into(), self.settings.profile().server.trim().into())
            }
            (_, Some(Status::UpToDate)) => (icon::CHECK_CIRCLE, theme::GREEN, "Ready to play".into(), summary()),
            (_, Some(Status::UpdateAvailable)) => {
                (icon::CLOUD_DOWN, theme::ACCENT, "Update available".into(), format!("{}  ·  update to play", summary()))
            }
            (_, Some(Status::NotInstalled { .. })) if self.existing => (
                icon::FOLDER,
                theme::ACCENT,
                "Existing install found".into(),
                format!(
                    "Update checks its files and downloads only what differs  ·  {} client",
                    platform_name(self.settings.profile().platform)
                ),
            ),
            (_, Some(Status::NotInstalled { bytes })) => (
                icon::CLOUD_DOWN,
                theme::ACCENT,
                "Not installed yet".into(),
                format!("{} to download  ·  {} client", human_bytes(*bytes), platform_name(self.settings.profile().platform)),
            ),
            (_, Some(Status::KeyChanged)) => (
                icon::WARNING,
                theme::RED,
                "The server's key changed".into(),
                "Its files are signed with a different key. Update to review it.".into(),
            ),
            (None, None) if self.settings.profile().server.trim().is_empty() => (
                icon::GLOBE,
                theme::WEAK,
                "Choose a server".into(),
                "Enter the patch server's address under Installation.".into(),
            ),
            (None, None) if self.installed.is_some() => (
                icon::PLUGS,
                theme::AMBER,
                "Can't check for updates".into(),
                "The server isn't reachable. You can still play the installed client.".into(),
            ),
            (None, None) => (icon::PLUGS, theme::RED, "Server unreachable".into(), "Check the address and try again.".into()),
        }
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        let (badge, color, title, detail) = self.headline();
        ui.horizontal(|ui| {
            theme::badge(ui, badge, color);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.add_space(3.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).family(theme::display()).size(23.0).color(theme::TEXT));
                ui.add(egui::Label::new(RichText::new(detail).color(theme::WEAK)).truncate());
            });
        });
        if let Some(e) = &self.error {
            ui.add_space(4.0);
            theme::callout(ui, icon::WARNING_CIRCLE, theme::RED, e);
        }
        if self.working() {
            ui.add_space(8.0);
            self.progress_ui(ui, color);
        }
        ui.add_space(12.0);
        self.actions_ui(ui);
    }

    fn progress_ui(&mut self, ui: &mut egui::Ui, color: Color32) {
        let r = &mut self.run;
        theme::progress(ui, r.fraction(), color);
        if !matches!(r.stage, Stage::Downloading) {
            return;
        }
        r.sample();
        let rate = r.rate();
        let stat = |ui: &mut egui::Ui, label: &str, value: String| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new(label.to_uppercase()).size(10.5).color(theme::WEAK).extra_letter_spacing(0.8));
                ui.label(RichText::new(value).family(theme::semibold()).size(14.5).color(theme::TEXT));
            });
            ui.add_space(18.0);
        };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            stat(ui, "Downloaded", format!("{} / {}", human_bytes(r.done_bytes), human_bytes(r.total_bytes)));
            stat(ui, "Speed", rate.map_or("–".into(), |v| format!("{}/s", human_bytes(v as u64))));
            let left = rate.filter(|v| *v > 0.0).map(|v| r.total_bytes.saturating_sub(r.done_bytes) as f64 / v);
            stat(ui, "Time left", left.map_or("–".into(), human_duration));
            stat(ui, "Files", format!("{} / {}", r.files_done, r.files_total));
        });
        ui.spacing_mut().item_spacing.y = 2.0;
        // Always three rows, so files starting and finishing don't move what's below.
        for i in 0..3 {
            let p = r.current.get(i).map_or(" ", String::as_str);
            ui.add(egui::Label::new(RichText::new(p).monospace().size(11.5).color(theme::WEAK)).truncate());
        }
    }

    fn actions_ui(&mut self, ui: &mut egui::Ui) {
        let working = self.working();
        let has_server = !self.settings.profile().server.trim().is_empty();
        let installed = self.installed.is_some();
        let can_play = !self.missing_password();
        let play_hint = |r: egui::Response| r.on_disabled_hover_text("Enter your password (or clear the username)");
        let needs_sync =
            matches!(self.status, Some(Status::NotInstalled { .. } | Status::UpdateAvailable | Status::KeyChanged));
        let sync_label = if installed || self.existing { "Update" } else { "Install" };
        let sync_icon = if installed || self.existing { icon::UPDATE } else { icon::DOWNLOAD };
        let big = vec2(200.0_f32.min(ui.available_width() * 0.45), 46.0);
        let small = vec2(104.0, 34.0);
        // Enter outside a text field (and outside a dialog) presses the yellow button.
        let enter = !self.show_settings
            && !self.show_profile
            && self.trust.is_none()
            && ui.memory(|m| m.focused().is_none())
            && ui.input(|i| i.key_pressed(egui::Key::Enter));

        ui.horizontal(|ui| {
            if working {
                if theme::big_button(ui, Kind::Danger, icon::X, "Cancel", true, big).clicked() {
                    self.cancel();
                }
            } else if needs_sync || !installed {
                if theme::big_button(ui, Kind::Primary, sync_icon, sync_label, has_server, big).clicked() || (enter && has_server) {
                    self.start(Job::Sync { full: false }, None);
                }
                if installed && play_hint(theme::big_button(ui, Kind::Secondary, icon::PLAY, "Play", can_play, big)).clicked() {
                    self.play();
                }
            } else if play_hint(theme::big_button(ui, Kind::Primary, icon::PLAY, "Play", can_play, big)).clicked() || enter {
                self.play();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let enabled = !working && has_server && installed;
                if theme::big_button(ui, Kind::Secondary, icon::WRENCH, "Repair", enabled, small)
                    .on_hover_text("Rehash every file and download whatever differs (slow, for damaged installs)")
                    .clicked()
                {
                    self.start(Job::Sync { full: true }, None);
                }
                if theme::big_button(ui, Kind::Secondary, icon::SHIELD_CHECK, "Verify", enabled, small)
                    .on_hover_text("Compare the install with the server's files; change nothing")
                    .clicked()
                {
                    self.start(Job::Verify { full: false }, None);
                }
            });
        });
    }

    fn account_ui(&mut self, ui: &mut egui::Ui) {
        theme::card_title(ui, icon::USER, "Account");
        theme::field_label(ui, "Username");
        let accounts = self.settings.profile().accounts.clone();
        let (mut pick, mut forget) = (None, None);
        ui.horizontal(|ui| {
            let width = if accounts.is_empty() { ui.available_width() } else { ui.available_width() - 42.0 };
            let r = ui.add(theme::text_field(&mut self.settings.profile_mut().username).hint_text("Optional").desired_width(width));
            if r.changed() {
                self.account_changed();
            }
            if accounts.is_empty() {
                return;
            }
            let current = self.settings.profile().username.trim().to_string();
            let button = theme::icon_button(ui, icon::CARET_DOWN, "Accounts used with this profile");
            egui::Popup::menu(&button).show(|ui| {
                for a in &accounts {
                    ui.horizontal(|ui| {
                        let text = if a.saved_password { format!("{}   {}", a.username, icon::KEY) } else { a.username.clone() };
                        let r = ui.add(egui::Button::selectable(a.username == current, text).min_size(vec2(180.0, 0.0)));
                        if r.on_hover_text(if a.saved_password { "Password saved" } else { "Password not saved" }).clicked() {
                            pick = Some(a.username.clone());
                        }
                        let x = ui.add(egui::Button::new(RichText::new(icon::X).color(theme::WEAK)).frame(false));
                        if x.on_hover_text("Forget this account and its saved password").clicked() {
                            forget = Some(a.username.clone());
                        }
                    });
                }
            });
        });
        if let Some(u) = pick {
            self.settings.profile_mut().username = u;
            self.account_changed();
        }
        if let Some(u) = forget {
            self.forget_account(&u);
        }
        theme::field_label(ui, "Password");
        let hint = if self.loading_password.is_some() { "Reading the saved password…" } else { "" };
        let r = ui.add(theme::text_field(&mut self.password).password(true).hint_text(hint).desired_width(f32::INFINITY));
        if r.changed() {
            self.password_of = None;
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && self.installed.is_some() {
            self.play();
        }
        let has_username = !self.settings.profile().username.trim().is_empty();
        let r = ui
            .add_enabled(has_username, egui::Checkbox::new(&mut self.remember, "Remember password"))
            .on_hover_text("Saved in the system's credential store when you press Play");
        if r.changed() && !self.remember {
            // unticked: forget it now rather than at the next Play
            let i = self.settings.current;
            let p = self.settings.profile_mut();
            let (server, username) = (p.server.clone(), p.username.trim().to_string());
            if let Some(a) = p.accounts.iter_mut().find(|a| a.username == username && a.saved_password) {
                a.saved_password = false;
                self.forget_password(i, server, username);
            }
        }
        if let Some(e) = &self.secret_error {
            ui.add(egui::Label::new(RichText::new(format!("{} {e}", icon::WARNING_CIRCLE)).size(12.0).color(theme::AMBER)).wrap());
        }
        ui.add_space(2.0);
        if self.missing_password() {
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{} Enter your password to play.", icon::WARNING_CIRCLE)).size(12.0).color(theme::AMBER),
                )
                .wrap(),
            );
        }
        ui.add(
            egui::Label::new(
                RichText::new("With a username, Play logs you in automatically. Remembered passwords are kept in the system's credential store, never in the settings.")
                    .size(12.0)
                    .color(theme::WEAK),
            )
            .wrap(),
        );
    }

    fn installation_ui(&mut self, ui: &mut egui::Ui) {
        let idle = !self.working();
        let mut changed = false;
        theme::card_title(ui, icon::CUBE, "Installation");

        theme::field_label(ui, "Patch server");
        ui.horizontal(|ui| {
            let edit = theme::text_field(&mut self.settings.profile_mut().server)
                .hint_text("http://lmo.example.org:8080")
                .desired_width(ui.available_width() - 42.0);
            let r = ui.add_enabled(idle, edit);
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let connect = ui.add_enabled_ui(idle, |ui| theme::icon_button(ui, icon::UPDATE, "Connect")).inner;
            changed |= connect.clicked() || enter;
        });

        theme::field_label(ui, "Install folder");
        ui.horizontal(|ui| {
            let edit = theme::text_field(&mut self.settings.profile_mut().dir).desired_width(ui.available_width() - 42.0);
            let r = ui.add_enabled(idle, edit);
            self.dir_edited |= r.changed();
            changed |= r.lost_focus() && self.dir_edited;
            if ui.add_enabled_ui(idle, |ui| theme::icon_button(ui, icon::FOLDER, "Browse…")).inner.clicked() {
                self.browse();
            }
        });

        theme::field_label(ui, "Client");
        let options: Vec<_> = Platform::ALL
            .into_iter()
            .map(|p| (p, if p == Platform::Windows { icon::WINDOWS } else { icon::LINUX }, platform_name(p)))
            .collect();
        let before = self.settings.profile().platform;
        if theme::segmented(ui, &mut self.settings.profile_mut().platform, &options, idle) {
            changed = true;
            let p = self.settings.profile_mut();
            if p.runner == settings::default_runner(before) {
                p.runner = settings::default_runner(p.platform).into();
            }
        }

        if changed {
            self.location_changed();
        }
    }

    fn server_ui(&mut self, ui: &mut egui::Ui) {
        let idle = !self.working();
        let mut pick = None;
        let Some(info) = &self.info else { return };
        theme::card(ui, |ui| {
            theme::card_title(ui, icon::SERVER, &format!("Server  ·  {}", info.name));
            egui::Grid::new("info").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                let key = |ui: &mut egui::Ui, k: &str| {
                    ui.label(RichText::new(k).color(theme::WEAK));
                };
                key(ui, "Game server");
                // the chosen game mode's universe agent (its port picks the mode)
                let current = self.settings.profile().mode.trim();
                let addr = info.modes.iter().find(|m| m.id == current).or(info.modes.first()).map_or(&info.universe_addr, |m| &m.universe_addr);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(addr).monospace());
                    if let Some((color, text)) = self.game.as_ref().and_then(game_line) {
                        ui.label(RichText::new(text).color(color));
                    }
                });
                ui.end_row();
                if !info.modes.is_empty() {
                    key(ui, "Game Mode");
                    let current = self.settings.profile().mode.trim().to_string();
                    let shown = info.modes.iter().find(|m| m.id == current).or(info.modes.first());
                    let label = shown.map_or(String::new(), |m| m.name.clone());
                    egui::ComboBox::from_id_salt("game-mode")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            for m in &info.modes {
                                let selected = shown.is_some_and(|s| s.id == m.id);
                                let mut hover = m.description.clone();
                                if m.requires.as_deref() == Some("editor") {
                                    hover.push_str(" For the server's world editors (GM level 1 and up).");
                                }
                                if ui.add_enabled(idle, egui::Button::selectable(selected, &m.name)).on_hover_text(hover).clicked() && !selected {
                                    pick = Some(m.id.clone());
                                }
                            }
                        })
                        .response
                        .on_hover_text("Each game mode has its own characters. Changing it updates the client's settings (one small file).");
                    ui.end_row();
                } else if let Some(kind) = &info.kind {
                    key(ui, "Game Mode");
                    let (name, about) = kind_label(kind);
                    ui.label(RichText::new(name).strong()).on_hover_text(about);
                    ui.end_row();
                }
                if let Some(site) = &info.website {
                    key(ui, "Website");
                    let open = self.game.as_ref().and_then(|g| g.registration_open) == Some(true);
                    ui.horizontal(|ui| {
                        ui.hyperlink_to(format!("{} {site}", icon::GLOBE), site);
                        if open {
                            ui.hyperlink_to("Create an account", site);
                        }
                    });
                    ui.end_row();
                }
                key(ui, "Clients");
                let platforms: Vec<String> = info
                    .platforms
                    .iter()
                    .map(|p| if info.ready.contains(p) { platform_name(*p).into() } else { format!("{} (indexing)", platform_name(*p)) })
                    .collect();
                ui.label(platforms.join(", "));
                ui.end_row();
                key(ui, "Signing key");
                let short = format!("{}…{}", &info.manifest_key[..12.min(info.manifest_key.len())], &info.manifest_key[info.manifest_key.len().saturating_sub(8)..]);
                ui.label(RichText::new(short).monospace())
                    .on_hover_text(format!("ed25519 key the server signs its file lists with:\n{}", group_hex(&info.manifest_key)));
                ui.end_row();
                key(ui, "Login key");
                ui.label(if info.login_public_key.is_some() { "Patched into the client" } else { "None (the client can't log in)" });
                ui.end_row();
            });
            for w in &info.warnings {
                ui.add_space(2.0);
                theme::callout(ui, icon::WARNING, theme::AMBER, w);
            }
        });
        if let Some(mode) = pick {
            self.log(Level::Info, format!("game mode: {mode}"));
            self.settings.profile_mut().mode = mode;
            // an install gets the mode's settings now (an update changes LocalConfig.xml only)
            let job = if self.installed.is_some() { Job::Sync { full: false } } else { Job::Check };
            self.start(job, None);
        }
    }

    fn verified_ui(&mut self, ui: &mut egui::Ui) {
        let Some(plan) = &self.verified else { return };
        theme::card(ui, |ui| {
            if plan.is_up_to_date() {
                theme::card_title(ui, icon::SHIELD_CHECK, "Verify");
                theme::callout(ui, icon::CHECK_CIRCLE, theme::GREEN, &format!("All {} files are intact.", plan.unchanged));
                return;
            }
            let n = plan.download.len() + plan.fix_mode.len() + plan.remove.len();
            theme::card_title(ui, icon::SHIELD_CHECK, &format!("Verify found {n} differences"));
            ui.label(
                RichText::new(format!("Update or Repair fixes them ({} to download).", human_bytes(plan.download_bytes())))
                    .color(theme::WEAK),
            );
            egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, what: &str, color: Color32, path: &str| {
                    ui.horizontal(|ui| {
                        ui.add_sized([64.0, 16.0], egui::Label::new(RichText::new(what).size(12.0).color(color)));
                        ui.add(egui::Label::new(RichText::new(path).monospace()).truncate());
                    });
                };
                for c in &plan.download {
                    if c.reason == Reason::Missing {
                        row(ui, "missing", theme::RED, &c.entry.path);
                    } else {
                        row(ui, "changed", theme::AMBER, &c.entry.path);
                    }
                }
                for e in &plan.fix_mode {
                    row(ui, "mode", theme::BLUE, &e.path);
                }
                for r in &plan.remove {
                    row(ui, "remove", theme::WEAK, r);
                }
            });
        });
    }

    /// Choosing, adding and editing profiles.
    fn profile_bar(&mut self, ui: &mut egui::Ui) {
        let idle = !self.working();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  Profile", icon::USER)).family(theme::semibold()).color(theme::WEAK));
            let mut pick = None;
            ui.add_enabled_ui(idle, |ui| {
                egui::ComboBox::from_id_salt("profile")
                    .selected_text(RichText::new(&self.settings.profile().name).family(theme::semibold()))
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (i, p) in self.settings.profiles.iter().enumerate() {
                            let detail = match p.username.trim() {
                                "" => platform_name(p.platform).to_string(),
                                u => format!("{u}  ·  {}", platform_name(p.platform)),
                            };
                            let text = format!("{}   {detail}", p.name);
                            if ui.selectable_label(i == self.settings.current, text).clicked() {
                                pick = Some(i);
                            }
                        }
                    })
                    .response
                    .on_hover_text("Each profile has its own server, install, client, account and way of starting it")
            });
            if let Some(i) = pick {
                self.switch_profile(i);
            }
            if ui.add_enabled(idle, egui::Button::new("Edit…")).clicked() {
                self.show_profile = true;
            }
            if ui.add_enabled(idle, egui::Button::new("New")).on_hover_text("A copy of this profile without the account").clicked() {
                self.new_profile();
            }
        });
    }

    /// The header button's icon and tooltip while a patcher update waits (to install or to
    /// restart into).
    fn patcher_pending(&self) -> Option<(&'static str, String)> {
        if let Some(version) = &self.patcher_updated {
            return Some((icon::CHECK_CIRCLE, format!("Patcher {version} is installed: restart to use it")));
        }
        let r = self.patcher_update.as_ref()?;
        Some((icon::CLOUD_DOWN, format!("Patcher {} is available (you have {})", r.version, worker::PATCHER_VERSION)))
    }

    /// A newer build of this patcher, or the one just installed.
    fn patcher_banner(&mut self, ui: &mut egui::Ui) {
        if std::mem::take(&mut self.reveal_patcher_banner) {
            ui.scroll_to_cursor(Some(Align::TOP));
        }
        if let Some(version) = self.patcher_updated.clone() {
            theme::callout(ui, icon::CHECK_CIRCLE, theme::GREEN, &format!("The patcher was updated to {version}. Restart it to use the new version."));
            if ui.button("Restart the patcher").clicked() {
                let ctx = ui.ctx().clone();
                self.restart(&ctx);
            }
            return;
        }
        let Some(r) = self.patcher_update.as_ref().filter(|_| !self.patcher_banner_hidden) else { return };
        let text = format!(
            "The server offers version {} of this patcher (you have {}, {} to download).",
            r.version,
            worker::PATCHER_VERSION,
            human_bytes(r.size)
        );
        theme::callout(ui, icon::CLOUD_DOWN, theme::BLUE, &text);
        ui.horizontal(|ui| {
            let label = if self.patcher_installing { "Updating the patcher…" } else { "Update the patcher" };
            if ui.add_enabled(!self.patcher_installing, egui::Button::new(label)).clicked() {
                self.update_patcher();
            }
            if ui.add_enabled(!self.patcher_installing, egui::Button::new("Not now")).clicked() {
                self.patcher_banner_hidden = true;
            }
        });
    }

    fn profile_window(&mut self, ctx: &egui::Context) {
        if !self.show_profile {
            return;
        }
        let mut delete = false;
        let can_delete = self.settings.profiles.len() > 1;
        let modal = egui::Modal::new(egui::Id::unique("profile")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(440.0_f32.min(ctx.content_rect().width() - 80.0));
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::USER).size(20.0).color(theme::ACCENT));
                ui.label(RichText::new("Profile").family(theme::display()).size(20.0));
            });
            ui.add_space(6.0);
            let p = self.settings.profile_mut();
            theme::field_label(ui, "Name");
            ui.add(theme::text_field(&mut p.name).desired_width(f32::INFINITY));
            ui.label(
                RichText::new("Server, install folder, client and username are set in the main window.")
                    .size(12.0)
                    .color(theme::WEAK),
            );
            theme::field_label(ui, "Start the client with");
            let hint = match settings::default_runner(p.platform) {
                "" => "none (starts it directly)",
                r => r,
            };
            ui.add(theme::text_field(&mut p.runner).hint_text(hint).desired_width(f32::INFINITY));
            ui.label(
                RichText::new("A program to run the client through, e.g. wine, proton run, gamemoderun. Empty starts it directly.")
                    .size(12.0)
                    .color(theme::WEAK),
            );
            theme::field_label(ui, "Extra arguments");
            ui.add(theme::text_field(&mut p.args).hint_text("none").desired_width(f32::INFINITY));
            theme::field_label(ui, "Environment");
            ui.add(
                egui::TextEdit::multiline(&mut p.env)
                    .hint_text("NAME=value, one per line")
                    .desired_rows(3)
                    .font(egui::TextStyle::Monospace)
                    .margin(Margin::symmetric(10, 7))
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let done = theme::big_button(ui, Kind::Primary, "", "Done", true, vec2(96.0, 34.0)).clicked();
                if can_delete
                    && theme::big_button(ui, Kind::Danger, icon::X, "Delete profile", true, vec2(150.0, 34.0))
                        .on_hover_text("Forgets this profile; its install stays on disk")
                        .clicked()
                {
                    delete = true;
                }
                done
            })
            .inner
        });
        let p = self.settings.profile_mut();
        if p.name.trim().is_empty() {
            p.name = "Profile".into();
        }
        if delete {
            self.show_profile = false;
            self.delete_profile();
        } else if modal.inner || modal.should_close() {
            self.show_profile = false;
        }
    }

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let caret = if self.settings.show_log { icon::CARET_DOWN } else { icon::CARET_UP };
            let toggle = ui
                .add(egui::Button::new(RichText::new(format!("{}  Activity  {caret}", icon::LIST)).family(theme::semibold()).color(theme::WEAK)).frame_when_inactive(false))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if toggle.clicked() {
                self.settings.show_log = !self.settings.show_log;
            }
            if let Some((level, line)) = self.log.back().filter(|_| !self.settings.show_log) {
                ui.add(egui::Label::new(RichText::new(line).size(12.0).color(level_color(*level))).truncate());
            }
        });
        if !self.settings.show_log {
            return;
        }
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            if self.log.is_empty() {
                ui.label(RichText::new("Nothing yet.").color(theme::WEAK));
            }
            for (level, line) in &self.log {
                ui.label(RichText::new(line).color(level_color(*level)).monospace());
            }
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let modal = egui::Modal::new(egui::Id::unique("settings")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(380.0_f32.min(ctx.content_rect().width() - 80.0));
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::GEAR).size(20.0).color(theme::ACCENT));
                ui.label(RichText::new("Settings").family(theme::display()).size(20.0));
            });
            ui.add_space(6.0);
            theme::field_label(ui, "Parallel downloads");
            ui.add(egui::Slider::new(&mut self.settings.jobs, 1..=16));
            ui.add_space(4.0);
            ui.checkbox(&mut self.settings.check_patcher_updates, "Offer newer versions of this patcher")
                .on_hover_text("When the server has a newer build signed with the key your install trusts");
            ui.label(RichText::new(format!("{} {}", worker::PATCHER_NAME, worker::PATCHER_VERSION)).size(12.0).color(theme::WEAK));
            if let Some((_, tip)) = self.patcher_pending() {
                ui.label(RichText::new(tip).size(12.0).color(theme::BLUE));
            }
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                theme::big_button(ui, Kind::Primary, "", "Done", true, vec2(96.0, 34.0)).clicked()
            })
            .inner
        });
        if modal.inner || modal.should_close() {
            self.show_settings = false;
        }
    }

    fn trust_window(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.trust else { return };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("trust")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(440.0_f32.min(ctx.content_rect().width() - 80.0));
            let changed = t.expected.is_some();
            ui.horizontal(|ui| {
                if changed {
                    theme::badge(ui, icon::WARNING, theme::RED);
                } else {
                    theme::badge(ui, icon::KEY, theme::ACCENT);
                }
                ui.add_space(4.0);
                let title = if changed { "The server's key changed" } else { "Trust this server?" };
                ui.label(RichText::new(title).family(theme::display()).size(21.0));
            });
            ui.add_space(6.0);
            let body = if changed {
                "Its files are signed with a different key than the one this install trusts. Only accept \
                 it if the server's operator says they changed the key; otherwise someone may be serving \
                 altered files."
            } else {
                "This is the first install from this server. Its files are signed with the key below, and \
                 every later update must be signed with the same key."
            };
            ui.add(egui::Label::new(body).wrap());
            ui.add_space(6.0);
            let key_box = |ui: &mut egui::Ui, label: &str, key: &str, color: Color32| {
                theme::field_label(ui, label);
                egui::Frame::new().fill(theme::FIELD).corner_radius(8).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(group_hex(key)).monospace().size(13.0).color(color));
                });
            };
            if let Some(expected) = &t.expected {
                key_box(ui, "Trusted until now", expected, theme::WEAK);
                key_box(ui, "Now signed with", &t.got, theme::RED);
            } else {
                key_box(ui, "Signing key", &t.got, theme::TEXT);
                ui.label(RichText::new("If the operator published its key, check that it matches.").size(12.0).color(theme::WEAK));
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (kind, accept) = if changed { (Kind::Danger, "Accept the new key") } else { (Kind::Primary, "Trust and continue") };
                if theme::big_button(ui, kind, icon::KEY, accept, true, vec2(190.0, 38.0)).clicked() {
                    decision = Some(true);
                }
                if theme::big_button(ui, Kind::Secondary, "", "Cancel", true, vec2(96.0, 38.0)).clicked() {
                    decision = Some(false);
                }
            });
        });
        if let Some(accept) = decision {
            let t = self.trust.take().unwrap();
            if accept {
                self.settings.trust_key(&t.got);
                self.start(t.req.job, Some(t.got));
            }
        }
    }
}

impl eframe::App for PatcherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        if self.working() {
            // Keep the rate and ETA moving between progress messages.
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        } else {
            let next = self.recheck_at.map_or(self.ping_at, |t| t.min(self.ping_at));
            ui.ctx().request_repaint_after(next.saturating_duration_since(Instant::now()));
        }

        egui::Panel::top("header").frame(egui::Frame::NONE).exact_size(84.0).show(ui, |ui| self.header_ui(ui));

        let log_frame = egui::Frame::new()
            .fill(Color32::from_rgb(0x0a, 0x0c, 0x11))
            .inner_margin(Margin::symmetric(16, 8))
            .stroke(egui::Stroke::new(1.0, theme::CARD_STROKE));
        let log_panel = if self.settings.show_log {
            egui::Panel::bottom("log-open").resizable(true).default_size(150.0).min_size(80.0)
        } else {
            egui::Panel::bottom("log-closed").resizable(false).exact_size(38.0)
        };
        log_panel.frame(log_frame).show(ui, |ui| self.log_ui(ui));

        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin::same(18))).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 14.0;
                self.profile_bar(ui);
                self.patcher_banner(ui);
                theme::card(ui, |ui| self.status_ui(ui));
                self.verified_ui(ui);
                if ui.available_width() >= 600.0 {
                    ui.columns(2, |cols| {
                        theme::card(&mut cols[0], |ui| self.account_ui(ui));
                        theme::card(&mut cols[1], |ui| self.installation_ui(ui));
                    });
                } else {
                    theme::card(ui, |ui| self.installation_ui(ui));
                    theme::card(ui, |ui| self.account_ui(ui));
                }
                self.server_ui(ui);
            });
        });

        let ctx = ui.ctx().clone();
        self.settings_window(&ctx);
        self.profile_window(&ctx);
        self.trust_window(&ctx);
        #[cfg(debug_assertions)]
        dev_screenshot(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }
}

/// "online, 3 players" and its colour, from the API's status.
/// A server kind (`Info::kind`) as shown: a name, and what it means.
fn kind_label(kind: &str) -> (String, &'static str) {
    match kind {
        "progression" => ("Progression".into(), "XP, levels and minifigure ranks, as live"),
        "adventure" => ("Adventure".into(), "Everything unlocked: every minifigure and upgrade from the start"),
        "world-editor" => ("World editor".into(), "A survey server: nothing attacks; place and fix the worlds' content with the dev UI's world editor"),
        other => (other.to_string(), "Named by the server's host"),
    }
}

fn game_line(g: &GameStatus) -> Option<(Color32, String)> {
    let s = g.status.as_ref()?;
    Some(match (s.game_server_online, s.maintenance, s.players_online) {
        (false, ..) => (theme::RED, "offline".into()),
        (true, Some(true), _) => (theme::AMBER, "maintenance".into()),
        (true, _, Some(1)) => (theme::GREEN, "online, 1 player".into()),
        (true, _, Some(n)) => (theme::GREEN, format!("online, {n} players")),
        (true, _, None) => (theme::GREEN, "online".into()),
    })
}

fn dialog_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0, theme::CARD_STROKE))
        .corner_radius(16)
        .inner_margin(24)
        .shadow(egui::Shadow { offset: [0, 10], blur: 40, spread: 0, color: Color32::from_black_alpha(160) })
}

fn level_color(level: Level) -> Color32 {
    match level {
        Level::Info => theme::WEAK,
        Level::Warn => theme::AMBER,
        Level::Error => theme::RED,
    }
}

/// Debug builds: with `LMOXIDE_GUI_SCREENSHOT=<dir>`, F12 saves the window as
/// `<dir>/shot-<n>.ppm` (for checking the layout from scripts).
#[cfg(debug_assertions)]
fn dev_screenshot(ctx: &egui::Context) {
    let Some(dir) = std::env::var_os("LMOXIDE_GUI_SCREENSHOT") else { return };
    if !ctx.input(|i| i.key_pressed(egui::Key::F12)) {
        return;
    }
    let dir = PathBuf::from(dir);
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
        let n = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
        let mut ppm = format!("P6\n{} {}\n255\n", image.size[0], image.size[1]).into_bytes();
        ppm.extend(image.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]));
        let _ = std::fs::write(dir.join(format!("shot-{n}.ppm")), ppm);
    })));
}

/// The client's executable, whose presence marks an install the patcher didn't make.
fn executable_name(p: Platform) -> &'static str {
    match p {
        Platform::Windows => "LMO.exe",
        Platform::Linux => "LMO",
    }
}

fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Windows => "Windows",
        Platform::Linux => "Linux",
    }
}

/// A 64-character key as two lines of two groups of 16.
fn group_hex(key: &str) -> String {
    let groups: Vec<_> = key.as_bytes().chunks(16).map(String::from_utf8_lossy).collect();
    groups.chunks(2).map(|line| line.join(" ")).collect::<Vec<_>>().join("\n")
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

fn human_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min {} s", s / 60, s % 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(3 << 30), "3.0 GiB");
        assert_eq!(human_duration(75.0), "1 min 15 s");
        assert_eq!(group_hex(&"ab".repeat(32)).lines().map(|l| l.len()).collect::<Vec<_>>(), [33, 33]);
    }
}
