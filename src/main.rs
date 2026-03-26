#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod generated_dirs;

use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
const BASE_URL:   &str = "https://myrient.erista.me/files/";
const DONATE_URL: &str = "https://myrient.erista.me/donate/";

// Shared HTTP client — one per process lifetime
static CLIENT: Lazy<reqwest::blocking::Client> = Lazy::new(|| {
    reqwest::blocking::Client::builder()
        .user_agent("myrient-dl/1.0")
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client")
});

// ── Colours ───────────────────────────────────────────────────────────────────
const C_BG:      Color32 = Color32::from_rgb(0x0d, 0x10, 0x14);
const C_SURF:    Color32 = Color32::from_rgb(0x13, 0x18, 0x1f);
const C_SURF2:   Color32 = Color32::from_rgb(0x18, 0x1e, 0x27);
const C_BORDER:  Color32 = Color32::from_rgb(0x1f, 0x27, 0x33);
const C_BORDER2: Color32 = Color32::from_rgb(0x2a, 0x34, 0x44);
const C_ACC:     Color32 = Color32::from_rgb(0x3d, 0xe8, 0xa0);
const C_WARN:    Color32 = Color32::from_rgb(0xe8, 0xa0, 0x3d);
const C_ERR:     Color32 = Color32::from_rgb(0xe8, 0x50, 0x3d);
const C_BLUE:    Color32 = Color32::from_rgb(0x5b, 0x9c, 0xf6);
const C_TEXT:    Color32 = Color32::from_rgb(0xc8, 0xd4, 0xe3);
const C_MUTED:   Color32 = Color32::from_rgb(0x4a, 0x5a, 0x72);
const C_DIM:     Color32 = Color32::from_rgb(0x2a, 0x34, 0x44);
const C_FILE:    Color32 = Color32::from_rgb(0xa0, 0xc8, 0xe8);
const C_DOWNLOADED: Color32 = Color32::from_rgb(0x2a, 0x50, 0x3a); // muted green for downloaded files

// ── Semaphore ─────────────────────────────────────────────────────────────────
struct Semaphore { count: Mutex<usize>, cvar: Condvar }
impl Semaphore {
    fn new(n: usize) -> Self { Self { count: Mutex::new(n), cvar: Condvar::new() } }
    fn acquire(&self) -> SemGuard<'_> {
        let mut c = self.count.lock().unwrap();
        while *c == 0 { c = self.cvar.wait(c).unwrap(); }
        *c -= 1;
        SemGuard { sem: self }
    }
}
struct SemGuard<'a> { sem: &'a Semaphore }
impl Drop for SemGuard<'_> {
    fn drop(&mut self) { *self.sem.count.lock().unwrap() += 1; self.sem.cvar.notify_one(); }
}

// ── Data types ────────────────────────────────────────────────────────────────
#[derive(Clone, Debug)]
struct DirEntry {
    name:      Arc<str>,
    href:      Arc<str>,
    size:      Arc<str>,
    date:      Arc<str>,
    is_folder: bool,
    url:       Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum JobStatus {
    Waiting,
    Spooling,
    Downloading,
    Paused,
    Verifying,
    Done,
    Error(String),
}
impl JobStatus {
    fn label(&self) -> &str {
        match self {
            Self::Waiting     => "○ waiting",
            Self::Spooling    => "◌ spooling",
            Self::Downloading => "⟳ active",
            Self::Paused      => "⏸ paused",
            Self::Verifying   => "⧗ verifying",
            Self::Done        => "✓ done",
            Self::Error(_)    => "✕ error",
        }
    }
    fn color(&self) -> Color32 {
        match self {
            Self::Waiting     => C_MUTED,
            Self::Spooling    => C_BLUE,
            Self::Downloading => C_WARN,
            Self::Paused      => C_BLUE,
            Self::Verifying   => C_BLUE,
            Self::Done        => C_ACC,
            Self::Error(_)    => C_ERR,
        }
    }
    fn is_active(&self) -> bool {
        matches!(self, Self::Spooling | Self::Downloading | Self::Verifying)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QueueJob {
    id:           String,
    url:          String,
    name:         String,
    path:         String,
    status:       JobStatus,
    resume:       bool,
    retry_count:  u32,
    verified:     Option<bool>,
    #[serde(default)]
    file_size:    u64,
}

#[derive(Clone, Debug, Default)]
struct DownloadProgress {
    percent:     f32,
    speed_bps:   f64,   // bytes/sec
    eta_secs:    Option<u64>,
    spool_start: Option<Instant>,
}

// ── Settings ──────────────────────────────────────────────────────────────────
#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    dest_path:        String,
    concurrent:       usize,
    max_retries:      u32,
    verify_checksums: bool,
    queue_paused:     bool,
    retro_theme:      bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            dest_path:        "~/Downloads/myrient".into(),
            concurrent:       4,
            max_retries:      3,
            verify_checksums: true,
            queue_paused:     true,
            retro_theme:      false,
        }
    }
}

fn settings_path()    -> std::path::PathBuf { data_dir().join("settings.json") }
fn queue_path()       -> std::path::PathBuf { data_dir().join("queue.json") }
fn downloaded_path()  -> std::path::PathBuf { data_dir().join("downloaded.json") }
fn folder_sizes_path()-> std::path::PathBuf { data_dir().join("folder_sizes.json") }

fn load_folder_sizes() -> HashMap<String, u64> {
    let p = folder_sizes_path();
    if !p.exists() { return HashMap::new(); }
    serde_json::from_str(&std::fs::read_to_string(p).unwrap_or_default())
        .unwrap_or_default()
}
fn save_folder_sizes(m: &HashMap<String, u64>) {
    if let Ok(j) = serde_json::to_string(m) {
        std::fs::write(folder_sizes_path(), j).ok();
    }
}
fn data_dir()      -> std::path::PathBuf {
    let base = if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        std::path::PathBuf::from(d)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        std::path::PathBuf::from(home).join(".local").join("share")
    };
    let dir = base.join("myrient-dl");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn load_settings() -> Settings {
    let p = settings_path();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(cfg) = serde_json::from_str(&s) { return cfg; }
        }
    }
    Settings::default()
}
fn save_settings(s: &Settings) {
    if let Ok(j) = serde_json::to_string_pretty(s) {
        std::fs::write(settings_path(), j).ok();
    }
}
fn load_queue() -> Vec<QueueJob> {
    let p = queue_path();
    if !p.exists() { return vec![]; }
    serde_json::from_str(&std::fs::read_to_string(p).unwrap_or_default())
        .unwrap_or_default()
}
fn save_queue(queue: &[QueueJob]) {
    let saveable: Vec<QueueJob> = queue.iter().map(|j| {
        let mut j2 = j.clone();
        if j2.status.is_active() { j2.status = JobStatus::Waiting; j2.resume = true; }
        j2
    }).collect();
    if let Ok(json) = serde_json::to_string_pretty(&saveable) {
        std::fs::write(queue_path(), json).ok();
    }
}

fn load_downloaded() -> HashSet<String> {
    let p = downloaded_path();
    if !p.exists() { return HashSet::new(); }
    serde_json::from_str(&std::fs::read_to_string(p).unwrap_or_default())
        .unwrap_or_default()
}
fn save_downloaded(urls: &HashSet<String>) {
    if let Ok(json) = serde_json::to_string(urls) {
        std::fs::write(downloaded_path(), json).ok();
    }
}
// ── Download manager commands ──────────────────────────────────────────────────
#[allow(dead_code)]
enum DlCmd { Start(QueueJob, String, usize, u32, bool), Cancel(String), SetConcurrent(usize), Shutdown }
//                                          ^^^^ max_retries, verify_checksums

// ── Shared state ──────────────────────────────────────────────────────────────
struct Shared {
    browse_result:       Option<(String, Result<Vec<DirEntry>, String>)>,
    queue:               Vec<QueueJob>,
    progress:            HashMap<String, DownloadProgress>,
    active_dl:           usize,
    newly_completed:     Vec<String>,
    // Mirror of current settings — written by UI thread, read by download manager
    // so the manager can self-kick without waiting for a UI repaint.
    dl_settings:         DlSettings,
}

#[derive(Clone)]
struct DlSettings {
    dest:        String,
    concurrent:  usize,
    max_retries: u32,
    verify:      bool,
    paused:      bool,
}
impl Default for Shared {
    fn default() -> Self {
        Self {
            browse_result:   None,
            queue:           Vec::new(),
            progress:        HashMap::new(),
            active_dl:       0,
            newly_completed: Vec::new(),
            dl_settings:     DlSettings {
                dest:        shellexpand::tilde("~/Downloads/myrient").to_string(),
                concurrent:  4,
                max_retries: 3,
                verify:      true,
                paused:      true,
            },
        }
    }
}
impl Shared {
    fn push_log(&mut self, _msg: impl Into<String>, _is_err: bool) {
        // Log panel removed — no-op kept so call sites compile without changes
    }
}

// ── DirEntry parsing ──────────────────────────────────────────────────────────

/// Convert a rel_path (e.g. "No-Intro/Nintendo - Game Boy") to the key used
/// in DIR_INDEX (same format — no leading/trailing slash).
fn path_to_dir_key(path: &str) -> &str {
    path.trim_matches('/')
}

/// Serve directory listing from baked-in generated_dirs if available,
/// otherwise fall back to an HTTP request.
fn fetch_directory(url: &str, rel_path: &str) -> Result<Vec<DirEntry>, String> {
    let key = path_to_dir_key(rel_path);
    if let Some(baked) = generated_dirs::lookup(key) {
        if !baked.is_empty() {
            let entries = baked.into_iter().map(|e| {
                let file_url: Option<Arc<str>> = if !e.is_folder {
                    reqwest::Url::parse(url).ok()
                        .and_then(|b| b.join(&e.href).ok())
                        .map(|u| Arc::from(u.as_str()))
                } else { None };
                DirEntry {
                    name:      Arc::from(e.name.as_str()),
                    href:      Arc::from(e.href.as_str()),
                    size:      Arc::from(if e.size == "-" { "" } else { e.size.as_str() }),
                    date:      Arc::from(e.date.as_str()),
                    is_folder: e.is_folder,
                    url:       file_url,
                }
            }).collect();
            return Ok(entries);
        }
    }
    fetch_directory_http(url)
}

fn fetch_directory_http(url: &str) -> Result<Vec<DirEntry>, String> {
    let body = CLIENT.get(url).send().map_err(|e| e.to_string())?
        .text().map_err(|e| e.to_string())?;
    let doc   = scraper::Html::parse_document(&body);
    let tr    = scraper::Selector::parse("table tr").unwrap();
    let td    = scraper::Selector::parse("td").unwrap();
    let a_sel = scraper::Selector::parse("a").unwrap();
    let mut entries = Vec::new();

    for row in doc.select(&tr).skip(1) {
        let cells: Vec<_> = row.select(&td).collect();
        if cells.len() < 3 { continue; }
        let Some(link) = cells[0].select(&a_sel).next() else { continue };
        let href = link.value().attr("href").unwrap_or("").to_string();
        if href == "./" || href == "../" || href.is_empty() { continue; }
        let name     = link.text().collect::<String>().trim().trim_end_matches('/').to_string();
        let size_raw = cells[1].text().collect::<String>().trim().to_string();
        let date     = cells[2].text().collect::<String>().trim().to_string();
        let is_folder = href.ends_with('/');
        let file_url: Option<Arc<str>> = if !is_folder {
            reqwest::Url::parse(url).ok()
                .and_then(|b| b.join(&href).ok())
                .map(|u| Arc::from(u.as_str()))
        } else { None };

        entries.push(DirEntry {
            name:      Arc::from(name.as_str()),
            href:      Arc::from(href.as_str()),
            size:      Arc::from(if size_raw == "-" { "" } else { size_raw.as_str() }),
            date:      Arc::from(date.as_str()),
            is_folder,
            url:       file_url,
        });
    }
    entries.sort_by(|a, b| {
        b.is_folder.cmp(&a.is_folder)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

// ── Speed formatting ──────────────────────────────────────────────────────────

/// Recursively collect all file (url, name, size_bytes) triples under a folder URL.
/// Uses baked-in generated_dirs data when available — no HTTP needed.
fn collect_files(url: &str, out: &mut Vec<(String, String, u64)>) {
    let rel = url.trim_end_matches('/')
        .trim_start_matches(BASE_URL.trim_end_matches('/'))
        .trim_start_matches('/');
    if let Ok(entries) = fetch_directory(url, rel) {
        for e in entries {
            if e.is_folder {
                let sub_url = format!("{}{}{}", url.trim_end_matches('/'), "/", e.href);
                collect_files(&sub_url, out);
            } else if let Some(ref u) = e.url {
                out.push((u.to_string(), e.name.to_string(), parse_size_str(&e.size)));
            }
        }
    }
}
fn fmt_speed(bps: f64) -> String {
    match bps as u64 {
        b if b >= 1_000_000_000_000 => format!("{:.1} TB/s", bps / 1e12),
        b if b >= 1_000_000_000     => format!("{:.1} GB/s", bps / 1e9),
        b if b >= 1_000_000         => format!("{:.1} MB/s", bps / 1e6),
        b if b >= 1_000             => format!("{:.1} KB/s", bps / 1e3),
        _                           => format!("{:.0} B/s",  bps),
    }
}

fn fmt_eta(secs: u64) -> String {
    if secs >= 3600 { format!("{}h{}m", secs/3600, (secs%3600)/60) }
    else if secs >= 60 { format!("{}m{}s", secs/60, secs%60) }
    else { format!("{}s", secs) }
}

fn fmt_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000     => format!("{:.1} MB", b as f64 / 1e6),
        b if b >= 1_000         => format!("{:.1} KB", b as f64 / 1e3),
        b                       => format!("{} B", b),
    }
}

/// Parse Myrient's human-readable size strings e.g. "496.7 MiB", "1.8 GiB", "345 B"
fn parse_size_str(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() || s == "-" { return 0; }
    let mut parts = s.splitn(2, ' ');
    let num: f64 = parts.next().unwrap_or("").parse().unwrap_or(0.0);
    let unit = parts.next().unwrap_or("").trim().to_uppercase();
    let mult: u64 = match unit.as_str() {
        "TIB" | "TB" => 1_099_511_627_776,
        "GIB" | "GB" => 1_073_741_824,
        "MIB" | "MB" => 1_048_576,
        "KIB" | "KB" => 1_024,
        _             => 1,
    };
    (num * mult as f64) as u64
}

// ── Disk space check ──────────────────────────────────────────────────────────
fn free_bytes(path: &str) -> Option<u64> {
    let mut p = std::path::Path::new(path);
    let tmp;
    loop {
        if p.exists() { break; }
        match p.parent() {
            Some(parent) => { tmp = parent.to_path_buf(); p = &tmp; break; }
            None => return None,
        }
    }

    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        use std::ffi::CString;
        let cpath = CString::new(p.to_str()?).ok()?;
        unsafe {
            let mut stat: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();
            if libc::statvfs(cpath.as_ptr(), stat.as_mut_ptr()) == 0 {
                let s = stat.assume_init();
                return Some(s.f_bavail as u64 * s.f_frsize as u64);
            }
        }
        None
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free_bytes = 0u64;
        unsafe {
            if windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_bytes,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) != 0 {
                return Some(free_bytes);
            }
        }
        None
    }

    #[cfg(not(any(unix, windows)))]
    None
}

// ── Checksum verification ─────────────────────────────────────────────────────
fn verify_file(file_path: &str, file_url: &str) -> Option<bool> {
    let md5_url = format!("{}.md5", file_url.trim_end_matches('/'));
    let sfv_url = format!("{}.sfv", file_url.trim_end_matches('/'));

    if let Ok(resp) = CLIENT.get(&md5_url).send() {
        if resp.status().is_success() {
            if let Ok(body) = resp.text() {
                let expected = body.split_whitespace().next()?.to_lowercase();
                if expected.len() == 32 {
                    let data   = std::fs::read(file_path).ok()?;
                    let actual = format!("{:x}", md5::compute(&data));
                    return Some(actual == expected);
                }
            }
        }
    }

    if let Ok(resp) = CLIENT.get(&sfv_url).send() {
        if resp.status().is_success() {
            if let Ok(body) = resp.text() {
                for line in body.lines() {
                    let line = line.trim();
                    if line.starts_with(';') || line.is_empty() { continue; }
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let expected_crc = parts.last()?.to_lowercase();
                        let data = std::fs::read(file_path).ok()?;
                        let actual_crc = format!("{:08x}", crc32(&data));
                        return Some(actual_crc == expected_crc);
                    }
                }
            }
        }
    }
    None
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 { crc = (crc >> 1) ^ 0xEDB8_8320; }
            else { crc >>= 1; }
        }
    }
    !crc
}

// ── Helpers ───────────────────────────────────────────────────────────────────
fn next_id() -> String {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("{:08x}", C.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

fn mono(text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text).font(FontId::monospace(size)).color(color)
}

fn hline(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, C_BORDER);
}

fn vsep(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, 24.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, C_BORDER);
}

fn panel_frame(fill: Color32) -> egui::Frame { egui::Frame::none().fill(fill) }

/// Per-frame colour skin — computed once in update() based on current theme.
/// Vivid theme uses slightly warm-tinted surfaces and animated accents.
fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let h1 = bytes.next().and_then(|c| (c as char).to_digit(16));
            let h2 = bytes.next().and_then(|c| (c as char).to_digit(16));
            if let (Some(h1), Some(h2)) = (h1, h2) {
                out.push(char::from(((h1 << 4) | h2) as u8));
                continue;
            }
        }
        out.push(b as char);
    }
    out
}

// ── Download manager ──────────────────────────────────────────────────────────
fn download_manager(rx: std::sync::mpsc::Receiver<DlCmd>, shared: Arc<Mutex<Shared>>) {
    let mut procs: HashMap<String, std::sync::mpsc::Sender<()>> = HashMap::new();
    let mut concurrent = 4usize;
    let mut sem        = Arc::new(Semaphore::new(concurrent));

    // Kick waiting jobs up to the concurrency limit using current dl_settings.
    let do_kick = |shared: &Arc<Mutex<Shared>>,
                   procs:  &mut HashMap<String, std::sync::mpsc::Sender<()>>,
                   sem:    &Arc<Semaphore>,
                   conc:   usize| {
        let (settings, jobs): (DlSettings, Vec<QueueJob>) = {
            let s = shared.lock().unwrap();
            if s.dl_settings.paused { return; }
            let active = s.queue.iter().filter(|j| j.status.is_active()).count();
            let slots  = conc.saturating_sub(active);
            if slots == 0 { return; }
            let mut waiting: Vec<&QueueJob> = s.queue.iter()
                .filter(|j| j.status == JobStatus::Waiting)
                .collect();
            waiting.sort_by_key(|j| if j.resume { 0u8 } else { 1u8 });
            (s.dl_settings.clone(), waiting.into_iter().take(slots).cloned().collect())
        };
        for job in jobs {
            if procs.contains_key(&job.id) { continue; }
            {
                let mut s = shared.lock().unwrap();
                if s.queue.iter().find(|j| j.id == job.id)
                    .map(|j| j.status.is_active()).unwrap_or(false) { continue; }
                if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) { j.status = JobStatus::Spooling; }
                s.progress.insert(job.id.clone(), DownloadProgress { spool_start: Some(Instant::now()), ..Default::default() });
                s.active_dl += 1;
                save_queue(&s.queue);
            }
            let (kill_tx, kill_rx) = std::sync::mpsc::channel::<()>();
            procs.insert(job.id.clone(), kill_tx);
            let shared2 = Arc::clone(shared);
            let sem2    = Arc::clone(sem);
            let dest2   = settings.dest.clone();
            let retries = settings.max_retries;
            let verify  = settings.verify;
            thread::spawn(move || {
                let _permit = sem2.acquire();
                {
                    let mut s = shared2.lock().unwrap();
                    if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) {
                        if j.status == JobStatus::Spooling { j.status = JobStatus::Downloading; }
                    }
                    if let Some(p) = s.progress.get_mut(&job.id) { p.spool_start = None; }
                }
                let estimated = estimated_size(&job.url);
                if let Some(free) = free_bytes(&dest2) {
                    if estimated > 0 && free < estimated + estimated / 10 {
                        shared2.lock().unwrap().push_log(format!("⚠ Low disk space for {}", job.name), true);
                    }
                }
                let final_status = run_with_retries(&job, &dest2, &shared2, &kill_rx, retries);
                if verify && final_status == JobStatus::Done {
                    let dest_file = guess_dest_path(&dest2, &job.url);
                    if let Some(ref path) = dest_file {
                        { let mut s = shared2.lock().unwrap(); if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) { j.status = JobStatus::Verifying; } }
                        let verified = verify_file(path, &job.url);
                        let mut s = shared2.lock().unwrap();
                        if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) {
                            j.verified = verified; j.status = JobStatus::Done;
                            match verified {
                                Some(true)  => s.push_log(format!("✓ Verified: {}", job.name), false),
                                Some(false) => s.push_log(format!("⚠ Checksum FAIL: {}", job.name), true),
                                None => {}
                            }
                        }
                    } else {
                        let mut s = shared2.lock().unwrap();
                        if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) { j.status = JobStatus::Done; }
                    }
                } else {
                    let mut s = shared2.lock().unwrap();
                    if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) {
                        let paused = j.status == JobStatus::Paused;
                        if !paused { j.status = final_status.clone(); }
                        if !paused { s.push_log(if final_status == JobStatus::Done { format!("Done: {}", job.name) } else { format!("Failed: {}", job.name) }, final_status != JobStatus::Done); }
                    }
                }
                {
                    let mut s = shared2.lock().unwrap();
                    s.progress.remove(&job.id);
                    s.active_dl = s.active_dl.saturating_sub(1);
                    if matches!(s.queue.iter().find(|j| j.id == job.id).map(|j| &j.status), Some(JobStatus::Done)) {
                        s.newly_completed.push(job.url.clone());
                    }
                    s.queue.retain(|j| !matches!(j.status, JobStatus::Done));
                    save_queue(&s.queue);
                    // Persist the downloaded URL immediately — don't wait for the UI thread
                    let completed_url = job.url.clone();
                    let downloaded_path = {
                        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                        std::path::PathBuf::from(
                            std::env::var("XDG_DATA_HOME").unwrap_or_else(|_|
                                format!("{}/.local/share", home)
                            )
                        ).join("myrient-dl").join("downloaded.json")
                    };
                    // Load, insert, save — all inline in the manager thread
                    if let Ok(data) = std::fs::read_to_string(&downloaded_path) {
                        if let Ok(mut set) = serde_json::from_str::<HashSet<String>>(&data) {
                            set.insert(completed_url);
                            if let Ok(j) = serde_json::to_string(&set) {
                                std::fs::write(&downloaded_path, j).ok();
                            }
                        }
                    } else {
                        // File doesn't exist yet — create it
                        let mut set = HashSet::new();
                        set.insert(completed_url);
                        if let Ok(j) = serde_json::to_string(&set) {
                            std::fs::write(&downloaded_path, j).ok();
                        }
                    }
                }
            });
        }
    };

    // Background watcher: self-kick every second so downloads continue
    // even when the UI window is hidden, minimised, or the screen is locked.
    {
        let shared3 = Arc::clone(&shared);
        let (watcher_tx, watcher_rx) = std::sync::mpsc::channel::<()>();
        // We send a unit on watcher_tx each time the manager processes a command,
        // so the watcher knows the manager is alive.
        // Simpler: just spawn a thread that sends Kick commands on the main rx.
        // We use a separate channel for that.
        let _ = watcher_tx; // suppress warning — used structurally below
        let _ = watcher_rx;
        // Actually: just run the watcher inline below by making the manager loop
        // also respond to a timer. We do this by making rx.recv() timeout-based.
        let _ = shared3; // used in the recv_timeout loop below
    }

    // Use recv_timeout so the manager wakes up every second to self-kick,
    // regardless of whether the UI is sending commands.
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(DlCmd::Shutdown) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(DlCmd::Cancel(id)) => { if let Some(tx) = procs.remove(&id) { let _ = tx.send(()); } }
            Ok(DlCmd::SetConcurrent(n)) => {
                concurrent = n; sem = Arc::new(Semaphore::new(concurrent));
                do_kick(&shared, &mut procs, &sem, concurrent);
            }
            Ok(DlCmd::Start(..)) => {
                // UI thread sends this to signal "kick now" — settings already in shared.dl_settings
                concurrent = shared.lock().unwrap().dl_settings.concurrent;
                do_kick(&shared, &mut procs, &sem, concurrent);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // 1-second heartbeat — kick any waiting jobs without UI involvement
                concurrent = shared.lock().unwrap().dl_settings.concurrent;
                do_kick(&shared, &mut procs, &sem, concurrent);
            }
        }
        // Clean up finished proc entries
        procs.retain(|id, _| {
            shared.lock().unwrap().queue.iter().any(|j| &j.id == id && j.status.is_active())
        });
    }
}

fn run_with_retries(
    job:         &QueueJob,
    dest:        &str,
    shared:      &Arc<Mutex<Shared>>,
    kill_rx:     &std::sync::mpsc::Receiver<()>,
    max_retries: u32,
) -> JobStatus {
    let mut attempt = job.retry_count;

    loop {
        if attempt > 0 {
            {
                let mut s = shared.lock().unwrap();
                if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) {
                    j.retry_count = attempt;
                }
                s.push_log(format!("Retrying ({}/{}) {}", attempt, max_retries, job.name), false);
            }
            let delay = Duration::from_secs(2u64.pow(attempt.min(6)));
            thread::sleep(delay);
        }

        // Check for cancel before starting
        if kill_rx.try_recv().is_ok() {
            let mut s = shared.lock().unwrap();
            if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) {
                j.status = JobStatus::Paused; j.resume = true;
            }
            return JobStatus::Paused;
        }

        // ── Native reqwest download ──────────────────────────────────────────
        // Mirrors wget1 -r -np -nH --cut-dirs=1 -P <dest> [-c] <url>
        // Path: dest / segs[2..] (segs[0]="files", segs[1]=top-folder, rest=relative)
        let file_path = match guess_dest_path(dest, &job.url) {
            Some(p) => p,
            None => {
                let msg = format!("Cannot determine output path for {}", job.url);
                shared.lock().unwrap().push_log(format!("Error: {}", msg), true);
                return JobStatus::Error(msg);
            }
        };

        // Create parent directories
        if let Some(parent) = std::path::Path::new(&file_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let msg = format!("Cannot create directory: {}", e);
                shared.lock().unwrap().push_log(format!("Error: {}", msg), true);
                return JobStatus::Error(msg);
            }
        }

        // Check existing size for resume
        let existing_bytes = if job.resume || attempt > 0 {
            std::fs::metadata(&file_path).ok().map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        // Build request
        let mut req = CLIENT.get(&job.url);
        if existing_bytes > 0 {
            req = req.header("Range", format!("bytes={}-", existing_bytes));
        }

        let response = match req.timeout(Duration::from_secs(60)).send() {
            Ok(r) => r,
            Err(e) => {
                attempt += 1;
                if attempt > max_retries {
                    return JobStatus::Error(e.to_string());
                }
                continue;
            }
        };

        let status = response.status();
        // 416 = Range Not Satisfiable — file already complete
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            return JobStatus::Done;
        }
        if !status.is_success() {
            attempt += 1;
            if attempt > max_retries {
                return JobStatus::Error(format!("HTTP {}", status));
            }
            continue;
        }

        let is_partial    = status == reqwest::StatusCode::PARTIAL_CONTENT;
        let total_bytes   = response.content_length()
            .map(|n| n + if is_partial { existing_bytes } else { 0 });

        // Open file — append for resume, truncate for fresh start
        let mut file = match if is_partial && existing_bytes > 0 {
            std::fs::OpenOptions::new().append(true).open(&file_path)
        } else {
            std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&file_path)
        } {
            Ok(f) => f,
            Err(e) => return JobStatus::Error(format!("Cannot open {}: {}", file_path, e)),
        };

        // Mark as Downloading
        {
            let mut s = shared.lock().unwrap();
            if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) {
                j.status = JobStatus::Downloading;
            }
        }

        // Stream body with progress updates
        let mut downloaded = existing_bytes;
        let mut last_update = std::time::Instant::now();
        let mut last_bytes  = existing_bytes;
        let jid2    = job.id.clone();
        let jname2  = job.name.clone();
        let mut response = response;

        let result: Result<(), String> = (|| {
            use std::io::Read;
            let mut buf = vec![0u8; 256 * 1024]; // 256 KB chunks
            loop {
                // Check cancel
                if kill_rx.try_recv().is_ok() {
                    let mut s = shared.lock().unwrap();
                    if let Some(j) = s.queue.iter_mut().find(|j| j.id == jid2) {
                        j.status = JobStatus::Paused; j.resume = true;
                    }
                    return Err("cancelled".into());
                }

                let n = response.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 { break; }

                file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                downloaded += n as u64;

                // Update progress ~4x/sec
                let now = std::time::Instant::now();
                let elapsed = now.duration_since(last_update).as_secs_f64();
                if elapsed >= 0.25 {
                    let bps   = (downloaded - last_bytes) as f64 / elapsed;
                    let pct   = total_bytes.map(|t| downloaded as f32 / t as f32 * 100.0).unwrap_or(0.0);
                    let eta   = if bps > 0.0 {
                        total_bytes.map(|t| ((t.saturating_sub(downloaded)) as f64 / bps) as u64)
                    } else { None };
                    {
                        let mut s = shared.lock().unwrap();
                        if let Some(p) = s.progress.get_mut(&jid2) {
                            p.percent   = pct.min(100.0);
                            p.speed_bps = bps;
                            p.eta_secs  = eta;
                        }
                    }
                    last_update = now;
                    last_bytes  = downloaded;
                }
            }
            Ok(())
        })();

        match result {
            Err(e) if e == "cancelled" => return JobStatus::Paused,
            Err(e) => {
                attempt += 1;
                if attempt > max_retries { return JobStatus::Error(e); }
                continue;
            }
            Ok(()) => {
                // Final 100% progress update
                {
                    let mut s = shared.lock().unwrap();
                    if let Some(p) = s.progress.get_mut(&job.id) {
                        p.percent = 100.0; p.speed_bps = 0.0; p.eta_secs = Some(0);
                    }
                    s.push_log(format!("Done: {}", jname2), false);
                }
                return JobStatus::Done;
            }
        }
    }
}

fn estimated_size(url: &str) -> u64 {
    let parts: Vec<String> = reqwest::Url::parse(url).ok()
        .and_then(|u| Some(u.path_segments()?.map(|s| s.to_string()).collect::<Vec<_>>()))
        .unwrap_or_default();
    if parts.len() >= 2 {
        if let Some(bytes) = generated_dirs::folder_size(&parts[1]) {
            return bytes;
        }
    }
    0
}

fn guess_dest_path(dest: &str, url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 { return None; }
    let mut path = std::path::PathBuf::from(dest);
    for seg in &segs[2..] { path.push(url_decode(seg)); }
    Some(path.to_string_lossy().into_owned())
}

// ── App ───────────────────────────────────────────────────────────────────────
#[derive(PartialEq, Clone, Copy)]
enum BrowserTab { Browse, Search }

struct App {
    shared:               Arc<Mutex<Shared>>,
    settings:             Settings,
    settings_dirty:       bool,
    crumb_stack:          Vec<(String, String)>,
    current_path:         String,
    entries:              Vec<DirEntry>,
    loading:              bool,
    load_error:           Option<String>,
    queued_urls:          HashSet<String>,
    selected_urls:        HashSet<String>,
    downloaded_urls:      HashSet<String>,
    queue_selected:       HashSet<String>,
    folder_selected:      HashSet<String>, // folder hrefs selected (checkbox, not yet queued)
    folder_sizes:         HashMap<String, u64>,
    scroll_positions:     HashMap<String, f32>,
    pending_scroll_restore: bool,
    last_queue_click_idx: Option<usize>,
    status_msg:           String,
    status_active:        bool,
    dl_tx:                std::sync::mpsc::Sender<DlCmd>,
    folder_pick_tx:       std::sync::mpsc::Sender<Option<String>>,
    folder_pick_rx:       std::sync::mpsc::Receiver<Option<String>>,
    filter_query:         String,
    search_query:         String,
    search_open:          bool,
    browser_tab:          BrowserTab,
    search_include:       String,
    search_exclude:       String,
    // Panel sizing (fraction or pixels)
    browser_frac:         f32,   // browser width as fraction of central panel
    dl_panel_h:           f32,   // active downloads panel height in px
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = load_settings();
        apply_theme(&cc.egui_ctx, settings.retro_theme);
        setup_fonts(&cc.egui_ctx);
        let shared     = Arc::new(Mutex::new(Shared::default()));
        let saved      = load_queue();
        let queued_urls: HashSet<String> = saved.iter().map(|j| j.url.clone()).collect();
        {
            let mut s = shared.lock().unwrap();
            s.queue = saved;
            let n = s.queue.len();
            if n > 0 { s.push_log(format!("Loaded {} queued item(s) from disk", n), false); }
            // Initialise dl_settings from persisted settings so manager has correct values immediately
            let dest = shellexpand::tilde(&settings.dest_path).to_string();
            s.dl_settings = DlSettings {
                dest,
                concurrent:  settings.concurrent,
                max_retries: settings.max_retries,
                verify:      settings.verify_checksums,
                paused:      settings.queue_paused,
            };
        }

        let (dl_tx, dl_rx) = std::sync::mpsc::channel::<DlCmd>();
        { let s2 = Arc::clone(&shared); thread::spawn(move || download_manager(dl_rx, s2)); }

        let (fp_tx, fp_rx) = std::sync::mpsc::channel::<Option<String>>();

        let mut app = Self {
            shared, settings, settings_dirty: false,
            crumb_stack: vec![], current_path: String::new(),
            entries: vec![], loading: false, load_error: None,
            queued_urls,
            selected_urls:   HashSet::new(),
            downloaded_urls: load_downloaded(),
            queue_selected:  HashSet::new(),
            folder_selected:   HashSet::new(),
            folder_sizes:    load_folder_sizes(),
            scroll_positions: HashMap::new(),
            pending_scroll_restore: false,
            last_queue_click_idx: None,
            status_msg:    "Ready".to_string(),
            status_active: false,
            dl_tx, folder_pick_tx: fp_tx, folder_pick_rx: fp_rx,
            filter_query:  String::new(),
            search_query:  String::new(),
            search_open:   false,
            browser_tab:   BrowserTab::Browse,
            search_include: String::new(),
            search_exclude: String::new(),
            browser_frac:  0.62,
            dl_panel_h:    30.0 + 62.0 * 3.0,
        };
        app.navigate(String::new());
        // Start building search index in background so first search is instant
        generated_dirs::warm_search_index();
        app
    }

    fn navigate(&mut self, path: String) {
        // Save scroll position for the path we're leaving
        // (actual offset is written by draw_browser each frame via egui ctx)
        self.current_path  = path.clone();
        self.entries.clear();
        self.load_error    = None;
        self.loading       = true;
        self.status_active = true;
        self.status_msg    = "Fetching directory…".into();
        self.filter_query.clear();
        self.selected_urls.clear();
        self.folder_selected.clear();
        let shared   = Arc::clone(&self.shared);
        let url      = format!("{}{}", BASE_URL, path);
        let rel_path = path.clone();
        thread::spawn(move || {
            let result = fetch_directory(&url, &rel_path);
            shared.lock().unwrap().browse_result = Some((path, result));
        });
    }

    fn poll(&mut self) {
        if let Some((path, result)) = self.shared.lock().unwrap().browse_result.take() {
            // Discard if user navigated away before this result arrived
            if path != self.current_path {
                // result is stale — drop it, keep loading state as-is
            } else {
                self.loading = false; self.status_active = false;
                match result {
                Ok(e) => {
                    let folders = e.iter().filter(|x| x.is_folder).count();
                    let files   = e.iter().filter(|x| !x.is_folder).count();
                    let page_bytes: u64 = e.iter()
                        .filter(|x| !x.is_folder)
                        .map(|x| parse_size_str(&x.size))
                        .sum();

                    // Cache this folder's file-total size keyed by its path.
                    // The key is current_path with trailing slash stripped, e.g.
                    // "" for root, "No-Intro/" → "No-Intro", "No-Intro/Nintendo - Game Boy/" → …
                    let path_key = self.current_path.trim_end_matches('/').to_string();
                    if !path_key.is_empty() && page_bytes > 0 {
                        self.folder_sizes.insert(path_key.clone(), page_bytes);
                        save_folder_sizes(&self.folder_sizes);
                    }

                    let mut parts = vec![];
                    if folders > 0 { parts.push(format!("{} folder{}", folders, if folders==1{""} else {"s"})); }
                    if files   > 0 { parts.push(format!("{} file{}", files, if files==1{""} else {"s"})); }
                    if page_bytes > 0 { parts.push(fmt_size(page_bytes)); }
                    self.status_msg = if parts.is_empty() { "Empty".into() } else { parts.join("  ·  ") };
                    self.entries    = e;
                    self.load_error = None;
                    self.pending_scroll_restore = true;
                }
                Err(e) => { self.load_error = Some(e.clone()); self.status_msg = format!("Error: {}", e); }
            }
            } // end else (path matched)
        }

        if let Ok(Some(path)) = self.folder_pick_rx.try_recv() {
            self.settings.dest_path = path;
            self.settings_dirty = true;
        }

        if self.settings_dirty {
            save_settings(&self.settings);
            self.settings_dirty = false;
            // Keep dl_settings in sync so the manager thread always has current values
            let dest = shellexpand::tilde(&self.settings.dest_path).to_string();
            let mut s = self.shared.lock().unwrap();
            s.dl_settings = DlSettings {
                dest,
                concurrent:  self.settings.concurrent,
                max_retries: self.settings.max_retries,
                verify:      self.settings.verify_checksums,
                paused:      self.settings.queue_paused,
            };
        }

        // Drain newly completed URLs into the persistent downloaded set
        let completed: Vec<String> = {
            let mut s = self.shared.lock().unwrap();
            std::mem::take(&mut s.newly_completed)
        };
        if !completed.is_empty() {
            for url in completed { self.downloaded_urls.insert(url); }
            save_downloaded(&self.downloaded_urls);
        }

        // Sync queued_urls and queue_selected with the actual queue
        {
            let s = self.shared.lock().unwrap();
            let queue_ids:  HashSet<String> = s.queue.iter().map(|j| j.id.clone()).collect();
            let queue_urls: HashSet<String> = s.queue.iter().map(|j| j.url.clone()).collect();
            self.queued_urls.retain(|u| queue_urls.contains(u));
            self.queue_selected.retain(|id| queue_ids.contains(id));
        }

        // Auto-kick: if queue is running, there are waiting jobs, and free slots exist
        let (has_waiting, active) = {
            let s = self.shared.lock().unwrap();
            let waiting = s.queue.iter().any(|j| j.status == JobStatus::Waiting);
            (waiting, s.active_dl)
        };
        if !self.settings.queue_paused && has_waiting && active < self.settings.concurrent {
            self.kick_downloads();
        }
    }

    fn add_to_queue(&mut self, url: String, name: String, file_size: u64) -> bool {
        if self.queued_urls.contains(&url) { return false; }
        self.queued_urls.insert(url.clone());
        let job = QueueJob {
            id: next_id(), url: url.clone(), name: name.clone(),
            path: if self.current_path.is_empty() { "/".into() } else { self.current_path.clone() },
            status: JobStatus::Waiting, resume: false, retry_count: 0, verified: None,
            file_size,
        };
        {
            let mut s = self.shared.lock().unwrap();
            s.queue.push(job.clone());
            s.push_log(format!("Queued: {}", name), false);
            save_queue(&s.queue);
        }
        // kick_downloads() is called by poll() automatically when slots are free,
        // so we don't need to call it here for every file added
        true
    }

    fn kick_downloads(&mut self) {
        let dest = shellexpand::tilde(&self.settings.dest_path).to_string();
        if std::fs::create_dir_all(&dest).is_err() { return; }
        // Sync current settings into shared so the manager thread can self-kick
        // independently of the UI (survives screen lock, minimise, etc.)
        {
            let mut s = self.shared.lock().unwrap();
            s.dl_settings = DlSettings {
                dest:        dest,
                concurrent:  self.settings.concurrent,
                max_retries: self.settings.max_retries,
                verify:      self.settings.verify_checksums,
                paused:      self.settings.queue_paused,
            };
        }
        // Signal the manager to kick now (it will also kick every second on its own)
        let _ = self.dl_tx.send(DlCmd::Start(
            QueueJob { id: String::new(), url: String::new(), name: String::new(),
                       path: String::new(), status: JobStatus::Waiting, resume: false,
                       retry_count: 0, verified: None, file_size: 0 },
            String::new(), 0, 0, false));
    }

    fn toggle_folder_selected(&mut self, folder_href: String) {
        if self.folder_selected.contains(&folder_href) {
            self.folder_selected.remove(&folder_href);
        } else {
            self.folder_selected.insert(folder_href);
        }
    }

    fn resume_job(&mut self, id: &str) {
        {
            let mut s = self.shared.lock().unwrap();
            if let Some(j) = s.queue.iter_mut().find(|j| j.id == id) {
                j.status = JobStatus::Waiting; j.resume = true;
            }
        }
        self.kick_downloads();
    }

    fn remove_from_queue(&mut self, id: &str) {
        let _ = self.dl_tx.send(DlCmd::Cancel(id.to_string()));
        let mut s = self.shared.lock().unwrap();
        if let Some(pos) = s.queue.iter().position(|j| j.id == id) {
            self.queued_urls.remove(&s.queue[pos].url);
            s.progress.remove(id);
            s.queue.remove(pos);
        }
        save_queue(&s.queue);
    }

    fn pause_all_active(&mut self) {
        let ids: Vec<String> = {
            let s = self.shared.lock().unwrap();
            s.queue.iter()
                .filter(|j| j.status.is_active())
                .map(|j| j.id.clone())
                .collect()
        };
        for id in ids {
            let _ = self.dl_tx.send(DlCmd::Cancel(id.clone()));
            let mut s = self.shared.lock().unwrap();
            if let Some(j) = s.queue.iter_mut().find(|j| j.id == id) {
                j.status = JobStatus::Paused;
                j.resume = true;
            }
        }
        let mut s = self.shared.lock().unwrap();
        save_queue(&s.queue);
        s.push_log("Paused all active downloads", false);
    }

}

// ── egui visuals & fonts ──────────────────────────────────────────────────────
fn dark_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill       = C_BG;   v.window_fill      = C_SURF;
    v.faint_bg_color   = C_SURF2; v.extreme_bg_color = C_BORDER;
    v.window_stroke    = Stroke::new(1.0, C_BORDER);
    v.widgets.noninteractive.bg_fill   = C_SURF;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, C_BORDER);
    v.widgets.inactive.bg_fill         = C_SURF2;
    v.widgets.inactive.bg_stroke       = Stroke::new(1.0, C_BORDER2);
    v.widgets.hovered.bg_fill          = Color32::from_rgb(0x20, 0x28, 0x35);
    v.widgets.hovered.bg_stroke        = Stroke::new(1.0, C_BORDER2);
    v.widgets.active.bg_fill           = Color32::from_rgb(0x1a, 0x22, 0x2c);
    v.widgets.active.bg_stroke         = Stroke::new(1.0, C_ACC);
    v.selection.bg_fill                = Color32::from_rgb(0x1e, 0x3a, 0x2f);
    v.selection.stroke                 = Stroke::new(1.0, C_ACC);
    v.override_text_color              = Some(C_TEXT);
    v.hyperlink_color                  = C_ACC;
    v.warn_fg_color = C_WARN; v.error_fg_color = C_ERR;
    v
}

fn retro_visuals() -> egui::Visuals {
    // Vivid green-phosphor CRT palette
    let bg      = Color32::from_rgb(0x04, 0x08, 0x05);
    let surf    = Color32::from_rgb(0x07, 0x10, 0x09);
    let surf2   = Color32::from_rgb(0x0c, 0x18, 0x0e);
    let border  = Color32::from_rgb(0x14, 0x2e, 0x18);
    let border2 = Color32::from_rgb(0x1e, 0x48, 0x24);
    let acc     = C_ACC;
    let text    = Color32::from_rgb(0x9a, 0xff, 0xb4);  // bright phosphor green
    let mut v = egui::Visuals::dark();
    v.panel_fill       = bg;
    v.window_fill      = surf;
    v.faint_bg_color   = surf2;
    v.extreme_bg_color = border;
    v.window_stroke    = Stroke::new(1.0, border2);
    v.widgets.noninteractive.bg_fill   = surf;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    v.widgets.inactive.bg_fill         = surf2;
    v.widgets.inactive.bg_stroke       = Stroke::new(1.0, border2);
    v.widgets.hovered.bg_fill          = Color32::from_rgb(0x0e, 0x28, 0x12);
    v.widgets.hovered.bg_stroke        = Stroke::new(1.0, border2);
    v.widgets.active.bg_fill           = Color32::from_rgb(0x08, 0x1e, 0x0c);
    v.widgets.active.bg_stroke         = Stroke::new(1.0, acc);
    v.selection.bg_fill                = Color32::from_rgb(0x08, 0x28, 0x10);
    v.selection.stroke                 = Stroke::new(1.0, acc);
    v.override_text_color              = Some(text);
    v.hyperlink_color                  = acc;
    v.warn_fg_color = C_WARN; v.error_fg_color = C_ERR;
    v
}

fn apply_theme(ctx: &egui::Context, retro: bool) {
    ctx.set_visuals(if retro { retro_visuals() } else { dark_visuals() });
}

fn setup_fonts(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (egui::TextStyle::Small,     FontId::monospace(10.0)),
        (egui::TextStyle::Body,      FontId::proportional(13.0)),
        (egui::TextStyle::Monospace, FontId::monospace(12.0)),
        (egui::TextStyle::Button,    FontId::monospace(11.0)),
        (egui::TextStyle::Heading,   FontId::monospace(14.0)),
    ].into();
    // Wide, always-visible scrollbars — much easier to grab on Windows/macOS
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width:                   10.0,
        handle_min_length:           24.0,
        bar_inner_margin:            2.0,
        bar_outer_margin:            0.0,
        floating:                    false,
        floating_allocated_width:    0.0,
        dormant_background_opacity:  1.0,
        active_background_opacity:   1.0,
        interact_background_opacity: 1.0,
        dormant_handle_opacity:      0.7,
        active_handle_opacity:       1.0,
        interact_handle_opacity:     1.0,
        ..Default::default()
    };
    ctx.set_style(style);
}

// ── eframe::App ───────────────────────────────────────────────────────────────
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();

        // Snapshot only what's needed for rendering — avoid cloning the full queue
        let (has_spooling, has_waiting, queue_len_for_ui, active_dl) = {
            let s = self.shared.lock().unwrap();
            (
                s.queue.iter().any(|j| j.status == JobStatus::Spooling),
                s.queue.iter().any(|j| j.status == JobStatus::Waiting),
                s.queue.len(),
                s.active_dl,
            )
        };
        let prog_snap = {
            let s = self.shared.lock().unwrap();
            s.progress.clone() // only active downloads — small
        };

        if self.loading || active_dl > 0 || has_spooling || has_waiting || self.settings.retro_theme {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        if self.status_active && active_dl == 0 {
            if !has_spooling {
                let (done, errs) = {
                    let s = self.shared.lock().unwrap();
                    let done = s.queue.iter().filter(|j| j.status == JobStatus::Done).count();
                    let errs = s.queue.iter().filter(|j| matches!(j.status, JobStatus::Error(_))).count();
                    (done, errs)
                };
                self.status_active = false;
                if done + errs > 0 {
                    self.status_msg = format!("Finished — {} done{}", done,
                        if errs > 0 { format!(", {} failed", errs) } else { String::new() });
                }
            }
        }
        if active_dl > 0 { self.status_active = true; }

        // Update window title with live stats
        {
            let total_bps: f64 = prog_snap.values().map(|p| p.speed_bps).sum();
            let waiting = { self.shared.lock().unwrap().queue.iter().filter(|j| j.status == JobStatus::Waiting).count() };
            let title = if active_dl > 0 {
                if total_bps > 0.0 {
                    format!("myrient-dl  —  ↓ {}  ·  {} active  ·  {} queued", fmt_speed(total_bps), active_dl, waiting)
                } else {
                    format!("myrient-dl  —  {} active  ·  {} queued", active_dl, waiting)
                }
            } else if queue_len_for_ui > 0 {
                format!("myrient-dl  —  {} queued", waiting)
            } else {
                "myrient-dl".to_string()
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }


        egui::TopBottomPanel::top("toolbar")
            .frame(panel_frame(C_SURF))
            .exact_height(44.0)
            .show(ctx, |ui: &mut egui::Ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui: &mut egui::Ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("myrient-dl")
                        .font(FontId::monospace(14.0)).color(C_ACC).strong());
                    ui.add_space(10.0); vsep(ui); ui.add_space(10.0);

                    // Right-side controls first so dest field gets what's left
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                        ui.add_space(10.0);

                        // Retro theme toggle — rightmost
                        let retro = self.settings.retro_theme;
                        let label = if retro { "retro" } else { "dim" };
                        let col   = if retro { C_ACC } else { C_MUTED };
                        if ui.add(egui::Button::new(mono(label, 9.0, col))
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::new(0.5, C_BORDER2))
                            .min_size(Vec2::new(38.0, 20.0))
                        ).on_hover_text("Toggle retro theme").clicked() {
                            self.settings.retro_theme = !self.settings.retro_theme;
                            self.settings_dirty = true;
                            apply_theme(ctx, self.settings.retro_theme);
                        }
                        ui.add_space(6.0); vsep(ui); ui.add_space(6.0);

                        ui.label(mono("Verify", 9.0, C_MUTED));
                        ui.add_space(4.0);
                        let ver_resp = ui.checkbox(&mut self.settings.verify_checksums, "");
                        if ver_resp.changed() { self.settings_dirty = true; }

                        ui.add_space(6.0); vsep(ui); ui.add_space(6.0);
                        ui.scope(|ui| {
                            ui.spacing_mut().slider_width = 50.0;
                            let ret_resp = ui.add(
                                egui::Slider::new(&mut self.settings.max_retries, 0..=10)
                                    .show_value(true).clamp_to_range(true)
                            );
                            if ret_resp.changed() { self.settings_dirty = true; }
                        });
                        ui.add_space(4.0);
                        ui.label(mono("RETRIES", 9.0, C_MUTED));

                        ui.add_space(6.0); vsep(ui); ui.add_space(6.0);
                        ui.scope(|ui| {
                            ui.spacing_mut().slider_width = 60.0;
                            let conc_resp = ui.add(
                                egui::Slider::new(&mut self.settings.concurrent, 1..=16)
                                    .show_value(true).clamp_to_range(true)
                            );
                            if conc_resp.changed() {
                                self.settings_dirty = true;
                                let _ = self.dl_tx.send(DlCmd::SetConcurrent(self.settings.concurrent));
                            }
                        });
                        ui.add_space(4.0);
                        ui.label(mono("THREADS", 9.0, C_MUTED));

                        ui.add_space(6.0); vsep(ui); ui.add_space(6.0);

                        // Dest field fills whatever is left
                        let dest_w = (ui.available_width() - 50.0).max(80.0);
                        if ui.add(
                            egui::Button::new(mono("[ ]", 10.0, C_MUTED))
                                .fill(C_SURF2).stroke(Stroke::new(1.0, C_BORDER2))
                                .min_size(Vec2::new(26.0, 22.0))
                        ).on_hover_text("Browse for folder").clicked() {
                            let tx = self.folder_pick_tx.clone();
                            thread::spawn(move || {
                                let r = rfd::FileDialog::new().pick_folder();
                                let _ = tx.send(r.map(|p| p.to_string_lossy().into_owned()));
                            });
                        }
                        ui.add_space(4.0);
                        ui.label(mono("DEST", 9.0, C_MUTED));
                        ui.add_space(4.0);
                        let dest_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.settings.dest_path)
                                .font(FontId::monospace(12.0))
                                .desired_width(dest_w)
                                .text_color(C_TEXT)
                        );
                        if dest_resp.changed() { self.settings_dirty = true; }
                    });
                });
            });

        egui::TopBottomPanel::top("donate_bar")
            .frame(egui::Frame::none()
                .fill(C_BG)
                .inner_margin(egui::Margin::symmetric(14.0, 4.0)))
            .exact_height(22.0)
            .show(ctx, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    ui.label(mono("♥  Myrient is a free community resource — ", 9.5, C_ACC));
                    ui.hyperlink_to(mono("consider donating", 9.5, C_ACC), DONATE_URL);
                });
            });

        // ── Active downloads panel — always visible, fixed 5-slot height ──────
        let queue_snap: Vec<QueueJob> = self.shared.lock().unwrap().queue.clone();
        let active_jobs: Vec<&QueueJob> = queue_snap.iter()
            .filter(|j| j.status.is_active()).collect();

        let row_h = 62.0;

        egui::TopBottomPanel::bottom("active_dl_bottom")
            .frame(panel_frame(C_SURF))
            .resizable(true)
            .min_height(30.0 + row_h)
            .max_height(30.0 + row_h * 8.0)
            .default_height(self.dl_panel_h)
            .show(ctx, |ui: &mut egui::Ui| {
                self.dl_panel_h = ui.available_height() + 4.0; // keep in sync for save
                // Header bar — always shown
                egui::Frame::none().fill(C_SURF2).inner_margin(egui::Margin::symmetric(12.0, 4.0))
                    .show(ui, |ui: &mut egui::Ui| {
                        ui.horizontal(|ui: &mut egui::Ui| {
                            let dot_col = if self.status_active {
                                if self.settings.retro_theme {
                                    let t = ctx.input(|i| i.time);
                                    let a = (((t * std::f64::consts::TAU / 1.8).sin() as f32 + 1.0) / 2.0 * 155.0 + 100.0) as u8;
                                    Color32::from_rgba_premultiplied(
                                        (C_ACC.r() as f32 * a as f32 / 255.0) as u8,
                                        (C_ACC.g() as f32 * a as f32 / 255.0) as u8,
                                        (C_ACC.b() as f32 * a as f32 / 255.0) as u8, a)
                                } else { C_ACC }
                            } else { C_DIM };
                            ui.label(mono("●", 8.0, dot_col));
                            ui.add_space(6.0);
                            if active_jobs.is_empty() {
                                ui.label(mono("No active downloads", 9.0, C_MUTED));
                            } else {
                                let total_bps: f64 = active_jobs.iter()
                                    .filter_map(|j| prog_snap.get(&j.id))
                                    .map(|p| p.speed_bps)
                                    .sum();
                                ui.label(mono("DOWNLOADING", 9.0, C_MUTED));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                                    ui.label(mono(format!("{}/{} slots", active_dl, self.settings.concurrent), 9.0, C_MUTED));
                                    if total_bps > 0.0 {
                                        ui.add_space(10.0); vsep(ui); ui.add_space(10.0);
                                        ui.label(mono(format!("↓ {}", fmt_speed(total_bps)), 9.0, C_ACC));
                                    }
                                });
                            }
                        });
                    });
                hline(ui);
                egui::ScrollArea::vertical().id_source("act_scroll").auto_shrink([false;2])
                    .show(ui, |ui: &mut egui::Ui| {
                        let w = ui.available_width();
                        if active_jobs.is_empty() {
                            ui.add_space(row_h * 2.0);
                            ui.vertical_centered(|ui: &mut egui::Ui| {
                                ui.label(mono("No active downloads", 11.0, C_DIM));
                            });
                        } else {
                            for job in &active_jobs {
                                let prog = prog_snap.get(&job.id).cloned().unwrap_or_default();
                                self.draw_active_row(ui, job, &prog, w, row_h, ctx, C_ACC);
                            }
                        }
                    });
            });

        // ── Central panel ────────────────────────────────────────────────────
        let browser_frac = self.browser_frac;
        egui::SidePanel::left("browser_panel")
            .resizable(true)
            .min_width(240.0)
            .max_width(1200.0)
            .default_width(ctx.screen_rect().width() * browser_frac)
            .frame(egui::Frame::none().fill(C_BG)
                .stroke(Stroke::new(0.0, C_BG))) // no internal border
            .show(ctx, |ui: &mut egui::Ui| {
                self.browser_frac = (ui.available_width() / ctx.screen_rect().width()).clamp(0.2, 0.85);
                self.draw_browser(ui);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(C_BG))
            .show(ctx, |ui: &mut egui::Ui| {
                self.draw_queue(ui, &queue_snap, active_dl);
            });
    }
}

// ── Active row ────────────────────────────────────────────────────────────────
impl App {
    fn draw_active_row(&self, ui: &mut egui::Ui, job: &QueueJob, prog: &DownloadProgress,
                        avail_w: f32, row_h: f32, ctx: &egui::Context, bar_color: Color32) {
        let (row_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, row_h), egui::Sense::hover());
        ui.painter().line_segment([row_rect.left_bottom(), row_rect.right_bottom()],
            Stroke::new(1.0, C_BORDER));

        let pad   = 12.0;
        let btn_w = 60.0;
        let content_w = avail_w - pad * 2.0 - btn_w - 8.0;

        let name_y = row_rect.min.y + 8.0;
        let bar_y  = name_y + 16.0;
        let meta_y = bar_y + 7.0;
        let bar_h  = 5.0;
        let bar_x  = row_rect.min.x + pad;

        // Name
        let name_rect = egui::Rect::from_min_size(egui::pos2(bar_x, name_y), Vec2::new(content_w, 14.0));
        let ng = ui.painter().layout_no_wrap(job.name.clone(), FontId::monospace(11.0), C_TEXT);
        ui.painter().with_clip_rect(name_rect).galley(egui::pos2(bar_x, name_y), ng, C_TEXT);

        // Bar background
        let bar_rect = egui::Rect::from_min_size(egui::pos2(bar_x, bar_y), Vec2::new(content_w, bar_h));
        ui.painter().rect_filled(bar_rect, 2.0, C_BORDER2);

        let is_spooling = prog.spool_start.is_some() || prog.percent == 0.0;
        if is_spooling {
            let t = ctx.input(|i| i.time) as f32;
            let sweep = content_w * 0.3;
            let pos   = ((t * 0.8) % 1.4) - 0.15;
            let fx    = (bar_x + pos * content_w).clamp(bar_x, bar_x + content_w);
            let fw    = sweep.min((bar_x + content_w) - fx);
            if fw > 0.0 {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::pos2(fx, bar_y), Vec2::new(fw, bar_h)),
                    2.0, Color32::from_rgba_premultiplied(C_BLUE.r(), C_BLUE.g(), C_BLUE.b(), 180));
            }
            ui.painter().text(egui::pos2(bar_x, meta_y + bar_h), egui::Align2::LEFT_TOP,
                "spooling…", FontId::monospace(9.0), C_MUTED);
        } else {
            let filled = (content_w * prog.percent / 100.0).clamp(0.0, content_w);
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(bar_x, bar_y), Vec2::new(filled, bar_h)),
                2.0, bar_color);

            // Verified indicator on bar
            if let Some(v) = job.verified {
                let vx = bar_x + content_w - 14.0;
                ui.painter().text(egui::pos2(vx, bar_y - 1.0), egui::Align2::LEFT_TOP,
                    if v {"✓"} else {"⚠"}, FontId::monospace(9.0), if v {C_ACC} else {C_WARN});
            }

            let speed_str = if prog.speed_bps > 0.0 { fmt_speed(prog.speed_bps) } else { "…".into() };
            let eta_str   = prog.eta_secs.map(fmt_eta).unwrap_or_else(|| "…".into());
            let meta = format!("{}%  {}  ETA {}", prog.percent as u32, speed_str, eta_str);
            ui.painter().text(egui::pos2(bar_x, meta_y + bar_h), egui::Align2::LEFT_TOP,
                meta, FontId::monospace(9.0), C_MUTED);
        }

        // Pause button — fixed right side, no overlap
        let btn_cx  = row_rect.max.x - pad - btn_w / 2.0;
        let btn_rect = egui::Rect::from_center_size(
            egui::pos2(btn_cx, row_rect.center().y), Vec2::new(btn_w - 4.0, 20.0));
        let btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
        ui.painter().rect_filled(btn_rect, 2.0, C_SURF2);
        ui.painter().rect_stroke(btn_rect, 2.0, Stroke::new(1.0, C_BORDER2));
        ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "⏸ pause",
            FontId::monospace(9.5), if btn_resp.hovered() { C_WARN } else { C_MUTED });

        if btn_resp.clicked() {
            let _ = self.dl_tx.send(DlCmd::Cancel(job.id.clone()));
            let mut s = self.shared.lock().unwrap();
            if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) {
                j.status = JobStatus::Paused; j.resume = true;
            }
            s.push_log(format!("Paused: {}", job.name), false);
            save_queue(&s.queue);
        }
    }

    // ── Browser ───────────────────────────────────────────────────────────────
    fn draw_browser(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui: &mut egui::Ui| {
            // Tab bar: Browse | Search
            egui::Frame::none().fill(C_SURF).inner_margin(egui::Margin::symmetric(10.0, 4.0))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.horizontal(|ui: &mut egui::Ui| {
                        let browse_active = self.browser_tab == BrowserTab::Browse;
                        let search_active = self.browser_tab == BrowserTab::Search;
                        if ui.add(egui::Button::new(mono("BROWSE", 9.5,
                            if browse_active { C_TEXT } else { C_MUTED }))
                            .fill(if browse_active { C_SURF2 } else { Color32::TRANSPARENT })
                            .stroke(if browse_active { Stroke::new(1.0, C_BORDER2) } else { Stroke::NONE })
                            .min_size(Vec2::new(60.0, 20.0))
                        ).clicked() { self.browser_tab = BrowserTab::Browse; }
                        ui.add_space(4.0);
                        let lbl = if !self.search_query.is_empty() {
                            mono(format!("SEARCH  ·  {}", &self.search_query[..self.search_query.len().min(20)]), 9.5, C_ACC)
                        } else { mono("SEARCH", 9.5, if search_active { C_TEXT } else { C_MUTED }) };
                        if ui.add(egui::Button::new(lbl)
                            .fill(if search_active { C_SURF2 } else { Color32::TRANSPARENT })
                            .stroke(if search_active { Stroke::new(1.0, C_BORDER2) } else { Stroke::NONE })
                            .min_size(Vec2::new(60.0, 20.0))
                        ).clicked() { self.browser_tab = BrowserTab::Search; }
                    });
                });
            hline(ui);

            match self.browser_tab {
                BrowserTab::Search => { self.draw_search_tab(ui); return; }
                BrowserTab::Browse => {}
            }

            // ── Breadcrumb bar ──
            egui::Frame::none().fill(C_SURF).inner_margin(egui::Margin::symmetric(10.0, 5.0))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.set_min_height(26.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.horizontal_wrapped(|ui: &mut egui::Ui| {
                            let is_root = self.crumb_stack.is_empty();
                            if ui.add(egui::Button::new(
                                mono("/files/", 11.0, if is_root {C_TEXT} else {C_BLUE})).frame(false)
                            ).clicked() && !is_root {
                                self.crumb_stack.clear(); self.navigate(String::new());
                            }
                            let stack = self.crumb_stack.clone();
                            for (i, (label, path)) in stack.iter().enumerate() {
                                ui.label(mono("›", 12.0, C_DIM));
                                let is_tail = i == stack.len() - 1;
                                if ui.add(egui::Button::new(
                                    mono(label, 11.0, if is_tail {C_TEXT} else {C_BLUE})).frame(false)
                                ).clicked() && !is_tail {
                                    self.crumb_stack.truncate(i+1);
                                    self.navigate(path.clone());
                                    break;
                                }
                            }
                        });
                        // Directory info right-aligned in breadcrumb bar
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                            if !self.status_msg.is_empty() && !self.loading {
                                ui.label(mono(&self.status_msg, 9.0, C_MUTED));
                            } else if self.loading {
                                ui.label(mono("fetching…", 9.0, C_DIM));
                            }
                        });
                    });
                });
            hline(ui);

            // Filter + select all bar
            egui::Frame::none().fill(C_SURF).inner_margin(egui::Margin::symmetric(10.0, 5.0))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.set_min_height(28.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.label(mono("filter", 9.0, C_MUTED));
                        ui.add_space(4.0);

                        // Right-side buttons first so filter field gets remaining space
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                            let has_files   = !self.selected_urls.is_empty();
                            let has_folders = !self.folder_selected.is_empty();
                            if has_files || has_folders {
                                let file_n   = self.selected_urls.iter()
                                    .filter(|u| !self.queued_urls.contains(u.as_str()))
                                    .count();
                                let folder_n = self.folder_selected.len();
                                let btn_label = match (file_n, folder_n) {
                                    (f, 0) => format!("+ Add {} file{} to queue", f, if f==1{""} else {"s"}),
                                    (0, d) => format!("+ Add {} folder{} to queue", d, if d==1{""} else {"s"}),
                                    (f, d) => format!("+ Add {} file{} + {} folder{}", f, if f==1{""} else {"s"}, d, if d==1{""} else {"s"}),
                                };
                                if ui.add(egui::Button::new(mono(&btn_label, 9.0, Color32::from_rgb(0x04,0x0a,0x08)))
                                    .fill(C_ACC)
                                    .stroke(Stroke::NONE)
                                    .min_size(Vec2::new(120.0, 18.0))
                                ).clicked() {
                                    // Queue selected files immediately
                                    let to_queue: Vec<(String,String,u64)> = self.entries.iter()
                                        .filter(|e| !e.is_folder)
                                        .filter(|e| e.url.as_ref().map(|u| self.selected_urls.contains(u.as_ref())).unwrap_or(false))
                                        .filter(|e| e.url.as_ref().map(|u| !self.queued_urls.contains(u.as_ref())).unwrap_or(false))
                                        .filter_map(|e| e.url.as_ref().map(|u| (u.to_string(), e.name.to_string(), parse_size_str(&e.size))))
                                        .collect();
                                    let file_count = to_queue.len();
                                    for (url, name, sz) in to_queue { self.add_to_queue(url, name, sz); }
                                    self.selected_urls.clear();

                                    // For each selected folder, spawn a scan+queue thread
                                    let folder_count = self.folder_selected.len();
                                    for folder_href in self.folder_selected.drain().collect::<Vec<_>>() {
                                        let folder_url = format!("{}{}", BASE_URL, folder_href);
                                        let shared     = Arc::clone(&self.shared);
                                        let queued     = self.queued_urls.clone();
                                        let downloaded = self.downloaded_urls.clone();
                                        let dest_path  = self.current_path.clone();
                                        thread::spawn(move || {
                                            let mut urls: Vec<(String, String, u64)> = Vec::new();
                                            collect_files(&folder_url, &mut urls);
                                            let mut s = shared.lock().unwrap();
                                            let job_path = folder_href.trim_end_matches('/').to_string();
                                            let mut added = 0usize;
                                            for (url, name, file_size) in urls {
                                                if downloaded.contains(&url) { continue; }
                                                if queued.contains(&url) { continue; }
                                                let job = QueueJob {
                                                    id: next_id(), url: url.clone(), name,
                                                    path: job_path.clone(),
                                                    status: JobStatus::Waiting, resume: false,
                                                    retry_count: 0, verified: None, file_size,
                                                };
                                                s.queue.push(job);
                                                added += 1;
                                            }
                                            if added > 0 { save_queue(&s.queue); }
                                            let _ = dest_path;
                                        });
                                    }

                                    if file_count > 0 || folder_count > 0 {
                                        self.status_msg    = format!("Queuing {} file{} + {} folder{}…",
                                            file_count, if file_count==1{""} else {"s"},
                                            folder_count, if folder_count==1{""} else {"s"});
                                        self.status_active = true;
                                    }
                                }
                                ui.add_space(6.0);
                            }
                            // Always show deselect / select all
                            let has_sel = !self.selected_urls.is_empty() || !self.folder_selected.is_empty();
                            if ui.add(egui::Button::new(mono(if has_sel { "deselect" } else { "select all" }, 9.0, C_MUTED))
                                .fill(Color32::TRANSPARENT)
                                .stroke(Stroke::new(0.5, C_BORDER2))
                                .min_size(Vec2::new(64.0, 18.0))
                            ).clicked() {
                                if has_sel {
                                    self.selected_urls.clear();
                                    self.folder_selected.clear();
                                } else {
                                    let q = self.filter_query.to_lowercase();
                                    for e in self.entries.iter().filter(|e| !e.is_folder) {
                                        if q.is_empty() || e.name.to_lowercase().contains(&q) {
                                            if let Some(ref u) = e.url {
                                                if !self.downloaded_urls.contains(u.as_ref()) {
                                                    self.selected_urls.insert(u.to_string());
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Filter field and count — fill remaining space left-to-right
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui: &mut egui::Ui| {
                                let mut filter_w = ui.available_width() - 4.0;
                                if !self.filter_query.is_empty() { filter_w -= 60.0; } // reserve for count + x
                                let filter_w = filter_w.max(40.0);
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.filter_query)
                                        .font(FontId::monospace(12.0))
                                        .desired_width(filter_w)
                                        .hint_text("type to filter…")
                                        .text_color(C_TEXT)
                                );
                                if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                    self.filter_query.clear();
                                }
                                if !self.filter_query.is_empty() {
                                    ui.add_space(4.0);
                                    let q       = self.filter_query.to_lowercase();
                                    let folders = self.entries.iter().filter(|e|  e.is_folder && e.name.to_lowercase().contains(&q)).count();
                                    let files   = self.entries.iter().filter(|e| !e.is_folder && e.name.to_lowercase().contains(&q)).count();
                                    let label   = match (folders, files) {
                                        (0, f) => format!("{} file{}", f, if f==1{""} else {"s"}),
                                        (d, 0) => format!("{} folder{}", d, if d==1{""} else {"s"}),
                                        (d, f) => format!("{}f {}d", f, d),
                                    };
                                    ui.label(mono(label, 9.0, C_MUTED));
                                    ui.add_space(2.0);
                                    if ui.add(egui::Button::new(mono("x", 10.0, C_MUTED)).frame(false)
                                        .min_size(Vec2::new(14.0, 14.0))).clicked() {
                                        self.filter_query.clear();
                                    }
                                }
                            });
                        });
                    });
                });
            hline(ui);

            // Column headers — clean, no overlap
            let avail_w   = ui.available_width();
            let fixed_l   = 10.0 + 18.0 + 4.0; // pad + icon + gap
            let size_w    = 72.0;
            let date_w    = 116.0;
            let fixed_r   = size_w + date_w + 10.0;
            let name_w    = (avail_w - fixed_l - fixed_r).max(60.0);
            let size_x    = fixed_l + name_w;
            let date_x    = size_x + size_w;

            egui::Frame::none().fill(C_SURF).inner_margin(egui::Margin::symmetric(0.0, 3.0))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.set_min_height(20.0);
                    let (r, _) = ui.allocate_exact_size(Vec2::new(avail_w, 16.0), egui::Sense::hover());
                    ui.painter().text(egui::pos2(r.min.x + fixed_l, r.center().y),
                        egui::Align2::LEFT_CENTER, "NAME", FontId::monospace(9.0), C_DIM);
                    ui.painter().text(egui::pos2(r.min.x + size_x + size_w - 4.0, r.center().y),
                        egui::Align2::RIGHT_CENTER, "SIZE", FontId::monospace(9.0), C_DIM);
                    ui.painter().text(egui::pos2(r.min.x + date_x + date_w - 4.0, r.center().y),
                        egui::Align2::RIGHT_CENTER, "MODIFIED", FontId::monospace(9.0), C_DIM);
                });
            hline(ui);

            // Apply filter: build index list of visible entries — no clone
            let visible: Vec<usize> = if self.filter_query.is_empty() {
                (0..self.entries.len()).collect()
            } else {
                let q = self.filter_query.to_lowercase();
                self.entries.iter().enumerate()
                    .filter(|(_, e)| e.name.to_lowercase().contains(&q))
                    .map(|(i, _)| i)
                    .collect()
            };
            let entries = &self.entries; // borrow, no clone
            let row_h   = 26.0;

            if self.loading {
                ui.add_space(12.0);
                ui.horizontal(|ui: &mut egui::Ui| {
                    ui.add_space(14.0); ui.spinner(); ui.add_space(8.0);
                    ui.label(mono("Fetching…", 12.0, C_MUTED));
                });
            } else if let Some(ref err) = self.load_error.clone() {
                ui.add_space(12.0);
                ui.horizontal(|ui: &mut egui::Ui| {
                    ui.add_space(14.0);
                    ui.label(mono(format!("⚠ {}", err), 12.0, C_ERR));
                });
            } else if entries.is_empty() {
                ui.add_space(12.0);
                ui.horizontal(|ui: &mut egui::Ui| {
                    ui.add_space(14.0);
                    ui.label(mono("Empty directory", 12.0, C_DIM));
                });
            } else {
                    let total_rows = visible.len();
                    let mut open_path:        Option<String>             = None;
                    let mut queue_folder_req: Option<(String,String,String)> = None;
                    let mut queue_file_req:   Option<(String,String,u64)>  = None;

                    let saved_offset = self.scroll_positions.get(&self.current_path).copied().unwrap_or(0.0);
                    let mut sa = egui::ScrollArea::vertical()
                        .id_source("browser_rows")
                        .auto_shrink([false;2]);
                    if self.pending_scroll_restore {
                        sa = sa.vertical_scroll_offset(saved_offset);
                        self.pending_scroll_restore = false;
                    }
                    let scroll_out = sa.show_rows(ui, row_h, total_rows, |ui, row_range| {
                        for vis_idx in row_range {
                            let entry = &entries[visible[vis_idx]];
                            let avail_w = ui.available_width();
                        let is_queued     = entry.url.as_ref()
                            .map(|u| self.queued_urls.contains(u.as_ref()))
                            .unwrap_or(false);
                        let is_downloaded = entry.url.as_ref()
                            .map(|u| self.downloaded_urls.contains(u.as_ref()))
                            .unwrap_or(false);

                        let (row_rect, response) = ui.allocate_exact_size(
                            Vec2::new(avail_w, row_h), egui::Sense::click());
                        let hovered = response.hovered();

                        // Check if the click landed on the checkbox area (left 20px)
                        // We do this before drawing so we can suppress the row click below
                        let click_pos = ui.input(|i| i.pointer.interact_pos());
                        // Detect click in the left 24px zone — used by folder checkbox
                        let left_zone_clicked = response.clicked()
                            && click_pos.map(|p| {
                                p.x < row_rect.min.x + 24.0
                            }).unwrap_or(false);

                        if hovered {
                            ui.painter().rect_filled(row_rect, 0.0,
                                Color32::from_rgba_premultiplied(255,255,255,8));
                        } else if entry.url.as_ref().map(|u| self.selected_urls.contains(u.as_ref())).unwrap_or(false) {
                            ui.painter().rect_filled(row_rect, 0.0,
                                Color32::from_rgba_premultiplied(0x3d, 0xe8, 0xa0, 18));
                            // Left accent bar to indicate selection without washing out text
                            let accent_rect = egui::Rect::from_min_size(
                                row_rect.min, Vec2::new(2.0, row_rect.height()));
                            ui.painter().rect_filled(accent_rect, 0.0, C_ACC);
                        }
                        ui.painter().line_segment(
                            [row_rect.left_bottom(), row_rect.right_bottom()],
                            Stroke::new(1.0, Color32::from_rgb(0x0f,0x14,0x1a)));

                        let base_x = row_rect.min.x;
                        let cy     = row_rect.center().y;

                        // Left-side indicator / checkbox
                        // Folders: a subtle checkbox that blends with the design
                        // Files: just a dot indicator (no checkbox — selection shown by row highlight)
                        let _is_selected = entry.url.as_ref()
                            .map(|u| self.selected_urls.contains(u.as_ref()))
                            .unwrap_or(false);

                        if entry.is_folder {
                            let folder_full_href = format!("{}{}", self.current_path, entry.href);
                            let is_folder_sel = self.folder_selected.contains(&folder_full_href);
                            // Small checkbox, left-aligned, colour blends with border palette
                            let cb_rect = egui::Rect::from_center_size(
                                egui::pos2(base_x + 10.0, cy), Vec2::splat(12.0));
                            if is_folder_sel {
                                ui.painter().rect_filled(cb_rect, 2.0, C_ACC);
                                ui.painter().text(cb_rect.center(), egui::Align2::CENTER_CENTER,
                                    "✓", FontId::monospace(9.0), C_BG);
                            } else {
                                // Subtle — matches border colour, only visible on hover
                                let col = if hovered { C_BORDER2 } else { C_BORDER };
                                ui.painter().rect_stroke(cb_rect, 2.0, Stroke::new(1.0, col));
                            }
                        } else {
                            // File: small filled dot to indicate type — no checkbox
                            let dot_col = if is_downloaded { C_DOWNLOADED }
                                else if is_queued { C_ACC }
                                else { C_BORDER2 };
                            ui.painter().circle_filled(egui::pos2(base_x + 10.0, cy), 3.0, dot_col);
                        }

                        // No folder icon ('>') — folder is indicated by blue text colour alone

                        let name_color = if is_queued { C_ACC }
                            else if is_downloaded { C_DOWNLOADED }
                            else if entry.is_folder { C_BLUE }
                            else { C_FILE };
                        let name_clip_w = if entry.is_folder {
                            (name_w - 130.0).max(40.0) // always reserve space for buttons on folders
                        } else {
                            name_w
                        };
                        let clip_rect = egui::Rect::from_min_size(
                            egui::pos2(base_x + fixed_l, row_rect.min.y),
                            Vec2::new(name_clip_w, row_h));
                        let g = ui.painter().layout_no_wrap(entry.name.to_string(), FontId::monospace(11.0), name_color);
                        ui.painter().with_clip_rect(clip_rect)
                            .galley(egui::pos2(base_x + fixed_l, cy - g.size().y/2.0), g, name_color);

                        // Size — right-aligned in its column
                        let folder_key = entry.href.trim_end_matches('/');
                        let size_str = if entry.is_folder {
                            let full_key = if self.current_path.is_empty() {
                                folder_key.to_string()
                            } else {
                                format!("{}{}", self.current_path.trim_end_matches('/'), "/")
                                    + folder_key
                            };
                            // Lazy cache first, then compiled-in fallback
                            self.folder_sizes.get(&full_key)
                                .copied()
                                .or_else(|| generated_dirs::folder_size(&full_key))
                                .map(fmt_size)
                                .unwrap_or_else(|| "—".into())
                        } else {
                            if entry.size.is_empty() { "—".into() } else { entry.size.to_string() }
                        };
                        ui.painter().text(
                            egui::pos2(base_x + size_x + size_w - 4.0, cy),
                            egui::Align2::RIGHT_CENTER, &size_str,
                            FontId::monospace(10.0), C_MUTED);

                        // Date
                        ui.painter().text(
                            egui::pos2(base_x + date_x + date_w - 4.0, cy),
                            egui::Align2::RIGHT_CENTER, entry.date.as_ref(),
                            FontId::monospace(10.0), C_DIM);

                        // Folder checkbox interaction — left side 20px zone
                        let mut btn_open_clicked   = false;
                        let mut btn_folder_clicked = false;
                        if entry.is_folder {
                            let _folder_full_href = format!("{}{}", self.current_path, entry.href);
                            let fcb_hit = egui::Rect::from_min_size(
                                row_rect.min, Vec2::new(24.0, row_rect.height()));
                            let fcb_clicked = response.clicked()
                                && click_pos.map(|p| fcb_hit.contains(p)).unwrap_or(false);
                            if fcb_clicked { btn_folder_clicked = true; }

                            // "-> open" hover button
                            let btn_h  = 18.0;
                            let open_w = 52.0;
                            let open_rect = egui::Rect::from_min_size(
                                egui::pos2(row_rect.max.x - open_w - 8.0, cy - btn_h/2.0),
                                Vec2::new(open_w, btn_h));
                            let open_resp = ui.allocate_rect(open_rect, egui::Sense::click());
                            if hovered {
                                ui.painter().rect_filled(open_rect, 2.0, C_SURF2);
                                ui.painter().rect_stroke(open_rect, 2.0, Stroke::new(1.0, C_BORDER2));
                                ui.painter().text(open_rect.center(), egui::Align2::CENTER_CENTER,
                                    "-> open", FontId::monospace(9.0),
                                    if open_resp.hovered() { C_TEXT } else { C_MUTED });
                            }
                            btn_open_clicked = open_resp.clicked();
                        }

                        // Handle interactions
                        if btn_folder_clicked {
                            let url  = format!("{}{}{}", BASE_URL, self.current_path, entry.href);
                            let href = format!("{}{}", self.current_path, entry.href);
                            queue_folder_req = Some((url, href, entry.name.to_string()));
                        } else if btn_open_clicked || (response.clicked() && entry.is_folder && !left_zone_clicked) {
                            let new_path = format!("{}{}", self.current_path, entry.href);
                            open_path = Some(new_path.clone());
                            self.crumb_stack.push((entry.name.to_string(), new_path));
                        } else if response.double_clicked() && !entry.is_folder {
                            if let Some(ref url) = entry.url {
                                if !is_queued {
                                    queue_file_req = Some((url.to_string(), entry.name.to_string(), parse_size_str(&entry.size)));
                                    self.selected_urls.remove(url.as_ref());
                                }
                            }
                        } else if response.clicked() && !entry.is_folder {
                            // Row click — toggle file selection
                            if let Some(ref url) = entry.url {
                                if !is_queued && !is_downloaded {
                                    if self.selected_urls.contains(url.as_ref()) {
                                        self.selected_urls.remove(url.as_ref());
                                    } else {
                                        self.selected_urls.insert(url.to_string());
                                    }
                                }
                            }
                        }
                    }
                        }); // end show_rows

                    // Persist scroll position for this path every frame
                    self.scroll_positions.insert(
                        self.current_path.clone(),
                        scroll_out.state.offset.y,
                    );

                    // Apply deferred actions outside the entry loop
                    if let Some(path) = open_path { self.navigate(path); }
                    if let Some((_url, href, _name)) = queue_folder_req { self.toggle_folder_selected(href); }
                    if let Some((u,n,sz)) = queue_file_req {
                        self.add_to_queue(u, n.clone(), sz);
                        self.status_msg    = format!("Queued: {}", n);
                        self.status_active = true;
                    }
            } // end else
        });
    }

    // ── Search tab ────────────────────────────────────────────────────────────
    fn draw_search_tab(&mut self, ui: &mut egui::Ui) {
        let avail_w = ui.available_width();

        // Search input + filter controls
        egui::Frame::none().fill(C_SURF).inner_margin(egui::Margin::symmetric(10.0, 6.0))
            .show(ui, |ui: &mut egui::Ui| {
                ui.vertical(|ui: &mut egui::Ui| {
                    // Main search bar
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.label(mono("⌕", 13.0, C_MUTED));
                        ui.add_space(4.0);
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.search_query)
                                .font(FontId::monospace(12.0))
                                .desired_width(ui.available_width() - 30.0)
                                .hint_text("search all files…")
                                .text_color(C_TEXT)
                        );
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.search_query.clear();
                        }
                        if !self.search_query.is_empty() {
                            if ui.add(egui::Button::new(mono("✕", 10.0, C_MUTED)).frame(false)).clicked() {
                                self.search_query.clear();
                            }
                        }
                        let _ = resp;
                    });
                    ui.add_space(4.0);
                    // Directory filters
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.label(mono("only:", 9.0, C_DIM));
                        ui.add_space(2.0);
                        ui.add(egui::TextEdit::singleline(&mut self.search_include)
                            .font(FontId::monospace(9.5))
                            .desired_width(120.0)
                            .hint_text("e.g. No-Intro")
                            .text_color(C_TEXT));
                        ui.add_space(8.0);
                        ui.label(mono("exclude:", 9.0, C_DIM));
                        ui.add_space(2.0);
                        ui.add(egui::TextEdit::singleline(&mut self.search_exclude)
                            .font(FontId::monospace(9.5))
                            .desired_width(120.0)
                            .hint_text("e.g. BIOS")
                            .text_color(C_TEXT));
                    });
                });
            });
        hline(ui);

        if self.search_query.is_empty() {
            ui.add_space(20.0);
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.add_space(14.0);
                ui.label(mono("Type to search across all baked-in folders", 11.0, C_DIM));
            });
            return;
        }

        let q        = self.search_query.to_lowercase();
        let inc      = self.search_include.to_lowercase();
        let exc      = self.search_exclude.to_lowercase();

        let results: Vec<(String, String, String, u64)> = generated_dirs::search(&q)
            .filter(|e| inc.is_empty() || e.folder.to_lowercase().contains(&inc))
            .filter(|e| exc.is_empty() || !e.folder.to_lowercase().contains(&exc))
            .take(500)
            .map(|e| (e.name.clone(), e.folder.to_string(), format!("{}/{}", e.folder, e.name), e.size_bytes))
            .collect();

        let n = results.len();
        egui::Frame::none().fill(C_SURF2).inner_margin(egui::Margin::symmetric(10.0, 3.0))
            .show(ui, |ui: &mut egui::Ui| {
                ui.label(mono(
                    if n >= 500 { "500+ matches (refine your search)".to_string() }
                    else { format!("{} match{}", n, if n==1{""} else {"es"}) },
                    9.0, C_MUTED));
            });
        hline(ui);

        let row_h = 30.0;
        egui::ScrollArea::vertical().id_source("search_tab_results").auto_shrink([false;2])
            .show_rows(ui, row_h, results.len(), |ui, range| {
                for (name, folder, full_path, size) in &results[range] {
                    let (row, resp) = ui.allocate_exact_size(Vec2::new(avail_w, row_h), egui::Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(row, 0.0, Color32::from_rgba_premultiplied(255,255,255,6));
                    }
                    ui.painter().line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, C_BORDER));
                    let cx = row.min.x + 10.0;
                    let top_y = row.min.y + 8.0;
                    let bot_y = row.min.y + 22.0;
                    // File name on top line
                    ui.painter().text(egui::pos2(cx, top_y), egui::Align2::LEFT_TOP,
                        name, FontId::monospace(11.0), C_FILE);
                    // Folder path on bottom line, muted
                    ui.painter().text(egui::pos2(cx, bot_y), egui::Align2::LEFT_TOP,
                        folder, FontId::monospace(9.0), C_MUTED);
                    // Size right-aligned
                    if *size > 0 {
                        ui.painter().text(egui::pos2(row.max.x - 8.0, top_y), egui::Align2::RIGHT_TOP,
                            fmt_size(*size), FontId::monospace(9.0), C_MUTED);
                    }
                    if resp.clicked() {
                        let folder_path = full_path.rsplit_once('/').map(|(p,_)| format!("{}/", p)).unwrap_or_default();
                        self.crumb_stack.clear();
                        let mut path = String::new();
                        for seg in folder_path.trim_end_matches('/').split('/').filter(|s| !s.is_empty()) {
                            path = format!("{}{}/", path, seg);
                            self.crumb_stack.push((url_decode(seg), path.clone()));
                        }
                        let nav_path = self.crumb_stack.last().map(|(_,p)| p.clone()).unwrap_or_default();
                        self.browser_tab = BrowserTab::Browse;
                        self.navigate(nav_path);
                    }
                }
            });
    }

    // ── Queue pane ────────────────────────────────────────────────────────────
    fn draw_queue(&mut self, ui: &mut egui::Ui, queue: &[QueueJob], active_dl: usize) {
        ui.vertical(|ui: &mut egui::Ui| {
            egui::Frame::none().fill(C_SURF).inner_margin(egui::Margin::symmetric(10.0,5.0))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.set_min_height(26.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.label(mono("QUEUE", 9.0, C_MUTED));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                            if !queue.is_empty() {
                                let total_bytes: u64 = queue.iter().map(|j| j.file_size).sum();
                                let count_str = format!("{} item{}", queue.len(), if queue.len()==1{""} else {"s"});
                                if total_bytes > 0 {
                                    ui.label(mono(format!("{}  ·  {}", count_str, fmt_size(total_bytes)), 9.0, C_MUTED));
                                } else {
                                    ui.label(mono(count_str, 9.0, C_MUTED));
                                }
                            }
                        });
                    });
                });
            hline(ui);

            egui::Frame::none().fill(C_SURF).inner_margin(egui::Margin::symmetric(10.0,3.0))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.set_min_height(20.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.label(mono("STATUS", 9.0, C_DIM));
                        ui.add_space(28.0);
                        ui.label(mono("FILE", 9.0, C_DIM));
                    });
                });
            hline(ui);

            let avail_w  = ui.available_width();
            let has_sel  = !self.queue_selected.is_empty();
            let footer_h = if has_sel { 130.0 } else { 72.0 };
            let scroll_h = (ui.available_height() - footer_h).max(40.0);
            let mut to_remove:     Option<String>            = None;
            let mut to_resume:     Option<String>            = None;
            let mut clicked_idx:   Option<(usize, bool)>     = None; // (idx, shift_held)

            let cb_w     = 20.0;
            let status_w = (avail_w * 0.22).clamp(65.0, 88.0);
            let rm_w     = 24.0;
            let res_w    = 62.0;
            let row_h    = 38.0;
            let n_rows   = queue.len();

            egui::ScrollArea::vertical().id_source("queue_scroll")
                .max_height(scroll_h).auto_shrink([false;2])
                .show_rows(ui, row_h, n_rows, |ui, row_range| {
                    ui.set_min_width(avail_w);

                    if queue.is_empty() {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui: &mut egui::Ui| {
                            ui.label(mono("No files queued.", 11.0, C_DIM));
                            ui.add_space(4.0);
                            ui.label(mono("Click files to add them.", 10.0, C_DIM));
                        });
                        return;
                    }

                    let shift = ui.input(|i| i.modifiers.shift);

                    for idx in row_range {
                        let job    = &queue[idx];
                        let is_sel = self.queue_selected.contains(&job.id);

                        let (row_rect, row_resp) = ui.allocate_exact_size(
                            Vec2::new(avail_w, row_h), egui::Sense::click());
                        ui.painter().line_segment([row_rect.left_bottom(), row_rect.right_bottom()],
                            Stroke::new(1.0, C_BORDER));

                        if is_sel {
                            ui.painter().rect_filled(row_rect, 0.0,
                                Color32::from_rgba_premultiplied(0x2a, 0x34, 0x44, 40));
                            let accent = egui::Rect::from_min_size(row_rect.min, Vec2::new(2.0, row_h));
                            ui.painter().rect_filled(accent, 0.0, C_MUTED);
                        }

                        if row_resp.clicked() { clicked_idx = Some((idx, shift)); }

                        let cy     = row_rect.center().y;
                        let base_x = row_rect.min.x;

                        // Checkbox
                        let cb_rect = egui::Rect::from_center_size(
                            egui::pos2(base_x + cb_w / 2.0, cy), Vec2::splat(9.0));
                        if is_sel { ui.painter().rect_filled(cb_rect, 1.5, C_MUTED); }
                        else      { ui.painter().rect_stroke(cb_rect, 1.5, Stroke::new(0.5, C_BORDER2)); }

                        // Status label
                        let status_clip = egui::Rect::from_min_size(
                            egui::pos2(base_x + cb_w, row_rect.min.y), Vec2::new(status_w, row_h));
                        let sg = ui.painter().layout_no_wrap(job.status.label().to_string(), FontId::monospace(9.0), job.status.color());
                        ui.painter().with_clip_rect(status_clip).galley(
                            egui::pos2(base_x + cb_w, cy - sg.size().y/2.0), sg, job.status.color());

                        // Verified badge
                        if let Some(v) = job.verified {
                            ui.painter().text(egui::pos2(base_x + cb_w + status_w - 2.0, cy),
                                egui::Align2::RIGHT_CENTER,
                                if v {"✓"} else {"⚠"}, FontId::monospace(9.0),
                                if v {C_ACC} else {C_WARN});
                        }

                        // Name + path
                        let has_resume = job.status == JobStatus::Paused;
                        let right_edge = row_rect.max.x - rm_w - 6.0 - if has_resume { res_w + 4.0 } else { 0.0 };
                        let info_x = base_x + cb_w + status_w + 4.0;

                        // File size — right-aligned before the buttons
                        let size_str = if job.file_size > 0 { fmt_size(job.file_size) } else { String::new() };
                        let size_w = if size_str.is_empty() { 0.0 } else { 58.0 };
                        if !size_str.is_empty() {
                            ui.painter().text(
                                egui::pos2(right_edge - 4.0, cy),
                                egui::Align2::RIGHT_CENTER,
                                &size_str, FontId::monospace(9.0), C_MUTED);
                        }

                        let info_w = (right_edge - size_w - info_x - 4.0).max(20.0);
                        let info_rect = egui::Rect::from_min_size(egui::pos2(info_x, row_rect.min.y), Vec2::new(info_w, row_h));
                        let ng = ui.painter().layout_no_wrap(job.name.clone(), FontId::monospace(11.0), C_TEXT);
                        let decoded_path = url_decode(&job.path);
                        let pg = ui.painter().layout_no_wrap(decoded_path, FontId::monospace(9.0), C_MUTED);
                        let bh = ng.size().y + 2.0 + pg.size().y;
                        let ty = cy - bh/2.0;
                        let ng_h = ng.size().y;
                        ui.painter().with_clip_rect(info_rect).galley(egui::pos2(info_x, ty), ng, C_TEXT);
                        ui.painter().with_clip_rect(info_rect).galley(egui::pos2(info_x, ty + ng_h + 2.0), pg, C_MUTED);

                        // Resume button
                        if has_resume {
                            let rx = row_rect.max.x - rm_w - 6.0 - res_w;
                            let r  = egui::Rect::from_min_size(egui::pos2(rx, cy - 9.0), Vec2::new(res_w, 18.0));
                            let rr = ui.allocate_rect(r, egui::Sense::click());
                            ui.painter().rect_filled(r, 2.0, C_SURF2);
                            ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, C_BORDER2));
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "▶ resume",
                                FontId::monospace(9.0), if rr.hovered() {C_ACC} else {C_MUTED});
                            if rr.clicked() { to_resume = Some(job.id.clone()); }
                        }

                        // Remove button
                        if !job.status.is_active() {
                            let rx = row_rect.max.x - rm_w / 2.0 - 4.0;
                            let r  = egui::Rect::from_center_size(egui::pos2(rx, cy), Vec2::splat(18.0));
                            let rr = ui.allocate_rect(r, egui::Sense::click());
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "✕",
                                FontId::monospace(11.0), if rr.hovered() {C_ERR} else {C_DIM});
                            if rr.clicked() { to_remove = Some(job.id.clone()); }
                        }
                    }
                });

            // Apply deferred actions
            if let Some(id) = to_remove { self.remove_from_queue(&id); self.queue_selected.remove(&id); }
            if let Some(id) = to_resume { self.resume_job(&id); }
            if let Some((idx, shift)) = clicked_idx {
                let job_id = queue[idx].id.clone();
                if shift {
                    // Shift-click: select range from last clicked to this row
                    if let Some(last) = self.last_queue_click_idx {
                        let lo = last.min(idx);
                        let hi = last.max(idx);
                        for i in lo..=hi {
                            if i < queue.len() {
                                self.queue_selected.insert(queue[i].id.clone());
                            }
                        }
                    } else {
                        if self.queue_selected.contains(&job_id) {
                            self.queue_selected.remove(&job_id);
                        } else {
                            self.queue_selected.insert(job_id.clone());
                        }
                    }
                } else {
                    if self.queue_selected.contains(&job_id) {
                        self.queue_selected.remove(&job_id);
                        self.last_queue_click_idx = None;
                    } else {
                        self.queue_selected.insert(job_id.clone());
                        self.last_queue_click_idx = Some(idx);
                    }
                }
                if shift { self.last_queue_click_idx = Some(idx); }
            }

            // Footer
            egui::Frame::none().fill(C_SURF).inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .show(ui, |ui: &mut egui::Ui| {
                    let waiting = queue.iter().filter(|j| j.status == JobStatus::Waiting).count();
                    let paused  = queue.iter().filter(|j| j.status == JobStatus::Paused).count();
                    let errs    = queue.iter().filter(|j| matches!(j.status, JobStatus::Error(_))).count();
                    let stat = if queue.is_empty() { "No items queued".into() }
                        else if active_dl > 0 { format!("{} active  ·  {} waiting{}", active_dl, waiting,
                            if errs > 0 { format!("  ·  {} failed", errs) } else { String::new() }) }
                        else { format!("{} waiting  ·  {} paused{}", waiting, paused,
                            if errs > 0 { format!("  ·  {} failed", errs) } else { String::new() }) };
                    ui.label(mono(stat, 9.0, C_MUTED));
                    ui.add_space(6.0);

                    if !self.queue_selected.is_empty() {
                        let n = self.queue_selected.len();
                        // Keep selected only
                        if ui.add(
                            egui::Button::new(mono(format!("⊘  Remove unselected ({} kept)", n), 11.0, C_WARN))
                                .fill(C_SURF2).stroke(Stroke::new(1.0, C_BORDER2))
                                .min_size(Vec2::new(ui.available_width(), 26.0))
                        ).clicked() {
                            let to_remove: Vec<String> = queue.iter()
                                .filter(|j| !self.queue_selected.contains(&j.id) && !j.status.is_active())
                                .map(|j| j.id.clone())
                                .collect();
                            for id in to_remove { self.remove_from_queue(&id); }
                            self.queue_selected.clear();
                            self.last_queue_click_idx = None;
                        }
                        ui.add_space(4.0);
                        // Remove selected
                        if ui.add(
                            egui::Button::new(mono(format!("✕  Remove {} selected", n), 11.0, C_ERR))
                                .fill(C_SURF2).stroke(Stroke::new(1.0, C_BORDER2))
                                .min_size(Vec2::new(ui.available_width(), 26.0))
                        ).clicked() {
                            let ids: Vec<String> = self.queue_selected.drain().collect();
                            for id in ids { self.remove_from_queue(&id); }
                            self.last_queue_click_idx = None;
                        }
                        ui.add_space(4.0);
                    }

                    ui.horizontal(|ui: &mut egui::Ui| {
                        let has_active = active_dl > 0;
                        if ui.add_enabled(has_active,
                            egui::Button::new(mono("⏸ Pause active", 11.0, if has_active { C_WARN } else { C_DIM }))
                                .fill(C_SURF2).stroke(Stroke::new(1.0, C_BORDER2))
                                .min_size(Vec2::new(120.0, 26.0))
                        ).clicked() { self.pause_all_active(); }
                        ui.add_space(6.0);
                        let qp = self.settings.queue_paused;
                        let (label, fg, bg) = if qp {
                            ("▶  Start queue", Color32::from_rgb(0x04,0x0a,0x08), C_ACC)
                        } else {
                            ("⏸  Pause queue", C_TEXT, C_SURF2)
                        };
                        if ui.add(
                            egui::Button::new(mono(label, 11.0, fg).strong())
                                .fill(bg)
                                .stroke(if qp { Stroke::NONE } else { Stroke::new(1.0, C_BORDER2) })
                                .min_size(Vec2::new(ui.available_width(), 26.0))
                        ).clicked() {
                            self.settings.queue_paused = !self.settings.queue_paused;
                            self.settings_dirty = true;
                            // Immediately sync to shared so the manager thread picks it up
                            self.shared.lock().unwrap().dl_settings.paused = self.settings.queue_paused;
                            if !self.settings.queue_paused { self.kick_downloads(); }
                        }
                    });
                });
        });
    }
}

// ── main ──────────────────────────────────────────────────────────────────────
fn main() -> eframe::Result<()> {
    eframe::run_native(
        "myrient-dl",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("myrient-dl")
                .with_inner_size([1150.0, 720.0])
                .with_min_inner_size([800.0, 500.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            Box::new(App::new(cc))
        }),
    )
}
