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

Browse Myrient's entire directory tree, select files and folders, and download everything concurrently with live progress bars, speed, and ETA. No browser, no wget, no command line required. Also ships a full TUI CLI for scripting and headless use.

Written in **Rust** with **egui**. Downloads are handled entirely in-process via `reqwest` — no external tools needed on any platform.

---

## Features

### Browsing
- **Instant navigation** — the full Myrient directory tree is embedded in the binary as a compressed index (`generated_dirs.bin`), so browsing any folder is instant with no network round-trip. New folders added to Myrient are fetched live via HTTP and automatically added to the local index for future instant access
- **Live directory browser** — folder/file icons, sizes, and modification dates
- **Breadcrumb navigation** — click any crumb to jump back; scroll position is remembered per folder
- **Filter bar** — type to filter the current directory; Escape to clear
- **Baked-in folder sizes** — sizes pre-calculated and compiled in; visible at a glance without network requests
- **Virtual scrolling** — only visible rows rendered; fast even with thousands of items

### Search
- **Global search tab** — switch to the Search tab to search across the entire embedded tree instantly
- **Include / exclude filters** — narrow results to a specific collection (e.g. `No-Intro`) or exclude one (e.g. `BIOS`) with free-text or dropdown selectors
- **Results show filename + folder path** — at a glance see exactly where each file lives
- **Queue from search** — hover any result and click `+` to add it to the queue immediately
- **Open folder from search** — click `→` to navigate directly to the folder containing the result, with the file pre-selected
- **Row click** — clicking a result navigates to its folder

### Selection & queuing
- **Click to select files** — single click selects/deselects; shift-click for ranges
- **Folder checkboxes** — small checkbox on the left of every folder row
- **Select all / deselect** — button in the filter bar, respects active filter
- **Add to queue** — queues all selected files and folders; folder contents scanned recursively
- **Persistent queue** — saved to `~/.local/share/myrient-dl/queue.json`, restored on relaunch
- **Resume-first** — when restarting the queue, previously interrupted downloads resume before new ones begin

### Downloads
- **Pure Rust downloader** — downloads run in-process via `reqwest` streaming; no external tools
- **Resume support** — HTTP `Range` headers continue interrupted downloads from the exact byte
- **Concurrent downloads** — configurable 1–16 simultaneous downloads
- **Correct folder structure** — files saved preserving Myrient's path hierarchy
- **Automatic retries** — configurable retry count with exponential backoff
- **Checksum verification** — fetches `.md5` sidecars and verifies; shows `✓` or `⚠`
- **Disk space check** — warns if available space is tight before starting

### Progress & monitoring
- **Live progress bars** — percentage, speed, and ETA per download, updated continuously
- **Window title** live stats: `myrient-dl  —  ↓ 87.3 MB/s  ·  6 active  ·  331 queued`
- **Queue totals** — item count and total file size in the queue header

### Queue management
- **Shift-click multi-select** in the queue
- **Remove selected** — bulk remove
- **Keep selected only** — remove everything except your selection
- **Pause & resume** individual downloads

### UI
- **Dark / light theme** — toggle with the `dark`/`light` button in the toolbar; or check `auto` to follow the OS system theme preference automatically
- **Resizable panels** — drag the browser/queue divider and the downloads panel edge
- **Static scrollbars** — always visible, 10px wide — easy to grab on Windows and macOS
- **Persistent settings** — all settings saved between sessions

### CLI mode (`myrient-dl-cli`)

A full terminal UI that mirrors the GUI — same download engine, same queue file, same settings.

```bash
myrient-dl-cli
```

| Key | Action |
|-----|--------|
| `↑` `↓` `j` `k` | Navigate list |
| `Enter` `l` `→` | Open folder / queue file |
| `Backspace` `h` `←` | Go back |
| `f` | Filter current directory |
| `Space` | Select / deselect item |
| `a` | Select all visible files |
| `A` | Deselect all |
| `q` | Queue selected files |
| `/` | Search across entire tree |
| `Tab` | Switch browser / queue panes |
| `s` | Start / pause queue |
| `x` | Remove selected queue items |
| `Q` | Quit |

---

## Installation

### Pre-built binaries

Download the latest release for your platform from the [Releases](../../releases) page.

| Platform | GUI | CLI |
|----------|-----|-----|
| Linux x86_64 | `myrient-dl-linux-x86_64` | `myrient-dl-cli-linux-x86_64` |
| Windows x86_64 | `myrient-dl-windows-x86_64.exe` | `myrient-dl-cli-windows-x86_64.exe` |
| macOS Universal | `myrient-dl.app` | `myrient-dl-cli-macos-universal` |

> **Windows:** right-click → Run anyway to bypass SmartScreen on first run.  
> **macOS:** drag `myrient-dl.app` to Applications. Right-click → Open on first launch to bypass Gatekeeper.  
> **Linux:** `chmod +x myrient-dl-linux-x86_64` then run.

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

---

## Directory tree data

The embedded directory index (`generated_dirs.bin`) enables instant navigation without HTTP requests. It's stored as a zstd-compressed per-folder block index. New folders visited at runtime are automatically persisted to `~/.local/share/myrient-dl/generated_dirs.bin`, which takes precedence over the embedded data on future launches.

To regenerate the index from scratch or refresh changed folders:

```bash
# Rebuild from existing cache (fast — seconds):
cargo run --bin fetch_sizes -- --dirs-only

# Re-crawl folders whose modification date changed on the server:
cargo run --bin fetch_sizes -- --refresh

# Full crawl from scratch (several hours, resumes if interrupted):
cargo run --bin fetch_sizes

# Then rebuild the app to embed the new data:
cargo build --release
```

The crawler checkpoints to `fetch_sizes_cache.json`. Without running `fetch_sizes`, the app still works fully — navigation just makes live HTTP requests for unknown folders and persists them locally.

---

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| Dest | `~/Downloads/myrient` | Root folder for all downloads |
| Threads | 4 | Simultaneous downloads (1–16) |
| Retries | 3 | Retry attempts on failure |
| Verify | ✓ | Check MD5 after download |
| Theme | dark | `dark` / `light` / `auto` (follows OS) |

---

## Architecture

```
┌─────────────┐   browse    ┌─────────────────────────────┐
│  egui UI    │ ── thread ─▶│  reqwest + scraper          │
│  (main      │             │  (worker thread per nav)    │
│   thread)   │◀─ Mutex ───│                             │
└──────┬──────┘             └─────────────────────────────┘
       │                    ┌─────────────────────────────┐
       │                    │  generated_dirs.bin         │
       │                    │  (zstd per-folder blocks,   │
       │                    │   local file auto-updated)  │
       │                    └─────────────────────────────┘
       │ DlCmd channel
       ▼
┌─────────────────┐  semaphore  ┌──────────────────────┐
│ Download manager│ ───────────▶│  reqwest streaming   │
│ thread          │             │  (one thread/job)    │
│                 │◀─ progress─│  Range: resume       │
└─────────────────┘             └──────────────────────┘
```

---

## Dependencies

| Crate | Purpose |
|-------|---------|
| `eframe` / `egui` | Native GUI |
| `reqwest` | HTTP client — browsing and downloading |
| `scraper` | HTML parsing for live directory fetches |
| `serde` / `serde_json` | Settings, queue persistence |
| `zstd` | Block compression for `generated_dirs.bin` |
| `shellexpand` | `~/` path expansion |
| `rfd` | Native folder picker dialog |
| `md5` | Checksum verification |
| `clap` | CLI argument parsing |
| `libc` | `statvfs` disk space (Unix) |
| `windows-sys` | `GetDiskFreeSpaceExW` (Windows) |
| `rayon` | Parallel crawling in `fetch_sizes` |
| `ratatui` / `crossterm` | Terminal UI for CLI mode |

---

## Contributing

Pull requests welcome. Some ideas:

- **Import/export** queue as a plain URL list
- **Torrent support** — Myrient provides `.torrent` files for some collections
- **Desktop notifications** on completion
- **Bandwidth limiting** per job or globally

---

## License

MIT — see [LICENSE](LICENSE)

---

## Acknowledgements

[Myrient](https://myrient.erista.me) is a free, community-run preservation archive. Please [consider donating](https://myrient.erista.me/donate/) to help keep it running.
