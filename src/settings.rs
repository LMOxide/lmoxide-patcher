//! What the window remembers between runs (eframe's storage, `~/.local/share/lmoxide-patcher/`
//! on Linux). Never the password.

use lmoxide_patch::Platform;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Patch server URL, e.g. `http://192.168.5.69:8080`.
    pub server: String,
    /// Install directory.
    pub dir: String,
    pub platform: Platform,
    /// Parallel downloads.
    pub jobs: usize,
    /// Remembered for automatic login.
    pub username: String,
    /// Command the Windows client is started with on other systems (e.g. `wine`); empty runs
    /// `LMO.exe` directly.
    pub windows_runner: String,
    /// The activity log is expanded.
    pub show_log: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server: std::env::var("LMOXIDE_SERVER").unwrap_or_default(),
            dir: lmoxide_patch::default_install_dir().map(|d| d.display().to_string()).unwrap_or_default(),
            platform: Platform::host(),
            jobs: 4,
            username: String::new(),
            windows_runner: if cfg!(windows) { String::new() } else { "wine".into() },
            show_log: false,
        }
    }
}
