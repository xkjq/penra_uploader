use reqwest::blocking::Client;
use reqwest::blocking::multipart::{Form, Part};
use std::path::{Path, PathBuf};
use std::fs::File;
use blake3;
use std::collections::{HashMap, HashSet};
use dicom_object::open_file;
use dicom_object::Tag;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub hash: String,
    pub is_duplicate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesInfo {
    pub series_uid: String,
    pub files: Vec<FileEntry>,
    pub duplicate_series_urls: Vec<String>,
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
}


pub struct UploadResult {
    pub uploaded: Vec<(String, String)>,
    pub duplicates: Vec<(String, String)>,
    pub failed: Vec<String>,
    pub duplicate_series: HashSet<String>,
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
                return v.get("skip_ssl").and_then(|b| b.as_bool()).unwrap_or(false);
            }
        }
    }
    false
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
    let p = log_file_path();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let entry = format!("[{}] {}\n", now, msg);
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&p).and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()));
}

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

pub fn token_username() -> Option<String> {
    if let Some(t) = load_api_token() {
        let base = base_site_url();
        let token_check = format!("{}{}", base, "/api/atlas/token_check");
        let client = reqwest::blocking::Client::new();
        if let Ok(r) = client.post(&token_check).header("Authorization", format!("Bearer {}", t)).send() {
            let status = r.status();
            if let Ok(body) = r.text() {
                if let Some(pf) = save_body_to_file(&body) {
                    log_rpc(&format!("Response {}: {} BODY_FILE:{}", token_check, status, pf.display()));
                } else {
                    log_rpc(&format!("Response {}: {} body: (failed to save body)", token_check, status));
                }
                if status.is_success() {
                    if let Ok(j) = serde_json::from_str::<serde_json::Value>(&body) {
                        if j.get("valid").and_then(|b| b.as_bool()).unwrap_or(false) {
                            return j.get("username").and_then(|s| s.as_str()).map(|s| s.to_string()).or(Some("API token".to_string()));
                        }
                    }
                }
            } else {
                log_rpc(&format!("Response {}: {} (failed to read body)", token_check, status));
            }
        }
    }
    None
}

fn make_client(token: Option<&str>) -> Result<Client, String> {
    let mut b = reqwest::blocking::Client::builder();
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

fn calculate_hash(path: &Path) -> Option<String> {
    // Prefer hashing the PixelData element (7FE0,0010) if present
    if let Ok(obj) = open_file(path) {
        if let Ok(elem) = obj.element(Tag(0x7FE0, 0x0010)) {
            // Try a few ways to obtain raw bytes from the element/value.
            // Different versions of the dicom crates expose different helpers,
            // so attempt a couple of reasonably-supported approaches and
            // fall back to hashing the whole file.
            // 1) If the element exposes a raw value-to-bytes conversion
            if let Ok(bytes) = elem.to_bytes() {
                return Some(blake3::hash(&bytes).to_hex().to_string());
            }

            // 2) Try taking the element value as a string/byte slice representation
            if let Ok(s) = elem.to_str() {
                let b = s.as_bytes();
                return Some(blake3::hash(b).to_hex().to_string());
            }
        }
    }

    // Fallback: hash the entire file bytes
    match std::fs::read(path) {
        Ok(bytes) => Some(blake3::hash(&bytes).to_hex().to_string()),
        Err(_) => None,
    }
}

pub fn upload_anon_dir(anon_dir: &Path, case_id: Option<&str>) -> Result<UploadResult, String> {
    let files: Vec<PathBuf> = match std::fs::read_dir(anon_dir) {
        Ok(r) => r.filter_map(|e| e.ok().map(|ent| ent.path())).collect(),
        Err(e) => return Err(format!("read_dir failed: {}", e)),
    };

    let mut client = make_client(load_api_token().as_deref())?;
    let base = base_site_url();
    let hash_check_url = format!("{}{}", base, "/api/atlas/check_image_hashes/");

    // compute hashes
    let mut path_to_hash = HashMap::new();
    let mut hash_list = Vec::new();
    for p in &files {
        if p.extension().map(|e| e == "dcm").unwrap_or(false) {
            if let Some(h) = calculate_hash(p) {
                path_to_hash.insert(p.clone(), h.clone());
                hash_list.push(h);
            }
        }
    }

    // precheck duplicates
    let mut duplicate_hashes = HashSet::new();
    let mut pre_duplicates: Vec<(String, String)> = Vec::new();
    let mut pre_duplicate_series: HashSet<String> = HashSet::new();

    if !hash_list.is_empty() {
        let _ = std::fs::read_dir(&anon_dir); // keep usage
        log_rpc(&format!("POST {} with {} hashes", hash_check_url, hash_list.len()));
        let resp = client.post(&hash_check_url).json(&hash_list).send();
        if let Ok(r) = resp {
            let status = r.status();
            if let Ok(body) = r.text() {
                if let Some(pf) = save_body_to_file(&body) {
                    log_rpc(&format!("Response {}: {} BODY_FILE:{}", hash_check_url, status, pf.display()));
                } else {
                    log_rpc(&format!("Response {}: {} body: (failed to save body)", hash_check_url, status));
                }
                if status.is_success() {
                    if let Ok(map) = serde_json::from_str::<serde_json::Value>(&body) {
                        if map.is_object() {
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
                                                                pre_duplicate_series.insert(full);
                                                            }
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
                log_rpc(&format!("Response {}: {} (failed to read body)", hash_check_url, status));
            }
        }
    }

    let files_after_precheck: Vec<PathBuf> = path_to_hash.iter().filter_map(|(p,h)| if duplicate_hashes.contains(h) { None } else { Some(p.clone()) }).collect();

    // Prepare multipart and chunk
    let mut files_to_upload: Vec<(PathBuf, String)> = Vec::new();
    for p in &files_after_precheck {
        if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
            files_to_upload.push((p.clone(), fname.to_string()));
        }
    }

    let chunk_size = 10usize;
    let mut uploaded = Vec::new();
    let mut duplicates = Vec::new();
    let mut failed = Vec::new();
    let mut duplicate_series = pre_duplicate_series;

    let total_chunks = (files_to_upload.len() + chunk_size - 1) / chunk_size;

    for (i, chunk) in files_to_upload.chunks(chunk_size).enumerate() {
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
                            log_rpc(&format!("Response {}: {} BODY_FILE:{}", endpoint, status, pf.display()));
                        } else {
                            log_rpc(&format!("Response {}: {} body: (failed to save body)", endpoint, status));
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
                                                                log_rpc(&format!("Deleted uploaded file: {}", p.display()));
                                                            } else {
                                                                log_rpc(&format!("Failed to delete uploaded file: {}", p.display()));
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
                                                                log_rpc(&format!("Deleted duplicate local file: {}", p.display()));
                                                            } else {
                                                                log_rpc(&format!("Failed to delete duplicate local file: {}", p.display()));
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
                        log_rpc(&format!("Response {}: {} (failed to read body)", endpoint, status));
                    }
                }
                Err(e) => { log_rpc(&format!("Request error {}: {}", endpoint, e)); }
            }
        }

        if !success {
            // mark chunk files as failed
            for (_p, fname) in &chunk_pairs {
                failed.push(fname.clone());
            }
        }
    }

    Ok(UploadResult { uploaded, duplicates, failed, duplicate_series })
}

/// Scan an anonymised directory for files ready to upload, grouped by DICOM SeriesInstanceUID.
pub fn scan_for_upload(anon_dir: &Path) -> Result<Vec<SeriesInfo>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let rd = std::fs::read_dir(anon_dir).map_err(|e| format!("read_dir failed: {}", e))?;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().map(|ex| ex.eq_ignore_ascii_case("dcm")).unwrap_or(false) {
            files.push(p);
        }
    }

    // early return empty
    if files.is_empty() {
        return Ok(Vec::new());
    }

    // compute hashes and series mapping
    let mut series_map: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
    let mut hash_list: Vec<String> = Vec::new();
    for p in &files {
        let h_opt = calculate_hash(p);
        let h = h_opt.clone().unwrap_or_else(|| "".to_string());
        if h_opt.is_some() {
            hash_list.push(h.clone());
        }
        // try read SeriesInstanceUID
        let series_uid = match open_file(p) {
            Ok(obj) => obj.element(Tag(0x0020,0x000E)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string()).unwrap_or_else(|| "NO_SERIES".to_string()),
            Err(_) => "NO_SERIES".to_string(),
        };
        series_map.entry(series_uid).or_default().push((p.clone(), h));
    }

    // precheck duplicates via server
    let mut duplicate_hashes: HashSet<String> = HashSet::new();
    let mut duplicate_series_urls: HashMap<String, Vec<String>> = HashMap::new();
    if !hash_list.is_empty() {
        let client = make_client(load_api_token().as_deref()).map_err(|e| e)?;
        let base = base_site_url();
        let hash_check_url = format!("{}{}", base, "/api/atlas/check_image_hashes/");
        log_rpc(&format!("POST {} with {} hashes", hash_check_url, hash_list.len()));
        if let Ok(r) = client.post(&hash_check_url).json(&hash_list).send() {
            let status = r.status();
            if let Ok(body) = r.text() {
                if let Some(pf) = save_body_to_file(&body) {
                    log_rpc(&format!("Response {}: {} BODY_FILE:{}", hash_check_url, status, pf.display()));
                } else {
                    log_rpc(&format!("Response {}: {} body: (failed to save body)", hash_check_url, status));
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
                log_rpc(&format!("Response {}: {} (failed to read body)", hash_check_url, status));
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
            if let Some(u) = duplicate_series_urls.get(h) {
                for s in u { urls.push(s.clone()); }
            }
            if let Ok(md) = std::fs::metadata(p) {
                total_bytes = total_bytes.saturating_add(md.len());
            }
            entries.push(FileEntry { path: p.clone(), hash: h.clone(), is_duplicate: is_dup });
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

        out.push(SeriesInfo {
            series_uid,
            files: entries,
            duplicate_series_urls: urls,
            patient_name,
            examination,
            patient_id,
            study_date,
            modality,
            series_description,
            series_number,
            file_count: items.len(),
            total_bytes,
        });
    }

    Ok(out)
}
