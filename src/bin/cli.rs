// myrient-dl-cli — full terminal UI for myrient.erista.me
//
// Controls:
//   ^/v j/k   navigate list
//   Enter/l    open folder or queue file
//   h/Bksp     go back up
//   Space      select/deselect item
//   a          select all visible files
//   A          deselect all
//   q          add selected to queue
//   Tab        switch pane (browser <> queue)
//   s          start/pause queue
//   x          remove selected queue items
//   /          search across whole tree
//   Esc        cancel search / clear
//   Q          quit

#[path = "../generated_dirs.rs"]
#[allow(dead_code, unused_imports)]
mod generated_dirs;
//   Q          quit

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
    Terminal,
};
use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};


// ---- Symbol set: Unicode where supported, ASCII fallback ----
struct Symbols {
    folder:    &'static str,
    checked:   &'static str,
    file:      &'static str,
    sep:       &'static str,   // breadcrumb separator
    dl_arrow:  &'static str,   // download speed prefix
    paused:    &'static str,
    running:   &'static str,
    ellipsis:  &'static str,
    bar:       &'static str,   // header separator
}

impl Symbols {
    fn unicode() -> Self {
        Self { folder: "▶", checked: "✓", file: "·", sep: "›",
               dl_arrow: "↓", paused: "⏸", running: "▶", ellipsis: "…", bar: "│" }
    }
    fn ascii() -> Self {
        Self { folder: ">", checked: "+", file: ".", sep: ">",
               dl_arrow: "v", paused: "||", running: ">", ellipsis: "...", bar: "|" }
    }
    fn detect() -> Self {
        // Check LANG/LC_ALL for UTF-8, or any known Unicode-capable terminal program.
        // On Windows, UTF-8 mode is signalled by PYTHONIOENCODING, WT_SESSION (Windows Terminal),
        // or the user having run 'chcp 65001'.
        let is_utf8 = std::env::var("LANG").unwrap_or_default().to_uppercase().contains("UTF")
            || std::env::var("LC_ALL").unwrap_or_default().to_uppercase().contains("UTF")
            || std::env::var("LC_CTYPE").unwrap_or_default().to_uppercase().contains("UTF")
            || std::env::var("TERM_PROGRAM").is_ok()   // iTerm2, VSCode terminal, etc.
            || std::env::var("WT_SESSION").is_ok()     // Windows Terminal
            || std::env::var("TERM").map(|t| t.contains("256color") || t == "xterm").unwrap_or(false);
        if is_utf8 { Self::unicode() } else { Self::ascii() }
    }
}

// -- Shared types -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum JobStatus {
    Waiting, Spooling, Downloading, Paused, Verifying, Done, Error(String),
}
impl JobStatus {
    fn is_active(&self) -> bool { matches!(self, Self::Spooling | Self::Downloading | Self::Verifying) }
    fn label(&self) -> &str {
        match self {
            Self::Waiting     => "waiting",
            Self::Spooling    => "spooling",
            Self::Downloading => "active",
            Self::Paused      => "paused",
            Self::Verifying   => "verify",
            Self::Done        => "done",
            Self::Error(_)    => "error",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct QueueJob {
    id: String, url: String, name: String, path: String,
    status: JobStatus, resume: bool, retry_count: u32,
    verified: Option<bool>,
    #[serde(default)] file_size: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Settings {
    dest_path: String, concurrent: usize, max_retries: u32,
    verify_checksums: bool, queue_paused: bool,
    #[serde(default)] retro_theme: bool,
}
impl Default for Settings {
    fn default() -> Self { Self { dest_path: "~/Downloads/myrient".into(), concurrent: 4, max_retries: 3, verify_checksums: true, queue_paused: true, retro_theme: false } }
}

#[derive(Clone, Default)]
struct Progress { percent: f32, speed_bps: f64, eta_secs: Option<u64> }

#[derive(Default)]
struct Shared { queue: Vec<QueueJob>, progress: HashMap<String, Progress>, active_dl: usize }

struct Semaphore { count: Mutex<usize>, cvar: std::sync::Condvar }
impl Semaphore {
    fn new(n: usize) -> Self { Self { count: Mutex::new(n), cvar: std::sync::Condvar::new() } }
    fn try_acquire(&self) -> bool { let mut g = self.count.lock().unwrap(); if *g > 0 { *g -= 1; true } else { false } }
    fn acquire(&self) { let mut g = self.count.lock().unwrap(); while *g == 0 { g = self.cvar.wait(g).unwrap(); } *g -= 1; }
    fn release(&self) { *self.count.lock().unwrap() += 1; self.cvar.notify_one(); }
    fn set_limit(&self, n: usize) { let mut g = self.count.lock().unwrap(); *g = n; self.cvar.notify_all(); }
}

// -- Persistence ---------------------------------------------------------------

fn data_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_DATA_HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".local").join("share")
    });
    let d = base.join("myrient-dl"); std::fs::create_dir_all(&d).ok(); d
}
fn load_settings() -> Settings { let p = data_dir().join("settings.json"); std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default() }
fn save_settings(s: &Settings) { if let Ok(j) = serde_json::to_string_pretty(s) { std::fs::write(data_dir().join("settings.json"), j).ok(); } }
fn load_queue() -> Vec<QueueJob> { let p = data_dir().join("queue.json"); std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default() }
fn save_queue(q: &[QueueJob]) {
    let s: Vec<QueueJob> = q.iter().map(|j| { let mut j2 = j.clone(); if j2.status.is_active() { j2.status = JobStatus::Waiting; j2.resume = true; } j2 }).collect();
    if let Ok(j) = serde_json::to_string_pretty(&s) { std::fs::write(data_dir().join("queue.json"), j).ok(); }
}
fn load_downloaded() -> HashSet<String> { let p = data_dir().join("downloaded.json"); std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default() }
#[allow(dead_code)]
fn save_downloaded(d: &HashSet<String>) { if let Ok(j) = serde_json::to_string(d) { std::fs::write(data_dir().join("downloaded.json"), j).ok(); } }

fn url_decode(s: &str) -> String {
    let mut out = String::new(); let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' { let h1 = bytes.next().unwrap_or(b'0'); let h2 = bytes.next().unwrap_or(b'0'); let hex = format!("{}{}", h1 as char, h2 as char); if let Ok(n) = u8::from_str_radix(&hex, 16) { out.push(n as char); continue; } }
        out.push(b as char);
    }
    out
}
fn next_id() -> String { format!("{:x}{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(), rand_u32()) }
fn rand_u32() -> u32 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos() }
fn fmt_size(b: u64) -> String { match b { b if b >= 1_000_000_000 => format!("{:.1}GB", b as f64/1e9), b if b >= 1_000_000 => format!("{:.1}MB", b as f64/1e6), b if b >= 1_000 => format!("{:.1}KB", b as f64/1e3), b => format!("{}B", b) } }
fn fmt_speed(bps: f64) -> String { match bps as u64 { b if b >= 1_000_000_000 => format!("{:.1}GB/s", bps/1e9), b if b >= 1_000_000 => format!("{:.1}MB/s", bps/1e6), b if b >= 1_000 => format!("{:.1}KB/s", bps/1e3), _ => format!("{:.0}B/s", bps) } }
fn fmt_eta(s: u64) -> String { if s >= 3600 { format!("{}h{}m", s/3600, (s%3600)/60) } else if s >= 60 { format!("{}m{}s", s/60, s%60) } else { format!("{}s", s) } }
fn parse_size_str(s: &str) -> u64 { let s=s.trim(); if s.is_empty()||s=="-"{return 0;} let mut p=s.splitn(2,' '); let num:f64=p.next().unwrap_or("").parse().unwrap_or(0.0); let u=p.next().unwrap_or("").trim().to_uppercase(); let m:u64=match u.as_str(){"TIB"|"TB"=>1_099_511_627_776,"GIB"|"GB"=>1_073_741_824,"MIB"|"MB"=>1_048_576,"KIB"|"KB"=>1_024,_=>1}; (num*m as f64) as u64 }

// -- HTTP ----------------------------------------------------------------------

static CLIENT: once_cell::sync::Lazy<reqwest::blocking::Client> = once_cell::sync::Lazy::new(|| {
    reqwest::blocking::Client::builder().user_agent("myrient-dl-cli/1.0").timeout(Duration::from_secs(30)).build().expect("client")
});

const BASE_URL: &str = "https://myrient.erista.me/files/";

#[derive(Clone)]
struct DirEntry { name: String, href: String, size: String, is_folder: bool, url: Option<String> }

fn fetch_dir(url: &str) -> Result<Vec<DirEntry>, String> {
    let body = CLIENT.get(url).send().map_err(|e| e.to_string())?.text().map_err(|e| e.to_string())?;
    let doc = scraper::Html::parse_document(&body);
    let tr = scraper::Selector::parse("table tr").unwrap();
    let td = scraper::Selector::parse("td").unwrap();
    let a  = scraper::Selector::parse("a").unwrap();
    let mut entries = Vec::new();
    for row in doc.select(&tr).skip(1) {
        let cells: Vec<_> = row.select(&td).collect();
        if cells.len() < 3 { continue; }
        let Some(link) = cells[0].select(&a).next() else { continue };
        let href = link.value().attr("href").unwrap_or("").to_string();
        if href == "./" || href == "../" || href.is_empty() { continue; }
        let name = link.text().collect::<String>().trim().trim_end_matches('/').to_string();
        let size = cells[1].text().collect::<String>().trim().to_string();
        let is_folder = href.ends_with('/');
        let file_url = if !is_folder { reqwest::Url::parse(url).ok().and_then(|b| b.join(&href).ok()).map(|u| u.to_string()) } else { None };
        entries.push(DirEntry { name, href, size, is_folder, url: file_url });
    }
    entries.sort_by(|a,b| b.is_folder.cmp(&a.is_folder).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

fn guess_dest_path(dest: &str, url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 { return None; }
    let mut path = std::path::PathBuf::from(dest);
    for seg in &segs[2..] { path.push(url_decode(seg)); }
    Some(path.to_string_lossy().into_owned())
}

// -- Download engine -----------------------------------------------------------

fn download_job(job: &QueueJob, dest: &str, shared: &Arc<Mutex<Shared>>, kill_rx: &std::sync::mpsc::Receiver<()>, max_retries: u32) -> JobStatus {
    let mut attempt = job.retry_count;
    loop {
        if attempt > 0 { thread::sleep(Duration::from_secs(2u64.pow(attempt.min(6)))); }
        if kill_rx.try_recv().is_ok() {
            let mut s = shared.lock().unwrap();
            if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) { j.status = JobStatus::Paused; j.resume = true; }
            return JobStatus::Paused;
        }
        let file_path = match guess_dest_path(dest, &job.url) { Some(p) => p, None => return JobStatus::Error("bad URL".into()) };
        if let Some(parent) = std::path::Path::new(&file_path).parent() { std::fs::create_dir_all(parent).ok(); }
        let existing = if job.resume || attempt > 0 { std::fs::metadata(&file_path).ok().map(|m| m.len()).unwrap_or(0) } else { 0 };
        let mut req = CLIENT.get(&job.url);
        if existing > 0 { req = req.header("Range", format!("bytes={}-", existing)); }
        let resp = match req.timeout(Duration::from_secs(60)).send() {
            Ok(r) => r, Err(e) => { attempt += 1; if attempt > max_retries { return JobStatus::Error(e.to_string()); } continue; }
        };
        let status = resp.status();
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE { return JobStatus::Done; }
        if !status.is_success() { attempt += 1; if attempt > max_retries { return JobStatus::Error(format!("HTTP {}", status)); } continue; }
        let is_partial = status == reqwest::StatusCode::PARTIAL_CONTENT;
        let total = resp.content_length().map(|n| n + if is_partial { existing } else { 0 });
        let mut file = match if is_partial && existing > 0 { std::fs::OpenOptions::new().append(true).open(&file_path) } else { std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&file_path) } { Ok(f) => f, Err(e) => return JobStatus::Error(e.to_string()) };
        { let mut s = shared.lock().unwrap(); if let Some(j) = s.queue.iter_mut().find(|j| j.id == job.id) { j.status = JobStatus::Downloading; } }
        let mut downloaded = existing;
        let mut last_update = Instant::now();
        let mut last_bytes = existing;
        let jid = job.id.clone();
        let mut resp = resp;
        let result: Result<(), String> = (|| {
            use std::io::{Read, Write};
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                if kill_rx.try_recv().is_ok() {
                    let mut s = shared.lock().unwrap();
                    if let Some(j) = s.queue.iter_mut().find(|j| j.id == jid) { j.status = JobStatus::Paused; j.resume = true; }
                    return Err("cancelled".into());
                }
                let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                downloaded += n as u64;
                let now = Instant::now();
                if now.duration_since(last_update).as_secs_f64() >= 0.25 {
                    let bps = (downloaded - last_bytes) as f64 / now.duration_since(last_update).as_secs_f64();
                    let pct = total.map(|t| downloaded as f32 / t as f32 * 100.0).unwrap_or(0.0);
                    let eta = if bps > 0.0 { total.map(|t| ((t.saturating_sub(downloaded)) as f64 / bps) as u64) } else { None };
                    let mut s = shared.lock().unwrap();
                    if let Some(p) = s.progress.get_mut(&jid) { p.percent = pct.min(100.0); p.speed_bps = bps; p.eta_secs = eta; }
                    last_update = now; last_bytes = downloaded;
                }
            }
            Ok(())
        })();
        match result {
            Err(e) if e == "cancelled" => return JobStatus::Paused,
            Err(e) => { attempt += 1; if attempt > max_retries { return JobStatus::Error(e); } continue; }
            Ok(()) => { let mut s = shared.lock().unwrap(); if let Some(p) = s.progress.get_mut(&job.id) { p.percent = 100.0; p.speed_bps = 0.0; } return JobStatus::Done; }
        }
    }
}

// -- Download manager ----------------------------------------------------------

#[allow(dead_code)]
enum DlCmd { Start(QueueJob, String, u32), Cancel(String), SetConcurrent(usize), Shutdown }

fn start_dl_manager(shared: Arc<Mutex<Shared>>, settings: Settings) -> std::sync::mpsc::Sender<DlCmd> {
    let (tx, rx) = std::sync::mpsc::channel::<DlCmd>();
    thread::spawn(move || {
        let sem = Arc::new(Semaphore::new(settings.concurrent));
        let mut kill_txs: HashMap<String, std::sync::mpsc::Sender<()>> = HashMap::new();
        for cmd in rx {
            match cmd {
                DlCmd::Shutdown => break,
                DlCmd::Cancel(id) => { if let Some(tx) = kill_txs.remove(&id) { let _ = tx.send(()); } }
                DlCmd::SetConcurrent(n) => { sem.set_limit(n); }
                DlCmd::Start(job, dest, max_retries) => {
                    // Only spawn if a semaphore slot is immediately available
                    // This prevents thread exhaustion from large queues
                    if !sem.try_acquire() { continue; }
                    let sem2 = Arc::clone(&sem);
                    let shared2 = Arc::clone(&shared);
                    let (kill_tx, kill_rx) = std::sync::mpsc::channel();
                    kill_txs.insert(job.id.clone(), kill_tx);
                    let jid = job.id.clone();
                    thread::spawn(move || {
                        { let mut s = shared2.lock().unwrap(); s.active_dl += 1; s.progress.insert(jid.clone(), Progress::default()); if let Some(j) = s.queue.iter_mut().find(|j| j.id == jid) { j.status = JobStatus::Spooling; } }
                        let status = download_job(&job, &dest, &shared2, &kill_rx, max_retries);
                        { let mut s = shared2.lock().unwrap(); s.active_dl = s.active_dl.saturating_sub(1); if let Some(j) = s.queue.iter_mut().find(|j| j.id == jid) { j.status = status.clone(); if status == JobStatus::Done { j.resume = false; } } }
                        sem2.release();
                        if status == JobStatus::Done { save_queue(&shared2.lock().unwrap().queue); }
                    });
                }
            }
        }
    });
    tx
}

// -- TUI App -------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum Pane { Browser, Queue }

struct App {
    settings:       Settings,
    shared:         Arc<Mutex<Shared>>,
    dl_tx:          std::sync::mpsc::Sender<DlCmd>,
    // Browser
    current_path:   String,
    crumb_stack:    Vec<(String, String)>,
    entries:        Vec<DirEntry>,
    loading:        bool,
    load_error:     Option<String>,
    browser_state:  ListState,
    selected_urls:  HashSet<String>,
    downloaded:     HashSet<String>,
    queued_urls:    HashSet<String>,
    filter_query:   String,
    filtering:      bool,   // filter input active
    // Queue
    queue_state:    ListState,
    queue_sel:      HashSet<String>,
    // Search
    search_query:   String,
    searching:      bool,
    search_results: Vec<(String, String)>,
    search_state:   ListState,
    // UI
    active_pane:    Pane,
    status_msg:     String,
    sym:            Symbols,
    frame:          u64,   // incremented each draw, drives animations
    force_refresh:  bool,
    browse_rx:      std::sync::mpsc::Receiver<(String, Result<Vec<DirEntry>, String>)>,
    browse_tx_clone: std::sync::mpsc::SyncSender<(String, Result<Vec<DirEntry>, String>)>,
}

impl App {
    fn new() -> Self {
        let settings = load_settings();
        let shared   = Arc::new(Mutex::new(Shared { queue: load_queue(), ..Default::default() }));
        let queued_urls: HashSet<String> = shared.lock().unwrap().queue.iter().map(|j| j.url.clone()).collect();
        let dl_tx    = start_dl_manager(Arc::clone(&shared), settings.clone());
        let (btx, brx) = std::sync::mpsc::sync_channel(4);
        App {
            settings, shared, dl_tx,
            current_path: String::new(), crumb_stack: vec![], entries: vec![],
            loading: false, load_error: None,
            browser_state: ListState::default(), selected_urls: HashSet::new(),
            downloaded: load_downloaded(), queued_urls,
            filter_query: String::new(), filtering: false,
            queue_state: ListState::default(), queue_sel: HashSet::new(),
            search_query: String::new(), searching: false,
            search_results: vec![], search_state: ListState::default(),
            active_pane: Pane::Browser, status_msg: "Ready".into(), sym: Symbols::detect(), frame: 0, force_refresh: false,
            browse_rx: brx, browse_tx_clone: btx,
        }
    }

    fn navigate(&mut self, path: String) {
        self.current_path = path.clone();
        self.entries.clear(); self.load_error = None; self.loading = true;
        self.selected_urls.clear(); self.browser_state = ListState::default();
        self.filter_query.clear();

        // Check local index first (skip if force_refresh)
        let force = self.force_refresh;
        self.force_refresh = false;
        let key = path.trim_matches('/').to_string();
        if !force {
            if let Some(baked) = generated_dirs::lookup(&key) {
                if !baked.is_empty() {
                    self.loading = false;
                    let url_base = format!("{}{}", BASE_URL, path);
                    let entries: Vec<DirEntry> = baked.into_iter().map(|e| {
                        let file_url = if !e.is_folder {
                            reqwest::Url::parse(&url_base).ok()
                                .and_then(|b| b.join(&e.href).ok())
                                .map(|u| u.to_string())
                        } else { None };
                        DirEntry { name: e.name, href: e.href, size: e.size, is_folder: e.is_folder, url: file_url }
                    }).collect();
                    let folders = entries.iter().filter(|x| x.is_folder).count();
                    let files   = entries.iter().filter(|x| !x.is_folder).count();
                    self.status_msg = format!("{} folders  {}  files", folders, files);
                    self.entries = entries;
                    return;
                }
            }
        }

        // Live HTTP fetch (also persists result to local cache)
        let url = format!("{}{}", BASE_URL, path);
        let tx = self.browse_tx_clone.clone();
        thread::spawn(move || {
            let result = fetch_dir(&url);
            // Persist to local index if successful
            if let Ok(ref entries) = result {
                let persist_entries: Vec<generated_dirs::DirEntry> = entries.iter().map(|e| {
                    generated_dirs::DirEntry {
                        name:       e.name.clone(),
                        href:       e.href.clone(),
                        size:       e.size.clone(),
                        size_bytes: 0,
                        date:       String::new(),
                        is_folder:  e.is_folder,
                    }
                }).collect();
                generated_dirs::persist_folder(key, persist_entries, 0);
            }
            let _ = tx.send((path, result));
        });
    }

    fn poll_browse(&mut self) {
        if let Ok((path, result)) = self.browse_rx.try_recv() {
            // Folder-queue result
            if let Some(folder_path) = path.strip_prefix("__folder_queue__") {
                if let Ok(entries) = result {
                    let n: usize = entries.iter().filter(|e| !e.is_folder).filter(|e| {
                        e.url.as_ref().map(|u| !self.queued_urls.contains(u)).unwrap_or(false)
                    }).count();
                    for e in entries.into_iter().filter(|e| !e.is_folder) {
                        if let Some(url) = e.url {
                            let sz = parse_size_str(&e.size);
                            self.add_to_queue(url, e.name, sz);
                        }
                    }
                    self.status_msg = format!("Queued {} file{} from {}", n, if n==1{""} else {"s"}, folder_path.rsplit('/').next().unwrap_or(folder_path));
                }
                return;
            }
            if path == self.current_path {
                self.loading = false;
                match result {
                    Ok(e) => {
                        let folders = e.iter().filter(|x| x.is_folder).count();
                        let files   = e.iter().filter(|x| !x.is_folder).count();
                        self.status_msg = format!("{} folders  {}  files", folders, files);
                        self.entries = e;
                    }
                    Err(e) => { self.load_error = Some(e.clone()); self.status_msg = format!("Error: {}", e); }
                }
            }
        }
    }

    fn kick_downloads(&mut self) {
        let dest = shellexpand::tilde(&self.settings.dest_path).to_string();
        if self.settings.queue_paused { return; }
        let (active, jobs): (usize, Vec<QueueJob>) = {
            let s = self.shared.lock().unwrap();
            let active = s.queue.iter().filter(|j| j.status.is_active()).count();
            let slots = self.settings.concurrent.saturating_sub(active);
            let mut waiting: Vec<&QueueJob> = s.queue.iter().filter(|j| j.status == JobStatus::Waiting).collect();
            waiting.sort_by_key(|j| if j.resume { 0u8 } else { 1u8 });
            let jobs = waiting.into_iter().take(slots).cloned().collect();
            (active, jobs)
        };
        if active >= self.settings.concurrent { return; }
        for job in jobs {
            let _ = self.dl_tx.send(DlCmd::Start(job, dest.clone(), self.settings.max_retries));
        }
    }

    fn add_to_queue(&mut self, url: String, name: String, size: u64) {
        if self.queued_urls.contains(&url) { return; }
        self.queued_urls.insert(url.clone());
        let job = QueueJob { id: next_id(), url, name, path: self.current_path.clone(), status: JobStatus::Waiting, resume: false, retry_count: 0, verified: None, file_size: size };
        let mut s = self.shared.lock().unwrap();
        s.queue.push(job);
        save_queue(&s.queue);
    }

    fn queue_selected(&mut self) {
        let to_add: Vec<(String, String, u64)> = self.entries.iter()
            .filter(|e| !e.is_folder && e.url.as_ref().map(|u| self.selected_urls.contains(u)).unwrap_or(false))
            .filter(|e| e.url.as_ref().map(|u| !self.queued_urls.contains(u)).unwrap_or(false))
            .filter_map(|e| e.url.as_ref().map(|u| (u.clone(), e.name.clone(), parse_size_str(&e.size))))
            .collect();
        let n = to_add.len();
        for (url, name, sz) in to_add { self.add_to_queue(url, name, sz); }
        self.selected_urls.clear();
        self.status_msg = format!("Queued {} file{}", n, if n==1{""} else {"s"});
    }

    fn do_search(&mut self) {
        if self.search_query.is_empty() { self.search_results.clear(); return; }
        let q = self.search_query.to_lowercase();
        // Search baked-in tree via per-block lookup, otherwise fall back to current entries
        let results: Vec<(String, String)> = if generated_dirs::folder_count() > 0 {
            generated_dirs::search(&q)
                .take(300)
                .map(|e| (e.name.clone(), format!("{}/{}", e.folder, e.name)))
                .collect()
        } else {
            self.entries.iter()
                .filter(|e| !e.is_folder && e.name.to_lowercase().contains(&q))
                .map(|e| (e.name.clone(), e.href.clone()))
                .collect()
        };
        self.search_results = results;
        self.search_state = ListState::default();
        if !self.search_results.is_empty() { self.search_state.select(Some(0)); }
    }

    fn browser_up(&mut self) {
        let n = if self.searching { self.search_results.len() } else { self.entries.len() };
        if n == 0 { return; }
        let state = if self.searching { &mut self.search_state } else { &mut self.browser_state };
        let i = state.selected().map(|i| if i == 0 { n-1 } else { i-1 }).unwrap_or(0);
        state.select(Some(i));
    }

    fn browser_down(&mut self) {
        let n = if self.searching { self.search_results.len() } else { self.entries.len() };
        if n == 0 { return; }
        let state = if self.searching { &mut self.search_state } else { &mut self.browser_state };
        let i = state.selected().map(|i| (i+1) % n).unwrap_or(0);
        state.select(Some(i));
    }

    fn browser_enter(&mut self) {
        if self.searching {
            if let Some(i) = self.search_state.selected() {
                if let Some((_, href)) = self.search_results.get(i).cloned() {
                    // Navigate to folder containing result
                    let folder = href.rsplit_once('/').map(|(p,_)| format!("{}/", p)).unwrap_or_default();
                    self.crumb_stack.clear();
                    let mut path = String::new();
                    for seg in folder.trim_end_matches('/').split('/').filter(|s| !s.is_empty()) {
                        path = format!("{}{}/", path, seg);
                        self.crumb_stack.push((url_decode(seg), path.clone()));
                    }
                    self.searching = false; self.search_query.clear(); self.search_results.clear();
                    let nav_path = self.crumb_stack.last().map(|(_,p)| p.clone()).unwrap_or_default();
                    self.navigate(nav_path);
                }
            }
            return;
        }
        if let Some(i) = self.browser_state.selected() {
            if let Some(entry) = self.entries.get(i).cloned() {
                if entry.is_folder {
                    let new_path = format!("{}{}", self.current_path, entry.href);
                    self.crumb_stack.push((entry.name.clone(), new_path.clone()));
                    self.navigate(new_path);
                } else if let Some(url) = entry.url.clone() {
                    if !self.queued_urls.contains(&url) {
                        let sz = parse_size_str(&entry.size);
                        self.add_to_queue(url, entry.name.clone(), sz);
                        self.status_msg = format!("Queued: {}", entry.name);
                    }
                }
            }
        }
    }

    fn browser_back(&mut self) {
        if self.searching { self.searching = false; self.search_query.clear(); self.search_results.clear(); return; }
        self.crumb_stack.pop();
        let path = self.crumb_stack.last().map(|(_,p)| p.clone()).unwrap_or_default();
        self.navigate(path);
    }

    fn toggle_select(&mut self) {
        if let Some(i) = self.browser_state.selected() {
            if let Some(entry) = self.entries.get(i) {
                if entry.is_folder {
                    // Selecting a folder marks it for queuing via q
                    // (no URL to track, just note the href)
                } else if let Some(url) = &entry.url {
                    if self.selected_urls.contains(url.as_str()) { self.selected_urls.remove(url.as_str()); }
                    else if !self.queued_urls.contains(url.as_str()) && !self.downloaded.contains(url.as_str()) { self.selected_urls.insert(url.clone()); }
                }
            }
        }
    }

    fn queue_folder_at_cursor(&mut self) {
        let entry = if let Some(i) = self.browser_state.selected() {
            self.entries.get(i).cloned()
        } else { None };
        let Some(entry) = entry else { return };
        if !entry.is_folder { self.queue_selected(); return; }
        let folder_path = format!("{}{}", self.current_path, entry.href);
        let folder_url  = format!("{}{}", BASE_URL, folder_path);
        let tx = self.browse_tx_clone.clone();
        let key = folder_path.trim_matches('/').to_string();
        self.status_msg = format!("Fetching {} for queuing…", entry.name);
        thread::spawn(move || {
            // Try local cache first
            let result = if let Some(baked) = generated_dirs::lookup(&key) {
                if !baked.is_empty() {
                    let url_base = folder_url.clone();
                    let entries: Vec<DirEntry> = baked.into_iter().filter_map(|e| {
                        if e.is_folder { return None; }
                        let url = reqwest::Url::parse(&url_base).ok()
                            .and_then(|b| b.join(&e.href).ok())
                            .map(|u| u.to_string())?;
                        Some(DirEntry { name: e.name, href: e.href, size: e.size, is_folder: false, url: Some(url) })
                    }).collect();
                    Ok(entries)
                } else { fetch_dir(&folder_url) }
            } else { fetch_dir(&folder_url) };
            let _ = tx.send((format!("__folder_queue__{}", folder_path), result));
        });
    }

    fn queue_up(&mut self) {
        let n = self.shared.lock().unwrap().queue.len();
        if n == 0 { return; }
        let i = self.queue_state.selected().map(|i| if i==0 {n-1} else {i-1}).unwrap_or(0);
        self.queue_state.select(Some(i));
    }

    fn queue_down(&mut self) {
        let n = self.shared.lock().unwrap().queue.len();
        if n == 0 { return; }
        let i = self.queue_state.selected().map(|i| (i+1)%n).unwrap_or(0);
        self.queue_state.select(Some(i));
    }

    fn queue_toggle_sel(&mut self) {
        let id = { let s = self.shared.lock().unwrap(); self.queue_state.selected().and_then(|i| s.queue.get(i).map(|j| j.id.clone())) };
        if let Some(id) = id { if !self.queue_sel.remove(&id) { self.queue_sel.insert(id); } }
    }

    fn remove_queue_sel(&mut self) {
        let to_remove: Vec<String> = self.queue_sel.drain().collect();
        let mut s = self.shared.lock().unwrap();
        s.queue.retain(|j| !to_remove.contains(&j.id));
        save_queue(&s.queue);
        self.queued_urls.retain(|u| s.queue.iter().any(|j| &j.url == u));
    }
}

// -- Drawing -------------------------------------------------------------------

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.area();
    let green  = Color::Rgb(0x3d, 0xe8, 0xa0);
    let blue   = Color::Rgb(0x5b, 0x9c, 0xf6);
    let muted  = Color::Rgb(0x4a, 0x5a, 0x72);
    let dim    = Color::Rgb(0x2a, 0x34, 0x44);
    let text   = Color::Rgb(0xc8, 0xd4, 0xe3);
    let warn   = Color::Rgb(0xe8, 0xa0, 0x3d);
    let err    = Color::Rgb(0xe8, 0x50, 0x3d);
    let file_c = Color::Rgb(0xa0, 0xc8, 0xe8);

    // Layout: header | main | status bar
    let outer = Layout::default().direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(2)]).split(size);

    // Header bar
    let (queue_len, active_dl, total_bps) = {
        let s = app.shared.lock().unwrap();
        let bps: f64 = s.progress.values().map(|p| p.speed_bps).sum();
        (s.queue.len(), s.active_dl, bps)
    };
    let header_text = if active_dl > 0 {
        format!(" myrient-dl-cli  {bar} {arrow} {spd}  {bar} {act} active  {bar} {q} queued  {bar} t:{t} r:{r}  {bar} dest: {dest}", bar=app.sym.bar, arrow=app.sym.dl_arrow, spd=fmt_speed(total_bps), act=active_dl, q=queue_len, t=app.settings.concurrent, r=app.settings.max_retries, dest=app.settings.dest_path)
    } else {
        format!(" myrient-dl-cli  {} {} queued {} dest: {}", app.sym.bar, queue_len, app.sym.bar, app.settings.dest_path)
    };
    f.render_widget(Paragraph::new(header_text).style(Style::default().fg(green).bg(Color::Rgb(0x0e,0x15,0x11))), outer[0]);

    // Main: browser | queue
    let main = Layout::default().direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)]).split(outer[1]);

    // -- Browser panel --
    let browser_block = Block::default().title(if app.active_pane == Pane::Browser { " [ BROWSER ] " } else { " BROWSER " })
        .borders(Borders::ALL).border_style(Style::default().fg(if app.active_pane == Pane::Browser { green } else { dim }));
    let inner_browser = browser_block.inner(main[0]);
    f.render_widget(browser_block, main[0]);

    // Breadcrumbs + search
    let filter_line_h = if app.filtering || !app.filter_query.is_empty() { 1u16 } else { 0 };
    let browser_layout = Layout::default().direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                               // breadcrumbs
            Constraint::Length(if app.searching { 1 } else { 0 }), // search bar
            Constraint::Length(filter_line_h),                   // filter bar
            Constraint::Min(0),                                  // entries
        ]).split(inner_browser);

    // Breadcrumb line
    let crumb_str = if app.crumb_stack.is_empty() { "/files/".to_string() }
        else { format!("/files/ {} {}", app.sym.sep, app.crumb_stack.iter().map(|(l,_)| l.as_str()).collect::<Vec<_>>().join(&format!(" {} ", app.sym.sep))) };
    f.render_widget(Paragraph::new(crumb_str).style(Style::default().fg(text)), browser_layout[0]);

    // Search bar
    if app.searching {
        let search_bar = format!("/ {}_", app.search_query);
        f.render_widget(Paragraph::new(search_bar).style(Style::default().fg(green)), browser_layout[1]);
    }

    // Filter bar
    if filter_line_h > 0 {
        let filter_bar = format!("filter: {}{}", app.filter_query, if app.filtering { "_" } else { "" });
        let col = if app.filter_query.is_empty() { muted } else { green };
        f.render_widget(Paragraph::new(filter_bar).style(Style::default().fg(col)), browser_layout[2]);
    }

    // Entry list or search results — apply filter
    let list_area = browser_layout[3];
    if app.loading {
        f.render_widget(Paragraph::new("  fetching...").style(Style::default().fg(muted)), list_area);
    } else if let Some(ref e) = app.load_error.clone() {
        f.render_widget(Paragraph::new(format!("  error: {}", e)).style(Style::default().fg(err)), list_area);
    } else if app.searching {
        let items: Vec<ListItem> = app.search_results.iter().enumerate().map(|(i, (name, href))| {
            let sel = app.search_state.selected() == Some(i);
            let parent = href.rsplit_once('/').map(|(p,_)| p).unwrap_or("");
            let line = Line::from(vec![
                Span::styled(format!(" {:<50}", &name[..name.len().min(50)]), Style::default().fg(file_c)),
                Span::styled(format!("  {}", &parent[..parent.len().min(30)]), Style::default().fg(muted)),
            ]);
            ListItem::new(line).style(if sel { Style::default().bg(Color::Rgb(0x1a,0x28,0x1e)) } else { Style::default() })
        }).collect();
        let list = List::new(items).highlight_style(Style::default().bg(Color::Rgb(0x1a,0x28,0x1e)));
        f.render_stateful_widget(list, list_area, &mut app.search_state);
    } else {
        let fq = app.filter_query.to_lowercase();
        let filtered: Vec<&DirEntry> = app.entries.iter()
            .filter(|e| fq.is_empty() || e.name.to_lowercase().contains(&fq))
            .collect();
        let items: Vec<ListItem> = filtered.iter().map(|e| {
            let is_sel    = e.url.as_ref().map(|u| app.selected_urls.contains(u)).unwrap_or(false);
            let is_queued = e.url.as_ref().map(|u| app.queued_urls.contains(u)).unwrap_or(false);
            let is_dl     = e.url.as_ref().map(|u| app.downloaded.contains(u)).unwrap_or(false);
            let icon = if e.is_folder { app.sym.folder } else if is_dl { app.sym.checked } else { app.sym.file };
            let icon_style = if e.is_folder { Style::default().fg(blue) } else if is_dl { Style::default().fg(green) } else { Style::default().fg(muted) };
            let name_style = if is_queued { Style::default().fg(green) } else if is_dl { Style::default().fg(Color::Rgb(0x2a,0x50,0x3a)) } else if e.is_folder { Style::default().fg(blue) } else if is_sel { Style::default().fg(green).add_modifier(Modifier::BOLD) } else { Style::default().fg(file_c) };
            let prefix = if is_sel { &format!("[{}] ", app.sym.checked) } else { "    " };
            let size_str = if e.size.is_empty() || e.size == "-" { String::new() } else { format!(" {}", &e.size) };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", icon), icon_style),
                Span::styled(format!("{}{:<55}", prefix, &e.name[..e.name.len().min(55)]), name_style),
                Span::styled(size_str, Style::default().fg(muted)),
            ]))
        }).collect();
        let list = List::new(items).highlight_style(Style::default().bg(Color::Rgb(0x1a,0x28,0x1e)));
        f.render_stateful_widget(list, list_area, &mut app.browser_state);
    }

    // -- Queue panel --
    let queue_block = Block::default().title(if app.active_pane == Pane::Queue { " [ QUEUE ] " } else { " QUEUE " })
        .borders(Borders::ALL).border_style(Style::default().fg(if app.active_pane == Pane::Queue { green } else { dim }));
    let inner_queue = queue_block.inner(main[1]);
    f.render_widget(queue_block, main[1]);

    let queue_layout = Layout::default().direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)]).split(inner_queue);

    let spinner_frames = if app.sym.folder == ">" {
        // ASCII fallback spinner
        ["|", "/", "-", "\\", "|", "/", "-", "\\"]
    } else {
        // Unicode spinner
        ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"]
    };
    let (queue_items, active_jobs): (Vec<ListItem>, Vec<(String, Progress)>) = {
        let s = app.shared.lock().unwrap();
        let items = s.queue.iter().enumerate().map(|(i, j)| {
            let sel = app.queue_sel.contains(&j.id);
            let (status_sym, status_col) = match &j.status {
                JobStatus::Waiting     => ("·", muted),
                JobStatus::Spooling    => (spinner_frames[(app.frame / 2) as usize % spinner_frames.len()], warn),
                JobStatus::Downloading => (spinner_frames[(app.frame / 2) as usize % spinner_frames.len()], green),
                JobStatus::Verifying   => (spinner_frames[(app.frame / 2) as usize % spinner_frames.len()], blue),
                JobStatus::Paused      => (if app.sym.folder == ">" { "=" } else { "‖" }, blue),
                JobStatus::Done        => ("+", green),
                JobStatus::Error(_)    => ("!", err),
            };
            let sel_sym = if sel { app.sym.checked } else { " " };
            let prog = s.progress.get(&j.id);
            let pct_str = prog.and_then(|p| if p.percent > 0.0 { Some(format!("{:3.0}%", p.percent)) } else { None });
            // Left: [sel] [status] name — name truncated to fill, no text on right
            let avail_name = 34usize;
            let name_trunc = &j.name[..j.name.len().min(avail_name)];
            let right = if let Some(pct) = pct_str {
                format!(" {}", pct)
            } else if j.file_size > 0 {
                format!(" {}", fmt_size(j.file_size))
            } else { String::new() };
            let line = Line::from(vec![
                Span::styled(format!("{} ", sel_sym), Style::default().fg(if sel { green } else { dim })),
                Span::styled(format!("{} ", status_sym), Style::default().fg(status_col)),
                Span::styled(format!("{:<34}", name_trunc), Style::default().fg(if matches!(j.status, JobStatus::Done) { muted } else { text })),
                Span::styled(right, Style::default().fg(muted)),
            ]);
            ListItem::new(line).style(if app.queue_state.selected() == Some(i) { Style::default().bg(Color::Rgb(0x1a,0x28,0x1e)) } else { Style::default() })
        }).collect();
        let active = s.queue.iter().filter(|j| j.status.is_active()).map(|j| (j.id.clone(), s.progress.get(&j.id).cloned().unwrap_or_default())).collect();
        (items, active)
    };

    let list = List::new(queue_items).highlight_style(Style::default().bg(Color::Rgb(0x1a,0x28,0x1e)));
    f.render_stateful_widget(list, queue_layout[0], &mut app.queue_state);

    // Active download gauge(s)
    if !active_jobs.is_empty() {
        let (_, prog) = &active_jobs[0];
        let speed_str = if prog.speed_bps > 0.0 { fmt_speed(prog.speed_bps) } else { app.sym.ellipsis.to_string() };
        let eta_str   = prog.eta_secs.map(fmt_eta).unwrap_or_else(|| "...".into());
        let label = format!(" {} {}  ETA {}  ({} active  t:{} r:{})", app.sym.dl_arrow, speed_str, eta_str, active_jobs.len(), app.settings.concurrent, app.settings.max_retries);
        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::TOP))
            .gauge_style(Style::default().fg(green).bg(Color::Rgb(0x0c,0x18,0x0e)))
            .ratio((prog.percent as f64 / 100.0).clamp(0.0, 1.0))
            .label(label);
        f.render_widget(gauge, queue_layout[1]);
    } else {
        let state_label = if app.settings.queue_paused {
            format!(" {} paused  (s to start)  threads:{} retries:{}", app.sym.paused, app.settings.concurrent, app.settings.max_retries)
        } else {
            format!(" {} running  +- threads:{}  [] retries:{}", app.sym.running, app.settings.concurrent, app.settings.max_retries)
        };
        f.render_widget(Paragraph::new(state_label).style(Style::default().fg(muted)).block(Block::default().borders(Borders::TOP)), queue_layout[1]);
    }

    // Status bar — 2 lines: status message + hint row 1, hint row 2
    let sep = if app.sym.bar == "|" { "  |  " } else { "  │  " };
    let (hint1, hint2): (String, String) = if app.filtering {
        (format!("type to filter{sep}Esc clear{sep}jk navigate"), format!("Space select{sep}q queue selected{sep}Enter close filter"))
    } else if app.searching {
        (format!("type to search{sep}Esc clear{sep}jk navigate"), format!("Space queue item{sep}Enter go to folder"))
    } else {
        (
            format!("jk/↑↓ navigate{sep}Enter open{sep}Space select{sep}a all{sep}A deselect{sep}q queue files/folder{sep}/ search{sep}f filter{sep}Tab switch"),
            format!("s pause/resume{sep}+- threads{sep}[] retries{sep}R refresh{sep}x remove{sep}Q/^C quit"),
        )
    };
    let line1 = Line::from(vec![
        Span::styled(format!(" {}  {} ", app.status_msg, sep.trim()), Style::default().fg(text)),
        Span::styled(&hint1, Style::default().fg(muted)),
    ]);
    let line2 = Line::from(vec![
        Span::styled(format!(" {}", &hint2), Style::default().fg(muted)),
    ]);
    f.render_widget(Paragraph::new(vec![line1, line2]), outer[2]);
}

// -- Event loop ----------------------------------------------------------------

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();
    app.navigate(String::new());
    generated_dirs::init();
    generated_dirs::warm_search_index();

    'main: loop {
        app.poll_browse();

        // Auto-kick downloads
        { let s = app.shared.lock().unwrap(); let has_waiting = s.queue.iter().any(|j| j.status == JobStatus::Waiting); let active = s.active_dl; drop(s); if !app.settings.queue_paused && has_waiting && active < app.settings.concurrent { app.kick_downloads(); } }

        terminal.draw(|f| draw(f, &mut app))?;
        app.frame = app.frame.wrapping_add(1);

        // Process at most N key events per frame to prevent scroll spam lockup.
        // Any excess events are discarded — the UI stays responsive.
        const MAX_EVENTS_PER_FRAME: usize = 4;

        if !event::poll(Duration::from_millis(16))? { continue; }

        let mut processed = 0;
        while processed < MAX_EVENTS_PER_FRAME && event::poll(Duration::from_millis(0))? {
            if let Ok(Event::Key(key)) = event::read() {
                // Only handle key press events — ignore Release and Repeat to prevent double input
                if key.kind != crossterm::event::KeyEventKind::Press {
                    continue;
                }
                processed += 1;

            // Filter mode: typing updates filter, but nav/select/queue still work
            if app.filtering {
                match key.code {
                    KeyCode::Esc => { app.filtering = false; app.filter_query.clear(); continue; }
                    KeyCode::Backspace => { app.filter_query.pop(); app.browser_state.select(Some(0)); continue; }
                    // Action keys fall through; everything else is filter input
                    KeyCode::Up | KeyCode::Down | KeyCode::Enter
                    | KeyCode::Char(' ') | KeyCode::Char('q')
                    | KeyCode::Char('j') | KeyCode::Char('k') => {} // fall through
                    KeyCode::Char(c) => { app.filter_query.push(c); app.browser_state.select(Some(0)); continue; }
                    _ => { continue; }
                }
            }

            // Search mode captures most keys
            if app.searching {
                match key.code {
                    KeyCode::Esc => { app.searching = false; app.search_query.clear(); app.search_results.clear(); }
                    KeyCode::Enter => { app.browser_enter(); }
                    KeyCode::Up   | KeyCode::Char('k') => app.browser_up(),
                    KeyCode::Down | KeyCode::Char('j') => app.browser_down(),
                    KeyCode::Backspace => { app.search_query.pop(); app.do_search(); }
                    // Space: queue the highlighted search result directly
                    KeyCode::Char(' ') => {
                        if let Some(i) = app.search_state.selected() {
                            if let Some((name, full_path)) = app.search_results.get(i).cloned() {
                                let url = format!("{}{}", BASE_URL, full_path);
                                if !app.queued_urls.contains(&url) {
                                    let n = name.clone();
                                    app.add_to_queue(url, n.clone(), 0);
                                    app.status_msg = format!("Queued: {}", n);
                                }
                            }
                        }
                    }
                    KeyCode::Char(c) => { app.search_query.push(c); app.do_search(); }
                    _ => {}
                }
                continue;
            }

            match key.code {
                // Quit — Q, Ctrl+C, or Ctrl+Q
                KeyCode::Char('Q') => {
                    save_queue(&app.shared.lock().unwrap().queue);
                    let _ = app.dl_tx.send(DlCmd::Shutdown);
                    break 'main;
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    save_queue(&app.shared.lock().unwrap().queue);
                    let _ = app.dl_tx.send(DlCmd::Shutdown);
                    break 'main;
                }

                // Navigation
                KeyCode::Tab => { app.active_pane = if app.active_pane == Pane::Browser { Pane::Queue } else { Pane::Browser }; }
                KeyCode::Up   | KeyCode::Char('k') => { if app.active_pane == Pane::Browser { app.browser_up(); } else { app.queue_up(); } }
                KeyCode::Down | KeyCode::Char('j') => { if app.active_pane == Pane::Browser { app.browser_down(); } else { app.queue_down(); } }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    if app.active_pane == Pane::Browser { app.browser_enter(); }
                }
                KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                    if app.active_pane == Pane::Browser && !app.crumb_stack.is_empty() { app.browser_back(); }
                }

                // Browser actions
                KeyCode::Char(' ') => {
                    if app.active_pane == Pane::Browser { app.toggle_select(); }
                    else { app.queue_toggle_sel(); }
                }
                KeyCode::Char('a') => {
                    if app.active_pane == Pane::Browser {
                        for e in &app.entries { if !e.is_folder { if let Some(u) = &e.url { if !app.queued_urls.contains(u) && !app.downloaded.contains(u) { app.selected_urls.insert(u.clone()); } } } }
                    }
                }
                KeyCode::Char('A') => { app.selected_urls.clear(); app.queue_sel.clear(); }
                KeyCode::Char('q') => {
                    if app.active_pane == Pane::Browser {
                        // If cursor is on a folder, queue all its files; otherwise queue selected files
                        let on_folder = app.browser_state.selected()
                            .and_then(|i| app.entries.get(i))
                            .map(|e| e.is_folder)
                            .unwrap_or(false);
                        if on_folder { app.queue_folder_at_cursor(); }
                        else { app.queue_selected(); }
                    }
                }
                KeyCode::Char('x') => {
                    if app.active_pane == Pane::Queue && !app.queue_sel.is_empty() { app.remove_queue_sel(); }
                }

                // Search
                KeyCode::Char('/') => { app.searching = true; app.search_query.clear(); }

                // Filter
                KeyCode::Char('f') => {
                    if app.active_pane == Pane::Browser {
                        app.filtering = true;
                        app.filter_query.clear();
                        app.browser_state.select(Some(0));
                    }
                }
                KeyCode::Esc => {
                    // Clear filter if active
                    if !app.filter_query.is_empty() { app.filter_query.clear(); }
                }

                // Queue start/pause
                KeyCode::Char('s') => {
                    app.settings.queue_paused = !app.settings.queue_paused;
                    save_settings(&app.settings);
                    app.status_msg = if app.settings.queue_paused { "Queue paused".into() } else { "Queue started".into() };
                    if !app.settings.queue_paused { app.kick_downloads(); }
                }

                // Refresh — force re-fetch current folder from HTTP
                KeyCode::Char('R') => {
                    if app.active_pane == Pane::Browser {
                        app.force_refresh = true;
                        let path = app.current_path.clone();
                        app.navigate(path);
                        app.status_msg = "Refreshing from server…".into();
                    }
                }

                // Concurrent downloads: + / -
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    app.settings.concurrent = (app.settings.concurrent + 1).min(16);
                    save_settings(&app.settings);
                    let _ = app.dl_tx.send(DlCmd::SetConcurrent(app.settings.concurrent));
                    app.status_msg = format!("Threads: {}", app.settings.concurrent);
                }
                KeyCode::Char('-') => {
                    app.settings.concurrent = (app.settings.concurrent.saturating_sub(1)).max(1);
                    save_settings(&app.settings);
                    let _ = app.dl_tx.send(DlCmd::SetConcurrent(app.settings.concurrent));
                    app.status_msg = format!("Threads: {}", app.settings.concurrent);
                }
                // Retries: [ / ]
                KeyCode::Char(']') => {
                    app.settings.max_retries = (app.settings.max_retries + 1).min(10);
                    save_settings(&app.settings);
                    app.status_msg = format!("Retries: {}", app.settings.max_retries);
                }
                KeyCode::Char('[') => {
                    app.settings.max_retries = app.settings.max_retries.saturating_sub(1);
                    save_settings(&app.settings);
                    app.status_msg = format!("Retries: {}", app.settings.max_retries);
                }

                _ => {}
            }
            } // end Event::Key match
        } // end while processed < MAX_EVENTS_PER_FRAME
        // Discard any remaining events in the queue this frame
        while event::poll(Duration::from_millis(0))? { let _ = event::read(); }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}
