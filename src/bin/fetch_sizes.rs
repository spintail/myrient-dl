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
const DIRS_BIN:    &str = "src/generated_dirs.bin";
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

    write_dirs(&cache.dirs, &paths);
    println!("\nWrote {} folder entries to {}", paths.len(), DIRS_BIN);
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

fn parse_existing_index(data: &[u8]) -> Option<HashMap<String, (usize, usize)>> {
    if data.len() < 4 { return None; }
    let num = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;

    // Try both index formats:
    //   Old: [u16 path_len][path][u32 offset][u32 clen]                     (8 bytes after path)
    //   New: [u16 path_len][path][u32 offset][u32 clen][u64 total_bytes]    (16 bytes after path)
    // Detect by attempting to parse with each stride and checking the first block is valid zstd.
    for stride in [8usize, 16usize] {
        if let Some(map) = try_parse_index(data, num, stride) {
            // Validate: check first entry's block starts with a valid zstd magic
            if let Some(&(offset, clen)) = map.values().next() {
                if let Some(block) = data.get(offset..offset+clen) {
                    // zstd magic: 0xFD2FB528 (little-endian: 28 B5 2F FD)
                    if block.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
                        return Some(map);
                    }
                }
            }
        }
    }
    None
}

fn try_parse_index(data: &[u8], num: usize, stride: usize) -> Option<HashMap<String, (usize, usize)>> {
    let mut map = HashMap::with_capacity(num);
    let mut pos = 4usize;
    for _ in 0..num {
        if pos + 2 > data.len() { break; }
        let path_len = u16::from_le_bytes(data[pos..pos+2].try_into().ok()?) as usize;
        pos += 2;
        if pos + path_len + stride > data.len() { break; }
        let path = std::str::from_utf8(&data[pos..pos+path_len]).ok()?.to_string();
        pos += path_len;
        let offset = u32::from_le_bytes(data[pos..pos+4].try_into().ok()?) as usize; pos += 4;
        let clen   = u32::from_le_bytes(data[pos..pos+4].try_into().ok()?) as usize; pos += 4;
        if stride == 16 { pos += 8; }
        map.insert(path, (offset, clen));
    }
    Some(map)
}

fn write_dirs(dirs: &HashMap<String, CachedDir>, paths: &[&String]) {
    // Compact binary format: see parse_index() in generated_dirs.rs for layout.
    //
    // Incremental update: read existing generated_dirs.bin if present.
    // For each folder, check if the entry count matches what's in the cache.
    // If yes, copy the compressed block directly — no recompression needed.
    // Only compress blocks that are new or changed.

    // Load existing bin for incremental reuse
    let existing: Option<Vec<u8>> = std::fs::read(project_path(DIRS_BIN)).ok();
    let existing_index: HashMap<String, (usize, usize)> = existing.as_ref()
        .and_then(|data| parse_existing_index(data))
        .unwrap_or_default();
    let reused = std::sync::atomic::AtomicUsize::new(0);

    let mut index_buf: Vec<u8> = Vec::new();
    let mut blocks_buf: Vec<u8> = Vec::new();
    index_buf.extend_from_slice(&(paths.len() as u32).to_le_bytes());

    let mut total_entries = 0usize;
    let mut total_raw = 0usize;

    // Encode each folder's entries into raw binary (sequential — fast)
    // then compress all blocks in parallel at level 19 (CPU-bound, benefits from parallelism)
    struct FolderBlock<'a> {
        path:        &'a str,
        total_bytes: u64,
        raw:         Vec<u8>,
        #[allow(dead_code)] num_entries: usize,
        // If Some, this block can be copied from the existing file unchanged
        existing_block: Option<Vec<u8>>,
    }

    let mut folder_blocks: Vec<FolderBlock> = Vec::with_capacity(paths.len());
    let mut sizes_found = 0usize;
    let mut sizes_zero  = 0usize;

    for path in paths {
        let d = &dirs[*path];
        if d.total_bytes > 0 { sizes_found += 1; } else { sizes_zero += 1; }

        let num = d.entries.len().min(65535) as u16;
        total_entries += num as usize;

        // Check if existing block has the same entry count — if so, reuse it
        let existing_block = existing_index.get(path.as_str())
            .and_then(|&(offset, clen)| {
                let data = existing.as_ref()?;
                let block_bytes = data.get(offset..offset+clen)?;
                // Peek at the decompressed entry count (first 2 bytes of block)
                let decompressed = zstd::decode_all(block_bytes).ok()?;
                if decompressed.len() >= 2 {
                    let existing_num = u16::from_le_bytes(decompressed[0..2].try_into().ok()?);
                    if existing_num == num { Some(block_bytes.to_vec()) } else { None }
                } else { None }
            });

        if existing_block.is_some() {
            reused.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            folder_blocks.push(FolderBlock { path: path.as_str(), total_bytes: d.total_bytes, raw: vec![], num_entries: num as usize, existing_block });
            continue;
        }

        let mut block_raw: Vec<u8> = Vec::new();
        block_raw.extend_from_slice(&num.to_le_bytes());

        for e in d.entries.iter().take(num as usize) {
            let flags: u8 = if e.is_folder { 1 } else { 0 };
            block_raw.push(flags);
            write_str_field(&mut block_raw, &e.href);
            if !e.is_folder {
                let sz = parse_size(&e.size);
                block_raw.push(((sz >> 32) & 0xff) as u8);
                block_raw.extend_from_slice(&((sz & 0xffffffff) as u32).to_le_bytes());
            }
            let (dy, dm, dd) = parse_date_compact(&e.date);
            block_raw.push(dy);
            block_raw.push((dm << 4) | (dd & 0x0f));
        }
        total_raw += block_raw.len();
        folder_blocks.push(FolderBlock { path: path.as_str(), total_bytes: d.total_bytes, raw: block_raw, num_entries: num as usize, existing_block: None });
    }

    let total_to_compress = folder_blocks.iter().filter(|fb| fb.existing_block.is_none()).count();

    if total_to_compress > 0 {
        println!("Compressing {} blocks at zstd level 19 (parallel)…", total_to_compress);
    }

    // Dedicated progress reporter thread — receives completion signals and prints
    // in order, so output is never interleaved from racing threads.
    let (prog_tx, prog_rx) = std::sync::mpsc::sync_channel::<()>(256);
    let reporter = if total_to_compress > 0 {
        let total = total_to_compress;
        Some(std::thread::spawn(move || {
            let mut done = 0usize;
            let interval = (total / 20).max(1); // ~5% increments
            for () in prog_rx {
                done += 1;
                if done % interval == 0 || done == total {
                    println!("  [{}/{}]  {:.0}%", done, total, done as f64 / total as f64 * 100.0);
                }
            }
        }))
    } else { None };

    // Compress only new/changed blocks in parallel at level 19
    let compressed_blocks: Vec<Vec<u8>> = folder_blocks.par_iter()
        .map(|fb| {
            if let Some(ref existing) = fb.existing_block {
                existing.clone()
            } else {
                let result = zstd::encode_all(fb.raw.as_slice(), 19).expect("zstd compress");
                let _ = prog_tx.send(());
                result
            }
        })
        .collect();

    // Drop the sender so the reporter thread exits cleanly
    drop(prog_tx);
    if let Some(h) = reporter { let _ = h.join(); }

    let n_reused = reused.load(std::sync::atomic::Ordering::Relaxed);
    let n_compressed = folder_blocks.len() - n_reused;
    println!("  {} blocks reused, {} compressed fresh", n_reused, n_compressed);

    // Assemble index and block data in order
    for (fb, compressed) in folder_blocks.iter().zip(compressed_blocks.iter()) {
        let path_bytes = fb.path.as_bytes();
        let offset = blocks_buf.len() as u32;
        let clen   = compressed.len() as u32;
        blocks_buf.extend_from_slice(compressed);

        index_buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
        index_buf.extend_from_slice(path_bytes);
        index_buf.extend_from_slice(&offset.to_le_bytes());
        index_buf.extend_from_slice(&clen.to_le_bytes());
        index_buf.extend_from_slice(&fb.total_bytes.to_le_bytes());
    }

    // Patch offsets to absolute (past index)
    let index_size = index_buf.len();
    let mut pos = 4usize;
    while pos < index_size {
        let path_len = u16::from_le_bytes(index_buf[pos..pos+2].try_into().unwrap()) as usize;
        pos += 2 + path_len;
        let raw_offset = u32::from_le_bytes(index_buf[pos..pos+4].try_into().unwrap()) as usize;
        let abs_offset = (index_size + raw_offset) as u32;
        index_buf[pos..pos+4].copy_from_slice(&abs_offset.to_le_bytes());
        pos += 8 + 8; // offset(4) + clen(4) + total_bytes(8)
    }

    let mut out = index_buf;
    out.extend_from_slice(&blocks_buf);
    std::fs::write(project_path(DIRS_BIN), &out).expect("write generated_dirs.bin");
    let ratio_str = if total_raw > 0 {
        format!("raw {:.1} KB, ratio {:.1}x", total_raw as f64/1024.0, total_raw as f64/out.len() as f64)
    } else {
        format!("all blocks reused from existing file")
    };
    println!("Wrote {} entries in {} folders → {:.1} KB ({})",
        total_entries, paths.len(), out.len() as f64/1024.0, ratio_str);
    println!("  Folder sizes: {} with data, {} unknown (run full crawl to populate)",
        sizes_found, sizes_zero);
    println!("  → {}", DIRS_BIN);
}

/// Pack date string "2023-10-15 14:23" → (year-2000, month, day)
fn parse_date_compact(date: &str) -> (u8, u8, u8) {
    let b = date.as_bytes();
    let year  = parse_digits(b, 0, 4).saturating_sub(2000).min(255) as u8;
    let month = parse_digits(b, 5, 2).min(12) as u8;
    let day   = parse_digits(b, 8, 2).min(31) as u8;
    (year, month, day)
}

fn parse_digits(b: &[u8], offset: usize, len: usize) -> u32 {
    let mut n = 0u32;
    for i in 0..len {
        if let Some(&c) = b.get(offset + i) {
            if c >= b'0' && c <= b'9' { n = n * 10 + (c - b'0') as u32; }
        }
    }
    n
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
