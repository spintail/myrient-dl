<p align="center">
  <img src="logo.svg" alt="myrient-dl" width="480"/>
</p>

<p align="center">
  <strong>A native Linux desktop downloader for <a href="https://myrient.erista.me">myrient.erista.me</a></strong><br/>
  Built with Rust + egui. No browser. No Python runtime. One binary.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/rust-1.75%2B-orange?style=flat-square&logo=rust" alt="Rust 1.75+"/>
  <img src="https://img.shields.io/badge/platform-linux%20%7C%20windows%20%7C%20macos-blue?style=flat-square&logo=linux" alt="Linux | Windows | macOS"/>
  <img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="MIT License"/>
  <img src="https://img.shields.io/badge/egui-0.27-purple?style=flat-square" alt="egui 0.27"/>
</p>

---

## Overview

**myrient-dl** is a native cross-platform GUI application for browsing and downloading content from [Myrient](https://myrient.erista.me) — a free preservation archive hosting ROM sets, disc images, and software collections. It replaces the tedious process of navigating a web browser, manually copying links, and running wget commands by hand.

The app browses Myrient's directory listings directly, lets you queue individual files or entire folders, and downloads everything concurrently in the background using `wget1` — while showing live progress bars, download speeds, and ETA for every active transfer.

Everything is written in **Rust** and uses **egui** for a lightweight, native-feeling dark UI. No Electron, no web view, no runtime dependencies beyond the binary itself.

---

## Features

### Browsing
- **Live directory browser** — fetches and renders Myrient's Apache directory listings directly, with folder/file icons, file sizes, and modification dates
- **Breadcrumb navigation** — click any level of the path to jump back up the tree
- **Filter bar** — type to instantly filter the current directory by name; folders and files that don't match are hidden. Press Escape or click `✕` to clear. Match count shown as you type
- **Select All** — a button in the column header queues all visible unqueued files in one click, respecting the active filter (e.g. filter for "mario" then select all to queue only matching files)
- **Baked-in folder sizes** — top-level folder sizes are pre-calculated and compiled into the binary via `fetch_sizes`, so you can see at a glance how large each collection is before downloading
- **Virtual scrolling** — only visible rows are rendered, keeping memory usage low even in directories with thousands of entries

### Queue management
- **Click to queue** — click any file to instantly add it to the download queue; downloads begin automatically
- **Queue entire folders** — hover any folder to reveal a `+ folder` button that recursively scans and queues every file within it as individual jobs
- **DLC & update auto-queuing** — when you queue a game file, myrient-dl automatically searches the current directory for related DLC, update, and patch files and silently adds them to the queue
- **Persistent queue** — the queue is saved to `~/.local/share/myrient-dl/queue.json` on every change and restored when you reopen the app
- **Pause & resume** — pause any active download; it will resume with `wget1 -c` from where it left off

### Downloads
- **Concurrent downloads** — configurable 1–16 simultaneous downloads via a toolbar slider, showing `N threads` live as you adjust
- **Correct folder structure** — `--cut-dirs=1` strips only the `/files/` prefix; downloaded files land at `<dest>/<collection>/<subfolder>/...` preserving the full Myrient path hierarchy
- **Automatic retries** — configurable retry count (default 3) with exponential backoff (2s, 4s, 8s…) on failure
- **Checksum verification** — after each download completes, myrient-dl fetches the `.md5` or `.sfv` sidecar file if available and verifies the download, showing `✓` or `⚠` in the queue
- **Disk space check** — before starting, `statvfs` checks available space against the estimated download size and warns in the log if it's tight

### Progress & monitoring
- **Live progress bars** — each active download shows a progress bar with percentage, auto-scaled speed (`KB/s`, `MB/s`, `GB/s`), and ETA
- **Spooling animation** — an animated sweep shows the download is starting before the first progress data arrives from wget1
- **Active downloads panel** — a dedicated panel above the main content shows all in-progress downloads, scaling in height with the number of concurrent jobs
- **Event log** — a collapsible log panel at the bottom shows timestamped events: queued, started, progress milestones, completion, errors. Auto-scrolls during active downloads

### Settings
- **Persistent settings** — destination path, concurrency, retry count, and checksum toggle are saved to `~/.local/share/myrient-dl/settings.json`
- **Folder picker** — click the `📁` button next to the destination field to open a native GTK folder chooser dialog
- **Verify checksums toggle** — enable/disable post-download MD5/SFV verification per session

---

## Installation

### Prerequisites

```bash
# Fedora / RHEL
sudo dnf install rust cargo openssl-devel gtk3-devel \
                 wayland-devel libxkbcommon-devel mesa-libGL-devel \
                 libX11-devel libXcursor-devel libXrandr-devel libXi-devel

# Ubuntu / Debian
sudo apt install cargo libssl-dev libgtk-3-dev \
                 libwayland-dev libxkbcommon-dev libgl1-mesa-dev \
                 libx11-dev libxcursor-dev libxrandr-dev libxi-dev
```

You'll also need [`wget1`](https://www.gnu.org/software/wget/) installed and on your `PATH`. On most distributions this is provided by the `wget` package — check whether your distro ships `wget` as `wget1` or just `wget`, and adjust accordingly.

### Build

```bash
git clone https://github.com/yourusername/myrient-dl
cd myrient-dl

# Optional but recommended: pre-calculate folder sizes
# This crawls Myrient recursively (~5–10 minutes) and bakes the results
# into generated_sizes.rs so they're available at compile time.
cargo run --bin fetch_sizes

# Build the main app
cargo build --release

# Run
./target/release/myrient-dl
```

The release binary is fully self-contained — copy it anywhere and run it.

---

## Folder size data

Myrient's directory listings show `-` for folder sizes, so myrient-dl includes a companion tool that crawls the site, sums all file sizes recursively, and writes the results as a compile-time Rust source file.

```bash
cargo run --bin fetch_sizes
# → writes src/generated_sizes.rs
# → rebuild the app to include the new data
cargo build --release
```

You only need to do this once (or whenever Myrient's content changes significantly). The baked-in data is committed to the repo so you can also just build without running `fetch_sizes` — folder sizes will show `—` for any folder not in the data.

---

## Usage

### Browsing and queuing files

1. Launch the app — it opens at the Myrient `/files/` root
2. Click folders to navigate in; use the breadcrumb bar to go back up
3. Click any file row to queue it — it starts downloading immediately
4. Hover a folder row to reveal `→ open` and `+ folder` buttons
   - `→ open` navigates into the folder
   - `+ folder` recursively scans and queues every file within it

### Managing downloads

- The **Active Downloads** panel appears above the browser when downloads are running, showing a live progress bar per file
- Click **⏸ pause** on any active download to pause it; the queue row shows a **▶ resume** button to continue
- The **Queue** panel on the right shows all jobs with their status; click `✕` to remove completed or failed jobs
- **Clear done** removes all finished and errored jobs from the queue
- **▶ Start All** manually kicks off any waiting or paused jobs (useful if you've changed the destination path)

### Settings

All settings persist between sessions automatically:

| Setting | Default | Description |
|---|---|---|
| Dest | `~/Downloads/myrient` | Root folder for all downloads |
| Concurrent | 4 | Simultaneous wget1 processes |
| Retries | 3 | Retry attempts on failure (exponential backoff) |
| Verify | ✓ | Check MD5/SFV sidecar files after download |

---

## Project structure

```
myrient-dl/
├── Cargo.toml
└── src/
    ├── main.rs              # Full application (~1500 lines)
    ├── generated_sizes.rs   # Baked folder sizes (written by fetch_sizes)
    └── bin/
        └── fetch_sizes.rs   # Standalone crawler — run once to populate sizes
```

---

## How it works

### Architecture

The app uses a simple shared-state model:

```
┌─────────────┐     browse       ┌──────────────┐
│  egui UI    │ ──── thread ────▶ │  reqwest +   │
│  (main      │                  │  scraper     │
│   thread)   │ ◀── Arc<Mutex> ─ │  (worker     │
└─────────────┘   shared state   │   threads)   │
      │                          └──────────────┘
      │ DlCmd channel
      ▼
┌─────────────────┐   semaphore   ┌──────────────┐
│ Download manager│ ─────────────▶│  wget1       │
│ thread          │               │  subprocess  │
│ (owns kill_tx   │ ◀── stderr ── │  (per job)   │
│  per job)       │               └──────────────┘
└─────────────────┘
```

- **UI thread** — runs egui's update loop, polls `Arc<Mutex<Shared>>` for results, sends `DlCmd` to the download manager
- **Browse threads** — spawned per navigation, fetch + parse HTML via `reqwest` + `scraper`, write results back to shared state
- **Download manager thread** — owns a `Semaphore` for concurrency limiting, spawns one thread per job, manages kill signals for pause/cancel
- **Progress threads** — one per active download, reads wget1's stderr line by line and parses `--progress=dot:mega` output for percentage, speed, and ETA

### Memory optimisations

- `Arc<str>` for all URL and filename strings in `DirEntry` — avoids cloning on every render frame
- Single shared `reqwest::Client` (via `once_cell::Lazy`) — connection pool shared across all requests
- Virtual scrolling in the browser — only rows in the visible viewport are allocated
- `VecDeque` log capped at 500 entries — bounded memory regardless of session length

---

## Dependencies

| Crate | Purpose |
|---|---|
| `eframe` / `egui` | Native GUI |
| `reqwest` | HTTP client (blocking) |
| `scraper` | HTML parsing |
| `serde` / `serde_json` | Settings and queue persistence |
| `shellexpand` | `~/` expansion in paths |
| `rfd` | Native GTK folder picker |
| `chrono` | Log timestamps |
| `md5` | Checksum verification |
| `nix` | `statvfs` disk space check |
| `rayon` | Parallel folder crawling in `fetch_sizes` |
| `once_cell` | Lazy static HTTP client and folder size map |
| `regex-lite` | DLC/update name matching |

---

## Contributing

Pull requests welcome. A few areas that would make good contributions:

- **Import/export** queue as a plain text URL list
- **Torrent support** — Myrient provides `.torrent` files for some collections
- **Notification** on download completion (via `libnotify`)
- **System tray** integration for background downloading
- **Bandwidth limiting** — pass `--limit-rate` to wget1

---

## License

MIT — see [LICENSE](LICENSE)

---

## Acknowledgements

[Myrient](https://myrient.erista.me) is a free, community-run preservation archive. If you find it useful, please consider [donating](https://myrient.erista.me/donate/) to help keep it running.
