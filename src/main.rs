//! `lmoxide-patcher-gui`: installs, updates, verifies and starts a LEGO Minifigures Online client
//! from an LMOxide patch server. The window over `lmoxide_patch` (the library behind the
//! `lmoxide-patcher` CLI; protocol: `docs/PATCHING.md` in the lmoxide repo).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod install;
mod settings;
mod theme;
mod worker;

fn main() -> eframe::Result {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_app_id("lmoxide-patcher")
            .with_title("LMOxide Patcher")
            .with_icon(theme::window_icon())
            .with_inner_size([760.0, 720.0])
            .with_min_inner_size([520.0, 520.0]),
        // Debug screenshots (app.rs) of a window on a hidden workspace: under X11 without
        // vsync it keeps drawing (Wayland paces by frame callbacks, which hidden windows lack).
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: !(cfg!(debug_assertions) && std::env::var_os("LMOXIDE_GUI_SCREENSHOT").is_some()),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        "lmoxide-patcher",
        options,
        Box::new(move |cc| Ok(Box::new(app::PatcherApp::new(cc, runtime)))),
    )
}
