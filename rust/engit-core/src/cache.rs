//! Bundle cache integrity and cleanup commands.
//!
//! envoy automatically populates and evicts its local bundle cache
//! (`envoy_core::bundle_cache`) as part of normal stack resolution, but
//! exposes no CLI surface for inspecting or repairing that cache from the
//! outside. This module wraps `envoy_core::bundle_cache::BundleCache` with
//! operator-facing validate/prune commands for engit.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use envoy_core::bundle_cache::{resolve_bundle_cache_dir, BundleCache, BundleMeta};
use globset::Glob;

use crate::error::{EngitError, Result};

/// Resolve the default bundle cache directory using the same precedence
/// envoy itself uses (`ENVOY_BUNDLE_CACHE`, then user config, then the
/// platform default). Returns `None` when the cache is disabled via
/// `ENVOY_DISABLE_BUNDLE_CACHE`.
pub fn default_cache_dir() -> Option<PathBuf> {
    resolve_bundle_cache_dir(true)
}

/// Outcome of validating a single cached bundle entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheEntryStatus {
    /// Recomputed content hash and metadata sidecar both check out.
    Ok,
    /// Recomputed content hash does not match the entry's storage key.
    Corrupt,
    /// The storage directory referenced by the index is missing entirely.
    Missing,
    /// The storage directory exists but has no `.meta.json` sidecar.
    MetaMissing,
}

impl CacheEntryStatus {
    /// Short, fixed-width label used in validate report output.
    pub fn label(self) -> &'static str {
        match self {
            CacheEntryStatus::Ok => "OK",
            CacheEntryStatus::Corrupt => "CORRUPT",
            CacheEntryStatus::Missing => "MISSING",
            CacheEntryStatus::MetaMissing => "META_MISSING",
        }
    }
}

/// Validation result for one cached bundle (bundle ID + version pair).
#[derive(Debug, Clone)]
pub struct CacheEntryReport {
    pub bundle_id: String,
    pub version: String,
    pub status: CacheEntryStatus,
}

/// Full validation report for a cache directory.
#[derive(Debug, Clone, Default)]
pub struct CacheValidationReport {
    pub entries: Vec<CacheEntryReport>,
    /// Content-hash storage directories with no referencing index entry
    /// (left behind by interrupted writes or manual edits).
    pub orphaned_dirs: Vec<PathBuf>,
}

impl CacheValidationReport {
    /// `true` if any entry failed validation or any orphaned directory was found.
    pub fn has_problems(&self) -> bool {
        !self.orphaned_dirs.is_empty()
            || self
                .entries
                .iter()
                .any(|entry| entry.status != CacheEntryStatus::Ok)
    }
}

/// Validate every cached bundle's content hash and metadata sidecar, and
/// detect orphaned content-hash directories under the cache root.
///
/// Note: reading each entry via the cache's public `get` accessor also
/// refreshes that entry's recorded last-access time as a side effect. This
/// is harmless -- envoy's retention policy evicts by creation time and
/// total size, not last access -- but means running validate is not a
/// perfectly read-only operation on disk.
pub fn validate_cache(cache_dir: &Path) -> Result<CacheValidationReport> {
    let cache = open_cache(cache_dir)?;
    let mut report = CacheValidationReport::default();
    let mut referenced_hashes = HashSet::new();

    for (bundle_id, version) in owned_entries(&cache) {
        let status = match cache.get(&bundle_id, &version) {
            Ok(cached) => {
                referenced_hashes.insert(cached.content_hash.clone());
                classify_entry(&cached.path, &cached.content_hash)
            }
            Err(_) => CacheEntryStatus::Missing,
        };
        report.entries.push(CacheEntryReport {
            bundle_id,
            version,
            status,
        });
    }

    report.orphaned_dirs = find_orphaned_dirs(cache.root(), &referenced_hashes);

    Ok(report)
}

/// Selector describing which cached bundles a prune operation targets.
///
/// When `ids`, `pattern`, and `older_than` are all unset (the default),
/// `prune_cache` falls back to running envoy's own retention policy
/// (age/size based compaction) immediately rather than waiting for it to
/// trigger on the next cache write. `remove_orphans` is independent of the
/// other selectors, since orphaned directories have no index entry to match
/// against.
#[derive(Debug, Clone, Default)]
pub struct PruneSelector {
    pub ids: Vec<String>,
    pub pattern: Option<String>,
    pub older_than: Option<Duration>,
    pub remove_orphans: bool,
}

impl PruneSelector {
    fn is_default(&self) -> bool {
        self.ids.is_empty() && self.pattern.is_none() && self.older_than.is_none()
    }
}

/// Result of a prune operation.
#[derive(Debug, Clone, Default)]
pub struct PruneOutcome {
    pub removed: Vec<(String, String)>,
    pub removed_orphans: Vec<PathBuf>,
    /// Set when the default retention policy was applied instead of an
    /// explicit selector; holds the number of entries envoy's own
    /// `BundleCache::compact` reported as evicted.
    pub default_retention_evicted: Option<usize>,
}

/// Prune cached bundles matching `selector`. With `dry_run`, entries are
/// reported but not removed -- except for the default-retention fallback
/// path, which has no dry-run mode of its own (see [`PruneSelector`]).
pub fn prune_cache(
    cache_dir: &Path,
    selector: &PruneSelector,
    dry_run: bool,
) -> Result<PruneOutcome> {
    let mut cache = open_cache(cache_dir)?;
    let mut outcome = PruneOutcome::default();

    if selector.is_default() && !selector.remove_orphans {
        if dry_run {
            return Err(EngitError::Cache(
                "--dry-run requires an explicit selector (--id / --pattern / --older-than \
                 / --remove-orphans); the default retention policy has no preview mode."
                    .to_string(),
            ));
        }
        let evicted = cache
            .compact()
            .map_err(|source| EngitError::Cache(source.to_string()))?;
        outcome.default_retention_evicted = Some(evicted);
        return Ok(outcome);
    }

    if !selector.is_default() {
        let glob = match &selector.pattern {
            Some(pattern) => Some(
                Glob::new(pattern)
                    .map_err(|error| {
                        EngitError::Cache(format!("invalid --pattern glob {pattern:?}: {error}"))
                    })?
                    .compile_matcher(),
            ),
            None => None,
        };

        for (bundle_id, version) in owned_entries(&cache) {
            let id_ok = selector.ids.is_empty() || selector.ids.iter().any(|id| id == &bundle_id);
            let pattern_ok = glob.as_ref().is_none_or(|glob| glob.is_match(&bundle_id));
            let age_ok = match selector.older_than {
                None => true,
                Some(max_age) => entry_age(&cache, &bundle_id, &version)
                    .map(|age| age > max_age)
                    .unwrap_or(false),
            };

            if id_ok && pattern_ok && age_ok {
                let removed = dry_run
                    || cache
                        .remove(&bundle_id, &version)
                        .map_err(|source| EngitError::Cache(source.to_string()))?;
                if removed {
                    outcome.removed.push((bundle_id, version));
                }
            }
        }
    }

    if selector.remove_orphans {
        let referenced: HashSet<String> = owned_entries(&cache)
            .into_iter()
            .filter_map(|(id, version)| cache.get(&id, &version).ok())
            .map(|cached| cached.content_hash)
            .collect();
        for orphan in find_orphaned_dirs(cache.root(), &referenced) {
            let removed = dry_run || fs::remove_dir_all(&orphan).is_ok();
            if removed {
                outcome.removed_orphans.push(orphan);
            }
        }
    }

    Ok(outcome)
}

fn open_cache(cache_dir: &Path) -> Result<BundleCache> {
    BundleCache::new(cache_dir).map_err(|source| EngitError::Cache(source.to_string()))
}

/// Validate `cache_dir` and print a human-readable report. Returns an error
/// if validation itself could not run (e.g. the directory is unusable); a
/// clean exit with problems printed is *not* treated as an error here, so
/// callers that want a failing exit code on problems should check
/// [`CacheValidationReport::has_problems`] via [`validate_cache`] directly.
pub fn run_cache_validate(cache_dir: &Path) -> Result<CacheValidationReport> {
    let report = validate_cache(cache_dir)?;

    println!("Cache: {}", cache_dir.display());
    if report.entries.is_empty() && report.orphaned_dirs.is_empty() {
        println!("  (empty)");
        return Ok(report);
    }

    for entry in &report.entries {
        println!(
            "  [{}] {}@{}",
            entry.status.label(),
            entry.bundle_id,
            entry.version
        );
    }
    for orphan in &report.orphaned_dirs {
        println!("  [ORPHANED] {}", orphan.display());
    }

    if report.has_problems() {
        println!(
            "\n{} problem(s) found. Run `engit cache prune --remove-orphans` or remove \
             affected entries and re-fetch.",
            report
                .entries
                .iter()
                .filter(|entry| entry.status != CacheEntryStatus::Ok)
                .count()
                + report.orphaned_dirs.len()
        );
    } else {
        println!("\nNo problems found.");
    }

    Ok(report)
}

/// Prune `cache_dir` per `selector` and print a human-readable summary.
pub fn run_cache_prune(
    cache_dir: &Path,
    selector: &PruneSelector,
    dry_run: bool,
) -> Result<PruneOutcome> {
    let outcome = prune_cache(cache_dir, selector, dry_run)?;
    let verb = if dry_run { "Would remove" } else { "Removed" };

    if let Some(evicted) = outcome.default_retention_evicted {
        println!("Applied default retention policy: {evicted} entry(ies) evicted.");
        return Ok(outcome);
    }

    for (bundle_id, version) in &outcome.removed {
        println!("{verb} {bundle_id}@{version}");
    }
    for orphan in &outcome.removed_orphans {
        println!("{verb} orphaned directory {}", orphan.display());
    }
    if outcome.removed.is_empty() && outcome.removed_orphans.is_empty() {
        println!("Nothing matched the given selector.");
    }

    Ok(outcome)
}

/// Snapshot `cache.list()` into owned strings so callers can freely mutate
/// or re-borrow `cache` (e.g. via `get`/`remove`) while iterating.
fn owned_entries(cache: &BundleCache) -> Vec<(String, String)> {
    cache
        .list()
        .into_iter()
        .map(|(id, version)| (id.to_string(), version.to_string()))
        .collect()
}

fn classify_entry(storage_dir: &Path, expected_hash: &str) -> CacheEntryStatus {
    if !storage_dir.is_dir() {
        return CacheEntryStatus::Missing;
    }
    if !storage_dir.join(".meta.json").is_file() {
        return CacheEntryStatus::MetaMissing;
    }
    match BundleCache::compute_content_hash(storage_dir) {
        Ok(recomputed) if recomputed == expected_hash => CacheEntryStatus::Ok,
        _ => CacheEntryStatus::Corrupt,
    }
}

const BUNDLE_CONTENT_HASH_HEX_LEN: usize = 64;

fn is_expected_content_hash_dir_name(name: &str) -> bool {
    name.len() == BUNDLE_CONTENT_HASH_HEX_LEN
        && name
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn find_orphaned_dirs(
    cache_root: &Path,
    referenced_hashes: &HashSet<String>,
) -> Result<Vec<PathBuf>> {
    let mut orphans = Vec::new();
    let read_dir = fs::read_dir(cache_root).map_err(|err| {
        EngitError::Cache(format!(
            "failed to read cache directory {}: {}",
            cache_root.display(),
            err
        ))
    })?;

    for entry in read_dir {
        let entry = entry.map_err(|err| {
            EngitError::Cache(format!(
                "failed to read entry in cache directory {}: {}",
                cache_root.display(),
                err
            ))
        })?;
        let file_type = entry.file_type().map_err(|err| {
            EngitError::Cache(format!(
                "failed to read file type for cache entry {}: {}",
                entry.path().display(),
                err
            ))
        })?;
        if !file_type.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_expected_content_hash_dir_name(&name) {
            continue;
        }
        if !referenced_hashes.contains(&name) {
            orphans.push(entry.path());
        }
    }

    Ok(orphans)
}

/// Return how long ago a cached entry was created, or `None` if its
/// metadata sidecar is missing or unreadable.
fn entry_age(cache: &BundleCache, bundle_id: &str, version: &str) -> Option<Duration> {
    let cached = cache.get(bundle_id, version).ok()?;
    let contents = fs::read_to_string(cached.path.join(".meta.json")).ok()?;
    let meta: BundleMeta = serde_json::from_str(&contents).ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    Some(Duration::from_secs(now.saturating_sub(meta.created_at)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn create_sample_bundle(dir: &Path, name: &str, content: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn validate_reports_ok_for_a_healthy_entry() {
        let cache_dir = tempdir().unwrap();
        let source_dir = tempdir().unwrap();
        create_sample_bundle(source_dir.path(), "data.txt", "hello world");

        let mut cache = BundleCache::new(cache_dir.path()).unwrap();
        cache
            .store("test:bundle", "1.0.0", source_dir.path())
            .unwrap();

        let report = validate_cache(cache_dir.path()).unwrap();
        assert!(!report.has_problems());
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].status, CacheEntryStatus::Ok);
    }

    #[test]
    fn validate_detects_corrupted_content() {
        let cache_dir = tempdir().unwrap();
        let source_dir = tempdir().unwrap();
        create_sample_bundle(source_dir.path(), "data.txt", "hello world");

        let mut cache = BundleCache::new(cache_dir.path()).unwrap();
        let cached = cache
            .store("test:bundle", "1.0.0", source_dir.path())
            .unwrap();

        // Tamper with the stored payload after caching so the recomputed
        // content hash no longer matches the storage key.
        fs::write(cached.path.join("data.txt"), "tampered").unwrap();

        let report = validate_cache(cache_dir.path()).unwrap();
        assert!(report.has_problems());
        assert_eq!(report.entries[0].status, CacheEntryStatus::Corrupt);
    }

    #[test]
    fn validate_detects_missing_storage_directory() {
        let cache_dir = tempdir().unwrap();
        let source_dir = tempdir().unwrap();
        create_sample_bundle(source_dir.path(), "data.txt", "hello world");

        let mut cache = BundleCache::new(cache_dir.path()).unwrap();
        let cached = cache
            .store("test:bundle", "1.0.0", source_dir.path())
            .unwrap();
        fs::remove_dir_all(&cached.path).unwrap();

        let report = validate_cache(cache_dir.path()).unwrap();
        assert!(report.has_problems());
        assert_eq!(report.entries[0].status, CacheEntryStatus::Missing);
    }

    #[test]
    fn validate_detects_orphaned_storage_directories() {
        let cache_dir = tempdir().unwrap();
        fs::create_dir_all(cache_dir.path()).unwrap();
        // A content-hash-shaped directory with no index entry pointing to it.
        fs::create_dir_all(cache_dir.path().join("deadbeef")).unwrap();

        let report = validate_cache(cache_dir.path()).unwrap();
        assert!(report.has_problems());
        assert_eq!(report.orphaned_dirs.len(), 1);
    }

    #[test]
    fn prune_by_id_removes_only_matching_bundles() {
        let cache_dir = tempdir().unwrap();
        let source_a = tempdir().unwrap();
        let source_b = tempdir().unwrap();
        create_sample_bundle(source_a.path(), "a.txt", "a");
        create_sample_bundle(source_b.path(), "b.txt", "b");

        let mut cache = BundleCache::new(cache_dir.path()).unwrap();
        cache.store("test:a", "1.0.0", source_a.path()).unwrap();
        cache.store("test:b", "1.0.0", source_b.path()).unwrap();
        drop(cache);

        let selector = PruneSelector {
            ids: vec!["test:a".to_string()],
            ..Default::default()
        };
        let outcome = prune_cache(cache_dir.path(), &selector, false).unwrap();

        assert_eq!(
            outcome.removed,
            vec![("test:a".to_string(), "1.0.0".to_string())]
        );

        let cache = BundleCache::new(cache_dir.path()).unwrap();
        assert!(!cache.contains("test:a", "1.0.0"));
        assert!(cache.contains("test:b", "1.0.0"));
    }

    #[test]
    fn prune_dry_run_reports_without_removing() {
        let cache_dir = tempdir().unwrap();
        let source_dir = tempdir().unwrap();
        create_sample_bundle(source_dir.path(), "data.txt", "hello world");

        let mut cache = BundleCache::new(cache_dir.path()).unwrap();
        cache
            .store("test:bundle", "1.0.0", source_dir.path())
            .unwrap();
        drop(cache);

        let selector = PruneSelector {
            ids: vec!["test:bundle".to_string()],
            ..Default::default()
        };
        let outcome = prune_cache(cache_dir.path(), &selector, true).unwrap();
        assert_eq!(outcome.removed.len(), 1);

        let cache = BundleCache::new(cache_dir.path()).unwrap();
        assert!(cache.contains("test:bundle", "1.0.0"));
    }

    #[test]
    fn prune_by_pattern_matches_glob_against_bundle_id() {
        let cache_dir = tempdir().unwrap();
        let source_a = tempdir().unwrap();
        let source_b = tempdir().unwrap();
        create_sample_bundle(source_a.path(), "a.txt", "a");
        create_sample_bundle(source_b.path(), "b.txt", "b");

        let mut cache = BundleCache::new(cache_dir.path()).unwrap();
        cache.store("test:alpha", "1.0.0", source_a.path()).unwrap();
        cache.store("other:beta", "1.0.0", source_b.path()).unwrap();
        drop(cache);

        let selector = PruneSelector {
            pattern: Some("test:*".to_string()),
            ..Default::default()
        };
        let outcome = prune_cache(cache_dir.path(), &selector, false).unwrap();

        assert_eq!(
            outcome.removed,
            vec![("test:alpha".to_string(), "1.0.0".to_string())]
        );
    }

    #[test]
    fn prune_remove_orphans_deletes_unreferenced_directories() {
        let cache_dir = tempdir().unwrap();
        fs::create_dir_all(cache_dir.path()).unwrap();
        let orphan_dir = cache_dir.path().join("deadbeef");
        fs::create_dir_all(&orphan_dir).unwrap();

        let selector = PruneSelector {
            remove_orphans: true,
            ..Default::default()
        };
        let outcome = prune_cache(cache_dir.path(), &selector, false).unwrap();

        assert_eq!(outcome.removed_orphans, vec![orphan_dir.clone()]);
        assert!(!orphan_dir.exists());
    }

    #[test]
    fn prune_default_selector_applies_retention_policy() {
        let cache_dir = tempdir().unwrap();
        let source_dir = tempdir().unwrap();
        create_sample_bundle(source_dir.path(), "data.txt", "hello world");

        let mut cache = BundleCache::new(cache_dir.path()).unwrap();
        cache
            .store("test:bundle", "1.0.0", source_dir.path())
            .unwrap();
        drop(cache);

        let outcome = prune_cache(cache_dir.path(), &PruneSelector::default(), false).unwrap();
        assert_eq!(outcome.default_retention_evicted, Some(0));
    }

    #[test]
    fn prune_default_selector_rejects_dry_run() {
        let cache_dir = tempdir().unwrap();
        let result = prune_cache(cache_dir.path(), &PruneSelector::default(), true);
        assert!(result.is_err());
    }
}
