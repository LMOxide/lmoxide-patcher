//! Saved passwords, in the system's credential store (the Secret Service, e.g. GNOME Keyring or
//! KWallet, on Linux; Credential Manager on Windows; the Keychain on macOS), never in the
//! settings file. One entry per account and patch server.
//!
//! Every call talks to the store and may wait on it (an unlock prompt), so these run on their
//! own thread (under tokio the Secret Service client deadlocks on a runtime thread) and answer
//! with a message.

use keyring::{Entry, Error};

use crate::worker::{Msg, Outbox};

const SERVICE: &str = "lmoxide-patcher";

/// The account's entry: `username@server`, with the server as typed minus `http://` and a
/// trailing `/` (a bare `host:port` means `http://`).
fn entry(server: &str, username: &str) -> keyring::Result<Entry> {
    Entry::new(SERVICE, &format!("{}@{}", username.trim(), server_key(server)))
}

pub fn server_key(server: &str) -> &str {
    let s = server.trim().trim_end_matches('/');
    s.strip_prefix("http://").unwrap_or(s)
}

/// Fetches the saved password: [`Msg::Password`], `None` if there is none.
pub fn load(server: String, username: String, out: Outbox) {
    std::thread::spawn(move || {
        let result = match entry(&server, &username).and_then(|e| e.get_password()) {
            Ok(p) => Ok(Some(p)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("reading the saved password for {username}: {e}")),
        };
        out.send(Msg::Password { server, username, result });
    });
}

/// Saves the password (`Some`) or forgets it (`None`): [`Msg::PasswordStored`] with whether one
/// is saved now.
pub fn store(server: String, username: String, password: Option<String>, out: Outbox) {
    std::thread::spawn(move || {
        let result = entry(&server, &username).and_then(|e| match &password {
            Some(p) => e.set_password(p).map(|()| true),
            None => match e.delete_credential() {
                Ok(()) | Err(Error::NoEntry) => Ok(false),
                Err(e) => Err(e),
            },
        });
        let result = result.map_err(|e| {
            let what = if password.is_some() { "saving" } else { "forgetting" };
            format!("{what} the password for {username}: {e}")
        });
        out.send(Msg::PasswordStored { server, username, result });
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn server_key() {
        for s in ["http://h:8080", "h:8080", " http://h:8080/ "] {
            assert_eq!(super::server_key(s), "h:8080");
        }
        assert_eq!(super::server_key("https://h"), "https://h");
    }

    /// Uses the real credential store: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn round_trip() {
        let (tx, rx) = std::sync::mpsc::channel();
        let out = super::Outbox::new(tx, eframe::egui::Context::default());
        let (server, user) = ("test.invalid:1".to_string(), "lmoxide-test".to_string());
        super::store(server.clone(), user.clone(), Some("p w".into()), out.clone());
        assert!(matches!(rx.recv().unwrap().1, super::Msg::PasswordStored { result: Ok(true), .. }));
        super::load(server.clone(), user.clone(), out.clone());
        assert!(matches!(rx.recv().unwrap().1, super::Msg::Password { result: Ok(Some(p)), .. } if p == "p w"));
        super::store(server.clone(), user.clone(), None, out.clone());
        assert!(matches!(rx.recv().unwrap().1, super::Msg::PasswordStored { result: Ok(false), .. }));
        super::load(server, user, out);
        assert!(matches!(rx.recv().unwrap().1, super::Msg::Password { result: Ok(None), .. }));
    }
}
