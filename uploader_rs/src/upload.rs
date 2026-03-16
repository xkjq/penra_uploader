use reqwest::blocking::Client;
use reqwest::blocking::multipart::{Form, Part};
use std::path::{Path, PathBuf};
use std::fs::File;
use blake3;
use std::collections::{HashMap, HashSet};
use dicom_object::open_file;
use dicom_object::Tag;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub hash: String,
    pub is_duplicate: bool,
}

#[derive(Debug, Clone)]
pub struct SeriesInfo {
    pub series_uid: String,
    pub files: Vec<FileEntry>,
    pub duplicate_series_urls: Vec<String>,
}

pub struct UploadResult {
    pub uploaded: Vec<(String, String)>,
    pub duplicates: Vec<(String, String)>,
    pub failed: Vec<String>,
    pub duplicate_series: HashSet<String>,
}

fn base_site_url() -> String {
    std::env::var("UPLOADER_BASE_URL").unwrap_or_else(|_| "https://www.penracourses.org.uk".to_string())
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

pub fn token_username() -> Option<String> {
    if let Some(t) = load_api_token() {
        let base = base_site_url();
        let token_check = format!("{}{}", base, "/api/atlas/token_check");
        let client = reqwest::blocking::Client::new();
        if let Ok(r) = client.post(&token_check).header("Authorization", format!("Bearer {}", t)).send() {
            if r.status().is_success() {
                if let Ok(j) = r.json::<serde_json::Value>() {
                    if j.get("valid").and_then(|b| b.as_bool()).unwrap_or(false) {
                        return j.get("username").and_then(|s| s.as_str()).map(|s| s.to_string()).or(Some("API token".to_string()));
                    }
                }
            }
        }
    }
    None
}

fn make_client(token: Option<&str>) -> Result<Client, String> {
    let mut b = reqwest::blocking::Client::builder();
    if std::env::var("UPLOADER_SKIP_SSL_VERIFY").unwrap_or_default().to_lowercase() == "1" {
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
    // Try to read PixelData (7FE0,0010) would require parsing; fallback to file bytes
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
        let resp = client.post(&hash_check_url).json(&hash_list).send();
        if let Ok(r) = resp {
            if r.status().is_success() {
                if let Ok(map) = r.json::<serde_json::Value>() {
                    if map.is_object() {
                        if let Some(obj) = map.as_object() {
                            for (hash_val, info) in obj.iter() {
                                if info.is_object() {
                                    if let Some(id) = info.get("id") {
                                        if !id.is_null() {
                                            duplicate_hashes.insert(hash_val.clone());
                                            if let Some(urlv) = info.get("url") {
                                                if let Some(urls) = urlv.as_str() {
                                                    pre_duplicate_series.insert(urls.to_string());
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

            match client.post(&endpoint).multipart(form).send() {
                Ok(resp) => {
                    if resp.status().is_success() {
                        if let Ok(jsonv) = resp.json::<serde_json::Value>() {
                            if let Some(upl) = jsonv.get("uploaded").and_then(|v| v.as_array()) {
                                for it in upl {
                                    if let Some(arr) = it.as_array() {
                                        if arr.len() >= 2 {
                                            if let (Some(fname), Some(hash)) = (arr[0].as_str(), arr[1].as_str()) {
                                                uploaded.push((fname.to_string(), hash.to_string()));
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
                                                duplicates.push((fname.to_string(), hash.to_string()));
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
                }
                Err(_) => {}
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
        if let Ok(r) = client.post(&hash_check_url).json(&hash_list).send() {
            if r.status().is_success() {
                if let Ok(map) = r.json::<serde_json::Value>() {
                    if let Some(obj) = map.as_object() {
                        for (hash_val, info) in obj.iter() {
                            if info.is_object() {
                                if let Some(id) = info.get("id") {
                                    if !id.is_null() {
                                        duplicate_hashes.insert(hash_val.clone());
                                        if let Some(urlv) = info.get("url") {
                                            if let Some(urls) = urlv.as_str() {
                                                // note: may want to map by series later
                                                duplicate_series_urls.entry(hash_val.clone()).or_default().push(urls.to_string());
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
    }

    // build SeriesInfo
    let mut out: Vec<SeriesInfo> = Vec::new();
    for (series_uid, items) in series_map.into_iter() {
        let mut entries: Vec<FileEntry> = Vec::new();
        let mut urls: Vec<String> = Vec::new();
        for (p, h) in items {
            let is_dup = duplicate_hashes.contains(&h);
            if let Some(u) = duplicate_series_urls.get(&h) {
                for s in u { urls.push(s.clone()); }
            }
            entries.push(FileEntry { path: p, hash: h, is_duplicate: is_dup });
        }
        out.push(SeriesInfo { series_uid, files: entries, duplicate_series_urls: urls });
    }

    Ok(out)
}
