# Logpose

I keep a lot of side projects going at once, and I kept losing track of where I'd left each one.
Logpose is a small desktop app that scans the folders where your projects live and shows them on one
board: how long each has been idle, what's left uncommitted or unpushed, and what you were doing last
time, based on your Claude Code and Codex session logs.

Most of it runs locally and costs nothing: git state, file timestamps and the agent logs are read
straight from disk. The only part that spends tokens is the optional per-project summary ("where you
are / next steps"), which runs `claude -p` or `codex exec` on your existing subscription and is cached
until the project changes.

## What it does

- Finds projects under the roots you pick: git repos, folders with `CLAUDE.md` / `AGENTS.md`, and
  anything that shows up as a working directory in your agent logs. Nested repos are grouped under
  their parent.
- Scores each project for staleness and for "mess" (uncommitted files, unpushed commits, stashes, a
  rebase left half-way, and so on).
- Board view with drag and drop, plus a list view. Clicking a card opens a detail panel with
  recent commits, sessions, a parking note and the summary.
- Opens a project in your editor or file manager, or starts Claude Code / Codex in a terminal there
  with the summary as the first message.
- Stats from git history and session logs: activity per week, sessions per agent, busiest projects.
- Optional tray mode that watches for changes, and a weekly digest notification.
- Removing a project you're done with: it shows what will go (folder, Docker leftovers, sizes) and
  moves the folder to the Trash rather than deleting it.

Nothing leaves your machine except the summary calls you trigger yourself.

## Install

Linux builds are attached to each [release](https://github.com/Zannael/logpose/releases):

- **Debian / Ubuntu:** `sudo apt install ./Logpose_0.1.1_amd64.deb`
- **Fedora / openSUSE:** `sudo dnf install ./Logpose-0.1.1-1.x86_64.rpm`
- **Anything else:** download the AppImage, `chmod +x` it and run it.

The code has macOS and Windows paths in it, but I haven't built or tried it on either yet.

On first launch it asks which folders to scan. Settings go to `~/.config/logpose/config.toml` and the
local database to `~/.local/share/logpose/`. Agent logs are read from `~/.claude` and `~/.codex`, or
from `CLAUDE_CONFIG_DIR` / `CODEX_HOME` if set.

Summaries need the `claude` or `codex` CLI installed and logged in. Logpose looks for them on `PATH` and
in the usual install folders (`~/.local/bin`, npm, nvm, Homebrew…); if yours lives somewhere else, set
its path in Settings → Agents. Everything else works without them.

## Building from source

You need Rust 1.90+, Node with pnpm, and the
[Tauri 2 Linux prerequisites](https://tauri.app/start/prerequisites/) (webkit2gtk 4.1 and friends).

```sh
cd app
pnpm install
pnpm tauri dev      # run it
pnpm tauri build    # .deb, .rpm and AppImage in target/release/bundle/
```

There's also a small CLI that prints the scan without the GUI:

```sh
cargo run -p logpose-cli --release -- scan
```

Tests: `cargo test` for the Rust side, `pnpm check` in `app/` for the frontend.

## Layout

- `crates/logpose-core` – discovery, git scanning, log readers, scoring, stats, SQLite store. No GUI deps.
- `crates/logpose-cli` – the CLI above.
- `app/` – the Tauri 2 + SvelteKit desktop app.
- `tests/fixtures` – synthetic Claude Code and Codex logs used by the tests.

## License

MIT
