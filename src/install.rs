//! Starting the installed client.

use std::{path::Path, process::Stdio};

use lmoxide_patch::{Manifest, Platform};

/// Starts the client as the manifest's `launch` says, in `dir`. With a `username`, the
/// `login_args` log in automatically. `runner` wraps the Windows client on other systems.
pub fn launch(dir: &Path, m: &Manifest, username: &str, password: &str, runner: &str) -> anyhow::Result<()> {
    let wrapper: Vec<&str> =
        if m.platform == Platform::Windows && !cfg!(windows) { runner.split_whitespace().collect() } else { Vec::new() };
    let login = (!username.is_empty()).then_some((username, password));
    let mut cmd = m.launch.command(dir, login, &wrapper)?;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("starting {}: {e}", m.launch.executable))?;
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
            if let Ok(s) = std::fs::read_to_string(dir.join("out.txt"))
                && s.ends_with('\n')
            {
                return s;
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
