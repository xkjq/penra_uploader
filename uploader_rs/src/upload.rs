use reqwest::blocking::Client;
use reqwest::blocking::multipart::{Form, Part};
use std::path::{Path, PathBuf};
use std::fs::File;
use blake3;
use std::collections::{BTreeMap, HashMap, HashSet};
use dicom_object::open_file;
use dicom_object::Tag;
use dicom_pixeldata::PixelDecoder;
use std::time::{SystemTime, UNIX_EPOCH, Duration};
use serde::{Serialize, Deserialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use tracing::Level;

static SCAN_RUNNING: AtomicBool = AtomicBool::new(false);
static SCAN_PENDING: AtomicBool = AtomicBool::new(false);
const DUPLICATE_LOOKUP_TIMEOUT_SECS: u64 = 60;
const DUPLICATE_LOOKUP_BATCH_SIZE: usize = 50;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 200;
const MIN_REQUEST_TIMEOUT_SECS: u64 = 5;
// Short, bounded timeout for startup-time server checks (e.g. token validation)
// so a slow/unreachable server cannot stall application launch.
const STARTUP_TOKEN_CHECK_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, Default)]
struct DuplicateLookupResult {
    is_duplicate: bool,
    urls: Vec<String>,
}

static DUPLICATE_LOOKUP_CACHE: Lazy<Mutex<HashMap<String, DuplicateLookupResult>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyFileInfo {
    pub path: PathBuf,
    pub hash: String,
    pub series_uid: String,
    #[serde(default)]
    pub study_uid: Option<String>,
    #[serde(default)]
    pub load_order: u64,
    #[serde(default)]
    pub duplicate_checked: bool,
    pub is_duplicate: bool,
    pub duplicate_series_urls: Vec<String>,
    pub patient_name: Option<String>,
    pub examination: Option<String>,
    pub patient_id: Option<String>,
    pub study_date: Option<String>,
    pub modality: Option<String>,
    pub series_description: Option<String>,
    pub series_number: Option<String>,
    pub file_size: u64,
    #[serde(default)]
    pub burned_in_annotation_detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub hash: String,
    #[serde(default)]
    pub duplicate_checked: bool,
    pub is_duplicate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesInfo {
    pub series_uid: String,
    pub files: Vec<FileEntry>,
    pub duplicate_series_urls: Vec<String>,
    #[serde(default)]
    pub study_uid: Option<String>,
    #[serde(default)]
    pub loaded_order: u64,
    #[serde(default)]
    pub duplicate_checked_files: usize,
    // common metadata to present in the GUI
    pub patient_name: Option<String>,
    pub examination: Option<String>,
    pub patient_id: Option<String>,
    pub study_date: Option<String>,
    pub modality: Option<String>,
    pub series_description: Option<String>,
    pub series_number: Option<String>,
    pub file_count: usize,
    pub total_bytes: u64,
    #[serde(default)]
    pub burned_in_annotation_detected: bool,
}


pub struct UploadResult {
    pub uploaded: Vec<(String, String)>,
    pub duplicates: Vec<(String, String)>,
    pub failed: Vec<String>,
    pub duplicate_series: HashSet<String>,
}

static READY_FILES: Lazy<Mutex<HashMap<String, ReadyFileInfo>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_READY_ORDER: Lazy<AtomicU64> = Lazy::new(|| AtomicU64::new(1));
// Serializes destructive mutations of READY_FILES (the scan's full clear/rebuild)
// against incremental inserts (the anonymizer's upsert_ready_file). This prevents
// a concurrent startup scan from wiping entries added by an in-flight anonymization
// pass, which would otherwise cause outstanding DICOMs to be missed.
static READY_FILES_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

fn read_dicom_string(obj: &dicom_object::DefaultDicomObject, tag: Tag) -> Option<String> {
    obj.element(tag).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string())
}

fn detect_burned_in_annotation(obj: &dicom_object::DefaultDicomObject) -> bool {
    obj.element(Tag(0x0028, 0x0301))
        .ok()
        .and_then(|e| e.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("YES"))
        .unwrap_or(false)
}

fn is_timeout_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.to_string().to_ascii_lowercase().contains("timed out")
}

fn upload_timeout_user_message(timeout_secs: u64) -> String {
    format!(
        "Upload timed out after {}s. This can happen with large files. Increase 'Upload request timeout (seconds)' in Settings and retry.",
        timeout_secs
    )
}

#[allow(dead_code)]
fn sync_ready_order_counter(map: &HashMap<String, ReadyFileInfo>) {
    let max_order = map.values().map(|rf| rf.load_order).max().unwrap_or(0);
    NEXT_READY_ORDER.store(max_order.saturating_add(1), Ordering::SeqCst);
}

#[allow(dead_code)]
fn normalize_ready_manifest_load_order(map: &mut HashMap<String, ReadyFileInfo>) {
    if map.values().any(|rf| rf.load_order == 0) {
        let mut keys: Vec<String> = map.keys().cloned().collect();
        keys.sort();
        let mut next = map.values().map(|rf| rf.load_order).max().unwrap_or(0).saturating_add(1);
        for key in keys {
            if let Some(rf) = map.get_mut(&key) {
                if rf.load_order == 0 {
                    rf.load_order = next;
                    next = next.saturating_add(1);
                }
            }
        }
    }
    sync_ready_order_counter(map);
}

pub fn study_group_key(series: &SeriesInfo) -> String {
    if let Some(uid) = series.study_uid.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return format!("study_uid:{}", uid);
    }

    let mut parts: Vec<String> = Vec::new();
    for value in [
        series.patient_name.as_deref(),
        series.examination.as_deref(),
        series.study_date.as_deref(),
    ] {
        if let Some(text) = value.map(|s| s.trim()).filter(|s| !s.is_empty()) {
            parts.push(text.to_string());
        }
    }

    if parts.is_empty() {
        format!("study_fallback:{}", series.loaded_order)
    } else {
        format!("study_fallback:{}", parts.join("|"))
    }
}

fn record_uploaded_file(
    fname: &str,
    file_owner: &HashMap<String, (String, String)>,
    series_total_files: &HashMap<String, usize>,
    series_handled_files: &mut HashMap<String, usize>,
    study_total_series: &HashMap<String, usize>,
    study_completed_series: &mut HashMap<String, usize>,
    tx: &Option<std::sync::mpsc::Sender<String>>,
) {
    let Some((series_uid, study_key)) = file_owner.get(fname).cloned() else {
        return;
    };

    let series_total = series_total_files.get(&series_uid).copied().unwrap_or(0);
    if series_total == 0 {
        return;
    }

    let series_done = {
        let handled = series_handled_files.entry(series_uid.clone()).or_insert(0);
        *handled = handled.saturating_add(1);
        *handled >= series_total
    };

    if series_done {
        if let Some(ref s) = tx {
            let _ = s.send(format!("UPLOAD:SERIES_DONE:{}:{}", series_uid, study_key));
        }

        let study_total = study_total_series.get(&study_key).copied().unwrap_or(0);
        if study_total > 0 {
            let study_done = {
                let handled = study_completed_series.entry(study_key.clone()).or_insert(0);
                *handled = handled.saturating_add(1);
                *handled >= study_total
            };

            if study_done {
                if let Some(ref s) = tx {
                    let _ = s.send(format!("UPLOAD:STUDY_DONE:{}", study_key));
                }
            }
        }
    }
}

#[allow(dead_code)]
fn ready_manifest_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cfg = home.join(".uploader");
    let _ = std::fs::create_dir_all(&cfg);
    cfg.join("ready_manifest.json")
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn persist_ready_manifest_locked(_map: &HashMap<String, ReadyFileInfo>) {
    // No-op: we do not store a history of what we have uploaded locally on the machine
}

fn collect_ready_for_dir(anon_dir: &Path) -> Vec<ReadyFileInfo> {
    if let Ok(g) = READY_FILES.lock() {
        return g
            .values()
            .filter(|rf| rf.path.starts_with(anon_dir) && rf.path.exists())
            .cloned()
            .collect();
    }
    Vec::new()
}

#[allow(dead_code)]
pub fn load_ready_manifest() {
    // No-op: we do not store a history of what we have uploaded locally on the machine
}

pub fn evict_missing_ready_files() {
    if let Ok(mut g) = READY_FILES.lock() {
        let before = g.len();
        g.retain(|_, rf| rf.path.exists());
        if g.len() != before {
            persist_ready_manifest_locked(&g);
        }
    }
}

fn build_series_from_ready_files(items: &[ReadyFileInfo]) -> Vec<SeriesInfo> {
    // Use BTreeMap for deterministic series ordering in UI snapshots.
    let mut grouped: BTreeMap<String, Vec<ReadyFileInfo>> = BTreeMap::new();
    for it in items {
        grouped.entry(it.series_uid.clone()).or_default().push(it.clone());
    }

    let mut out: Vec<SeriesInfo> = Vec::new();
    for (series_uid, files) in grouped.into_iter() {
        let mut files = files;
        // Stable file ordering within each series prevents UI rows from jumping.
        files.sort_by(|a, b| a.load_order.cmp(&b.load_order).then_with(|| a.path.cmp(&b.path)));

        let mut entries: Vec<FileEntry> = Vec::new();
        let mut urls: HashSet<String> = HashSet::new();
        let mut total_bytes: u64 = 0;
        let mut duplicate_checked_files: usize = 0;
        let mut loaded_order = u64::MAX;
        let mut patient_name = None;
        let mut examination = None;
        let mut patient_id = None;
        let mut study_date = None;
        let mut modality = None;
        let mut series_description = None;
        let mut series_number = None;
        let mut study_uid = None;
        let mut burned_in_annotation_detected = false;

        for rf in &files {
            entries.push(FileEntry {
                path: rf.path.clone(),
                hash: rf.hash.clone(),
                duplicate_checked: rf.duplicate_checked,
                is_duplicate: rf.is_duplicate,
            });
            for u in &rf.duplicate_series_urls {
                urls.insert(u.clone());
            }
            total_bytes = total_bytes.saturating_add(rf.file_size);
            if rf.duplicate_checked {
                duplicate_checked_files = duplicate_checked_files.saturating_add(1);
            }
            loaded_order = loaded_order.min(rf.load_order);
            if patient_name.is_none() {
                patient_name = rf.patient_name.clone();
            }
            if examination.is_none() {
                examination = rf.examination.clone();
            }
            if patient_id.is_none() {
                patient_id = rf.patient_id.clone();
            }
            if study_date.is_none() {
                study_date = rf.study_date.clone();
            }
            if modality.is_none() {
                modality = rf.modality.clone();
            }
            if series_description.is_none() {
                series_description = rf.series_description.clone();
            }
            if series_number.is_none() {
                series_number = rf.series_number.clone();
            }
            if study_uid.is_none() {
                study_uid = rf.study_uid.clone();
            }
            burned_in_annotation_detected |= rf.burned_in_annotation_detected;
        }

        out.push(SeriesInfo {
            series_uid,
            files: entries,
            duplicate_series_urls: urls.into_iter().collect(),
            study_uid,
            loaded_order: if loaded_order == u64::MAX { 0 } else { loaded_order },
            duplicate_checked_files,
            patient_name,
            examination,
            patient_id,
            study_date,
            modality,
            series_description,
            series_number,
            file_count: files.len(),
            total_bytes,
            burned_in_annotation_detected,
        });
    }
    out
}

pub fn snapshot_ready_series(anon_dir: &Path) -> Vec<SeriesInfo> {
    let items = collect_ready_for_dir(anon_dir);
    build_series_from_ready_files(&items)
}

pub fn upsert_ready_file(path: &Path, known_hash: Option<String>) -> Result<(), String> {
    upsert_ready_file_internal(path, known_hash, true)
}

fn upsert_ready_file_internal(
    path: &Path,
    known_hash: Option<String>,
    persist_manifest: bool,
) -> Result<(), String> {
    let obj = open_file(path).map_err(|e| format!("open_file {}: {}", path.display(), e))?;
    // Empty hash means no PixelData was available; file remains uploadable
    // but is skipped by duplicate precheck (which only accepts pixel hashes).
    let hash = known_hash
        .or_else(|| calculate_pixel_hash_from_obj(&obj))
        .unwrap_or_default();

    let series_uid = obj
        .element(Tag(0x0020, 0x000E))
        .ok()
        .and_then(|e| e.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "NO_SERIES".to_string());
    let study_uid = read_dicom_string(&obj, Tag(0x0020, 0x000D));
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let load_order = NEXT_READY_ORDER.fetch_add(1, Ordering::SeqCst);

    let info = ReadyFileInfo {
        path: path.to_path_buf(),
        hash,
        series_uid,
        study_uid,
        load_order,
        duplicate_checked: false,
        is_duplicate: false,
        duplicate_series_urls: Vec::new(),
        patient_name: read_dicom_string(&obj, Tag(0x0010, 0x0010)),
        examination: read_dicom_string(&obj, Tag(0x0008, 0x1030)),
        patient_id: read_dicom_string(&obj, Tag(0x0010, 0x0020)),
        study_date: read_dicom_string(&obj, Tag(0x0008, 0x0020)),
        modality: read_dicom_string(&obj, Tag(0x0008, 0x0060)),
        series_description: read_dicom_string(&obj, Tag(0x0008, 0x103E)),
        series_number: read_dicom_string(&obj, Tag(0x0020, 0x0011)),
        file_size,
        burned_in_annotation_detected: detect_burned_in_annotation(&obj),
    };

    if let Ok(_guard) = READY_FILES_LOCK.lock() {
        if let Ok(mut g) = READY_FILES.lock() {
            g.insert(path_key(path), info);
            if persist_manifest {
                persist_ready_manifest_locked(&g);
            }
        }
    }
    Ok(())
}

pub fn remove_ready_file(path: &Path) {
    if let Ok(_guard) = READY_FILES_LOCK.lock() {
        if let Ok(mut g) = READY_FILES.lock() {
            if g.remove(&path_key(path)).is_some() {
                persist_ready_manifest_locked(&g);
            }
        }
    }
}

pub fn clear_duplicate_lookup_cache() {
    if let Ok(mut g) = DUPLICATE_LOOKUP_CACHE.lock() {
        g.clear();
    }
}

fn refresh_duplicate_lookup_cache(
    hashes: &[String],
    force_refresh: bool,
    tx: Option<std::sync::mpsc::Sender<String>>,
) -> Result<bool, String> {
    if hashes.is_empty() {
        return Ok(true);
    }

    let query_hashes: Vec<String> = if force_refresh {
        hashes.to_vec()
    } else if let Ok(cache) = DUPLICATE_LOOKUP_CACHE.lock() {
        hashes
            .iter()
            .filter(|h| !cache.contains_key((*h).as_str()))
            .cloned()
            .collect()
    } else {
        hashes.to_vec()
    };

    if query_hashes.is_empty() {
        return Ok(true);
    }

    // Avoid oversize payloads for large scans by splitting hash checks into batches.
    let mut dedup_seen: HashSet<String> = HashSet::new();
    let mut query_hashes_unique: Vec<String> = Vec::new();
    for h in query_hashes {
        if dedup_seen.insert(h.clone()) {
            query_hashes_unique.push(h);
        }
    }

    let client = make_client(load_api_token().as_deref())?;
    let base = base_site_url();
    let hash_check_url = format!("{}{}", base, "/api/atlas/check_image_hashes/");
    log_rpc(&format!(
        "POST {} with {} hashes (batch size {})",
        hash_check_url,
        query_hashes_unique.len(),
        DUPLICATE_LOOKUP_BATCH_SIZE
    ));

    let total = query_hashes_unique.len();
    let total_batches = (total + DUPLICATE_LOOKUP_BATCH_SIZE - 1) / DUPLICATE_LOOKUP_BATCH_SIZE;
    let mut looked_up = 0usize;
    let mut all_batches_succeeded = true;

    for (batch_i, batch) in query_hashes_unique
        .chunks(DUPLICATE_LOOKUP_BATCH_SIZE)
        .enumerate()
    {
        let batch_vec: Vec<String> = batch.to_vec();
        log_rpc_debug(&format!(
            "Duplicate lookup batch {}/{} ({} hashes)",
            batch_i + 1,
            total_batches,
            batch_vec.len()
        ));

        let r = match client
            .post(&hash_check_url)
            .timeout(Duration::from_secs(DUPLICATE_LOOKUP_TIMEOUT_SECS))
            .json(&batch_vec)
            .send()
        {
            Ok(r) => r,
            Err(e) => {
                all_batches_succeeded = false;
                log_rpc_warn(&format!(
                    "Duplicate lookup batch {}/{} failed: {}; continuing with remaining batches",
                    batch_i + 1,
                    total_batches,
                    e
                ));
                looked_up = looked_up.saturating_add(batch_vec.len());
                if let Some(ref s) = tx {
                    let prog = (looked_up as f32 / total as f32).clamp(0.0, 1.0);
                    let _ = s.send(format!("PROC:PROG:{}", prog));
                }
                continue;
            }
        };

        let status = r.status();
        let body = match r.text() {
            Ok(b) => b,
            Err(_) => {
                all_batches_succeeded = false;
                log_rpc_warn(&format!(
                    "Duplicate lookup batch {}/{} body read failed; continuing",
                    batch_i + 1,
                    total_batches
                ));
                looked_up = looked_up.saturating_add(batch_vec.len());
                if let Some(ref s) = tx {
                    let prog = (looked_up as f32 / total as f32).clamp(0.0, 1.0);
                    let _ = s.send(format!("PROC:PROG:{}", prog));
                }
                continue;
            }
        };

        if let Some(pf) = save_body_to_file(&body) {
            log_rpc_debug(&format!("Response {}: {} BODY_FILE:{}", hash_check_url, status, pf.display()));
        }

        if !status.is_success() {
            all_batches_succeeded = false;
            log_rpc_warn(&format!(
                "Duplicate lookup batch {}/{} failed HTTP {}; continuing",
                batch_i + 1,
                total_batches,
                status
            ));
            looked_up = looked_up.saturating_add(batch_vec.len());
            if let Some(ref s) = tx {
                let prog = (looked_up as f32 / total as f32).clamp(0.0, 1.0);
                let _ = s.send(format!("PROC:PROG:{}", prog));
            }
            continue;
        }

        let map = match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => v,
            Err(_) => {
                all_batches_succeeded = false;
                log_rpc_warn(&format!(
                    "Duplicate lookup batch {}/{} JSON parse failed; continuing",
                    batch_i + 1,
                    total_batches
                ));
                looked_up = looked_up.saturating_add(batch_vec.len());
                if let Some(ref s) = tx {
                    let prog = (looked_up as f32 / total as f32).clamp(0.0, 1.0);
                    let _ = s.send(format!("PROC:PROG:{}", prog));
                }
                continue;
            }
        };

        let obj = match map.as_object() {
            Some(o) => o,
            None => {
                all_batches_succeeded = false;
                log_rpc_warn(&format!(
                    "Duplicate lookup batch {}/{} response shape invalid; continuing",
                    batch_i + 1,
                    total_batches
                ));
                looked_up = looked_up.saturating_add(batch_vec.len());
                if let Some(ref s) = tx {
                    let prog = (looked_up as f32 / total as f32).clamp(0.0, 1.0);
                    let _ = s.send(format!("PROC:PROG:{}", prog));
                }
                continue;
            }
        };

        if let Ok(mut cache) = DUPLICATE_LOOKUP_CACHE.lock() {
            for h in &batch_vec {
                let mut res = DuplicateLookupResult::default();
                if let Some(info) = obj.get(h) {
                    if let Some(id) = info.get("id") {
                        if json_value_truthy(id) {
                            res.is_duplicate = true;
                            if let Some(urls) = info.get("url").and_then(|v| v.as_str()) {
                                let full = if urls.starts_with("http") {
                                    urls.to_string()
                                } else if urls.starts_with('/') {
                                    format!("{}{}", base.trim_end_matches('/'), urls)
                                } else {
                                    format!("{}/{}", base.trim_end_matches('/'), urls)
                                };
                                res.urls.push(full);
                            }
                        }
                    }
                }
                cache.insert(h.clone(), res);
            }
        }

        looked_up = looked_up.saturating_add(batch_vec.len());
        if let Some(ref s) = tx {
            let prog = (looked_up as f32 / total as f32).clamp(0.0, 1.0);
            let _ = s.send(format!("PROC:PROG:{}", prog));
        }
    }

    Ok(all_batches_succeeded)
}

fn ensure_ready_cache(anon_dir: &Path, tx: Option<std::sync::mpsc::Sender<String>>) -> Result<(), String> {
    // Hold READY_FILES_LOCK only around the destructive clear so an in-flight
    // anonymization upsert cannot be wiped out. The per-file rebuild below uses
    // upsert_ready_file_internal which acquires the lock itself per insert, so
    // we must not hold it across the loop (std::sync::Mutex is not reentrant).
    {
        let _guard = READY_FILES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut g) = READY_FILES.lock() {
            g.clear();
        }
    }

    let files = collect_files_recursive(anon_dir);
    let mut dcm_like: Vec<PathBuf> = Vec::new();
    for p in files {
        if p.is_file() {
            dcm_like.push(p);
        }
    }
    if dcm_like.is_empty() {
        return Ok(());
    }

    if let Some(ref s) = tx {
        let _ = s.send("PROC:STEP:Scanning files".to_string());
        let _ = s.send(format!("PROC:PROG:{}", 0.0));
    }

    let total = dcm_like.len();
    for (i, p) in dcm_like.iter().enumerate() {
        if upsert_ready_file_internal(p, None, false).is_err() {
            log_rpc_debug(&format!("Scan skipped non-DICOM file: {}", p.display()));
        }
        if let Some(ref s) = tx {
            let report_interval = std::cmp::max(1, total / 20);
            if (i % report_interval == 0) || (i + 1 == total) {
                let prog = ((i + 1) as f32 / total as f32).clamp(0.0, 1.0);
                let _ = s.send(format!("PROC:PROG:{}", prog));
            }
        }
    }
    Ok(())
}

pub fn refresh_duplicates_for_ready(
    anon_dir: &Path,
    tx: Option<std::sync::mpsc::Sender<String>>,
) -> Result<Vec<SeriesInfo>, String> {
    refresh_duplicates_for_ready_mode(anon_dir, tx, false)
}

pub fn refresh_duplicates_for_ready_force(
    anon_dir: &Path,
    tx: Option<std::sync::mpsc::Sender<String>>,
) -> Result<Vec<SeriesInfo>, String> {
    refresh_duplicates_for_ready_mode(anon_dir, tx, true)
}

fn refresh_duplicates_for_ready_mode(
    anon_dir: &Path,
    tx: Option<std::sync::mpsc::Sender<String>>,
    force_refresh: bool,
) -> Result<Vec<SeriesInfo>, String> {
    evict_missing_ready_files();
    let items = collect_ready_for_dir(anon_dir);
    if items.is_empty() {
        return Ok(Vec::new());
    }

    if let Some(ref s) = tx {
        let _ = s.send("PROC:STEP:Refreshing duplicate status".to_string());
        let _ = s.send(format!("PROC:PROG:{}", 0.0));
    }

    let hashes: Vec<String> = items.iter().map(|it| it.hash.clone()).filter(|h| !h.is_empty()).collect();
    let duplicate_lookup_succeeded = refresh_duplicate_lookup_cache(&hashes, force_refresh, tx.clone())?;

    if let Ok(mut g) = READY_FILES.lock() {
        let cache = DUPLICATE_LOOKUP_CACHE
            .lock()
            .ok()
            .map(|c| c.clone())
            .unwrap_or_default();
        for rf in g.values_mut() {
            if !rf.path.starts_with(anon_dir) {
                continue;
            }
            if rf.hash.is_empty() {
                // Cannot precheck duplicates without PixelData hash; treat as
                // check-complete for UI state so it does not stay "awaiting" forever.
                rf.duplicate_checked = true;
                continue;
            }
            if let Some(res) = cache.get(&rf.hash) {
                rf.is_duplicate = res.is_duplicate;
                rf.duplicate_series_urls = res.urls.clone();
                rf.duplicate_checked = true;
            } else if force_refresh {
                // Force refresh explicitly invalidates stale checked state for
                // hashes that have not returned from the lookup yet.
                rf.duplicate_checked = false;
            }
        }
        persist_ready_manifest_locked(&g);
    }

    if !duplicate_lookup_succeeded {
        if let Some(ref s) = tx {
            let _ = s.send("Duplicate refresh unavailable; keeping previous duplicate flags".to_string());
        }
    }

    if let Some(ref s) = tx {
        let _ = s.send(format!("PROC:PROG:{}", 1.0));
    }
    Ok(snapshot_ready_series(anon_dir))
}

pub fn base_site_url() -> String {
    // priority: env var -> saved config -> default
    if let Ok(env) = std::env::var("UPLOADER_BASE_URL") {
        if !env.is_empty() {
            return env;
        }
    }
    if let Some(cfg) = load_base_url() {
        if !cfg.is_empty() {
            return cfg;
        }
    }
    "https://www.penracourses.org.uk".to_string()
}

/// Collect files recursively under `dir` and return a Vec of PathBuf.
/// This is a simple stack-based traversal that avoids external deps.
pub fn collect_files_recursive(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    stack.push(dir.to_path_buf());
    while let Some(cur) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&cur) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    files.push(p);
                } else if p.is_dir() {
                    stack.push(p);
                }
            }
        }
    }
    files
}

fn config_file_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cfg = home.join(".uploader");
    let _ = std::fs::create_dir_all(&cfg);
    cfg.join("config.json")
}

pub fn load_base_url() -> Option<String> {
    let p = config_file_path();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(u) = v.get("base_url").and_then(|x| x.as_str()) {
                    return Some(u.to_string());
                }
            }
        }
    }
    None
}

pub fn save_base_url(url: &str) -> bool {
    let p = config_file_path();
    // merge with existing config if present
    let mut map = serde_json::Map::new();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(o) = v.as_object() {
                    for (k, val) in o {
                        map.insert(k.clone(), val.clone());
                    }
                }
            }
        }
    }
    map.insert("base_url".to_string(), serde_json::Value::String(url.to_string()));
    std::fs::write(p, serde_json::Value::Object(map).to_string()).is_ok()
}

pub fn load_skip_ssl() -> bool {
    let p = config_file_path();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                return v.get("skip_ssl").and_then(|b| b.as_bool()).unwrap_or(true);
            }
        }
    }
    true
}

pub fn save_skip_ssl(skip: bool) -> bool {
    let p = config_file_path();
    let mut map = serde_json::Map::new();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(o) = v.as_object() {
                    for (k, val) in o {
                        map.insert(k.clone(), val.clone());
                    }
                }
            }
        }
    }
    map.insert("skip_ssl".to_string(), serde_json::Value::Bool(skip));
    std::fs::write(p, serde_json::Value::Object(map).to_string()).is_ok()
}

pub fn load_theme() -> Option<String> {
    let p = config_file_path();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(t) = v.get("theme").and_then(|x| x.as_str()) {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

pub fn save_theme(theme: &str) -> bool {
    let p = config_file_path();
    let mut map = serde_json::Map::new();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(o) = v.as_object() {
                    for (k, val) in o {
                        map.insert(k.clone(), val.clone());
                    }
                }
            }
        }
    }
    map.insert("theme".to_string(), serde_json::Value::String(theme.to_string()));
    std::fs::write(p, serde_json::Value::Object(map).to_string()).is_ok()
}

pub fn load_parallelism() -> Option<usize> {
    let p = config_file_path();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(n) = v.get("parallelism").and_then(|x| x.as_u64()) {
                    return Some(n as usize);
                }
            }
        }
    }
    None
}

pub fn save_parallelism(n: usize) -> bool {
    let p = config_file_path();
    let mut map = serde_json::Map::new();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(o) = v.as_object() {
                    for (k, val) in o {
                        map.insert(k.clone(), val.clone());
                    }
                }
            }
        }
    }
    map.insert("parallelism".to_string(), serde_json::Value::Number(serde_json::Number::from(n as u64)));
    std::fs::write(p, serde_json::Value::Object(map).to_string()).is_ok()
}

pub fn load_request_timeout_secs() -> u64 {
    let p = config_file_path();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(n) = v.get("request_timeout_secs").and_then(|x| x.as_u64()) {
                    return n.max(MIN_REQUEST_TIMEOUT_SECS);
                }
            }
        }
    }
    DEFAULT_REQUEST_TIMEOUT_SECS
}

pub fn save_request_timeout_secs(timeout_secs: u64) -> bool {
    let p = config_file_path();
    let mut map = serde_json::Map::new();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(o) = v.as_object() {
                    for (k, val) in o {
                        map.insert(k.clone(), val.clone());
                    }
                }
            }
        }
    }
    map.insert(
        "request_timeout_secs".to_string(),
        serde_json::Value::Number(serde_json::Number::from(timeout_secs.max(MIN_REQUEST_TIMEOUT_SECS))),
    );
    std::fs::write(p, serde_json::Value::Object(map).to_string()).is_ok()
}

fn token_file_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cfg = home.join(".uploader");
    let _ = std::fs::create_dir_all(&cfg);
    cfg.join("api_token")
}

pub fn load_api_token() -> Option<String> {
    let p = token_file_path();
    if p.exists() {
        std::fs::read_to_string(p).ok()
    } else {
        None
    }
}

pub fn save_api_token(token: &str) -> bool {
    let p = token_file_path();
    std::fs::write(p, token).is_ok()
}

pub fn clear_api_token() -> bool {
    let p = token_file_path();
    if p.exists() { std::fs::remove_file(p).is_ok() } else { true }
}

pub fn log_file_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cfg = home.join(".uploader");
    let _ = std::fs::create_dir_all(&cfg);
    cfg.join("request_log.txt")
}

pub fn log_rpc(msg: &str) {
    // Emit structured log event via tracing; tracing subscriber in the GUI
    // will persist this into the app log file. Keep the plain string for
    // backwards compatibility with callers.
    tracing::info!(message = %msg);
}

pub fn load_log_level() -> Option<String> {
    let p = config_file_path();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(l) = v.get("log_level").and_then(|x| x.as_str()) {
                    return Some(l.to_string());
                }
            }
        }
    }
    None
}

pub fn save_log_level(level: &str) -> bool {
    let p = config_file_path();
    let mut map = serde_json::Map::new();
    if p.exists() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(o) = v.as_object() {
                    for (k, val) in o {
                        map.insert(k.clone(), val.clone());
                    }
                }
            }
        }
    }
    map.insert("log_level".to_string(), serde_json::Value::String(level.to_string()));
    std::fs::write(p, serde_json::Value::Object(map).to_string()).is_ok()
}

/// Emit an RPC-style log at a dynamic `Level`.
pub fn log_rpc_level(level: Level, msg: &str) {
    match level {
        Level::TRACE => tracing::trace!(message = %msg),
        Level::DEBUG => tracing::debug!(message = %msg),
        Level::INFO => tracing::info!(message = %msg),
        Level::WARN => tracing::warn!(message = %msg),
        Level::ERROR => tracing::error!(message = %msg),
    }
}

pub fn log_rpc_debug(msg: &str) { log_rpc_level(Level::DEBUG, msg); }
pub fn log_rpc_warn(msg: &str)  { log_rpc_level(Level::WARN, msg); }
pub fn log_rpc_error(msg: &str) { log_rpc_level(Level::ERROR, msg); }

fn bodies_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join(".uploader").join("bodies");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn save_body_to_file(body: &str) -> Option<PathBuf> {
    let dir = bodies_dir();
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let p = dir.join(format!("resp_{}.txt", ts));
    match std::fs::write(&p, body) {
        Ok(_) => Some(p),
        Err(_) => None,
    }
}

fn json_value_truthy(v: &serde_json::Value) -> bool {
    if v.is_boolean() {
        v.as_bool().unwrap_or(false)
    } else if v.is_number() {
        v.as_i64().map(|n| n != 0).unwrap_or(false)
    } else if v.is_string() {
        !v.as_str().unwrap_or("").is_empty()
    } else {
        false
    }
}

/// Cheap local check: does a saved API token file exist? No network involved.
pub fn has_api_token() -> bool {
    token_file_path().exists()
}

/// Fast, bounded token check for startup. Uses a short timeout so a slow or
/// unreachable server cannot block application launch for the full configured
/// request timeout. Callers that need a longer window should use
/// `token_username()` instead.
pub fn token_username_quick() -> Option<String> {
    token_username_with_timeout(Some(STARTUP_TOKEN_CHECK_TIMEOUT_SECS))
}

pub fn token_username() -> Option<String> {
    token_username_with_timeout(None)
}

fn token_username_with_timeout(timeout_secs: Option<u64>) -> Option<String> {
    if let Some(t) = load_api_token() {
        let base = base_site_url();
        let token_check = format!("{}{}", base, "/api/atlas/token_check");
        let client = match make_client_with_timeout(Some(&t), timeout_secs) {
            Ok(c) => c,
            Err(e) => {
                log_rpc_error(&format!("make_client failed: {}", e));
                return None;
            }
        };
        // Try header auth first; if it errors (network/proxy/ssl issues), fall back to POSTing JSON like the Python client.
        let header_resp = client.post(&token_check).header("Authorization", format!("Bearer {}", t)).send();
        let resp = match header_resp {
            Ok(r) => Ok(r),
            Err(e) => {
                log_rpc_debug(&format!("Header token_check failed (will try JSON body): {}", e));
                client.post(&token_check).json(&serde_json::json!({"token": t})).send()
            }
        };

        if let Ok(r) = resp {
            let status = r.status();
            if let Ok(body) = r.text() {
                if let Some(pf) = save_body_to_file(&body) {
                    log_rpc_debug(&format!("Response {}: {} BODY_FILE:{}", token_check, status, pf.display()));
                } else {
                    log_rpc_warn(&format!("Response {}: {} body: (failed to save body)", token_check, status));
                }
                if status.is_success() {
                    if let Ok(j) = serde_json::from_str::<serde_json::Value>(&body) {
                        if j.get("valid").and_then(|b| b.as_bool()).unwrap_or(false) {
                            return j.get("username").and_then(|s| s.as_str()).map(|s| s.to_string()).or(Some("API token".to_string()));
                        }
                    }
                }
            } else {
                log_rpc_warn(&format!("Response {}: {} (failed to read body)", token_check, status));
            }
        }
    }
    None
}

pub fn make_client(token: Option<&str>) -> Result<Client, String> {
    make_client_with_timeout(token, None)
}

fn make_client_with_timeout(token: Option<&str>, timeout_secs: Option<u64>) -> Result<Client, String> {
    let mut b = reqwest::blocking::Client::builder();
    let timeout = timeout_secs.unwrap_or_else(load_request_timeout_secs).max(MIN_REQUEST_TIMEOUT_SECS);
    b = b.timeout(Duration::from_secs(timeout));
    // priority: env var -> saved config -> default
    let skip = if let Ok(env) = std::env::var("UPLOADER_SKIP_SSL_VERIFY") {
        if !env.is_empty() { env.to_lowercase() == "1" } else { load_skip_ssl() }
    } else {
        load_skip_ssl()
    };
    if skip {
        b = b.danger_accept_invalid_certs(true);
    }
    // set default Authorization header when token provided
    if let Some(t) = token {
        let mut headers = reqwest::header::HeaderMap::new();
        let val = format!("Bearer {}", t);
        headers.insert(reqwest::header::AUTHORIZATION, reqwest::header::HeaderValue::from_str(&val).map_err(|e| format!("invalid token header: {}", e))?);
        b = b.default_headers(headers);
    }

    let client = b.build().map_err(|e| format!("client build failed: {}", e))?;
    Ok(client)
}

pub fn calculate_pixel_hash(path: &Path) -> Option<String> {
    // Duplicate detection must hash pixel data only.
    // Full-file hashing is intentionally disallowed because metadata changes
    // would produce different hashes for the same image content.
    if let Ok(obj) = open_file(path) {
        return calculate_pixel_hash_from_obj(&obj);
    }
    None
}

fn calculate_pixel_hash_from_obj(obj: &dicom_object::DefaultDicomObject) -> Option<String> {
    // Preferred: hash decoded pixel bytes.
    if let Ok(pixel_data) = obj.decode_pixel_data() {
        let bytes = pixel_data.data();
        return Some(blake3::hash(bytes).to_hex().to_string());
    }

    // If JPEG-LS Lossless (1.2.840.10008.1.2.4.80), decode fragments using CharLS
    let ts = obj
        .element(Tag(0x0002, 0x0010))
        .ok()
        .and_then(|e| e.to_str().ok())
        .map(|s| s.trim_end_matches(|c: char| c.is_whitespace() || c == '\0').to_string())
        .unwrap_or_else(|| {
            obj.meta()
                .transfer_syntax()
                .trim_end_matches(|c: char| c.is_whitespace() || c == '\0')
                .to_string()
        });

    if ts == "1.2.840.10008.1.2.4.80" {
        if let Ok(elem) = obj.element(Tag(0x7FE0, 0x0010)) {
            if let Some(fragments) = elem.value().fragments() {
                let mut charls = charls::CharLS::default();
                let mut hasher = blake3::Hasher::new();
                for frag in fragments {
                    if !frag.is_empty() {
                        if let Ok(decoded) = charls.decode(frag) {
                            hasher.update(&decoded);
                        } else {
                            return None;
                        }
                    }
                }
                return Some(hasher.finalize().to_hex().to_string());
            }
        }
    }

    // Prefer the PixelData element bytes when present for uncompressed data.
    if let Ok(elem) = obj.element(Tag(0x7FE0, 0x0010)) {
        if let Ok(bytes) = elem.to_bytes() {
            return Some(blake3::hash(&bytes).to_hex().to_string());
        }
    }
    None
}

pub fn upload_anon_dir(anon_dir: &Path, case_id: Option<&str>, tx: Option<std::sync::mpsc::Sender<String>>) -> Result<UploadResult, String> {
    ensure_ready_cache(anon_dir, tx.clone())?;
    let series = refresh_duplicates_for_ready(anon_dir, tx.clone())?;

    // Build file lists from series info returned by the scanner. Files already
    // marked as duplicates by the scanner's precheck will be skipped here.
    let mut files_to_upload: Vec<(PathBuf, String)> = Vec::new();
    let mut pre_duplicate_series: HashSet<String> = HashSet::new();
    let mut file_owner: HashMap<String, (String, String)> = HashMap::new();
    let mut series_total_files: HashMap<String, usize> = HashMap::new();
    let mut study_total_series: HashMap<String, usize> = HashMap::new();

    for si in &series {
        let study_key = study_group_key(si);
        series_total_files.insert(si.series_uid.clone(), si.files.len());
        *study_total_series.entry(study_key.clone()).or_insert(0) += 1;
        // collect duplicate series URLs for any series where at least one file is duplicate
        let mut series_has_dup = false;
        for f in &si.files {
            if let Some(fname) = f.path.file_name().and_then(|s| s.to_str()) {
                file_owner.insert(fname.to_string(), (si.series_uid.clone(), study_key.clone()));
            }
            if f.is_duplicate {
                series_has_dup = true;
            } else {
                files_to_upload.push((f.path.clone(), f.path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()));
            }
        }
        if series_has_dup {
            for url in &si.duplicate_series_urls {
                pre_duplicate_series.insert(url.clone());
            }
        }
    }

    // At this point `series` already contains duplication information from cache + precheck.
    // Use the precomputed lists we assembled above (`files_to_upload`, `pre_duplicate_series`).
    let chunk_size = 10usize;
    let mut uploaded = Vec::new();
    let mut duplicates = Vec::new();
    let mut failed = Vec::new();
    let mut duplicate_series = pre_duplicate_series.clone();
    let mut saw_timeout = false;
    let mut series_handled_files: HashMap<String, usize> = HashMap::new();
    let mut study_completed_series: HashMap<String, usize> = HashMap::new();
    // Prepare HTTP client and base URL for upload requests
    let client = make_client(load_api_token().as_deref())?;
    let base = base_site_url();

    let _total_chunks = (files_to_upload.len() + chunk_size - 1) / chunk_size;
    let total_files = files_to_upload.len();
    let mut files_processed = 0usize;

    // notify UI that upload is starting
    if let Some(ref s) = tx {
        let _ = s.send("PROC:STEP:Uploading files".to_string());
        if total_files > 0 { let _ = s.send(format!("PROC:PROG:{}", 0.0)); }
    }

    for (_i, chunk) in files_to_upload.chunks(chunk_size).enumerate() {
        let chunk_pairs: Vec<(PathBuf, String)> = chunk.iter().map(|(p, f)| (p.clone(), f.clone())).collect();

        let endpoint = if let Some(cid) = case_id { format!("{}/api/atlas/upload_dicom_case/{}", base, cid) } else { format!("{}/api/atlas/upload_dicom", base) };

        let mut success = false;
        for _attempt in 0..3 {
            // Rebuild the multipart form for each attempt (Form is not Clone)
            let mut form = Form::new();
            for (p, fname) in &chunk_pairs {
                if let Ok(f) = File::open(p) {
                    let part = Part::reader(f).file_name(fname.clone());
                    form = form.part("files", part);
                }
            }

            log_rpc(&format!("POST {} upload {} files", endpoint, chunk_pairs.len()));
            match client.post(&endpoint).multipart(form).send() {
                Ok(resp) => {
                    let status = resp.status();
                    if let Ok(body) = resp.text() {
                        if let Some(pf) = save_body_to_file(&body) {
                            log_rpc_debug(&format!("Response {}: {} BODY_FILE:{}", endpoint, status, pf.display()));
                        } else {
                            log_rpc_warn(&format!("Response {}: {} body: (failed to save body)", endpoint, status));
                        }
                        if status.is_success() {
                            if let Ok(jsonv) = serde_json::from_str::<serde_json::Value>(&body) {
                                if let Some(upl) = jsonv.get("uploaded").and_then(|v| v.as_array()) {
                                    for it in upl {
                                        if let Some(arr) = it.as_array() {
                                            if arr.len() >= 2 {
                                                if let (Some(fname), Some(hash)) = (arr[0].as_str(), arr[1].as_str()) {
                                                    // record uploaded and remove local file to avoid re-upload
                                                    uploaded.push((fname.to_string(), hash.to_string()));
                                                    // find matching path in this chunk and delete
                                                    for (p, f) in &chunk_pairs {
                                                        if f == fname {
                                                            if std::fs::remove_file(p).is_ok() {
                                                                log_rpc_debug(&format!("Deleted uploaded file: {}", p.display()));
                                                                remove_ready_file(p);
                                                                record_uploaded_file(
                                                                    fname,
                                                                    &file_owner,
                                                                    &series_total_files,
                                                                    &mut series_handled_files,
                                                                    &study_total_series,
                                                                    &mut study_completed_series,
                                                                    &tx,
                                                                );
                                                            } else {
                                                                log_rpc_warn(&format!("Failed to delete uploaded file: {}", p.display()));
                                                            }
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if let Some(dups) = jsonv.get("duplicates").and_then(|v| v.as_array()) {
                                    for it in dups {
                                        if let Some(arr) = it.as_array() {
                                            if arr.len() >= 2 {
                                                if let (Some(fname), Some(hash)) = (arr[0].as_str(), arr[1].as_str()) {
                                                    // record duplicate and delete local copy to keep anon dir clean
                                                    duplicates.push((fname.to_string(), hash.to_string()));
                                                    for (p, f) in &chunk_pairs {
                                                        if f == fname {
                                                            if std::fs::remove_file(p).is_ok() {
                                                                log_rpc_debug(&format!("Deleted duplicate local file: {}", p.display()));
                                                                remove_ready_file(p);
                                                                record_uploaded_file(
                                                                    fname,
                                                                    &file_owner,
                                                                    &series_total_files,
                                                                    &mut series_handled_files,
                                                                    &study_total_series,
                                                                    &mut study_completed_series,
                                                                    &tx,
                                                                );
                                                            } else {
                                                                log_rpc_warn(&format!("Failed to delete duplicate local file: {}", p.display()));
                                                            }
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if let Some(failedv) = jsonv.get("failed").and_then(|v| v.as_array()) {
                                    for it in failedv { if let Some(s) = it.as_str() { failed.push(s.to_string()); } }
                                }
                                if let Some(ds) = jsonv.get("duplicate_series").and_then(|v| v.as_array()) {
                                    for it in ds { if let Some(s) = it.as_str() { duplicate_series.insert(s.to_string()); } }
                                }
                            }
                            success = true;
                            break;
                        }
                    } else {
                        log_rpc_warn(&format!("Response {}: {} (failed to read body)", endpoint, status));
                    }
                }
                Err(e) => {
                    if is_timeout_error(&e) {
                        saw_timeout = true;
                        if let Some(ref s) = tx {
                            let _ = s.send(upload_timeout_user_message(load_request_timeout_secs()));
                        }
                    }
                    log_rpc_error(&format!("Request error {}: {}", endpoint, e));
                }
            }
        }

        if !success {
            // mark chunk files as failed
            for (_p, fname) in &chunk_pairs {
                failed.push(fname.clone());
            }
        }
            // update processed count and notify progress
            files_processed = files_processed.saturating_add(chunk_pairs.len());
            if let Some(ref s) = tx {
                if total_files > 0 {
                    let prog = (files_processed as f32 / total_files as f32).clamp(0.0, 1.0);
                    let _ = s.send(format!("PROC:PROG:{}", prog));
                }
            }
    }

    if saw_timeout {
        if let Some(ref s) = tx {
            let _ = s.send("One or more uploads timed out. Increase the upload timeout in Settings and retry failed files.".to_string());
        }
    }

    Ok(UploadResult { uploaded, duplicates, failed, duplicate_series })
}

#[allow(dead_code)]
/// Scan an anonymised directory for files ready to upload, grouped by DICOM SeriesInstanceUID.
pub fn scan_for_upload(anon_dir: &Path, tx: Option<std::sync::mpsc::Sender<String>>) -> Result<Vec<SeriesInfo>, String> {
    // Collect files recursively under anon_dir
    let mut files: Vec<PathBuf> = Vec::new();
    let all = collect_files_recursive(anon_dir);
    for p in all.into_iter() {
        if p.is_file() {
            // Accept files that either have a .dcm extension or can be opened as DICOM
            let mut accept = false;
            if p.extension().map(|ex| ex.eq_ignore_ascii_case("dcm")).unwrap_or(false) {
                accept = true;
            } else {
                if open_file(&p).is_ok() {
                    accept = true;
                }
            }
            if accept {
                files.push(p);
            }
        }
    }

    // early return empty
    if files.is_empty() {
        return Ok(Vec::new());
    }

    // compute hashes and series mapping — open each file once and reuse the object
    let mut series_map: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
    let mut series_first_seen: HashMap<String, u64> = HashMap::new();
    let mut hash_list: Vec<String> = Vec::new();

    let total_files = files.len();
    if let Some(ref s) = tx {
        let _ = s.send("PROC:STEP:Scanning files".to_string());
        let _ = s.send(format!("PROC:PROG:{}", 0.0));
    }

    for (i, p) in files.iter().enumerate() {
        // attempt to open as DICOM once
        let mut series_uid = "NO_SERIES".to_string();
        let mut h_opt: Option<String> = None;
        if let Ok(obj) = open_file(p) {
            // extract SeriesInstanceUID if present
            if let Ok(elem) = obj.element(Tag(0x0020,0x000E)) {
                if let Ok(sv) = elem.to_str() { series_uid = sv.to_string(); }
            }

            h_opt = calculate_pixel_hash_from_obj(&obj);
        }

        let h = h_opt.clone().unwrap_or_else(|| "".to_string());
        if h_opt.is_some() { hash_list.push(h.clone()); }
        series_first_seen.entry(series_uid.clone()).or_insert(i as u64);
        series_map.entry(series_uid).or_default().push((p.clone(), h));

        // report incremental progress
        if let Some(ref s) = tx {
            // throttle progress updates to ~10 updates
            let report_interval = std::cmp::max(1, total_files / 10);
            if (i % report_interval == 0) || (i + 1 == total_files) {
                let prog = ((i + 1) as f32 / total_files as f32).clamp(0.0, 1.0);
                let _ = s.send(format!("PROC:PROG:{}", prog));
            }
        }
    }

    // precheck duplicates via server
    let mut duplicate_hashes: HashSet<String> = HashSet::new();
    let mut duplicate_series_urls: HashMap<String, Vec<String>> = HashMap::new();
    if !hash_list.is_empty() {
        let client = make_client(load_api_token().as_deref()).map_err(|e| e)?;
        let base = base_site_url();
        let hash_check_url = format!("{}{}", base, "/api/atlas/check_image_hashes/");
        log_rpc(&format!("POST {} with {} hashes", hash_check_url, hash_list.len()));
        if let Ok(r) = client
            .post(&hash_check_url)
            .timeout(Duration::from_secs(DUPLICATE_LOOKUP_TIMEOUT_SECS))
            .json(&hash_list)
            .send()
        {
            let status = r.status();
            if let Ok(body) = r.text() {
                if let Some(pf) = save_body_to_file(&body) {
                    log_rpc_debug(&format!("Response {}: {} BODY_FILE:{}", hash_check_url, status, pf.display()));
                } else {
                    log_rpc_warn(&format!("Response {}: {} body: (failed to save body)", hash_check_url, status));
                }
                if status.is_success() {
                    if let Ok(map) = serde_json::from_str::<serde_json::Value>(&body) {
                        if let Some(obj) = map.as_object() {
                            for (hash_val, info) in obj.iter() {
                                if info.is_object() {
                                    if let Some(id) = info.get("id") {
                                        if json_value_truthy(id) {
                                            duplicate_hashes.insert(hash_val.clone());
                                            if let Some(urlv) = info.get("url") {
                                                if let Some(urls) = urlv.as_str() {
                                                    // ensure full URL includes base if server returned a relative path
                                                    let full = if urls.starts_with("http") {
                                                        urls.to_string()
                                                    } else if urls.starts_with('/') {
                                                        format!("{}{}", base.trim_end_matches('/'), urls)
                                                    } else {
                                                        format!("{}/{}", base.trim_end_matches('/'), urls)
                                                    };
                                                    duplicate_series_urls.entry(hash_val.clone()).or_default().push(full);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                log_rpc_warn(&format!("Response {}: {} (failed to read body)", hash_check_url, status));
            }
        }
    }

    // build SeriesInfo with common metadata
    let mut out: Vec<SeriesInfo> = Vec::new();
    for (series_uid, items) in series_map.into_iter() {
        let mut entries: Vec<FileEntry> = Vec::new();
        let mut urls: Vec<String> = Vec::new();
        let mut total_bytes: u64 = 0;
        for (p, h) in &items {
            let is_dup = duplicate_hashes.contains(h);
            let duplicate_checked = h.is_empty() || is_dup || duplicate_series_urls.contains_key(h);
            if let Some(u) = duplicate_series_urls.get(h) {
                for s in u { urls.push(s.clone()); }
            }
            if let Ok(md) = std::fs::metadata(p) {
                total_bytes = total_bytes.saturating_add(md.len());
            }
            entries.push(FileEntry {
                path: p.clone(),
                hash: h.clone(),
                duplicate_checked,
                is_duplicate: is_dup,
            });
        }

        // pick first file to extract study/series metadata
        let mut patient_name = None;
        let mut examination = None;
        let mut patient_id = None;
        let mut study_date = None;
        let mut modality = None;
        let mut series_description = None;
        let mut series_number = None;
        if let Some((first_path, _)) = items.get(0) {
            if let Ok(obj) = open_file(first_path) {
                patient_name = obj.element(Tag(0x0010,0x0010)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
                examination = obj.element(Tag(0x0008,0x1030)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
                patient_id = obj.element(Tag(0x0010,0x0020)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
                study_date = obj.element(Tag(0x0008,0x0020)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
                modality = obj.element(Tag(0x0008,0x0060)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
                series_description = obj.element(Tag(0x0008,0x103E)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
                series_number = obj.element(Tag(0x0020,0x0011)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
            }
        }

        let loaded_order = series_first_seen.get(&series_uid).copied().unwrap_or(0);

        out.push(SeriesInfo {
            series_uid,
            files: entries,
            duplicate_series_urls: urls,
            study_uid: None,
            loaded_order,
            duplicate_checked_files: items
                .iter()
                .filter(|(_, h)| h.is_empty() || duplicate_hashes.contains(h) || duplicate_series_urls.contains_key(h))
                .count(),
            patient_name,
            examination,
            patient_id,
            study_date,
            modality,
            series_description,
            series_number,
            file_count: items.len(),
            total_bytes,
            burned_in_annotation_detected: false,
        });
    }

    Ok(out)
}

/// A faster scan that does not attempt to open files as DICOMs.
///
/// This is useful in situations where the anon directory is trusted to contain
/// only DICOMs (or the caller doesn't need SeriesInstanceUID grouping) and we
/// want to avoid the overhead of parsing DICOM files. Files are grouped under
/// a single `NO_SERIES` series and hashes are computed from file bytes for
/// duplicate prechecks with the server.
pub fn scan_for_upload_quick(anon_dir: &Path, tx: Option<std::sync::mpsc::Sender<String>>) -> Result<Vec<SeriesInfo>, String> {
    // List-only quick scan: enumerate files recursively and report sizes. Do NOT read
    // file contents, compute hashes, or call the server. This is intended for
    // fast operations (like Remove all) where we only need a stable file list.
    let mut files: Vec<PathBuf> = Vec::new();
    let all = collect_files_recursive(anon_dir);
    for p in all.into_iter() {
        if p.is_file() {
            files.push(p);
        }
    }

    if files.is_empty() {
        return Ok(Vec::new());
    }

    let total_files = files.len();
    if let Some(ref s) = tx {
        let _ = s.send("PROC:STEP:Quick listing files".to_string());
        // send initial zero progress
        let _ = s.send(format!("PROC:PROG:{}", 0.0));
    }

    // Group under NO_SERIES with empty hashes and no duplicate flags.
    // Avoid calling `metadata` per file and avoid per-file progress updates; only
    // emit periodic progress to keep the UI responsive.
    let mut series_map: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
    let mut series_first_seen: HashMap<String, u64> = HashMap::new();
    let report_interval = std::cmp::max(1, total_files / 10); // ~10 updates
    for (i, p) in files.iter().enumerate() {
        let series_uid = "NO_SERIES".to_string();
        series_first_seen.entry(series_uid.clone()).or_insert(i as u64);
        let _ = series_map.entry(series_uid).or_default().push((p.clone(), "".to_string()));
        if let Some(ref s) = tx {
            if (i % report_interval == 0) || (i + 1 == total_files) {
                let prog = ((i + 1) as f32 / total_files as f32).clamp(0.0, 1.0);
                let _ = s.send(format!("PROC:PROG:{}", prog));
            }
        }
    }

    let mut out: Vec<SeriesInfo> = Vec::new();
    for (series_uid, items) in series_map.into_iter() {
        let mut entries: Vec<FileEntry> = Vec::new();
        let total_bytes: u64 = 0;
        for (p, _h) in &items {
            // avoid stat() to keep this fast; file sizes are non-critical for delete-only flows
            entries.push(FileEntry {
                path: p.clone(),
                hash: "".to_string(),
                duplicate_checked: true,
                is_duplicate: false,
            });
        }

        let loaded_order = series_first_seen.get(&series_uid).copied().unwrap_or(0);

        out.push(SeriesInfo {
            series_uid,
            files: entries,
            duplicate_series_urls: Vec::new(),
            study_uid: None,
            loaded_order,
            duplicate_checked_files: items.len(),
            patient_name: None,
            examination: None,
            patient_id: None,
            study_date: None,
            modality: None,
            series_description: None,
            series_number: None,
            file_count: items.len(),
            total_bytes,
            burned_in_annotation_detected: false,
        });
    }

    Ok(out)
}

/// Request a background scan. Coalesces concurrent requests so only one scan runs at a time.
pub fn request_scan(anon_dir: &Path, tx: Option<std::sync::mpsc::Sender<String>>) -> Result<(), String> {
    if SCAN_RUNNING.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        let anon_dir = anon_dir.to_path_buf();
        // spawn background thread to refresh duplicate status from cached ready entries
        std::thread::spawn(move || {
            loop {
                if let Err(e) = ensure_ready_cache(&anon_dir, tx.clone()) {
                    if let Some(ref s) = tx {
                        let _ = s.send(format!("Ready-cache bootstrap failed: {}", e));
                    }
                }

                match refresh_duplicates_for_ready(&anon_dir, tx.clone()) {
                    Ok(series) => {
                        // store parsed series in-memory for quick UI pickup
                        store_last_scan(series.clone());
                        if let Ok(json) = serde_json::to_string(&series) {
                            let _ = std::fs::write(".last_scan.json", json);
                            if let Some(ref s) = tx {
                                let _ = s.send("scan_written".to_string());
                                let _ = s.send("done".to_string());
                            }
                        } else if let Some(ref s) = tx {
                            let _ = s.send("scan_serialize_failed".to_string());
                        }
                    }
                    Err(e) => {
                        if let Some(ref s) = tx {
                            let _ = s.send(format!("Scan failed: {}", e));
                            let _ = s.send("done".to_string());
                        }
                    }
                }

                // If another scan request arrived while we were running,
                // immediately run one more pass so UI reflects latest state.
                if !SCAN_PENDING.swap(false, Ordering::SeqCst) {
                    break;
                }
                if let Some(ref s) = tx {
                    let _ = s.send("scan_rerun".to_string());
                }
            }
            SCAN_RUNNING.store(false, Ordering::SeqCst);
        });
        Ok(())
    } else {
        // a scan is already running; coalesce another pass when it completes
        SCAN_PENDING.store(true, Ordering::SeqCst);
        if let Some(ref s) = tx {
            let _ = s.send("scan_queued".to_string());
        }
        Ok(())
    }
}

// In-memory cache for the last parsed scan result. This lets background
// scanning threads parse the JSON and store the Vec<SeriesInfo> so the UI can
// quickly clone it without performing large deserializations on the UI thread.
static LAST_SCAN: Lazy<Mutex<Option<Vec<SeriesInfo>>>> = Lazy::new(|| Mutex::new(None));

pub fn store_last_scan(series: Vec<SeriesInfo>) {
    if let Ok(mut g) = LAST_SCAN.lock() {
        *g = Some(series);
    }
}

pub fn get_last_scan() -> Option<Vec<SeriesInfo>> {
    if let Ok(g) = LAST_SCAN.lock() {
        return g.clone();
    }
    None
}

/// Split the files of a single series into sub-series by the value of a DICOM tag keyword
/// (e.g. "ImageType", "EchoNumbers") or a `(GGGG,EEEE)` hex tag string.
///
/// Files are re-written in-place with a new SeriesInstanceUID derived deterministically from
/// the original UID + the group index (1-based; group 0 keeps the original UID).
/// Returns the number of distinct sub-series created, or an error if the tag is unknown /
/// all files have the same value (nothing to split).
pub fn split_series_by_tag(paths: &[PathBuf], tag_keyword: &str) -> Result<usize, String> {
    use dicom_core::DataDictionary;
    use dicom_core::dictionary::DataDictionaryEntry;
    use dicom_dictionary_std::StandardDataDictionary;
    use num_bigint::BigUint;

    // Resolve the keyword to a Tag.
    let split_tag = {
        let kw = tag_keyword.trim();
        // Try (XXXX,XXXX) hex notation first.
        let hex_re = kw.trim_start_matches('(').trim_end_matches(')');
        if let Some((g, e)) = hex_re.split_once(',') {
            let g = g.trim();
            let e = e.trim();
            match (u16::from_str_radix(g, 16), u16::from_str_radix(e, 16)) {
                (Ok(gv), Ok(ev)) => Tag(gv, ev),
                _ => {
                    // Fall through to keyword lookup.
                    StandardDataDictionary
                        .by_name(kw)
                        .map(|entry| entry.tag())
                        .ok_or_else(|| format!("Unknown DICOM tag keyword: {}", kw))?
                }
            }
        } else {
            StandardDataDictionary
                .by_name(kw)
                .map(|entry| entry.tag())
                .ok_or_else(|| format!("Unknown DICOM tag keyword: {}", kw))?
        }
    };

    // Read each file and get the tag value string (empty string if absent).
    let mut file_groups: Vec<(PathBuf, String)> = Vec::new();
    for p in paths {
        let val = open_file(p)
            .ok()
            .and_then(|obj| obj.element(split_tag).ok().and_then(|e| e.to_str().ok().map(|s| s.trim().to_string())))
            .unwrap_or_default();
        file_groups.push((p.clone(), val));
    }

    // Collect unique values in stable insertion order.
    let mut seen_vals: Vec<String> = Vec::new();
    {
        let mut set = std::collections::HashSet::new();
        for (_, v) in &file_groups {
            if set.insert(v.clone()) {
                seen_vals.push(v.clone());
            }
        }
    }

    if seen_vals.len() <= 1 {
        return Err(format!(
            "All files have the same value for {}; nothing to split",
            tag_keyword
        ));
    }

    // Helper: derive a new UID from the original series UID + group suffix.
    let derive_uid = |original_uid: &str, suffix: &str| -> String {
        let input = format!("{}:split:{}", original_uid, suffix);
        let h = blake3::hash(input.as_bytes());
        let num = BigUint::from_bytes_be(h.as_bytes());
        format!("2.25.{}", num)
    };

    // Read the original SeriesInstanceUID from the first file.
    let original_series_uid = open_file(&file_groups[0].0)
        .ok()
        .and_then(|obj| {
            obj.element(Tag(0x0020, 0x000E)).ok()
                .and_then(|e| e.to_str().ok().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| "UNKNOWN".to_string());

    // Pre-compute new UIDs: group-index 0 keeps the original UID; others get derived UIDs.
    let new_uid_for_group: Vec<String> = seen_vals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            if i == 0 {
                original_series_uid.clone()
            } else {
                derive_uid(&original_series_uid, v)
            }
        })
        .collect();

    // Rewrite files that belong to groups 1+ (group 0 keeps the existing UID).
    use dicom_core::header::VR;
    let mut _rewritten = 0usize;
    for (path, val) in &file_groups {
        let group_index = seen_vals.iter().position(|v| v == val).unwrap_or(0);
        if group_index == 0 {
            continue; // keep original UID
        }
        let new_uid = &new_uid_for_group[group_index];
        match open_file(path) {
            Ok(mut obj) => {
                let _ = obj.put_str(Tag(0x0020, 0x000E), VR::UI, new_uid);
                if let Err(e) = obj.write_to_file(path) {
                    return Err(format!("Failed to write {}: {}", path.display(), e));
                }
                // Update the ready manifest entry.
                if let Ok(mut guard) = READY_FILES.lock() {
                    let key = path_key(path);
                    if let Some(rfi) = guard.get_mut(&key) {
                        rfi.series_uid = new_uid.clone();
                    }
                }
                _rewritten += 1;
            }
            Err(e) => return Err(format!("Failed to open {}: {}", path.display(), e)),
        }
    }
    persist_ready_manifest_locked(&READY_FILES.lock().unwrap_or_else(|e| e.into_inner()).clone());

    Ok(seen_vals.len())
}

#[cfg(test)]
mod tests;
