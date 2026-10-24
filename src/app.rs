//! The window: server and install settings, status, progress, trust prompts, Play.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};
use lmoxide_patch::{
    Info, Manifest, Platform,
    sync::{Plan, Progress, Reason},
};
use tokio::{runtime::Runtime, task::JoinHandle};

use crate::{
    install,
    settings::Settings,
    worker::{self, Job, Msg, Outbox, Request, Status},
};

const LOG_LINES: usize = 500;

pub struct PatcherApp {
    rt: Runtime,
    settings: Settings,
    rx: Receiver<(u64, Msg)>,
    out: Outbox,
    /// The running job; `job_id` tags its messages.
    task: Option<(Job, JoinHandle<()>)>,
    job_id: u64,
    run: RunState,
    info: Option<Info>,
    status: Option<Status>,
    /// The verified manifest of the current install (for Play).
    installed: Option<Manifest>,
    password: String,
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
}

impl PatcherApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Runtime) -> Self {
        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
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
            status: None,
            installed: None,
            password: String::new(),
            trust: None,
            verified: None,
            log: VecDeque::new(),
        };
        app.location_changed();
        app
    }

    fn dir(&self) -> PathBuf {
        PathBuf::from(self.settings.dir.trim())
    }

    fn busy(&self) -> Option<Job> {
        self.task.as_ref().map(|(j, _)| *j)
    }

    fn log(&mut self, level: Level, msg: impl Into<String>) {
        if self.log.len() == LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back((level, msg.into()));
    }

    /// Server, directory or platform changed: forget what we knew and check again.
    fn location_changed(&mut self) {
        self.installed = install::installed(&self.dir()).filter(|m| m.platform == self.settings.platform);
        self.status = None;
        self.verified = None;
        if self.settings.server.trim().is_empty() {
            self.info = None;
            self.cancel();
        } else {
            self.start(Job::Check, None);
        }
    }

    fn start(&mut self, job: Job, trust: Option<String>) {
        if let Some((_, h)) = self.task.take() {
            h.abort();
        }
        if job != Job::Check {
            self.verified = None;
        }
        self.job_id += 1;
        self.run = RunState::default();
        let req = Request {
            job,
            server: self.settings.server.trim().to_string(),
            dir: self.dir(),
            platform: self.settings.platform,
            jobs: self.settings.jobs,
            trust,
        };
        self.task = Some((job, self.rt.spawn(worker::run(req, self.out.for_job(self.job_id)))));
    }

    fn cancel(&mut self) {
        if let Some((job, h)) = self.task.take() {
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
                let known = self.info.as_ref().map(|i| i.warnings.as_slice()).unwrap_or_default();
                let new: Vec<String> = info.warnings.iter().filter(|w| !known.contains(w)).cloned().collect();
                for w in new {
                    self.log(Level::Warn, format!("server: {w}"));
                }
                self.info = Some(info);
            }
            Msg::Status(s) => self.status = Some(s),
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
            Msg::Failed(e) => self.log(Level::Error, e),
            Msg::PickedDir(_) => unreachable!("handled in drain"),
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
        let finished = self.task.as_ref().is_some_and(|(_, h)| h.is_finished());
        while let Ok((id, msg)) = self.rx.try_recv() {
            match msg {
                Msg::PickedDir(d) => {
                    self.settings.dir = d.display().to_string();
                    self.location_changed();
                }
                msg if id == self.job_id => self.handle(msg),
                _ => {}
            }
        }
        if finished {
            self.task = None;
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

    fn play(&mut self) {
        let Some(m) = &self.installed else { return };
        let runner = if m.platform == Platform::Windows { self.settings.windows_runner.as_str() } else { "" };
        match install::launch(&self.dir(), m, self.settings.username.trim(), &self.password, runner) {
            Ok(()) => self.log(Level::Info, format!("started {}", m.launch.executable)),
            Err(e) => self.log(Level::Error, format!("{e:#}")),
        }
    }

    // ---- drawing ----

    fn location_ui(&mut self, ui: &mut egui::Ui) {
        let idle = !matches!(self.busy(), Some(Job::Sync { .. } | Job::Verify { .. }));
        let mut changed = false;
        egui::Grid::new("location").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Server");
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut self.settings.server).hint_text("http://lmo.example.org:8080");
                let r = ui.add_enabled(idle, edit.desired_width(ui.available_width() - 80.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add_enabled(idle, egui::Button::new("Connect")).clicked() || enter {
                    changed = true;
                }
            });
            ui.end_row();

            ui.label("Install to");
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut self.settings.dir);
                let r = ui.add_enabled(idle, edit.desired_width(ui.available_width() - 80.0));
                changed |= r.lost_focus();
                if ui.add_enabled(idle, egui::Button::new("Browse…")).clicked() {
                    self.browse();
                }
            });
            ui.end_row();

            ui.label("Client");
            ui.add_enabled_ui(idle, |ui| {
                let before = self.settings.platform;
                egui::ComboBox::from_id_salt("platform").selected_text(platform_name(before)).show_ui(ui, |ui| {
                    for p in Platform::ALL {
                        ui.selectable_value(&mut self.settings.platform, p, platform_name(p));
                    }
                });
                changed |= before != self.settings.platform;
            });
            ui.end_row();
        });
        if changed {
            self.location_changed();
        }
    }

    fn server_ui(&mut self, ui: &mut egui::Ui) {
        let Some(info) = &self.info else { return };
        egui::CollapsingHeader::new(format!("Server: {}", info.name)).id_salt("server").show(ui, |ui| {
            egui::Grid::new("info").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Game server");
                ui.monospace(&info.universe_addr);
                ui.end_row();
                ui.label("Clients");
                let platforms: Vec<String> = info
                    .platforms
                    .iter()
                    .map(|p| if info.ready.contains(p) { platform_name(*p).into() } else { format!("{} (indexing)", platform_name(*p)) })
                    .collect();
                ui.label(platforms.join(", "));
                ui.end_row();
                ui.label("Signing key");
                ui.monospace(&info.manifest_key).on_hover_text("ed25519 key the server signs its manifests with");
                ui.end_row();
                ui.label("Login key");
                ui.label(if info.login_public_key.is_some() { "patched into the client" } else { "none (client unpatched)" });
                ui.end_row();
            });
            for w in &info.warnings {
                ui.colored_label(ui.visuals().warn_fg_color, w);
            }
        });
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        let (text, color) = match (self.busy(), &self.status) {
            (Some(Job::Sync { .. }), _) => (if self.installed.is_some() { "Updating…" } else { "Installing…" }.into(), None),
            (Some(Job::Verify { .. }), _) => ("Verifying…".into(), None),
            (_, Some(Status::Indexing)) => ("The server is still preparing its client files…".into(), None),
            (Some(Job::Check), None) => ("Checking the server…".into(), None),
            (_, Some(Status::UpToDate)) => ("Up to date".into(), Some(Color32::from_rgb(0x3c, 0xb3, 0x71))),
            (_, Some(Status::UpdateAvailable)) => ("Update available".into(), Some(ui.visuals().warn_fg_color)),
            (_, Some(Status::NotInstalled { bytes })) => {
                (format!("Not installed ({} to download)", human_bytes(*bytes)), None)
            }
            (_, Some(Status::KeyChanged)) => {
                ("The server's signing key changed".into(), Some(ui.visuals().error_fg_color))
            }
            (None, None) if self.settings.server.trim().is_empty() => ("Enter the patch server's address".into(), None),
            (None, None) if self.installed.is_some() => ("Installed (server not reachable)".into(), None),
            (None, None) => ("Server not reachable".into(), None),
        };
        let mut text = RichText::new(text).size(20.0).strong();
        if let Some(c) = color {
            text = text.color(c);
        }
        ui.label(text);
        if let Some(m) = &self.installed {
            ui.weak(format!("{} client from {}", platform_name(m.platform), m.server));
        }
    }

    fn progress_ui(&mut self, ui: &mut egui::Ui) {
        let Some(job) = self.busy() else { return };
        if job == Job::Check {
            return;
        }
        let r = &mut self.run;
        match r.stage {
            Stage::Connecting => {
                ui.add(egui::ProgressBar::new(0.0).animate(true).text("Fetching the manifest…"));
            }
            Stage::Waiting => {
                ui.add(egui::ProgressBar::new(0.0).animate(true).text("Waiting for the server to finish indexing…"));
            }
            Stage::Checking { done, total } => {
                let f = if total == 0 { 1.0 } else { done as f32 / total as f32 };
                ui.add(egui::ProgressBar::new(f).text(format!("Checking files: {done} / {total}")));
            }
            Stage::Downloading => {
                r.sample();
                let f = if r.total_bytes == 0 { 1.0 } else { r.done_bytes as f32 / r.total_bytes as f32 };
                let mut text = format!("{} / {}", human_bytes(r.done_bytes), human_bytes(r.total_bytes));
                if let Some(rate) = r.rate() {
                    text += &format!("  ·  {}/s", human_bytes(rate as u64));
                    if rate > 0.0 {
                        let left = r.total_bytes.saturating_sub(r.done_bytes) as f64 / rate;
                        text += &format!("  ·  {} left", human_duration(left));
                    }
                }
                ui.add(egui::ProgressBar::new(f).text(text));
                ui.weak(format!("files: {} / {}", r.files_done, r.files_total));
                for p in r.current.iter().take(4) {
                    ui.add(egui::Label::new(RichText::new(p).monospace().weak()).truncate());
                }
            }
        }
    }

    fn actions_ui(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        let working = matches!(busy, Some(Job::Sync { .. } | Job::Verify { .. }));
        let has_server = !self.settings.server.trim().is_empty();
        let big = |s: &str| egui::Button::new(RichText::new(s).size(17.0)).min_size(egui::vec2(130.0, 34.0));

        ui.horizontal(|ui| {
            if working {
                if ui.add(big("Cancel")).clicked() {
                    self.cancel();
                }
            } else {
                let label = if self.installed.is_some() { "Update" } else { "Install" };
                let needed = matches!(
                    self.status,
                    Some(Status::NotInstalled { .. } | Status::UpdateAvailable | Status::KeyChanged)
                );
                let mut b = big(label);
                if needed {
                    b = b.fill(ui.visuals().selection.bg_fill);
                }
                if ui.add_enabled(has_server, b).clicked() {
                    self.start(Job::Sync { full: false }, None);
                }
            }
            let can_play = self.installed.is_some() && !matches!(busy, Some(Job::Sync { .. }));
            let mut play = big("Play");
            if can_play && matches!(self.status, Some(Status::UpToDate)) {
                play = play.fill(ui.visuals().selection.bg_fill);
            }
            if ui.add_enabled(can_play, play).clicked() {
                self.play();
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(!working && has_server, |ui| {
                if ui.button("Verify").on_hover_text("Compare the install with the server's files; change nothing").clicked() {
                    self.start(Job::Verify { full: false }, None);
                }
                if ui
                    .button("Repair")
                    .on_hover_text("Rehash every file and download whatever differs (slow, for damaged installs)")
                    .clicked()
                {
                    self.start(Job::Sync { full: true }, None);
                }
            });
        });
    }

    fn login_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("login").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Username");
            ui.add(egui::TextEdit::singleline(&mut self.settings.username).hint_text("optional: log in automatically"));
            ui.end_row();
            ui.label("Password");
            let r = ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && self.installed.is_some() {
                self.play();
            }
            ui.end_row();
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Settings").id_salt("settings").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Parallel downloads");
                ui.add(egui::DragValue::new(&mut self.settings.jobs).range(1..=16));
            });
            if !cfg!(windows) {
                ui.horizontal(|ui| {
                    ui.label("Run the Windows client with");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.windows_runner).hint_text("wine").desired_width(160.0));
                });
            }
        });
    }

    fn verified_ui(&mut self, ui: &mut egui::Ui) {
        let Some(plan) = &self.verified else { return };
        if plan.is_up_to_date() {
            return;
        }
        let n = plan.download.len() + plan.fix_mode.len() + plan.remove.len();
        egui::CollapsingHeader::new(format!("Verify found {n} differences ({} to download)", human_bytes(plan.download_bytes())))
            .id_salt("verified")
            .show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, what: &str, path: &str| {
                        ui.horizontal(|ui| {
                            ui.add_sized([60.0, 14.0], egui::Label::new(RichText::new(what).weak()));
                            ui.monospace(path);
                        });
                    };
                    for c in &plan.download {
                        row(ui, if c.reason == Reason::Missing { "missing" } else { "changed" }, &c.entry.path);
                    }
                    for e in &plan.fix_mode {
                        row(ui, "mode", &e.path);
                    }
                    for r in &plan.remove {
                        row(ui, "remove", r);
                    }
                });
            });
    }

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for (level, line) in &self.log {
                let color = match level {
                    Level::Info => ui.visuals().weak_text_color(),
                    Level::Warn => ui.visuals().warn_fg_color,
                    Level::Error => ui.visuals().error_fg_color,
                };
                ui.label(RichText::new(line).color(color).monospace());
            }
        });
    }

    fn trust_ui(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.trust else { return };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("trust")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            match &t.expected {
                None => {
                    ui.heading("Trust this server?");
                    ui.label(
                        "This is the first install from this server. Its files are signed with the key \
                         below; later updates must be signed with the same key.",
                    );
                    ui.add_space(4.0);
                    ui.monospace(group_hex(&t.got));
                    ui.add_space(4.0);
                    ui.weak("If the server's operator published its key, check that it matches.");
                }
                Some(expected) => {
                    ui.heading(RichText::new("The server's signing key changed!").color(ui.visuals().error_fg_color));
                    ui.label(
                        "The files are signed with a different key than the one this install trusts. \
                         Only accept it if the server's operator says they changed the key; otherwise \
                         someone may be serving altered files.",
                    );
                    ui.add_space(4.0);
                    ui.label("Trusted:");
                    ui.monospace(group_hex(expected));
                    ui.label("Now:");
                    ui.monospace(group_hex(&t.got));
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let accept = if t.expected.is_none() { "Trust and continue" } else { "Accept the new key" };
                if ui.button(accept).clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
            });
        });
        if let Some(accept) = decision {
            let t = self.trust.take().unwrap();
            if accept {
                self.start(t.req.job, Some(t.got));
            }
        }
    }
}

impl eframe::App for PatcherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        if matches!(self.busy(), Some(Job::Sync { .. } | Job::Verify { .. })) {
            // Keep the rate and ETA moving between progress messages.
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        egui::Panel::bottom("log").resizable(true).default_size(110.0).show(ui, |ui| {
            ui.add_space(4.0);
            self.log_ui(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(4.0);
                self.location_ui(ui);
                self.server_ui(ui);
                ui.separator();
                self.status_ui(ui);
                ui.add_space(4.0);
                self.progress_ui(ui);
                ui.add_space(6.0);
                self.actions_ui(ui);
                self.verified_ui(ui);
                ui.add_space(6.0);
                ui.separator();
                self.login_ui(ui);
                ui.add_space(4.0);
                self.settings_ui(ui);
            });
        });
        let ctx = ui.ctx().clone();
        self.trust_ui(&ctx);
        #[cfg(debug_assertions)]
        dev_screenshot(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
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
