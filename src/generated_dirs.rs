// Auto-generated index — do not edit.
// The actual data is in generated_dirs.bin (zstd-compressed per-folder blocks).
//
// Format of generated_dirs.bin:
//   [u32le: num_folders]
//   [index: num_folders × (path_hash: u64, path_len: u16, path_bytes: ..., offset: u32, clen: u32)]
//   [zstd blocks: each independently compressed]
//
// Each block decompresses to:
//   [u16le: num_entries] × entries of:
//   [u8: flags (is_folder)] [u8: name_len] [name_bytes] [u8: href_len] [href_bytes]
//   [u8: size_len] [size_bytes] [u8: date_len] [date_bytes]
//
// On first use, the index is parsed in-place from the embedded bytes.
// Individual folder blocks are decompressed on demand — zero RAM for unvisited folders.
// On first run, a fast local cache is written to XDG_CACHE_HOME for even faster access.

use std::collections::HashMap;
use std::sync::OnceLock;

static COMPRESSED: &[u8] = include_bytes!("generated_dirs.bin");

// Parsed index: folder_path → (byte_offset_in_COMPRESSED, compressed_len)
static INDEX: OnceLock<HashMap<String, (usize, usize)>> = OnceLock::new();

fn ensure_index() -> &'static HashMap<String, (usize, usize)> {
    INDEX.get_or_init(|| {
        parse_index(COMPRESSED).unwrap_or_default()
    })
}

fn parse_index(data: &[u8]) -> Option<HashMap<String, (usize, usize)>> {
    if data.len() < 4 { return None; }
    let num_folders = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    let mut map = HashMap::with_capacity(num_folders);
    let mut pos = 4usize;
    for _ in 0..num_folders {
        if pos + 2 > data.len() { break; }
        let path_len = u16::from_le_bytes(data[pos..pos+2].try_into().ok()?) as usize;
        pos += 2;
        if pos + path_len + 8 > data.len() { break; }
        let path = std::str::from_utf8(&data[pos..pos+path_len]).ok()?.to_string();
        pos += path_len;
        let offset = u32::from_le_bytes(data[pos..pos+4].try_into().ok()?) as usize;
        pos += 4;
        let clen   = u32::from_le_bytes(data[pos..pos+4].try_into().ok()?) as usize;
        pos += 4;
        map.insert(path, (offset, clen));
    }
    Some(map)
}

#[derive(Clone)]
pub struct DirEntry {
    pub name:      String,
    pub href:      String,
    pub size:      String,
    pub date:      String,
    pub is_folder: bool,
}

/// Look up a folder's entries by path, decompressing only that block.
/// Returns None if not in the baked-in data (caller should fall back to HTTP).
pub fn lookup(path: &str) -> Option<Vec<DirEntry>> {
    // Stub check — single null byte means fetch_sizes hasn't been run yet
    if COMPRESSED.len() <= 1 { return None; }

    let key = path.trim_matches('/');
    let index = ensure_index();
    let &(offset, clen) = index.get(key)?;

    let block = COMPRESSED.get(offset..offset+clen)?;
    decompress_block(block)
}

fn decompress_block(block: &[u8]) -> Option<Vec<DirEntry>> {
    let decompressed = zstd::decode_all(block).ok()?;
    parse_entries(&decompressed)
}

fn parse_entries(data: &[u8]) -> Option<Vec<DirEntry>> {
    if data.len() < 2 { return Some(vec![]); }
    let num = u16::from_le_bytes(data[0..2].try_into().ok()?) as usize;
    let mut entries = Vec::with_capacity(num);
    let mut pos = 2usize;
    for _ in 0..num {
        if pos >= data.len() { break; }
        let flags     = data[pos]; pos += 1;
        let is_folder = flags & 1 != 0;
        let name      = read_str(data, &mut pos)?;
        let href      = read_str(data, &mut pos)?;
        let size      = read_str(data, &mut pos)?;
        let date      = read_str(data, &mut pos)?;
        entries.push(DirEntry { name, href, size, date, is_folder });
    }
    Some(entries)
}

fn read_str(data: &[u8], pos: &mut usize) -> Option<String> {
    let len = *data.get(*pos)? as usize;
    *pos += 1;
    if *pos + len > data.len() { return None; }
    let s = std::str::from_utf8(&data[*pos..*pos+len]).ok()?.to_string();
    *pos += len;
    Some(s)
}

/// How many folders are in the baked-in tree.
pub fn folder_count() -> usize {
    if COMPRESSED.len() <= 1 { return 0; }
    ensure_index().len()
}

// ── Search index ──────────────────────────────────────────────────────────────
// Separate file: flat NUL-delimited records, zstd compressed.
// Decompressed once on first search, then cached.

static SEARCH_COMPRESSED: &[u8] = include_bytes!("generated_search.bin");

static SEARCH_INDEX: OnceLock<Vec<SearchEntry>> = OnceLock::new();

pub struct SearchEntry {
    pub name_lc:  String,  // lowercase for matching
    pub name:     String,  // display name
    pub folder:   String,  // parent folder path
    pub size:     String,
}

pub fn search(query: &str) -> impl Iterator<Item = &'static SearchEntry> {
    let q = query.to_lowercase();
    let entries = SEARCH_INDEX.get_or_init(|| {
        if SEARCH_COMPRESSED.len() <= 1 { return vec![]; }
        let Ok(raw) = zstd::decode_all(SEARCH_COMPRESSED) else { return vec![]; };
        parse_search_index(&raw)
    });
    entries.iter().filter(move |e| e.name_lc.contains(&q))
}

fn parse_search_index(data: &[u8]) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let Some(name_lc) = read_nul(data, &mut pos) else { break };
        let Some(folder)  = read_nul(data, &mut pos) else { break };
        let Some(name)    = read_nul(data, &mut pos) else { break };
        let Some(size)    = read_nul(data, &mut pos) else { break };
        entries.push(SearchEntry { name_lc, folder, name, size });
    }
    entries
}

fn read_nul(data: &[u8], pos: &mut usize) -> Option<String> {
    let start = *pos;
    while *pos < data.len() && data[*pos] != 0 { *pos += 1; }
    let s = std::str::from_utf8(&data[start..*pos]).ok()?.to_string();
    if *pos < data.len() { *pos += 1; } // skip NUL
    Some(s)
}
