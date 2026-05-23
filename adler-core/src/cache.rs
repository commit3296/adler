//! Cross-run result cache.
//!
//! Re-running a scan minutes apart should not re-hit every site. The cache
//! persists `Found` / `NotFound` verdicts keyed by `(site name, username)`
//! and guarded by:
//!
//! - a **TTL**: entries older than the configured age are ignored (and
//!   pruned on load), and
//! - a **site signature**: a deterministic hash of the site's URL template
//!   and signal list. If the site definition changes, its old cache entries
//!   no longer match and are treated as misses.
//!
//! `Uncertain` outcomes are intentionally never cached — they're transient
//! (rate limits, network blips) and caching them would freeze a temporary
//! failure for the whole TTL window.
//!
//! Access pattern is bulk: [`Cache::load`] once at scan start, in-memory
//! [`Cache::get`] / [`Cache::put`] during the scan, [`Cache::save`] once at
//! the end. There are no concurrent disk writes, so a plain JSON file with
//! an atomic temp-then-rename save is enough — no embedded database needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::check::{CheckOutcome, MatchKind};
use crate::error::Result;
use crate::site::Site;
use crate::username::Username;

const CACHE_VERSION: u32 = 1;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// In-memory cache backed by a JSON file.
#[derive(Debug)]
pub struct Cache {
    path: PathBuf,
    ttl: Duration,
    entries: HashMap<(String, String), Entry>,
    dirty: bool,
}

#[derive(Debug, Clone)]
struct Entry {
    signature: u64,
    stored_at: u64,
    outcome: CheckOutcome,
}

#[derive(Serialize, Deserialize)]
struct StoredEntry {
    site: String,
    username: String,
    signature: u64,
    stored_at: u64,
    outcome: CheckOutcome,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: Vec<StoredEntry>,
}

impl Cache {
    /// Load a cache from `path`, dropping entries older than `ttl`.
    ///
    /// Infallible: a missing, unreadable, or corrupt file yields an empty
    /// cache (a warning is logged). The cache should never be the reason a
    /// scan fails.
    pub fn load(path: PathBuf, ttl: Duration) -> Self {
        let mut cache = Self {
            path,
            ttl,
            entries: HashMap::new(),
            dirty: false,
        };
        let bytes = match std::fs::read(&cache.path) {
            Ok(b) => b,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return cache,
            Err(err) => {
                tracing::warn!(error = %err, path = %cache.path.display(), "cache read failed");
                return cache;
            }
        };
        let parsed: CacheFile = match serde_json::from_slice(&bytes) {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!(error = %err, "cache file corrupt; starting empty");
                return cache;
            }
        };
        if parsed.version != CACHE_VERSION {
            tracing::info!(
                found = parsed.version,
                expected = CACHE_VERSION,
                "cache version mismatch; starting empty"
            );
            return cache;
        }
        let now = now_unix();
        let ttl_secs = ttl.as_secs();
        for stored in parsed.entries {
            if now.saturating_sub(stored.stored_at) > ttl_secs {
                cache.dirty = true; // expired entry pruned; persist the smaller file
                continue;
            }
            cache.entries.insert(
                (stored.site, stored.username),
                Entry {
                    signature: stored.signature,
                    stored_at: stored.stored_at,
                    outcome: stored.outcome,
                },
            );
        }
        cache
    }

    /// Look up a cached outcome for `site` + `username`.
    ///
    /// Returns `None` on a miss, a TTL expiry, or a site-signature mismatch
    /// (the site definition changed since the entry was stored).
    pub fn get(&self, site: &Site, username: &Username) -> Option<CheckOutcome> {
        let key = (site.name.clone(), username.as_str().to_owned());
        let entry = self.entries.get(&key)?;
        if entry.signature != signature(site) {
            return None;
        }
        if now_unix().saturating_sub(entry.stored_at) > self.ttl.as_secs() {
            return None;
        }
        Some(entry.outcome.clone())
    }

    /// Store an outcome. `Uncertain` outcomes are ignored (not cached).
    pub fn put(&mut self, site: &Site, username: &Username, outcome: CheckOutcome) {
        if matches!(outcome.kind, MatchKind::Uncertain) {
            return;
        }
        let key = (site.name.clone(), username.as_str().to_owned());
        self.entries.insert(
            key,
            Entry {
                signature: signature(site),
                stored_at: now_unix(),
                outcome,
            },
        );
        self.dirty = true;
    }

    /// Persist the cache to disk if anything changed since load. Writes
    /// atomically (temp file + rename) and creates parent directories.
    pub fn save(&self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut entries: Vec<StoredEntry> = self
            .entries
            .iter()
            .map(|((site, username), entry)| StoredEntry {
                site: site.clone(),
                username: username.clone(),
                signature: entry.signature,
                stored_at: entry.stored_at,
                outcome: entry.outcome.clone(),
            })
            .collect();
        entries.sort_by(|a, b| {
            a.site
                .cmp(&b.site)
                .then_with(|| a.username.cmp(&b.username))
        });
        let file = CacheFile {
            version: CACHE_VERSION,
            entries,
        };
        let json = serde_json::to_string_pretty(&file)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the cache has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Delete the cache file at `path`. A missing file is not an error.
    pub fn clear(path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Default cache file location: `$XDG_CACHE_HOME/adler/cache.json`,
    /// falling back to `$HOME/.cache/adler/cache.json`, then a relative
    /// path if neither env var is set.
    pub fn default_path() -> PathBuf {
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(xdg).join("adler").join("cache.json");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".cache")
                .join("adler")
                .join("cache.json");
        }
        PathBuf::from("adler-cache.json")
    }
}

/// Deterministic FNV-1a hash of a site's URL template and signal list.
///
/// Must be stable across processes, so we cannot use the std `DefaultHasher`
/// (it's randomly seeded). FNV-1a over the serialized signals + URL is
/// deterministic and collision-resistant enough for cache invalidation.
fn signature(site: &Site) -> u64 {
    let signals = serde_json::to_string(&site.signals).unwrap_or_default();
    let mut hash = FNV_OFFSET;
    for byte in site.url.as_str().bytes().chain(signals.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{Signal, UrlTemplate};

    fn site(name: &str) -> Site {
        Site {
            name: name.into(),
            url: UrlTemplate::new("https://example.com/{username}").unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
        }
    }

    fn outcome(kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: "Example".into(),
            url: "https://example.com/alice".into(),
            kind,
            reason: None,
            elapsed_ms: 5,
            enrichment: std::collections::BTreeMap::new(),
            evidence: Vec::new(),
        }
    }

    fn tmp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "adler-cache-test-{tag}-{}.json",
            std::process::id()
        ));
        p
    }

    fn empty_cache(ttl: Duration) -> Cache {
        Cache {
            path: tmp_path("mem"),
            ttl,
            entries: HashMap::new(),
            dirty: false,
        }
    }

    #[test]
    fn put_then_get_round_trips() {
        let mut cache = empty_cache(Duration::from_secs(3600));
        let s = site("Example");
        let user = Username::new("alice").unwrap();
        cache.put(&s, &user, outcome(MatchKind::Found));
        let got = cache.get(&s, &user).unwrap();
        assert_eq!(got.kind, MatchKind::Found);
    }

    #[test]
    fn uncertain_is_not_cached() {
        let mut cache = empty_cache(Duration::from_secs(3600));
        let s = site("Example");
        let user = Username::new("alice").unwrap();
        cache.put(&s, &user, outcome(MatchKind::Uncertain));
        assert!(cache.get(&s, &user).is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn get_misses_on_different_username() {
        let mut cache = empty_cache(Duration::from_secs(3600));
        let s = site("Example");
        cache.put(
            &s,
            &Username::new("alice").unwrap(),
            outcome(MatchKind::Found),
        );
        assert!(cache.get(&s, &Username::new("bob").unwrap()).is_none());
    }

    #[test]
    fn get_misses_when_signature_changes() {
        let mut cache = empty_cache(Duration::from_secs(3600));
        let s = site("Example");
        let user = Username::new("alice").unwrap();
        cache.put(&s, &user, outcome(MatchKind::Found));

        // Same name, different signals → different signature → miss.
        let mut changed = site("Example");
        changed.signals = vec![Signal::StatusNotFound { codes: vec![404] }];
        assert!(cache.get(&changed, &user).is_none());
    }

    #[test]
    fn get_misses_on_expired_entry() {
        let mut cache = empty_cache(Duration::from_secs(0));
        let s = site("Example");
        let user = Username::new("alice").unwrap();
        // stored_at = now, ttl = 0 → already expired (now - stored_at > 0 is
        // false at the same second, so force an old timestamp).
        cache.entries.insert(
            ("Example".into(), "alice".into()),
            Entry {
                signature: signature(&s),
                stored_at: now_unix().saturating_sub(10),
                outcome: outcome(MatchKind::Found),
            },
        );
        assert!(cache.get(&s, &user).is_none());
    }

    #[test]
    fn save_and_load_round_trip() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let s = site("Example");
        let user = Username::new("alice").unwrap();
        {
            let mut cache = Cache::load(path.clone(), Duration::from_secs(3600));
            cache.put(&s, &user, outcome(MatchKind::Found));
            cache.save().unwrap();
        }
        let reloaded = Cache::load(path.clone(), Duration::from_secs(3600));
        let got = reloaded.get(&s, &user).unwrap();
        assert_eq!(got.kind, MatchKind::Found);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_drops_expired_entries() {
        let path = tmp_path("expiry");
        // Write a cache file by hand with a stored_at two hours in the past.
        let file = CacheFile {
            version: CACHE_VERSION,
            entries: vec![StoredEntry {
                site: "Example".into(),
                username: "alice".into(),
                signature: signature(&site("Example")),
                stored_at: now_unix().saturating_sub(7200),
                outcome: outcome(MatchKind::Found),
            }],
        };
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();
        // TTL of 1 hour → the 2-hour-old entry is pruned.
        let reloaded = Cache::load(path.clone(), Duration::from_secs(3600));
        assert!(reloaded.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_yields_empty_cache() {
        let path = tmp_path("corrupt");
        std::fs::write(&path, b"this is not json {{{").unwrap();
        let cache = Cache::load(path.clone(), Duration::from_secs(3600));
        assert!(cache.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clear_removes_file_and_tolerates_missing() {
        let path = tmp_path("clear");
        std::fs::write(&path, b"{}").unwrap();
        Cache::clear(&path).unwrap();
        assert!(!path.exists());
        // Second clear on a missing file is fine.
        Cache::clear(&path).unwrap();
    }

    #[test]
    fn signature_is_deterministic() {
        let s = site("Example");
        assert_eq!(signature(&s), signature(&s));
    }
}
