///! Run with: cargo run --bin fetch_sizes
///!
///! Crawls myrient.erista.me completely and emits two files:
///!   src/generated_sizes/   — folder sizes at every depth (chunked for compilation)
///!   src/generated_dirs.rs  — full directory listing for every folder
///!
///! Features:
///!   - Parallel crawling with stdlib threads
///!   - Checkpoints to fetch_sizes_cache.json — resume by re-running
///!   - Retries with exponential backoff
///!   - Progress output

use rayon::prelude::*;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BASE_URL:    &str = "https://myrient.erista.me/files/";
const SIZES_PATH:  &str = "src/generated_sizes";
const DIRS_BIN:    &str = "src/generated_dirs.bin";
const SEARCH_BIN:  &str = "src/generated_search.bin";
const CACHE_PATH:  &str = "fetch_sizes_cache.json";
const MAX_RETRIES: u32  = 3;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDir {
    entries:     Vec<CachedEntry>,
    total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
struct CachedEntry {
    name:      String,
    href:      String,
    size:      String,
    date:      String,
    is_folder: bool,
}

/// Cache: maps rel_path → CachedDir (only written once the folder is fully crawled)
#[derive(Debug, Serialize, Deserialize, Default)]
struct Cache {
    dirs: HashMap<String, CachedDir>,
}

impl Cache {
    fn load() -> Self {
        if let Ok(d) = std::fs::read_to_string(cache_path()) {
            serde_json::from_str(&d).unwrap_or_default()
        } else { Cache::default() }
    }
    fn save(&self) {
        if let Ok(j) = serde_json::to_string(self) {
            std::fs::write(cache_path(), j).ok();
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let refresh   = std::env::args().any(|a| a == "--refresh");
    let dirs_only = std::env::args().any(|a| a == "--dirs-only");

    println!("myrient-dl fetch_sizes  (full tree — sizes + directory listings)");
    if dirs_only  { println!("Mode: --dirs-only (skip size crawl, write generated_dirs.rs from cache)"); }
    if refresh    { println!("Mode: --refresh (re-crawl folders whose date changed on server)"); }
    println!("Cache: {}", cache_path().display());

    let cache = Arc::new(Mutex::new(Cache::load()));
    let already = cache.lock().unwrap().dirs.len();
    if already > 0 { println!("Resuming — {} folder(s) cached so far\n", already); }

    let client = build_client();

    // --dirs-only: write generated_dirs.rs from cache (crawl only if cache is empty)
    if dirs_only {
        if already > 0 {
            // Cache is populated — just emit the dirs file immediately
            let cache = cache.lock().unwrap();
            let mut paths: Vec<&String> = cache.dirs.keys().collect();
            paths.sort();
            write_dirs(&cache.dirs, &paths);
            println!("Wrote {} folder listings to {} (from cache)", paths.len(), DIRS_BIN);
            println!("Skipped size recalculation — generated_sizes unchanged.");
            return;
        }

        // Cache is empty — crawl listings without size aggregation
        println!("Cache empty — fetching root…");
        let root_entries = match fetch_dir_retry(&client, BASE_URL) {
            Ok(e) => e,
            Err(e) => { eprintln!("Fatal: {}", e); std::process::exit(1); }
        };

        let top_folders: Vec<_> = root_entries.iter().filter(|e| e.is_folder).collect();
        println!("Found {} top-level folders — crawling listings (parallel, no size aggregation)\n", top_folders.len());

        // Use many threads for I/O-bound HTTP work
        rayon::ThreadPoolBuilder::new().num_threads(128).build_global().ok();

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        top_folders.par_iter().for_each(|entry| {
            let rel = entry.href.trim_end_matches('/').to_string();
            let url = format!("{}{}", BASE_URL, entry.href);
            let thread_client = Arc::clone(&client);
            crawl_listings(&thread_client, &url, &rel, &cache, &counter);
        });

        // Store root and do final save
        {
            let root_cached: Vec<CachedEntry> = root_entries.iter().map(entry_to_cached).collect();
            let mut c = cache.lock().unwrap();
            c.dirs.entry(String::new()).or_insert_with(|| CachedDir { entries: root_cached, total_bytes: 0 });
            c.save();
        }

        let n = counter.load(std::sync::atomic::Ordering::Relaxed);
        println!("\nCrawled {} folders.", n);

        let cache = cache.lock().unwrap();
        let mut paths: Vec<&String> = cache.dirs.keys().collect();
        paths.sort();
        write_dirs(&cache.dirs, &paths);
        println!("\nWrote {} folder listings to {}", paths.len(), DIRS_BIN);
        println!("Skipped size recalculation — generated_sizes unchanged.");
        return;
    }

    // Full crawl — fetch root
    println!("Fetching root…");
    let root_entries = match fetch_dir_retry(&client, BASE_URL) {
        Ok(e) => e,
        Err(e) => { eprintln!("Fatal: {}", e); std::process::exit(1); }
    };

    // In refresh mode, invalidate any cached folder whose top-level date changed
    if refresh {
        let mut c = cache.lock().unwrap();
        let mut invalidated = 0usize;
        for entry in root_entries.iter().filter(|e| e.is_folder) {
            let rel = entry.href.trim_end_matches('/');
            if let Some(cached) = c.dirs.get(rel) {
                let cached_date = cached.entries.first().map(|e| e.date.as_str()).unwrap_or("");
                if cached_date != entry.date.as_str() {
                    // Date changed — remove this folder and all descendants from cache
                    let prefix = format!("{}/", rel);
                    let stale: Vec<String> = c.dirs.keys()
                        .filter(|k| *k == rel || k.starts_with(&prefix))
                        .cloned()
                        .collect();
                    for key in stale { c.dirs.remove(&key); invalidated += 1; }
                }
            }
        }
        if invalidated > 0 {
            println!("Refresh: invalidated {} stale folder(s)\n", invalidated);
            c.save();
        } else {
            println!("Refresh: all cached folders are up to date\n");
        }
    }

    let top_folders: Vec<_> = root_entries.iter().filter(|e| e.is_folder).collect();
    println!("Found {} top-level folders\n", top_folders.len());

    rayon::ThreadPoolBuilder::new().num_threads(128).build_global().ok();
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Crawl each top-level folder in parallel with rayon
    top_folders.par_iter().for_each(|entry| {
        let rel     = entry.href.trim_end_matches('/').to_string();
        let url     = format!("{}{}", BASE_URL, entry.href);
        let thread_client = Arc::clone(&client);
        crawl(&thread_client, &url, &rel, &cache, &counter);
    });

    // Also store root itself
    {
        let root_cached: Vec<CachedEntry> = root_entries.iter().map(entry_to_cached).collect();
        let mut c = cache.lock().unwrap();
        c.dirs.entry(String::new()).or_insert_with(|| CachedDir {
            entries: root_cached,
            total_bytes: 0,
        });
        c.save();
    }

    let cache = cache.lock().unwrap();
    let mut paths: Vec<&String> = cache.dirs.keys().collect();
    paths.sort();

    write_sizes(&cache.dirs, &paths);
    write_dirs(&cache.dirs, &paths);

    println!("\nWrote {} folder entries to {} and {}", paths.len(), SIZES_PATH, DIRS_BIN);
    println!("Delete {} to re-crawl from scratch, or use --refresh to update changed folders.", cache_path().display());
}

// ── Crawl ─────────────────────────────────────────────────────────────────────

/// Recursively crawl a folder, storing its listing and size in the cache.
/// Returns total bytes under this folder.
fn crawl(
    client:  &Arc<reqwest::blocking::Client>,
    url:     &str,
    rel:     &str,
    cache:   &Arc<Mutex<Cache>>,
    counter: &Arc<std::sync::atomic::AtomicUsize>,
) -> u64 {
    // Return from cache if already done
    {
        let c = cache.lock().unwrap();
        if let Some(d) = c.dirs.get(rel) {
            return d.total_bytes;
        }
    }

    let entries = match fetch_dir_retry(client, url) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("  warn: {}: {}", rel, e);
            return 0;
        }
    };

    let (files, subfolders): (Vec<_>, Vec<_>) = entries.iter().partition(|e| !e.is_folder);
    let file_bytes: u64 = files.iter().map(|e| parse_size(&e.size)).sum();

    // Recurse subfolders in parallel — each gets its own client
    let sub_bytes: u64 = subfolders.par_iter().map(|sub| {
        let sub_rel = format!("{}/{}", rel, sub.href.trim_end_matches('/'));
        let sub_url = format!("{}{}{}", url.trim_end_matches('/'), "/", sub.href);
        let thread_client = Arc::clone(&client);
        crawl(&thread_client, &sub_url, &sub_rel, cache, counter)
    }).sum();

    let total_bytes = file_bytes + sub_bytes;
    let cached_entries: Vec<CachedEntry> = entries.iter().map(entry_to_cached).collect();

    let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    {
        let mut c = cache.lock().unwrap();
        c.dirs.insert(rel.to_string(), CachedDir { entries: cached_entries, total_bytes });
        if n % 500 == 0 { c.save(); }
    }
    if n % 500 == 0 { println!("[{}]", n); }

    total_bytes
}

/// Like crawl() but skips recursive size aggregation — just fetches and stores directory listings.
/// Used by --dirs-only mode where sizes are already known.
fn crawl_listings(
    client:  &Arc<reqwest::blocking::Client>,
    url:     &str,
    rel:     &str,
    cache:   &Arc<Mutex<Cache>>,
    counter: &Arc<std::sync::atomic::AtomicUsize>,
) {
    // Skip if already cached
    {
        let c = cache.lock().unwrap();
        if c.dirs.contains_key(rel) { return; }
    }

    let entries = match fetch_dir_retry(client, url) {
        Ok(e) => e,
        Err(e) => { eprintln!("  warn: {}: {}", rel, e); return; }
    };

    let subfolders: Vec<_> = entries.iter()
        .filter(|e| e.is_folder)
        .map(|e| e.href.clone())
        .collect();
    let cached_entries: Vec<CachedEntry> = entries.iter().map(entry_to_cached).collect();

    let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    {
        let mut c = cache.lock().unwrap();
        let total_bytes = c.dirs.get(rel).map(|d| d.total_bytes).unwrap_or(0);
        c.dirs.insert(rel.to_string(), CachedDir { entries: cached_entries, total_bytes });
        if n % 500 == 0 { c.save(); }
    }
    if n % 500 == 0 { println!("[{}]", n); }

    // Recurse into subfolders in parallel — each gets its own client for true concurrency
    subfolders.par_iter().for_each(|href| {
        let sub_rel = format!("{}/{}", rel, href.trim_end_matches('/'));
        let sub_url = format!("{}{}{}", url.trim_end_matches('/'), "/", href);
        let thread_client = Arc::clone(&client);
        crawl_listings(&thread_client, &sub_url, &sub_rel, cache, counter);
    });
}

fn entry_to_cached(e: &RawEntry) -> CachedEntry {
    CachedEntry {
        name:      e.name.clone(),
        href:      e.href.clone(),
        size:      e.size.clone(),
        date:      e.date.clone(),
        is_folder: e.is_folder,
    }
}

// ── Code generation ───────────────────────────────────────────────────────────

fn write_sizes(dirs: &HashMap<String, CachedDir>, paths: &[&String]) {
    const CHUNK: usize = 500;
    let all_entries: Vec<(&str, u64, &str)> = paths.iter()
        .filter(|p| dirs[p.as_str()].total_bytes > 0)
        .map(|p| {
            let d = &dirs[p.as_str()];
            let date = d.entries.first().map(|e| e.date.as_str()).unwrap_or("");
            (p.as_str(), d.total_bytes, date)
        })
        .collect();

    let chunks: Vec<_> = all_entries.chunks(CHUNK).collect();
    let dir = project_path(SIZES_PATH);
    std::fs::create_dir_all(&dir).expect("create generated_sizes dir");

    // Remove any stale chunk files from previous runs with different chunk counts
    if let Ok(read) = std::fs::read_dir(&dir) {
        for entry in read.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("chunk_") && s.ends_with(".rs") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Write one file per chunk
    for (i, chunk) in chunks.iter().enumerate() {
        let mut out = format!("// Auto-generated chunk {:03} — do not edit.\n\n", i);
        out += "use std::collections::HashMap;\n\n";
        out += &format!("pub fn fill_{:03}(m: &mut HashMap<&'static str, (u64, &'static str)>) {{\n", i);
        for (path, bytes, date) in chunk.iter() {
            out += &format!("        m.insert({}, ({}, {}));\n",
                rust_str(path), bytes, rust_str(date));
        }
        out += "}\n";
        std::fs::write(dir.join(format!("chunk_{:03}.rs", i)), out)
            .expect("write chunk");
    }

    // Write mod.rs
    let mut mod_out = String::from("// Auto-generated — do not edit.\n\n");
    for i in 0..chunks.len() { mod_out += &format!("mod chunk_{:03};\n", i); }
    mod_out += "\nuse std::collections::HashMap;\nuse std::sync::OnceLock;\n\n";
    mod_out += "pub static FOLDER_SIZES: OnceLock<HashMap<&'static str, (u64, &'static str)>> = OnceLock::new();\n\n";
    mod_out += "pub fn folder_sizes() -> &'static HashMap<&'static str, (u64, &'static str)> {\n";
    mod_out += "    FOLDER_SIZES.get_or_init(|| {\n        let mut m = HashMap::new();\n";
    for i in 0..chunks.len() {
        mod_out += &format!("        chunk_{:03}::fill_{:03}(&mut m);\n", i, i);
    }
    mod_out += "        m\n    })\n}\n";
    std::fs::write(dir.join("mod.rs"), mod_out).expect("write mod.rs");

    println!("Wrote {} entries across {} chunk files to {}/", all_entries.len(), chunks.len(), SIZES_PATH);
}

fn write_dirs(dirs: &HashMap<String, CachedDir>, paths: &[&String]) {
    // Binary format:
    //   [u32le: num_folders]
    //   [index entries: (u16le path_len)(path bytes)(u32le block_offset)(u32le block_clen)]
    //   [zstd-compressed blocks, one per folder]
    //
    // Each block decompresses to:
    //   [u16le: num_entries] × [(u8 flags)(u8 name_len)(name)(u8 href_len)(href)
    //                           (u8 size_len)(size)(u8 date_len)(date)]
    //
    // Blocks are compressed at zstd level 19 (max) for minimal binary size.
    // At runtime only the requested folder's block is decompressed — O(1) RAM.

    let mut index_buf: Vec<u8> = Vec::new();
    let mut blocks_buf: Vec<u8> = Vec::new();

    // Write num_folders
    index_buf.extend_from_slice(&(paths.len() as u32).to_le_bytes());

    let mut total_entries = 0usize;
    let mut total_raw = 0usize;

    for path in paths {
        let d = &dirs[*path];
        let path_bytes = path.as_bytes();

        // Encode entries into compact binary
        let mut block_raw: Vec<u8> = Vec::new();
        let num = d.entries.len().min(65535);
        block_raw.extend_from_slice(&(num as u16).to_le_bytes());
        for e in d.entries.iter().take(num) {
            let flags: u8 = if e.is_folder { 1 } else { 0 };
            block_raw.push(flags);
            write_str_field(&mut block_raw, &e.name);
            write_str_field(&mut block_raw, &e.href);
            write_str_field(&mut block_raw, &e.size);
            write_str_field(&mut block_raw, &e.date);
        }
        total_raw += block_raw.len();
        total_entries += num;

        // Compress block at level 19 (max compression for smallest binary)
        let compressed_block = zstd::encode_all(block_raw.as_slice(), 19)
            .expect("zstd compress block");

        let offset = blocks_buf.len() as u32;
        let clen   = compressed_block.len() as u32;
        blocks_buf.extend_from_slice(&compressed_block);

        // Write index entry
        index_buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
        index_buf.extend_from_slice(path_bytes);
        index_buf.extend_from_slice(&offset.to_le_bytes());
        index_buf.extend_from_slice(&clen.to_le_bytes());
    }

    // Prepend index before blocks — adjust offsets to skip past index
    let index_size = index_buf.len();
    // Rewrite offsets in index to be absolute (index_size + block_offset)
    // Index structure after the initial u32: entries of (u16 path_len)(path)(u32 offset)(u32 clen)
    let mut pos = 4usize; // skip num_folders
    while pos < index_size {
        let path_len = u16::from_le_bytes(index_buf[pos..pos+2].try_into().unwrap()) as usize;
        pos += 2 + path_len;
        // patch offset
        let raw_offset = u32::from_le_bytes(index_buf[pos..pos+4].try_into().unwrap()) as usize;
        let abs_offset = (index_size + raw_offset) as u32;
        index_buf[pos..pos+4].copy_from_slice(&abs_offset.to_le_bytes());
        pos += 8; // offset + clen
    }

    let mut out = index_buf;
    out.extend_from_slice(&blocks_buf);

    std::fs::write(project_path(DIRS_BIN), &out).expect("write generated_dirs.bin");
    println!("Wrote {} entries in {} folders → {:.1} KB (raw {:.1} KB, ratio {:.1}x) to {}",
        total_entries, paths.len(),
        out.len() as f64 / 1024.0,
        total_raw as f64 / 1024.0,
        total_raw as f64 / out.len() as f64,
        DIRS_BIN);

    // Build search index: compact list of (file_name_lowercase \0 folder_path \0 display_name \0 size \n)
    // Stored as a single zstd block in a separate file — decompressed once on first search.
    let mut search_raw: Vec<u8> = Vec::new();
    for path in paths {
        let d = &dirs[*path];
        let folder = path.as_str();
        for e in &d.entries {
            if e.is_folder { continue; }
            let name_lc = e.name.to_lowercase();
            // Format: name_lc NUL folder NUL display_name NUL size NUL
            search_raw.extend_from_slice(name_lc.as_bytes());
            search_raw.push(0);
            search_raw.extend_from_slice(folder.as_bytes());
            search_raw.push(0);
            search_raw.extend_from_slice(e.name.as_bytes());
            search_raw.push(0);
            search_raw.extend_from_slice(e.size.as_bytes());
            search_raw.push(0);
        }
    }
    let search_compressed = zstd::encode_all(search_raw.as_slice(), 19)
        .expect("zstd compress search");
    std::fs::write(project_path(SEARCH_BIN), &search_compressed).expect("write generated_search.bin");
    println!("Search index → {:.1} KB compressed (from {:.1} KB) to {}",
        search_compressed.len() as f64 / 1024.0,
        search_raw.len() as f64 / 1024.0,
        SEARCH_BIN);
}

fn write_str_field(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    // Truncate to 255 bytes max (field length is u8)
    let len = bytes.len().min(255);
    buf.push(len as u8);
    buf.extend_from_slice(&bytes[..len]);
}

/// Resolve a project-relative path. During `cargo run` CARGO_MANIFEST_DIR is set.
/// Otherwise falls back to current directory (user should run from project root).
fn project_path(rel: &str) -> std::path::PathBuf {
    if let Ok(d) = std::env::var("CARGO_MANIFEST_DIR") {
        return std::path::PathBuf::from(d).join(rel);
    }
    std::path::PathBuf::from(rel)
}

fn cache_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("FETCH_SIZES_CACHE") {
        return std::path::PathBuf::from(p);
    }
    let p = project_path(CACHE_PATH);
    if p.exists() { return p; }
    // Also check next to the executable (for running the compiled binary directly)
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe.parent().unwrap_or(std::path::Path::new(".")).join(CACHE_PATH);
        if candidate.exists() { return candidate; }
    }
    p
}

fn rust_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ── HTTP ──────────────────────────────────────────────────────────────────────

struct RawEntry {
    name: String, href: String, size: String, date: String, is_folder: bool,
}

fn fetch_dir_retry(client: &Arc<reqwest::blocking::Client>, url: &str) -> Result<Vec<RawEntry>, String> {
    let mut last = String::new();
    for attempt in 0..MAX_RETRIES {
        if attempt > 0 { std::thread::sleep(Duration::from_secs(2u64.pow(attempt))); }
        match fetch_dir(client, url) {
            Ok(e) => return Ok(e),
            Err(e) => last = e,
        }
    }
    Err(format!("failed after {} retries: {}", MAX_RETRIES, last))
}

fn fetch_dir(client: &Arc<reqwest::blocking::Client>, url: &str) -> Result<Vec<RawEntry>, String> {
    let body = client.get(url).send().map_err(|e| e.to_string())?
        .text().map_err(|e| e.to_string())?;
    let doc   = Html::parse_document(&body);
    let tr    = Selector::parse("table tr").unwrap();
    let td    = Selector::parse("td").unwrap();
    let a_sel = Selector::parse("a").unwrap();
    let mut out = Vec::new();
    for row in doc.select(&tr).skip(1) {
        let cells: Vec<_> = row.select(&td).collect();
        if cells.len() < 3 { continue; }
        let Some(link) = cells[0].select(&a_sel).next() else { continue };
        let href = link.value().attr("href").unwrap_or("").to_string();
        if href == "./" || href == "../" || href.is_empty() { continue; }
        let name      = link.text().collect::<String>().trim().trim_end_matches('/').to_string();
        let size      = cells[1].text().collect::<String>().trim().to_string();
        let date      = cells[2].text().collect::<String>().trim().to_string();
        let is_folder = href.ends_with('/');
        out.push(RawEntry { name, href, size, date, is_folder });
    }
    Ok(out)
}

fn build_client() -> Arc<reqwest::blocking::Client> {
    Arc::new(reqwest::blocking::Client::builder()
        .user_agent("myrient-dl-sizer/1.0")
        .timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(128)
        .build().expect("reqwest client"))
}

fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() || s == "-" { return 0; }
    let mut p = s.splitn(2, ' ');
    let n: f64 = p.next().unwrap_or("").parse().unwrap_or(0.0);
    let m: u64 = match p.next().unwrap_or("").trim().to_uppercase().as_str() {
        "TIB"|"TB" => 1_099_511_627_776,
        "GIB"|"GB" => 1_073_741_824,
        "MIB"|"MB" => 1_048_576,
        "KIB"|"KB" => 1_024,
        _          => 1,
    };
    (n * m as f64) as u64
}

#[allow(dead_code)]
fn human(b: u64) -> String {
    match b {
        b if b >= 1_099_511_627_776 => format!("{:.1} TiB", b as f64 / 1_099_511_627_776.0),
        b if b >= 1_073_741_824     => format!("{:.1} GiB", b as f64 / 1_073_741_824.0),
        b if b >= 1_048_576         => format!("{:.1} MiB", b as f64 / 1_048_576.0),
        b if b >= 1_024             => format!("{:.1} KiB", b as f64 / 1_024.0),
        b                           => format!("{} B", b),
    }
}
