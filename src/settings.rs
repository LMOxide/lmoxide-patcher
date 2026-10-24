//! What the window remembers between runs (eframe's storage, `~/.local/share/lmoxide-patcher/`
//! on Linux). Never the password.

use std::collections::BTreeMap;

use lmoxide_patch::Platform;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// At least one (see [`Settings::normalize`]).
    pub profiles: Vec<Profile>,
    /// Index of the profile in use.
    pub current: usize,
    /// Parallel downloads.
    pub jobs: usize,
    /// The activity log is expanded.
    pub show_log: bool,
    /// Offer newer builds of this patcher when the server has them.
    pub check_patcher_updates: bool,
    // The first version's flat settings, moved into the first profile on load (plain fields,
    // not a flattened struct: eframe stores RON, which can't read flattened structs back).
    #[serde(skip_serializing, deserialize_with = "plain")]
    server: Option<String>,
    #[serde(skip_serializing, deserialize_with = "plain")]
    dir: Option<String>,
    #[serde(skip_serializing, deserialize_with = "plain")]
    platform: Option<Platform>,
    #[serde(skip_serializing, deserialize_with = "plain")]
    username: Option<String>,
    #[serde(skip_serializing, deserialize_with = "plain")]
    windows_runner: Option<String>,
}

/// A value saved as itself (RON would want `Some(…)` for an `Option`).
fn plain<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// One way to play: a server, an install of one client, an account and how to start it. Several
/// profiles may share an install (e.g. two accounts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// Patch server URL, e.g. `http://192.168.5.69:8080`.
    pub server: String,
    /// Install directory.
    pub dir: String,
    pub platform: Platform,
    /// Remembered for automatic login.
    pub username: String,
    /// Program (and arguments) the client is started with, e.g. `wine`, `proton run`,
    /// `gamemoderun`; empty starts it directly.
    pub runner: String,
    /// Extra arguments after the server's own (whitespace-separated).
    pub args: String,
    /// Extra environment variables, `NAME=value` per line.
    pub env: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            current: 0,
            jobs: 4,
            show_log: false,
            check_patcher_updates: true,
            server: None,
            dir: None,
            platform: None,
            username: None,
            windows_runner: None,
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        let platform = Platform::host();
        Self {
            name: "Default".into(),
            server: std::env::var("LMOXIDE_SERVER").unwrap_or_default(),
            dir: lmoxide_patch::default_install_dir().map(|d| d.display().to_string()).unwrap_or_default(),
            platform,
            username: String::new(),
            runner: default_runner(platform).into(),
            args: String::new(),
            env: String::new(),
        }
    }
}

/// `wine` for the Windows client on other systems, nothing otherwise.
pub fn default_runner(platform: Platform) -> &'static str {
    if platform == Platform::Windows && !cfg!(windows) { "wine" } else { "" }
}

impl Settings {
    /// Makes loaded settings usable: moves first-version settings into a profile, makes sure
    /// there is one, keeps `current` in range.
    pub fn normalize(&mut self) {
        let (server, dir, platform, username, runner) =
            (self.server.take(), self.dir.take(), self.platform.take(), self.username.take(), self.windows_runner.take());
        if self.profiles.is_empty() {
            let mut p = Profile::default();
            if let Some(platform) = platform {
                p.platform = platform;
                p.runner = default_runner(platform).into();
            }
            p.server = server.unwrap_or(p.server);
            p.dir = dir.unwrap_or(p.dir);
            p.username = username.unwrap_or_default();
            if p.platform == Platform::Windows && !cfg!(windows) {
                p.runner = runner.unwrap_or(p.runner);
            }
            self.profiles.push(p);
        }
        self.current = self.current.min(self.profiles.len() - 1);
    }

    pub fn profile(&self) -> &Profile {
        &self.profiles[self.current]
    }

    pub fn profile_mut(&mut self) -> &mut Profile {
        &mut self.profiles[self.current]
    }

    /// A name not used by another profile: `base`, else `base 2`, `base 3`…
    pub fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.profiles.iter().any(|p| p.name == n);
        if !taken(base) {
            return base.into();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
    }
}

impl Profile {
    /// The runner as a program and its arguments (empty: start the client directly).
    pub fn runner_words(&self) -> Vec<&str> {
        self.runner.split_whitespace().collect()
    }

    pub fn extra_args(&self) -> Vec<&str> {
        self.args.split_whitespace().collect()
    }

    /// `NAME=value` lines; blank lines and lines without `=` are skipped.
    pub fn extra_env(&self) -> BTreeMap<&str, &str> {
        self.env
            .lines()
            .filter_map(|l| l.trim().split_once('='))
            .filter(|(k, _)| !k.trim().is_empty())
            .map(|(k, v)| (k.trim(), v))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_version_settings_become_a_profile() {
        // as the first version saved them (eframe storage is RON)
        let old = r#"(server:"http://h:8080",dir:"/games/lmo",platform:windows,jobs:6,username:"blake",windows_runner:"proton run",show_log:true)"#;
        let mut s: Settings = ron::from_str(old).unwrap();
        s.normalize();
        assert_eq!(s.profiles.len(), 1);
        let p = s.profile();
        assert_eq!((p.server.as_str(), p.dir.as_str(), p.platform, p.username.as_str()), ("http://h:8080", "/games/lmo", Platform::Windows, "blake"));
        if !cfg!(windows) {
            assert_eq!(p.runner, "proton run");
        }
        assert_eq!((s.jobs, s.show_log), (6, true));
        // saved again: no legacy fields, and it reads back
        s.profiles.push(Profile { name: "second".into(), ..Default::default() });
        s.current = 1;
        let saved = ron::to_string(&s).unwrap();
        assert!(!saved.contains("windows_runner"));
        let mut again: Settings = ron::from_str(&saved).unwrap();
        again.normalize();
        assert_eq!((again.profiles, again.current), (s.profiles, 1));
    }

    #[test]
    fn current_stays_in_range() {
        let mut s: Settings = ron::from_str(r#"(profiles:[(name:"a")],current:5)"#).unwrap();
        s.normalize();
        assert_eq!(s.current, 0);
        assert_eq!(s.unique_name("a"), "a 2");
        assert_eq!(s.unique_name("b"), "b");
    }

    #[test]
    fn launch_options() {
        let p = Profile { args: " -windowed  -fps 60 ".into(), env: "DXVK_HUD=fps\n\nbad line\n=x\nWINEDEBUG=-all".into(), ..Default::default() };
        assert_eq!(p.extra_args(), ["-windowed", "-fps", "60"]);
        assert_eq!(p.extra_env(), BTreeMap::from([("DXVK_HUD", "fps"), ("WINEDEBUG", "-all")]));
    }
}
