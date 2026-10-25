# Development

[← README](../README.md)

```sh
cargo build                  # debug build
cargo test
```

`cargo test` does not rebuild the app binary; run `cargo build` before trying a change by hand.

## Layout

| File | |
|---|---|
| `src/main.rs` | window and tokio runtime setup |
| `src/app.rs` | the window: header, profile bar, patcher update banner, status and progress, cards, log, settings, profile and trust dialogs |
| `src/theme.rs` | colours, fonts, window icon and the hand-drawn widgets (buttons, cards, progress bar) |
| `src/worker.rs` | background jobs (check, install/update/repair, verify) reporting over a channel |
| `src/install.rs` | starting the client (`Launch::command` plus the Windows runner) |
| `src/settings.rs` | remembered settings and profiles |
| `src/secrets.rs` | remembered passwords in the system credential store |
| `assets/fonts/` | Inter and Phosphor, subset to Latin and the icons used |

Adding an icon means re-subsetting the Phosphor font in `assets/fonts/`.

<details>
<summary>Settings format gotchas</summary>

eframe stores settings as RON: `#[serde(flatten)]` doesn't work, and an `Option` field is
written as `Some(...)`. The tests use the `ron` crate to check that old settings still load.

</details>

## Scripted screenshots

Debug builds save the window on F12 when started with `LMOXIDE_GUI_SCREENSHOT=<dir>`
(`<dir>/shot-<n>.ppm`). Under X11 (`env -u WAYLAND_DISPLAY`) vsync is then off, so this also
works with the window on a hidden workspace.

<details>
<summary>Testing without touching your own settings or install</summary>

- Override both `HOME` and `XDG_DATA_HOME`: eframe reads its settings from `XDG_DATA_HOME`, so
  `HOME` alone loads your real profile.
- Run a patch server on spare ports with its own scratch data dir; two servers sharing a
  `--data-dir` refuse to start:

  ```sh
  lmoxide-patch-server --http-port 18180 --linux-client-dir <client> --data-dir <scratch>
  ```

- A small fake client (a few files plus a real `LMO` executable and `LocalConfig.xml`) installs
  in seconds and gives the server no warnings.
- To save settings, close the window instead of killing the process (`kill` skips eframe's
  save on exit).

</details>

## Screenshots in the docs

The images in `docs/screenshots/` come from a debug build at 840×1100, run against a mock patch
server ("Brickville") with a fake client, using the F12 capture above.
