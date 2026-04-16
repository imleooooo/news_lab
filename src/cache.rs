use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const TTL_SECS: u64 = 3600; // 1 hour

/// One rendered item ready to be replayed from cache.
#[derive(Serialize, Deserialize, Clone)]
pub struct DisplayItem {
    pub title: String,
    pub content: String,
    pub url: String,
    pub color: String,
}

#[derive(Serialize, Deserialize)]
struct Entry<T> {
    created_at: u64,
    payload: T,
}

fn cache_dir() -> PathBuf {
    let base = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    base.join(".cache").join("news_lab")
}

/// FNV-1a 64-bit hash — deterministic across Rust versions and platforms.
fn hash_key(parts: &[&str]) -> String {
    const OFFSET: u64 = 14_695_981_039_346_656_037;
    const PRIME: u64 = 1_099_511_628_211;
    let mut h: u64 = OFFSET;
    for p in parts {
        for b in p.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(PRIME);
        }
        // Separator between parts to avoid "ab","c" == "a","bc"
        h ^= b'|' as u64;
        h = h.wrapping_mul(PRIME);
    }
    format!("{:016x}", h)
}

fn cache_path(parts: &[&str]) -> PathBuf {
    cache_dir().join(format!("{}.json", hash_key(parts)))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Return cached items and remaining TTL (seconds), or `None` on miss / expiry.
pub fn get(parts: &[&str]) -> Option<(Vec<DisplayItem>, u64)> {
    get_with_ttl(parts, TTL_SECS)
}

/// Return cached payload and remaining TTL (seconds), or `None` on miss / expiry.
pub fn get_with_ttl<T: DeserializeOwned>(parts: &[&str], ttl_secs: u64) -> Option<(T, u64)> {
    let path = cache_path(parts);
    let text = std::fs::read_to_string(&path).ok()?;
    let entry: Entry<T> = serde_json::from_str(&text).ok()?;

    let age = now_secs().saturating_sub(entry.created_at);
    if age >= ttl_secs {
        let _ = std::fs::remove_file(&path); // clean up expired file
        return None;
    }

    Some((entry.payload, ttl_secs - age))
}

/// Delete all cached `.json` entries. Returns the number of files removed.
pub fn clear_all() -> usize {
    let dir = cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("json")
            && std::fs::remove_file(entry.path()).is_ok()
        {
            count += 1;
        }
    }
    count
}

/// Write items to cache (silently ignores write errors).
pub fn put(parts: &[&str], items: &[DisplayItem]) {
    put_with_ttl(parts, items, TTL_SECS);
}

/// Write payload to cache with a custom TTL (silently ignores write errors).
pub fn put_with_ttl<T: Serialize + ?Sized>(parts: &[&str], payload: &T, _ttl_secs: u64) {
    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let entry = Entry {
        created_at: now_secs(),
        payload,
    };
    if let Ok(json) = serde_json::to_string_pretty(&entry) {
        let _ = std::fs::write(cache_path(parts), json);
    }
}
