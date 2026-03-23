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

Browse Myrient's entire directory tree, select files and folders, and download everything concurrently with live progress bars, speed, and ETA. No browser, no wget, no command line required. Also ships a full CLI mode for scripting and headless use.

Written in **Rust** with **egui**. Downloads are handled entirely in-process via `reqwest` — no external tools needed on any platform.

---

## Features

### Browsing
- **Instant navigation** — the full Myrient directory tree is compiled into the binary as a compressed blob (`generated_dirs.bin`), so browsing any folder is instant with no network round-trip
- **Live directory browser** — folder/file icons, sizes, and modification dates
- **Breadcrumb navigation** — click any crumb to jump back; scroll position is remembered per folder
- **Filter bar** — type to filter the current directory; Escape to clear
- **Search** — click `⌕ search` to open a global search across the entire baked-in tree. Results show instantly; click any result to navigate to that folder
- **Baked-in folder sizes** — sizes pre-calculated and compiled in; visible at a glance without network requests
- **Virtual scrolling** — only visible rows rendered in both panels; fast even with thousands of items

### Selection & queuing
- **Click to select files** — single click selects/deselects; shift-click for ranges
- **Folder checkboxes** — large `+` checkbox on the right of every folder row. Easy to click, doesn't interfere with the folder icon. Check multiple folders then add all at once
- **Select all / deselect** — button in the filter bar, respects active filter
- **Add to queue** — a single button queues all selected files and folders. Folder contents scanned recursively via HTTP
- **Persistent queue** — saved to `~/.local/share/myrient-dl/queue.json`, restored on relaunch
- **Resume-first** — when restarting the queue, previously interrupted downloads resume before new ones begin

### Downloads
- **Pure Rust downloader** — no `wget` or external tools; downloads run in-process via `reqwest` streaming
- **Resume support** — HTTP `Range` headers continue interrupted downloads from the exact byte
- **Concurrent downloads** — configurable 1–16 simultaneous downloads
- **Correct folder structure** — files saved to `<dest>/<collection>/<subfolder>/...`, preserving Myrient's path hierarchy
- **Automatic retries** — configurable retry count with exponential backoff
- **Checksum verification** — fetches `.md5` or `.sfv` sidecars and verifies; shows `✓` or `⚠`
- **Disk space check** — warns if available space is tight before starting

### Progress & monitoring
- **Live progress bars** — percentage, speed (KB/s → GB/s), and ETA per download, updated 4× per second
- **Total speed** in the downloads panel header
- **Window title** live stats: `myrient-dl  —  ↓ 87.3 MB/s  ·  6 active  ·  331 queued`
- **Queue totals** — item count and total file size in the queue header

### Queue management
- **Shift-click multi-select** in the queue
- **Remove selected** — bulk remove
- **Keep selected only** — remove everything except your selection
- **Pause & resume** individual downloads

### UI
- **Retro theme** — vivid green-phosphor CRT palette, toggleable with the `retro`/`dim` button in the toolbar
- **Resizable panels** — drag the browser/queue divider and the downloads panel edge
- **Static scrollbars** — always visible, 10px wide — easy to grab on Windows and macOS
- **Persistent settings** — all settings saved between sessions

### CLI mode

Run downloads from the terminal without launching the GUI:

```bash
# Download a single file
myrient-dl --url "https://myrient.erista.me/files/No-Intro/Nintendo%20-%20Game%20Boy/..." --dest ~/Downloads

# Process the existing saved queue
myrient-dl --cli

# With custom thread count
myrient-dl --cli --threads 8

# See all options
myrient-dl --help
```

CLI output matches the GUI's colour scheme — green `✓` for success, red `✕` for errors, yellow `⏸` for paused.

---

## Installation

### Pre-built binaries

Download the latest release for your platform from the [Releases](../../releases) page. No installation required.

> **Windows**: allow through SmartScreen on first run (right-click → Run anyway).  
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

> **Memory note:** the `generated_sizes` module contains ~67,000 folder sizes split across 135 source files to keep per-file compile memory manageable. On an 8 GB machine, try `cargo build --release -j1` if compilation OOMs.

---

## Directory tree data

The baked-in directory tree (`generated_dirs.bin`) enables instant navigation without any HTTP requests. It's stored as a deflate-compressed JSON blob embedded at compile time and decompressed lazily on first use.

To populate or refresh it, run `fetch_sizes`:

```bash
# First run: crawls the entire Myrient tree (several hours, resumes if interrupted)
cargo run --bin fetch_sizes

# Subsequent runs: only re-crawls folders whose modification date changed
cargo run --bin fetch_sizes -- --refresh

# Rebuild to embed the new data
cargo build --release
```

The crawler checkpoints to `fetch_sizes_cache.json`. Delete it to force a full re-crawl.

Without running `fetch_sizes`, the app still works — navigation just makes live HTTP requests instead of using the baked-in tree (same as before, just slower).

---

## Usage

### GUI
1. Launch — opens at the Myrient `/files/` root
2. Click folders to navigate; use breadcrumbs to go back
3. Use `⌕ search` to find files across the whole tree instantly
4. **Select files** by clicking rows (shift-click for ranges)
5. **Select folders** by clicking the `+` checkbox on the right of any folder row
6. Click **Add N files + N folders to queue**
7. Click **▶ Start queue** — resume-first downloads begin immediately

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
┌─────────────┐   browse    ┌─────────────────────────────┐
│  egui UI    │ ── thread ─▶│  reqwest + scraper          │
│  (main      │             │  (worker thread per nav)    │
│   thread)   │◀─ Mutex ───│                             │
└──────┬──────┘             └─────────────────────────────┘
       │                    ┌─────────────────────────────┐
       │                    │  generated_dirs.bin         │
       │                    │  (deflate-compressed tree,  │
       │                    │   instant local lookup)     │
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
| `serde` / `serde_json` | Settings, queue persistence, dir tree |
| `shellexpand` | `~/` path expansion |
| `rfd` | Native folder picker dialog |
| `md5` | Checksum verification |
| `flate2` | Deflate compression for `generated_dirs.bin` |
| `clap` | CLI argument parsing |
| `libc` | `statvfs` disk space (Unix) |
| `windows-sys` | `GetDiskFreeSpaceExW` (Windows) |
| `rayon` | Parallel crawling in `fetch_sizes` |
| `once_cell` | Lazy static HTTP client |

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

---

## CLI mode (`myrient-dl-cli`)

A full terminal UI that mirrors the GUI — same download engine, same queue file, same settings.

```
myrient-dl-cli
```

### Controls

| Key | Action |
|-----|--------|
| `↑` `↓` `j` `k` | Navigate list |
| `Enter` `l` `→` | Open folder / queue file |
| `Backspace` `h` `←` | Go back |
| `Space` | Select / deselect item |
| `a` | Select all visible files |
| `A` | Deselect all |
| `q` | Queue selected files |
| `Tab` | Switch between browser and queue panes |
| `s` | Start / pause queue |
| `x` | Remove selected queue items |
| `/` | Search across entire baked-in tree |
| `Esc` | Cancel search |
| `Q` | Quit |

### Platform availability

| Platform | GUI | CLI |
|----------|-----|-----|
| Linux | ✓ | ✓ |
| macOS | ✓ | ✓ |
| Windows | ✓ | ✓ |

---

## Regenerating `generated_dirs.bin` from existing cache

If you have already run `fetch_sizes` and have a `fetch_sizes_cache.json`, you do **not** need to re-crawl. Just run:

```bash
cargo run --bin fetch_sizes -- --dirs-only
# → writes src/generated_dirs.bin in seconds from the cache
cargo build --release
```

`--dirs-only` skips all HTTP requests when the cache is populated and immediately emits the binary from what's already stored locally.
