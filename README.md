# LMOxide Patcher (GUI)

A small desktop window that installs, updates, verifies and starts a LEGO Minifigures Online
client from an [LMOxide](https://github.com/LMOxide/lmoxide) patch server. It is the graphical
face of the `lmoxide-patch` crate, the same library the `lmoxide-patcher` command-line tool uses,
so both read and write the same install directories.

Built with Rust and [egui](https://github.com/emilk/egui) (eframe, OpenGL renderer): one native
binary, no web view. The look (dark theme, Inter type, Phosphor icons, the hand-drawn buttons,
cards and progress bar) lives in `src/theme.rs`; fonts are embedded, so nothing is needed at run
time.

## Building

The patcher library comes from the lmoxide repository by path, so check both out side by side:

```
lego-minifigures-online/
├── lmoxide/                 # server monorepo (crates/lmoxide-patch)
└── lmoxide-patcher-gui/     # this repo
```

```sh
cargo build --release        # target/release/lmoxide-patcher-gui
cargo test
```

On Linux, the window works under Wayland and X11. The folder picker ("Browse…") uses the XDG desktop
portal; without one, type the path instead.

## Using it

The window shows one status card at the top (what state the install is in, progress while
installing, and the main button), an **Account** and an **Installation** card, and the server's
details. The header's chip says whether the server answered; **Activity** at the bottom expands
into the log.

1. **Patch server** (Installation): the patch server's address, e.g. `http://192.168.5.69:8080` (a bare
   `host:port` means `http://`). `LMOXIDE_SERVER` fills it on the first start. Press Enter or
   the refresh button: the window shows the server's name, game address, clients on offer and
   any warnings, and whether the install is current. While the server is still indexing, it
   checks again every 10 seconds.
2. **Install folder**: the install directory (default `~/Games/LMO`). **Client**: Linux or
   Windows build (default: this system's).
3. **Install / Update** downloads whatever differs from the server's signed manifest and deletes
   the files the server removes. **Cancel** stops; the next update resumes interrupted downloads.
4. **Verify** compares the install with the manifest without changing anything and lists the
   differences. **Repair** rehashes every file (instead of trusting unchanged size and
   modification time) and fixes whatever differs.
5. **Play** starts the client as the manifest says (executable, arguments, environment). With a
   **Username**, the client logs in automatically with that name and the password typed in the
   window. The username is remembered; the password never is.

Enter (outside a text field) presses the yellow button. The gear opens the settings: parallel
downloads, and the command that runs the Windows client on other systems (`wine` by default).

### Trusting a server

Manifests are signed with the server's ed25519 key. The first install from a server shows that
key and asks before trusting it (the CLI trusts it silently); it is then stored in
`<dir>/.lmoxide/server-key`. If a later manifest is signed with a different key, the window says
so and shows both keys; only accept the new one if the server's operator changed it. File bodies
are checked against the signed sha256, so plain HTTP is safe against tampering.

Everything the patcher keeps lives in `<dir>/.lmoxide/` (trusted key, hash cache, last manifest,
partial downloads), exactly as with `lmoxide-patcher`; see `docs/PATCHING.md` in the lmoxide
repo. Window settings are kept by eframe (`~/.local/share/lmoxide-patcher/` on Linux).

## Layout

| File | |
|---|---|
| `src/main.rs` | window and tokio runtime setup |
| `src/app.rs` | the window: header, status and progress, cards, log, settings and trust dialogs |
| `src/theme.rs` | colours, fonts, window icon and the hand-drawn widgets |
| `src/worker.rs` | background jobs (check, install/update/repair, verify) reporting over a channel |
| `src/install.rs` | the installed manifest and starting the client |
| `src/settings.rs` | remembered settings |
| `assets/fonts/` | Inter and Phosphor, subset to Latin and the icons used |
| `docs/API-PROPOSALS.md` | changes to lmoxide this patcher would benefit from |

## Development

Debug builds take screenshots for scripted checks: run with
`LMOXIDE_GUI_SCREENSHOT=<dir>` and press F12 to save `<dir>/shot-<n>.ppm`. Under X11 (unset
`WAYLAND_DISPLAY`) vsync is then off, so this also works with the window on a hidden workspace.
Test against a patch server on spare ports with a scratch data dir, e.g.
`lmoxide-patch-server --http-port 18080 --linux-client-dir <dir> --data-dir <scratch>`.

## License

MIT, like LMOxide. The embedded fonts keep their own licenses: Inter (SIL Open Font License
1.1, `assets/fonts/Inter-LICENSE.txt`) and Phosphor Icons (MIT,
`assets/fonts/Phosphor-LICENSE.txt`).
