//! The local side: what is installed, and starting it.

use std::{
    path::Path,
    process::{Command, Stdio},
};

use lmoxide_patch::{
    Manifest, Platform, STATE_DIR, SignedManifest,
    client::stored_key,
    sync::safe_join,
};

/// The manifest the last install or update saved (`.lmoxide/manifest.json`), checked against the
/// install's trusted key. `None` if there is no install, or it doesn't verify.
pub fn installed(dir: &Path) -> Option<Manifest> {
    let key = stored_key(dir).ok()??;
    let data = std::fs::read(dir.join(STATE_DIR).join("manifest.json")).ok()?;
    let signed: SignedManifest = serde_json::from_slice(&data).ok()?;
    signed.verify(&key).ok()
}

/// Starts the client as the manifest's `launch` says, in `dir`. With a `username`, the
/// `login_args` log in automatically. `runner` wraps the Windows client on other systems.
pub fn launch(dir: &Path, m: &Manifest, username: &str, password: &str, runner: &str) -> anyhow::Result<()> {
    let l = &m.launch;
    anyhow::ensure!(!l.executable.is_empty(), "the server's manifest doesn't say how to start the client");
    let exe = safe_join(dir, &l.executable)?;
    anyhow::ensure!(exe.is_file(), "{} is missing; update the install", l.executable);

    let mut args: Vec<String> = l.args.clone();
    if !username.is_empty() {
        args.extend(l.login_args.iter().map(|a| a.replace("{username}", username).replace("{password}", password)));
    }
    let mut runner = runner.split_whitespace();
    let mut cmd = match runner.next() {
        Some(prog) if m.platform == Platform::Windows && !cfg!(windows) => {
            let mut c = Command::new(prog);
            c.args(runner).arg(&exe);
            c
        }
        _ => Command::new(&exe),
    };
    cmd.args(&args).envs(&l.env).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("starting {}: {e}", l.executable))?;
    // Reap it when it exits.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Duration};

    use lmoxide_patch::{Launch, manifest::MANIFEST_FORMAT};

    use super::*;

    fn manifest(dir: &Path) -> Manifest {
        let script = "#!/bin/sh\necho \"$PWD|$GREETING|$*\" > out.txt\n";
        std::fs::write(dir.join("LMO"), script).unwrap();
        std::fs::set_permissions(dir.join("LMO"), std::fs::Permissions::from_mode(0o755)).unwrap();
        Manifest {
            format: MANIFEST_FORMAT,
            server: "t".into(),
            platform: Platform::Linux,
            version: Manifest::version_of(&[]),
            generated_at: 0,
            login_public_key: None,
            universe_addr: "h:1".into(),
            files: vec![],
            remove: vec![],
            launch: Launch {
                executable: "LMO".into(),
                args: vec!["-verified".into()],
                login_args: ["-username", "{username}", "-password", "{password}"].map(String::from).to_vec(),
                env: BTreeMap::from([("GREETING".into(), "hi".into())]),
            },
        }
    }

    fn output(dir: &Path) -> String {
        for _ in 0..50 {
            if let Ok(s) = std::fs::read_to_string(dir.join("out.txt")) {
                if s.ends_with('\n') {
                    return s;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the client didn't start");
    }

    #[test]
    fn launches_with_env_and_login() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().canonicalize().unwrap();
        let m = manifest(&d);
        launch(&d, &m, "blake", "p w", "wine").unwrap();
        assert_eq!(output(&d), format!("{}|hi|-verified -username blake -password p w\n", d.display()));

        // no username: no login arguments
        std::fs::remove_file(d.join("out.txt")).unwrap();
        launch(&d, &m, "", "secret", "").unwrap();
        assert_eq!(output(&d), format!("{}|hi|-verified\n", d.display()));
    }

    #[test]
    fn refuses_missing_or_unsafe_executables() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = manifest(dir.path());
        m.launch.executable = "../LMO".into();
        assert!(launch(dir.path(), &m, "", "", "").is_err());
        m.launch.executable = "Missing".into();
        assert!(launch(dir.path(), &m, "", "", "").is_err());
    }
}
