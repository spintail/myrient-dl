// Auto-generated index — do not edit.
// Single source of truth for the baked-in Myrient tree:
//   navigation (per-folder entry blocks)
//   folder sizes (in index, from full crawl)
//   search (built lazily on first search from dir blocks)

use std::collections::HashMap;
use std::sync::OnceLock;

static COMPRESSED: &[u8] = include_bytes!("generated_dirs.bin");

// Index entry: (block_offset, block_clen, folder_total_bytes)
static INDEX: OnceLock<HashMap<String, (usize, usize, u64)>> = OnceLock::new();

fn ensure_index() -> &'static HashMap<String, (usize, usize, u64)> {
    INDEX.get_or_init(|| parse_index(COMPRESSED).unwrap_or_default())
}

fn parse_index(data: &[u8]) -> Option<HashMap<String, (usize, usize, u64)>> {
    if data.len() < 4 { return None; }
    let num_folders = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    let mut map = HashMap::with_capacity(num_folders);
    let mut pos = 4usize;
    for _ in 0..num_folders {
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

pub fn lookup(path: &str) -> Option<Vec<DirEntry>> {
    if COMPRESSED.len() <= 1 { return None; }
    let key = path.trim_matches('/');
    let &(offset, clen, _) = ensure_index().get(key)?;
    let block = COMPRESSED.get(offset..offset+clen)?;
    let raw = zstd::decode_all(block).ok()?;
    parse_block(&raw)
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

/// Returns the pre-computed total size of a folder (from the full crawl).
/// Key is the folder path without leading/trailing slashes, e.g. "No-Intro/Nintendo - Game Boy".
pub fn folder_size(path: &str) -> Option<u64> {
    if COMPRESSED.len() <= 1 { return None; }
    let key = path.trim_matches('/');
    ensure_index().get(key).map(|&(_, _, total)| total).filter(|&t| t > 0)
}

pub fn folder_count() -> usize {
    if COMPRESSED.len() <= 1 { return 0; }
    ensure_index().len()
}

// ── Search index — built lazily from dir blocks ───────────────────────────────

struct SearchData {
    buf:   &'static [u8],
    index: Vec<(u32, u32, u32, u32, u64)>,
}

static SEARCH: OnceLock<SearchData> = OnceLock::new();

/// Trigger search index build in the background. Call once at startup.
pub fn warm_search_index() {
    std::thread::spawn(|| { ensure_search(); });
}

fn ensure_search() -> &'static SearchData {
    SEARCH.get_or_init(|| {
        if COMPRESSED.len() <= 1 {
            return SearchData { buf: &[], index: vec![] };
        }
        let index_map = ensure_index();
        let mut raw: Vec<u8> = Vec::new();
        let mut offsets: Vec<(u32, u32, u32, u32, u64)> = Vec::new();

        for (folder_path, &(offset, clen, _)) in index_map {
            let Some(block) = COMPRESSED.get(offset..offset+clen) else { continue };
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

pub fn search(query: &str) -> impl Iterator<Item = SearchEntry<'static>> {
    let q_owned = query.to_lowercase();
    let data = ensure_search();
    data.index.iter().filter_map(move |&(hs, he, fs, fe, sz)| {
        let href_lc = std::str::from_utf8(&data.buf[hs as usize..he as usize]).ok()?;
        if !href_lc.contains(q_owned.as_str()) { return None; }
        let folder  = std::str::from_utf8(&data.buf[fs as usize..fe as usize]).ok()?;
        let name    = url_decode_name(href_lc);
        Some(SearchEntry { href_lc, folder, name, size_bytes: sz })
    })
}
