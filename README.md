<p align="center">
  <img src="logo.svg" alt="myrient-dl" width="480"/>
</p>

<p align="center">
  <strong>A native cross-platform desktop downloader for <a href="https://myrient.erista.me">myrient.erista.me</a></strong><br/>
  Built with Rust + egui. No browser. No Python. No external tools. One binary.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/rust-1.75%2B-orange?style=flat-square&logo=rust" alt="Rust 1.75+"/>
  <img src="https://img.shields.io/badge/platform-linux%20%7C%20windows%20%7C%20macos-blue?style=flat-square" alt="Linux | Windows | macOS"/>
  <img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="MIT License"/>
  <img src="https://img.shields.io/badge/egui-0.27-purple?style=flat-square" alt="egui 0.27"/>
</p>

---

## Overview

**myrient-dl** is a native cross-platform GUI app for browsing and downloading from [Myrient](https://myrient.erista.me) — a free community preservation archive hosting ROM sets, disc images, and software collections.

Browse Myrient's directory tree, select files and folders, and download everything concurrently with live progress bars, speed, and ETA — with no browser, no wget, and no command line required.

Written in **Rust** with **egui** for a lightweight, native dark UI. Downloads are handled entirely in-process using `reqwest` — no external tools needed on any platform.

---

## Features

### Browsing
- **Live directory browser** — fetches Myrient's directory listings directly, with folder/file icons, sizes, and modification dates
- **Breadcrumb navigation** — click any crumb to jump back up the tree; scroll position is remembered per folder
- **Filter bar** — type to instantly filter the current directory; press Escape to clear
- **Baked-in folder sizes** — top-level and subfolder sizes are pre-calculated and compiled into the binary, so you see sizes at a glance without waiting for network requests
- **Virtual scrolling** — only visible rows are rendered in both the browser and queue panels, keeping the UI fast even with thousands of items

### Selection & queuing
- **Click to select files** — click a file row to select/deselect it; shift-click to select a range
- **Select all / deselect** — button in the filter bar selects all visible unqueued files (respects the active filter)
- **Folder checkboxes** — check any folder to mark it for queuing; check multiple folders at once
- **Add to queue** — a single "Add N files + N folders to queue" button queues everything selected at once. Folder contents are scanned recursively via HTTP when queued
- **Persistent queue** — saved to `~/.local/share/myrient-dl/queue.json` on every change and restored on relaunch
- **Resume-first** — when you start the queue after a restart, previously interrupted downloads resume before new ones begin

### Downloads
- **Pure Rust downloader** — no `wget` or any external tool required; downloads run entirely in-process using `reqwest` streaming
- **Resume support** — uses HTTP `Range` headers to continue interrupted downloads from where they left off
- **Concurrent downloads** — configurable 1–16 simultaneous downloads via a toolbar slider
- **Correct folder structure** — files are saved to `<dest>/<collection>/<subfolder>/...`, preserving the full Myrient path hierarchy (equivalent to `wget --cut-dirs=1 -nH`)
- **Automatic retries** — configurable retry count (default 3) with exponential backoff on failure
- **Checksum verification** — fetches `.md5` or `.sfv` sidecar files after download and verifies, showing `✓` or `⚠` in the queue
- **Disk space check** — checks available space against the estimated size before starting

### Progress & monitoring
- **Live progress bars** — each active download shows percentage, speed (auto-scaled KB/s → GB/s), and ETA, updated 4× per second
- **Total speed in header** — the active downloads panel shows combined speed across all running transfers
- **Window title** — shows live stats (`myrient-dl  —  ↓ 87.3 MB/s  ·  6 active  ·  331 queued`) so you can monitor from the taskbar
- **Queue totals** — the queue header shows item count and total file size

### Queue management
- **Shift-click multi-select** — select ranges of queue items for bulk operations
- **Remove selected** — remove multiple queued items at once
- **Keep selected only** — remove everything except your selection (useful for trimming a large queue)
- **Pause & resume** — pause any active download; it resumes from the byte offset it stopped at

### UI
- **Retro theme** — toggle a vivid green-phosphor CRT palette from the toolbar (`retro` / `dim` button)
- **Resizable panels** — drag the divider between browser and queue; drag the active downloads panel edge to resize
- **Persistent settings** — all settings saved between sessions

---

## Installation

### Pre-built binaries

Download the latest release for your platform from the [Releases](../../releases) page. No installation required — just run the binary.

> **Windows**: you may need to allow the binary through SmartScreen on first run.  
> **macOS**: right-click → Open to bypass Gatekeeper on first run.

### Build from source

**Linux prerequisites:**
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

**Build:**
```bash
git clone https://github.com/yourusername/myrient-dl
cd myrient-dl
cargo build --release
./target/release/myrient-dl
```

> **Note on memory:** the `generated_sizes` module contains ~67,000 pre-computed folder sizes split across 135 source files to keep per-file compile memory manageable. If compilation OOMs on a low-memory machine, try `cargo build --release -j1` to limit parallelism.

---

## Folder size data

Myrient's directory listings show `-` for folder sizes. myrient-dl ships with pre-computed sizes baked in, but you can refresh them:

```bash
# First run: crawls entire Myrient tree (takes several hours, checkpoints as it goes)
cargo run --bin fetch_sizes

# Subsequent runs: only re-crawls folders whose modification date changed
cargo run --bin fetch_sizes -- --refresh

# Then rebuild to include the new data
cargo build --release
```

The crawler checkpoints to `fetch_sizes_cache.json` — if it's interrupted, re-running it resumes from where it left off. Delete the cache file to force a full re-crawl.

---

## Usage

1. Launch the app — it opens at the Myrient `/files/` root
2. Navigate folders by clicking them; use the breadcrumb bar to go back
3. **Select files** by clicking their rows (shift-click for ranges)
4. **Select folders** by clicking their checkboxes on the left
5. Click **Add N files + N folders to queue** to queue everything
6. Click **▶ Start queue** — downloads begin immediately, resuming any previous session's interrupted transfers first

### Settings

| Setting | Default | Description |
|---------|---------|-------------|
| Dest | `~/Downloads/myrient` | Root folder for all downloads |
| Threads | 4 | Simultaneous downloads (1–16) |
| Retries | 3 | Retry attempts on failure |
| Verify | ✓ | Check MD5/SFV after download |

---

## Architecture

```
┌─────────────┐   browse    ┌─────────────────┐
│  egui UI    │ ── thread ─▶│  reqwest +      │
│  (main      │             │  scraper        │
│   thread)   │◀─ Mutex ───│  (worker thread)│
└──────┬──────┘             └─────────────────┘
       │ DlCmd channel
       ▼
┌─────────────────┐  semaphore  ┌──────────────────────┐
│ Download manager│ ───────────▶│  reqwest streaming   │
│ thread          │             │  (one thread/job)    │
│                 │◀─ progress─│  Range: resume       │
└─────────────────┘             └──────────────────────┘
```

- **UI thread** — egui update loop, polls shared state, sends `DlCmd` to download manager
- **Browse threads** — spawned per navigation, fetch + parse HTML, write to shared state
- **Download manager** — owns a semaphore for concurrency limiting, spawns one thread per job
- **Download threads** — stream HTTP response directly to disk via `reqwest`, reporting progress back to shared state every 250ms

---

## Dependencies

| Crate | Purpose |
|-------|---------|
| `eframe` / `egui` | Native GUI framework |
| `reqwest` | HTTP client — browsing and downloading |
| `scraper` | HTML parsing for directory listings |
| `serde` / `serde_json` | Settings and queue persistence |
| `shellexpand` | `~/` path expansion |
| `rfd` | Native folder picker dialog |
| `md5` | Checksum verification |
| `libc` | `statvfs` disk space check (Unix) |
| `windows-sys` | `GetDiskFreeSpaceExW` (Windows) |
| `rayon` | Parallel crawling in `fetch_sizes` |
| `once_cell` | Lazy static HTTP client |

---

## Contributing

Pull requests welcome. Some ideas:

- **Import/export** queue as a plain URL list
- **Torrent support** — Myrient provides `.torrent` files for some collections
- **Desktop notifications** on completion
- **Bandwidth limiting** — cap download speed per job or globally

---

## License

MIT — see [LICENSE](LICENSE)

---

## Acknowledgements

[Myrient](https://myrient.erista.me) is a free, community-run preservation archive. If you find it useful, please [consider donating](https://myrient.erista.me/donate/).
