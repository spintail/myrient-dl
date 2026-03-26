// Dynamic directory index.
//
// On first run, copies the embedded generated_dirs.bin to the local data directory.
// All subsequent reads come from the local file, which is updated as the user browses.
// When a folder is fetched live from Myrient, it is persisted to the local file
// in the background so future lookups are instant.
//
// Format: see fetch_sizes/write_dirs for the binary layout.
// Writes are atomic: write to .tmp alongside the real file, then rename.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

// ── Embedded fallback ─────────────────────────────────────────────────────────
// Baked in at compile time — used only on first run to seed the local file.

static EMBEDDED: &[u8] = include_bytes!("generated_dirs.bin");

// ── Local file ────────────────────────────────────────────────────────────────
// Loaded at startup from the local data directory.
// Held in a RwLock so the index can be hot-reloaded after a write.

struct LocalIndex {
    /// Raw bytes of the local file (or embedded if local doesn't exist yet)
    data: Vec<u8>,
    /// Parsed index: path → (block_offset, block_clen, total_bytes)
    map:  HashMap<String, (usize, usize, u64)>,
}

static LOCAL: OnceLock<RwLock<LocalIndex>> = OnceLock::new();

fn local_bin_path() -> PathBuf {
    data_dir().join("generated_dirs.bin")
}

fn data_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/share")
        });
    base.join("myrient-dl")
}

/// Initialise on startup:
/// - If no local file exists, seed it from the embedded data.
/// - Load the local file into the in-memory index.
pub fn init() {
    let path = local_bin_path();
    std::fs::create_dir_all(path.parent().unwrap()).ok();

    // Seed from embedded if local file doesn't exist or is empty
    if !path.exists() || std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) <= 1 {
        if EMBEDDED.len() > 1 {
            std::fs::write(&path, EMBEDDED).ok();
        }
    }

    // Load local file (fall back to embedded bytes if read fails)
    let data = std::fs::read(&path).unwrap_or_else(|_| EMBEDDED.to_vec());
    let map  = parse_index(&data).unwrap_or_default();
    LOCAL.get_or_init(|| RwLock::new(LocalIndex { data, map }));
}

fn ensure_local() -> &'static RwLock<LocalIndex> {
    LOCAL.get_or_init(|| {
        // init() wasn't called — load embedded as fallback
        let data = EMBEDDED.to_vec();
        let map  = parse_index(&data).unwrap_or_default();
        RwLock::new(LocalIndex { data, map })
    })
}

// ── Index parsing ─────────────────────────────────────────────────────────────

fn parse_index(data: &[u8]) -> Option<HashMap<String, (usize, usize, u64)>> {
    if data.len() < 4 { return None; }
    let num = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    let mut map = HashMap::with_capacity(num);
    let mut pos = 4usize;
    for _ in 0..num {
        if pos + 2 > data.len() { break; }
        let path_len = u16::from_le_bytes(data[pos..pos+2].try_into().ok()?) as usize;
        pos += 2;
        if pos + path_len + 16 > data.len() { break; }
        let path   = std::str::from_utf8(&data[pos..pos+path_len]).ok()?.to_string();
        pos += path_len;
        let offset = u32::from_le_bytes(data[pos..pos+4].try_into().ok()?) as usize; pos += 4;
        let clen   = u32::from_le_bytes(data[pos..pos+4].try_into().ok()?) as usize; pos += 4;
        let total  = u64::from_le_bytes(data[pos..pos+8].try_into().ok()?);           pos += 8;
        map.insert(path, (offset, clen, total));
    }
    Some(map)
}

// ── Navigation ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DirEntry {
    pub name:       String,
    pub href:       String,
    pub size:       String,
    pub size_bytes: u64,
    pub date:       String,
    pub is_folder:  bool,
}

/// Look up a folder's entries. Returns None if not in the local index.
pub fn lookup(path: &str) -> Option<Vec<DirEntry>> {
    let key = path.trim_matches('/');
    let local = ensure_local().read().ok()?;
    let &(offset, clen, _) = local.map.get(key)?;
    let block = local.data.get(offset..offset+clen)?;
    let raw = zstd::decode_all(block).ok()?;
    parse_block(&raw)
}

/// Persist a freshly-fetched folder to the local index in the background.
/// Called after a live HTTP fetch so future lookups are instant.
/// `entries` are the raw HTTP entries; `total_bytes` is 0 if unknown.
pub fn persist_folder(path: String, entries: Vec<DirEntry>, total_bytes: u64) {
    std::thread::spawn(move || {
        if let Err(e) = do_persist(path, entries, total_bytes) {
            eprintln!("warn: failed to persist folder to index: {}", e);
        }
    });
}

fn do_persist(path: String, entries: Vec<DirEntry>, total_bytes: u64) -> std::io::Result<()> {
    let key = path.trim_matches('/').to_string();

    // Encode entries to compact binary block
    let block_raw = encode_block(&entries);
    let compressed = zstd::encode_all(block_raw.as_slice(), 3)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    let local = ensure_local();

    // Rebuild the full file with the new/updated block appended
    // We take a write lock for the entire operation to keep the file consistent
    let mut idx = local.write().map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "lock poisoned"))?;

    // Build new index + blocks, replacing the entry for `key` if it exists
    // Strategy: copy all existing blocks except the one being replaced, append new block
    let mut new_index: Vec<(String, usize, usize, u64)> = Vec::new(); // (path, offset, clen, total)
    let mut new_blocks: Vec<u8> = Vec::new();

    for (p, &(offset, clen, tot)) in &idx.map {
        if p == &key { continue; } // skip — we'll add the new version below
        let Some(block) = idx.data.get(offset..offset+clen) else { continue };
        let block_offset = new_blocks.len();
        new_blocks.extend_from_slice(block);
        new_index.push((p.clone(), block_offset, clen, tot));
    }

    // Append the new/updated block
    let new_block_offset = new_blocks.len();
    let new_clen = compressed.len();
    new_blocks.extend_from_slice(&compressed);
    new_index.push((key.clone(), new_block_offset, new_clen, total_bytes));

    // Serialise index header
    let mut header: Vec<u8> = Vec::new();
    header.extend_from_slice(&(new_index.len() as u32).to_le_bytes());
    for (p, _, _, _) in &new_index {
        let pb = p.as_bytes();
        header.extend_from_slice(&(pb.len() as u16).to_le_bytes());
        header.extend_from_slice(pb);
        header.extend_from_slice(&0u32.to_le_bytes()); // offset placeholder
        header.extend_from_slice(&0u32.to_le_bytes()); // clen placeholder
        header.extend_from_slice(&0u64.to_le_bytes()); // total_bytes placeholder
    }

    // Patch offsets now that we know the header size
    let header_size = header.len();
    let mut hpos = 4usize;
    for (_, block_offset, clen, total) in &new_index {
        let path_len = u16::from_le_bytes(header[hpos..hpos+2].try_into().unwrap()) as usize;
        hpos += 2 + path_len;
        let abs = (header_size + block_offset) as u32;
        header[hpos..hpos+4].copy_from_slice(&abs.to_le_bytes());          hpos += 4;
        header[hpos..hpos+4].copy_from_slice(&(*clen as u32).to_le_bytes()); hpos += 4;
        header[hpos..hpos+8].copy_from_slice(&total.to_le_bytes());          hpos += 8;
    }

    let mut out = header;
    out.extend_from_slice(&new_blocks);

    // Atomic write: write to .tmp then rename
    let bin_path = local_bin_path();
    let tmp_path = bin_path.with_extension("tmp");
    std::fs::write(&tmp_path, &out)?;
    std::fs::rename(&tmp_path, &bin_path)?;

    // Hot-reload the index in memory
    let new_map = parse_index(&out).unwrap_or_default();
    idx.data = out;
    idx.map  = new_map;

    Ok(())
}

fn encode_block(entries: &[DirEntry]) -> Vec<u8> {
    let mut buf = Vec::new();
    let num = entries.len().min(65535) as u16;
    buf.extend_from_slice(&num.to_le_bytes());
    for e in entries.iter().take(num as usize) {
        buf.push(if e.is_folder { 1u8 } else { 0u8 });
        write_str_field(&mut buf, &e.href);
        if !e.is_folder {
            buf.push(((e.size_bytes >> 32) & 0xff) as u8);
            buf.extend_from_slice(&((e.size_bytes & 0xffffffff) as u32).to_le_bytes());
        }
        // Pack date from "YYYY-MM-DD" string
        let (dy, dm, dd) = parse_date_compact(&e.date);
        buf.push(dy);
        buf.push((dm << 4) | (dd & 0x0f));
    }
    buf
}

fn write_str_field(buf: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let len = b.len().min(255);
    buf.push(len as u8);
    buf.extend_from_slice(&b[..len]);
}

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
            if c.is_ascii_digit() { n = n * 10 + (c - b'0') as u32; }
        }
    }
    n
}

fn parse_block(data: &[u8]) -> Option<Vec<DirEntry>> {
    if data.len() < 2 { return Some(vec![]); }
    let num = u16::from_le_bytes(data[0..2].try_into().ok()?) as usize;
    let mut entries = Vec::with_capacity(num);
    let mut pos = 2usize;
    for _ in 0..num {
        if pos >= data.len() { break; }
        let flags = *data.get(pos)?; pos += 1;
        let is_folder = flags & 1 != 0;
        let href = read_str(data, &mut pos)?;
        let name = url_decode_name(&href);
        let size_bytes = if !is_folder {
            if pos + 5 > data.len() { break; }
            let hi = data[pos] as u64; pos += 1;
            let lo = u32::from_le_bytes(data[pos..pos+4].try_into().ok()?) as u64; pos += 4;
            (hi << 32) | lo
        } else { 0u64 };
        let size = if size_bytes > 0 { fmt_size(size_bytes) } else { String::new() };
        let date = if pos + 2 <= data.len() {
            let y = data[pos] as u32 + 2000; pos += 1;
            let b = data[pos]; pos += 1;
            let m = (b >> 4) as u32;
            let d = (b & 0x0f) as u32;
            if m > 0 && d > 0 { format!("{:04}-{:02}-{:02}", y, m, d) } else { String::new() }
        } else { String::new() };
        entries.push(DirEntry { name, href, size, size_bytes, date, is_folder });
    }
    Some(entries)
}

fn read_str(data: &[u8], pos: &mut usize) -> Option<String> {
    let len = *data.get(*pos)? as usize; *pos += 1;
    if *pos + len > data.len() { return None; }
    let s = std::str::from_utf8(&data[*pos..*pos+len]).ok()?.to_string();
    *pos += len; Some(s)
}

pub fn url_decode_name(href: &str) -> String {
    let s = href.trim_end_matches('/');
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

fn fmt_size(b: u64) -> String {
    match b {
        b if b >= 1_099_511_627_776 => format!("{:.1} TiB", b as f64 / 1_099_511_627_776.0),
        b if b >= 1_073_741_824     => format!("{:.1} GiB", b as f64 / 1_073_741_824.0),
        b if b >= 1_048_576         => format!("{:.1} MiB", b as f64 / 1_048_576.0),
        b if b >= 1_024             => format!("{:.1} KiB", b as f64 / 1_024.0),
        b                           => format!("{} B", b),
    }
}

// ── Folder sizes ──────────────────────────────────────────────────────────────

pub fn folder_size(path: &str) -> Option<u64> {
    let key = path.trim_matches('/');
    ensure_local().read().ok()?.map.get(key).map(|&(_, _, t)| t).filter(|&t| t > 0)
}

pub fn folder_count() -> usize {
    ensure_local().read().map(|l| l.map.len()).unwrap_or(0)
}

/// Returns the sorted list of top-level folder names (no slashes, no sub-paths).
/// Used for the include/exclude dropdowns in the search tab.
pub fn top_level_folders() -> Vec<String> {
    let Ok(local) = ensure_local().read() else { return vec![]; };
    let mut folders: Vec<String> = local.map.keys()
        .filter(|k| !k.is_empty() && !k.contains('/'))
        .map(|k| k.clone())
        .collect();
    folders.sort();
    folders
}

// ── Search ────────────────────────────────────────────────────────────────────

struct SearchData {
    buf:   &'static [u8],
    index: Vec<(u32, u32, u32, u32, u64)>,
}

static SEARCH: OnceLock<SearchData> = OnceLock::new();

pub fn warm_search_index() {
    std::thread::spawn(|| { ensure_search(); });
}

fn ensure_search() -> &'static SearchData {
    SEARCH.get_or_init(|| {
        let local = match ensure_local().read() {
            Ok(l) => l,
            Err(_) => return SearchData { buf: &[], index: vec![] },
        };
        let mut raw: Vec<u8> = Vec::new();
        let mut offsets: Vec<(u32, u32, u32, u32, u64)> = Vec::new();

        for (folder_path, &(offset, clen, _)) in &local.map {
            let Some(block) = local.data.get(offset..offset+clen) else { continue };
            let Ok(decompressed) = zstd::decode_all(block) else { continue };
            let Some(entries) = parse_block(&decompressed) else { continue };
            for e in entries {
                if e.is_folder { continue; }
                let href_lc = e.href.to_lowercase();
                let hs = raw.len() as u32;
                raw.extend_from_slice(href_lc.as_bytes()); raw.push(0);
                let he = raw.len() as u32 - 1;
                let fs = raw.len() as u32;
                raw.extend_from_slice(folder_path.as_bytes()); raw.push(0);
                let fe = raw.len() as u32 - 1;
                offsets.push((hs, he, fs, fe, e.size_bytes));
            }
        }
        let buf: &'static [u8] = Box::leak(raw.into_boxed_slice());
        SearchData { buf, index: offsets }
    })
}

pub struct SearchEntry<'a> {
    pub href_lc:    &'a str,
    pub folder:     &'a str,
    pub name:       String,
    pub size_bytes: u64,
}

/// Returns true if the search index is ready for use.
pub fn search_ready() -> bool {
    SEARCH.get().is_some()
}

pub fn search(query: &str) -> Box<dyn Iterator<Item = SearchEntry<'static>> + 'static> {
    let q = query.to_lowercase();
    // Don't block — if the index isn't ready, return empty immediately.
    // The UI checks search_ready() and shows a loading indicator.
    let data = match SEARCH.get() {
        Some(d) => d,
        None    => return Box::new(std::iter::empty()),
    };
    Box::new(data.index.iter().filter_map(move |&(hs, he, fs, fe, sz)| {
        let href_lc = std::str::from_utf8(&data.buf[hs as usize..he as usize]).ok()?;
        if !href_lc.contains(q.as_str()) { return None; }
        let folder  = std::str::from_utf8(&data.buf[fs as usize..fe as usize]).ok()?;
        let name    = url_decode_name(href_lc);
        Some(SearchEntry { href_lc, folder, name, size_bytes: sz })
    }))
}
