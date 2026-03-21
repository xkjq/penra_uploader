use dicom_object::{open_file, FileDicomObject, Tag};
use blake3;
use chrono::{NaiveDate, Duration, NaiveTime, Timelike};
use dicom_core::header::{VR, Header};
use num_bigint::BigUint;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;
use std::fs::File;

fn hash_bytes(input: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let h = blake3::hash(input.as_bytes());
    out.copy_from_slice(&h.as_bytes()[..16]);
    out
}

fn uid_from_hash_bytes(bytes: &[u8]) -> String {
    let num = BigUint::from_bytes_be(bytes);
    format!("2.25.{}", num)
}

fn shift_date_by_study(study_uid: &str, seed: Option<&str>) -> i64 {
    let key = match seed {
        Some(s) if !s.is_empty() => format!("{}:{}", s, study_uid),
        _ => study_uid.to_string(),
    };
    let h = blake3::hash(key.as_bytes());
    let v = u64::from_le_bytes({
        let mut b = [0u8; 8];
        b.copy_from_slice(&h.as_bytes()[..8]);
        b
    });
    let range = 3650i64 * 2 + 1;
    let offset = (v % (range as u64)) as i64 - 3650;
    offset
}

fn minute_offset_by_study(study_uid: &str, seed: Option<&str>) -> i64 {
    let key = match seed {
        Some(s) if !s.is_empty() => format!("{}:{}", s, study_uid),
        _ => study_uid.to_string(),
    };
    let h = blake3::hash(key.as_bytes());
    let v = u64::from_le_bytes({
        let mut b = [0u8; 8];
        b.copy_from_slice(&h.as_bytes()[..8]);
        b
    });
    // produce a signed offset roughly centered around zero in range [-720,719]
    let raw = (v % 1440) as i64;
    raw - 720
}

fn vr_is_safe_private(vr: VR) -> bool {
    matches!(vr,
        VR::US | VR::SS | VR::UL | VR::SL | VR::FL | VR::FD |
        VR::IS | VR::DS | VR::CS |
        VR::DA | VR::DT | VR::TM
    )
}

fn sanitize_text_field(s: &str, orig_name: &str, orig_pid: &str, shift_days: i64) -> String {
    let mut out_tokens: Vec<String> = Vec::new();
    let name_lc = orig_name.to_lowercase();
    let pid_lc = orig_pid.to_lowercase();
    for tok in s.split_whitespace() {
        let t = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.' && c != '-');
        if t.is_empty() { continue; }
        let tl = t.to_lowercase();
        if !name_lc.is_empty() && tl.contains(&name_lc) { continue; }
        if !pid_lc.is_empty() && tl.contains(&pid_lc) { continue; }
        if tl.contains('@') && tl.contains('.') { continue; }
        // check for 8-digit YYYYMMDD dates and replace with shifted date
        if t.len() == 8 && t.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(dt) = NaiveDate::parse_from_str(t, "%Y%m%d") {
                let shifted = dt + Duration::days(shift_days);
                out_tokens.push(shifted.format("%Y%m%d").to_string());
                continue;
            } else {
                // if not a valid date, drop if it's long numeric
                continue;
            }
        }
        let digits_only: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits_only.len() >= 6 { continue; }
        out_tokens.push(tok.to_string());
    }
    out_tokens.join(" ").trim().to_string()
}

fn process_inmem_top<D: dicom_core::DataDictionary + Clone>(
    ds: &mut dicom_object::InMemDicomObject<D>,
    study_uid: &str,
    preserve_private: bool,
    seed: Option<&str>,
    clear_tags: &Vec<Tag>,
    date_tags: &Vec<Tag>,
    map: &mut HashMap<String,String>,
    text_vrs: &Vec<VR>,
    vr_whitelist: &Vec<Tag>
) {
    use dicom_core::value::Value;
    let mut to_remove: Vec<Tag> = Vec::new();
    let mut seq_tags: Vec<Tag> = Vec::new();
    let mut puts: Vec<(Tag, VR, String)> = Vec::new();

    let tags: Vec<Tag> = ds.iter().map(|el| el.tag()).collect();
    for t in tags {
        // fetch element fresh each iteration to avoid holding an immutable borrow across the loop
        let group = t.0;
        let el = match ds.element(t) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if (group & 1) == 1 {
            if preserve_private {
                // MITRA special-case
                let mitra_group = 0x0031u16;
                let mitra_element_low = 0x0020u16;
                if group == mitra_group && (t.1 & 0x00FF) == mitra_element_low {
                    let private_tag_group = (t.1 >> 8) as u16;
                    let creator_tag = Tag(mitra_group, private_tag_group);
                    if let Ok(creator_el) = ds.element(creator_tag) {
                        if let Ok(s) = creator_el.to_str() {
                            if s == "MITRA LINKED ATTRIBUTES 1.0" {
                                let orig = el.to_str().ok().map(|s| s.to_string()).unwrap_or_else(|| String::new());
                                let hb = hash_bytes(orig.as_ref());
                                let new_uid = uid_from_hash_bytes(&hb);
                                puts.push((t, VR::LO, new_uid));
                                continue;
                            }
                        }
                    }
                }

                // For preserved private tags: scan for date/time-like values and shift them,
                // but do not perform general clearing/hashing of other VRs.
                if el.vr() == VR::DA || date_tags.contains(&t) {
                    if let Ok(s) = el.to_str() {
                        if s.len() >= 8 {
                            let date_part = &s[0..8];
                            if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                                let shifted = dt + Duration::days(shift_date_by_study(study_uid, seed));
                                let new = shifted.format("%Y%m%d").to_string();
                                puts.push((t, VR::DA, new));
                                continue;
                            }
                        }
                    }
                }

                if el.vr() == VR::DT {
                    if let Ok(s) = el.to_str() {
                        if s.len() >= 8 {
                            let date_part = &s[0..8];
                            if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                                let shifted = dt + Duration::days(shift_date_by_study(study_uid, seed));
                                let new_date = shifted.format("%Y%m%d").to_string();
                                let mut new = s.to_string();
                                new.replace_range(0..8, &new_date);
                                puts.push((t, VR::DT, new));
                                continue;
                            }
                        }
                    }
                }

                if el.vr() == VR::TM || t == Tag(0x0010,0x0032) {
                    if let Ok(s) = el.to_str() {
                        let patterns = ["%H%M%S", "%H%M", "%H:%M:%S"];
                        let mut parsed: Option<NaiveTime> = None;
                        for p in &patterns {
                            if let Ok(tm) = NaiveTime::parse_from_str(&s, p) {
                                parsed = Some(tm);
                                break;
                            }
                        }
                        if let Some(tm) = parsed {
                            let minutes = minute_offset_by_study(study_uid, seed);
                            let shifted_time = tm + Duration::minutes(minutes);
                            let secs = shifted_time.num_seconds_from_midnight();
                            let new = NaiveTime::from_num_seconds_from_midnight_opt(secs, 0).map(|t| t.format("%H%M%S").to_string()).unwrap_or_else(|| s.to_string());
                            puts.push((t, VR::TM, new));
                            continue;
                        }
                    }
                }

                // leave other private tags untouched
                continue;
            }
            continue;
        }

        if clear_tags.contains(&t) {
            if el.vr() == VR::SQ {
                to_remove.push(t);
            } else {
                puts.push((t, el.vr(), "".to_string()));
            }
            continue;
        }

        if el.vr() == VR::UI {
            if let Ok(s) = el.to_str() {
                let hb = hash_bytes(s.as_ref());
                let new_uid = uid_from_hash_bytes(&hb);
                map.insert(format!("UID:{}", s), new_uid.clone());
                puts.push((t, VR::UI, new_uid));
                continue;
            }
        }

        if text_vrs.contains(&el.vr()) {
            if !vr_whitelist.contains(&t) {
                // default behavior: blank non-whitelisted text VRs
                puts.push((t, el.vr(), "".to_string()));
                continue;
            }
            // For whitelisted text tags (preserved descriptive fields), sanitize content
            // but skip patient name/id which are handled separately.
            if t == Tag(0x0010,0x0010) || t == Tag(0x0010,0x0020) {
                continue;
            }
            // derive original patient name/id from map keys if available
            let mut orig_name = String::new();
            let mut orig_pid = String::new();
            for k in map.keys() {
                if k.starts_with("PatientName:") {
                    orig_name = k["PatientName:".len()..].to_string();
                }
                if k.starts_with("PatientID:") {
                    orig_pid = k["PatientID:".len()..].to_string();
                }
            }
            if let Ok(s) = el.to_str() {
                let cleaned = sanitize_text_field(&s, &orig_name, &orig_pid, shift_date_by_study(study_uid, seed));
                puts.push((t, el.vr(), cleaned));
            } else {
                puts.push((t, el.vr(), "".to_string()));
            }
            continue;
        }

        if el.vr() == VR::DA || date_tags.contains(&t) {
            if let Ok(s) = el.to_str() {
                if s.len() >= 8 {
                    let date_part = &s[0..8];
                    if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                        let shifted = dt + Duration::days(shift_date_by_study(study_uid, seed));
                        let new = shifted.format("%Y%m%d").to_string();
                        puts.push((t, VR::DA, new));
                        continue;
                    }
                }
            }
        }

        if el.vr() == VR::DT {
            if let Ok(s) = el.to_str() {
                if s.len() >= 8 {
                    let date_part = &s[0..8];
                    if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                        let shifted = dt + Duration::days(shift_date_by_study(study_uid, seed));
                        let new_date = shifted.format("%Y%m%d").to_string();
                        let mut new = s.to_string();
                        new.replace_range(0..8, &new_date);
                        puts.push((t, VR::DT, new));
                        continue;
                    }
                }
            }
        }

        if el.vr() == VR::TM || t == Tag(0x0010,0x0032) {
            if let Ok(s) = el.to_str() {
                let patterns = ["%H%M%S", "%H%M", "%H:%M:%S"];
                let mut parsed: Option<NaiveTime> = None;
                for p in &patterns {
                    if let Ok(tm) = NaiveTime::parse_from_str(&s, p) {
                        parsed = Some(tm);
                        break;
                    }
                }
                if let Some(tm) = parsed {
                    let minutes = minute_offset_by_study(study_uid, seed);
                    let shifted_time = tm + Duration::minutes(minutes);
                    let secs = shifted_time.num_seconds_from_midnight();
                    let new = NaiveTime::from_num_seconds_from_midnight_opt(secs, 0).map(|t| t.format("%H%M%S").to_string()).unwrap_or_else(|| s.to_string());
                    puts.push((t, VR::TM, new));
                    continue;
                }
            }
        }

        if el.vr() == VR::SQ {
            seq_tags.push(t);
        }
    }

    for t in to_remove {
        let _ = ds.remove_element(t);
    }

    for (t, vr, val) in puts {
        let _ = ds.put_str(t, vr, &val);
    }

    for t in seq_tags {
        let _ = ds.update_value(t, |v| {
            if let Some(items) = v.items_mut() {
                for item in items.iter_mut() {
                    process_inmem_top(item, study_uid, preserve_private, seed, clear_tags, date_tags, map, text_vrs, vr_whitelist);
                }
            }
        });
    }
}

fn process_file<D: dicom_core::DataDictionary + Clone>(
    ds: &mut dicom_object::FileDicomObject<dicom_object::InMemDicomObject<D>>,
    study_uid: &str,
    preserve_private: bool,
    seed: Option<&str>,
    clear_tags: &Vec<Tag>,
    date_tags: &Vec<Tag>,
    map: &mut HashMap<String,String>,
    text_vrs: &Vec<VR>,
    vr_whitelist: &Vec<Tag>
) {
    use dicom_core::value::Value;
    let mut to_remove: Vec<Tag> = Vec::new();
    let mut seq_tags: Vec<Tag> = Vec::new();
    let mut puts: Vec<(Tag, VR, String)> = Vec::new();

    let tags: Vec<Tag> = ds.iter().map(|el| el.tag()).collect();
    for t in tags {
        // fetch element fresh each iteration to avoid holding an immutable borrow across the loop
        let group = t.0;
        let el = match ds.element(t) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if (group & 1) == 1 {
            if preserve_private {
                // MITRA global patient id special-case at file-level
                let mitra_group = 0x0031u16;
                let mitra_element_low = 0x0020u16;
                if group == mitra_group && (t.1 & 0x00FF) == mitra_element_low {
                    let private_tag_group = (t.1 >> 8) as u16;
                    let creator_tag = Tag(mitra_group, private_tag_group);
                    if let Ok(creator_el) = ds.element(creator_tag) {
                        if let Ok(s) = creator_el.to_str() {
                            if s == "MITRA LINKED ATTRIBUTES 1.0" {
                                if let Ok(elv) = ds.element(t) {
                                    let orig = elv.to_str().ok().map(|s| s.to_string()).unwrap_or_else(|| String::new());
                                    let hb = hash_bytes(orig.as_ref());
                                    let new_uid = uid_from_hash_bytes(&hb);
                                    puts.push((t, VR::LO, new_uid.clone()));
                                    map.insert(format!("UID:{}", orig), new_uid.clone());
                                }
                                continue;
                            }
                        }
                    }
                }

                // For preserved private tags: scan for date/time-like values and shift them,
                // but do not perform general clearing/hashing of other VRs.
                if let Ok(elv) = ds.element(t) {
                    if elv.vr() == VR::DA || date_tags.contains(&t) {
                        if let Ok(s) = elv.to_str() {
                            if s.len() >= 8 {
                                let date_part = &s[0..8];
                                if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                                    let shifted = dt + Duration::days(shift_date_by_study(study_uid, seed));
                                    let new = shifted.format("%Y%m%d").to_string();
                                    puts.push((t, VR::DA, new));
                                    continue;
                                }
                            }
                        }
                    }

                    if elv.vr() == VR::DT {
                        if let Ok(s) = elv.to_str() {
                            if s.len() >= 8 {
                                let date_part = &s[0..8];
                                if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                                    let shifted = dt + Duration::days(shift_date_by_study(study_uid, seed));
                                    let new_date = shifted.format("%Y%m%d").to_string();
                                    let mut new = s.to_string();
                                    new.replace_range(0..8, &new_date);
                                    puts.push((t, VR::DT, new));
                                    continue;
                                }
                            }
                        }
                    }

                    if elv.vr() == VR::TM || t == Tag(0x0010,0x0032) {
                        if let Ok(s) = elv.to_str() {
                            let patterns = ["%H%M%S", "%H%M", "%H:%M:%S"];
                            let mut parsed: Option<NaiveTime> = None;
                            for p in &patterns {
                                if let Ok(tm) = NaiveTime::parse_from_str(&s, p) {
                                    parsed = Some(tm);
                                    break;
                                }
                            }
                            if let Some(tm) = parsed {
                                let minutes = minute_offset_by_study(&study_uid, seed);
                                let shifted_time = tm + Duration::minutes(minutes);
                                let secs = shifted_time.num_seconds_from_midnight();
                                let new = NaiveTime::from_num_seconds_from_midnight_opt(secs, 0).map(|t| t.format("%H%M%S").to_string()).unwrap_or_else(|| s.to_string());
                                puts.push((t, VR::TM, new));
                                continue;
                            }
                        }
                    }
                }

                // skip all other private tags when preserving (do not blank)
                continue;
            }
            // skip all other private tags when preserving (do not blank)
            continue;
        }
        if clear_tags.contains(&t) {
            if el.vr() == VR::SQ {
                to_remove.push(t);
            } else {
                puts.push((t, el.vr(), "".to_string()));
            }
            continue;
        }
        if el.vr() == VR::UI {
            if let Ok(s) = el.to_str() {
                let hb = hash_bytes(s.as_ref());
                let new_uid = uid_from_hash_bytes(&hb);
                map.insert(format!("UID:{}", s), new_uid.clone());
                puts.push((t, VR::UI, new_uid));
                continue;
            }
        }
        if text_vrs.contains(&el.vr()) {
            if !vr_whitelist.contains(&t) {
                // default behavior: blank non-whitelisted text VRs
                puts.push((t, el.vr(), "".to_string()));
                continue;
            }
            // For whitelisted text tags (preserved descriptive fields), sanitize content
            // but skip patient name/id which are handled separately.
            if t == Tag(0x0010,0x0010) || t == Tag(0x0010,0x0020) {
                continue;
            }
            // derive original patient name/id from map keys if available
            let mut orig_name = String::new();
            let mut orig_pid = String::new();
            for k in map.keys() {
                if k.starts_with("PatientName:") {
                    orig_name = k["PatientName:".len()..].to_string();
                }
                if k.starts_with("PatientID:") {
                    orig_pid = k["PatientID:".len()..].to_string();
                }
            }
            if let Ok(s) = el.to_str() {
                let cleaned = sanitize_text_field(&s, &orig_name, &orig_pid, shift_date_by_study(study_uid, seed));
                puts.push((t, el.vr(), cleaned));
            } else {
                puts.push((t, el.vr(), "".to_string()));
            }
            continue;
        }
        if el.vr() == VR::DA || date_tags.contains(&t) {
            if let Ok(s) = el.to_str() {
                if s.len() >= 8 {
                    let date_part = &s[0..8];
                    if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                        let shifted = dt + Duration::days(shift_date_by_study(study_uid, seed));
                        let new = shifted.format("%Y%m%d").to_string();
                        puts.push((t, VR::DA, new));
                        continue;
                    }
                }
            }
        }
        if el.vr() == VR::DT {
            if let Ok(s) = el.to_str() {
                if s.len() >= 8 {
                    let date_part = &s[0..8];
                    if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                        let shifted = dt + Duration::days(shift_date_by_study(study_uid, seed));
                        let new_date = shifted.format("%Y%m%d").to_string();
                        let mut new = s.to_string();
                        new.replace_range(0..8, &new_date);
                        puts.push((t, VR::DT, new));
                        continue;
                    }
                }
            }
        }
        if el.vr() == VR::SQ {
            seq_tags.push(t);
        }
    }

    for t in to_remove {
        let _ = ds.remove_element(t);
    }

    for (t, vr, val) in puts {
        let _ = ds.put_str(t, vr, &val);
    }

    for t in seq_tags {
        let _ = ds.update_value(t, |v| {
            if let Some(items) = v.items_mut() {
                for item in items.iter_mut() {
                    process_inmem_top(item, study_uid, preserve_private, seed, clear_tags, date_tags, map, text_vrs, vr_whitelist);
                }
            }
        });
    }
}

pub fn anonymize_file(input: &Path, output_dir: &Path, remove_original: bool, preserve_private: bool, permit_burned_in: bool, seed: Option<&str>) -> Result<PathBuf, String> {
    let mut obj: FileDicomObject<_> = open_file(input).map_err(|e| format!("Failed to open DICOM: {}", e))?;

    // Detect Burned In Annotation (0028,0301) and fail unless permitted.
    if let Ok(elem) = obj.element(Tag(0x0028, 0x0301)) {
        if let Ok(s) = elem.to_str() {
            if s.eq_ignore_ascii_case("YES") && !permit_burned_in {
                return Err(format!("Burned In Annotation is YES in {}", input.display()));
            }
        }
    }

    let study_uid = obj.element(Tag(0x0020, 0x000D)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string()).unwrap_or_else(|| "NO_STUDY_UID".to_string());
    let pat_name = obj.element(Tag(0x0010, 0x0010)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string()).unwrap_or_else(|| "ANON".to_string());

    // Patient-level hashes should not vary by study so that the same patient
    // is anonymized consistently across multiple studies. Use the patient
    // name (and optional seed) only.
    let name_hash = hash_bytes(&format!("{}:{}", seed.unwrap_or(""), pat_name));
    let pn = format!("ANON-{}", &hex::encode(&name_hash)[..12]);
    let pid_hash = hash_bytes(&format!("{}:{}:id", seed.unwrap_or(""), pat_name));
    let pid = format!("ID-{}", &hex::encode(&pid_hash)[..12]);

    let mut map: HashMap<String, String> = HashMap::new();
    map.insert(format!("PatientName:{}", pat_name), pn.clone());
    map.insert(format!("PatientID:{}", pid), pid.clone());

    // Collect private-group tags; if not preserving, remove them now.
    // If preserving, remove only those private attributes that are not considered safe
    // (retain numeric-like VRs and private creator IDs so retained privates remain definable).
    let mut private_tags: Vec<Tag> = Vec::new();
    for el in obj.iter() {
        let t = el.tag();
        if (t.0 & 1) == 1 {
            private_tags.push(t);
        }
    }
    if !preserve_private {
        for t in &private_tags {
            let _ = obj.remove_element(*t);
        }
    } else {
        for t in &private_tags {
            // Private creator IDs are in elements 0x0010..0x00FF; always keep those
            if t.1 >= 0x0010 && t.1 <= 0x00FF {
                continue;
            }
            if let Ok(el) = obj.element(*t) {
                if !vr_is_safe_private(el.vr()) {
                    let _ = obj.remove_element(*t);
                }
            } else {
                let _ = obj.remove_element(*t);
            }
        }
    }

    let shift_days = shift_date_by_study(&study_uid, seed);

    // Capture original PatientID (for PHI scanning inside free-text)
    let orig_patient_id = obj.element(Tag(0x0010,0x0020)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string()).unwrap_or_else(|| String::new());

    let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, &pn);
    let _ = obj.put_str(Tag(0x0010, 0x0020), VR::LO, &pid);

    let mut clear_tags = vec![
        Tag(0x0008,0x0080), Tag(0x0008,0x0081), Tag(0x0008,0x1030), Tag(0x0008,0x103E),
        Tag(0x0010,0x1040), Tag(0x0010,0x4000), Tag(0x0008,0x0092), Tag(0x0008,0x0090),
        Tag(0x0008,0x1050), Tag(0x0008,0x1070), Tag(0x0008,0x0050), Tag(0x0020,0x0010),
        Tag(0x0018,0x1000), Tag(0x0010,0x1000), Tag(0x0010,0x1002), Tag(0x0008,0x0094),
        Tag(0x0008,0x1010), Tag(0x0008,0x1040), Tag(0x0008,0x1048), Tag(0x0008,0x1060),
        Tag(0x0008,0x1080), Tag(0x0008,0x1080), Tag(0x0008,0x2111), Tag(0x0010,0x1001),
        Tag(0x0010,0x0040), Tag(0x0010,0x1010), Tag(0x0010,0x1020), Tag(0x0010,0x1030),
        Tag(0x0010,0x1090), Tag(0x0010,0x2160), Tag(0x0010,0x2180), Tag(0x0010,0x21B0),
        Tag(0x0018,0x1030), Tag(0x0020,0x4000), Tag(0x0040,0x0275),
        Tag(0x0040,0xA730),
    ];

    let defaults: Vec<(Tag, VR, String)> = vec![(Tag(0x0008,0x1010), VR::SH, "ANON".to_string())];

    if let Ok(e) = obj.element(Tag(0x0020,0x000D)) {
        if let Ok(s) = e.to_str() {
            let orig = s.to_string();
            let h = blake3::hash(orig.as_bytes());
            let uid = uid_from_hash_bytes(&h.as_bytes()[..16]);
            let _ = obj.put_str(Tag(0x0020,0x000D), VR::UI, &uid);
            map.insert(format!("UID:{}", orig), uid.clone());
        }
    }
    if let Ok(e) = obj.element(Tag(0x0020,0x000E)) {
        if let Ok(s) = e.to_str() {
            let orig = s.to_string();
            let h = blake3::hash(orig.as_bytes());
            let uid = uid_from_hash_bytes(&h.as_bytes()[..16]);
            let _ = obj.put_str(Tag(0x0020,0x000E), VR::UI, &uid);
            map.insert(format!("UID:{}", orig), uid.clone());
        }
    }
    if let Ok(e) = obj.element(Tag(0x0008,0x0018)) {
        if let Ok(s) = e.to_str() {
            let orig = s.to_string();
            let h = blake3::hash(orig.as_bytes());
            let uid = uid_from_hash_bytes(&h.as_bytes()[..16]);
            let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, &uid);
            map.insert(format!("UID:{}", orig), uid.clone());
        }
    }

    let date_tags = vec![Tag(0x0008,0x0020), Tag(0x0008,0x0021), Tag(0x0008,0x0022), Tag(0x0008,0x0023), Tag(0x0010,0x0030), Tag(0x0008,0x002A)];

    use dicom_object::InMemDicomObject;

    let text_vrs = vec![VR::UT, VR::LT, VR::SH, VR::LO, VR::PN];
    let mut vr_whitelist = vec![Tag(0x0010,0x0010), Tag(0x0010,0x0020)];
    // Preserve these descriptive fields but sanitize their content for PHI
    let preserve_text_tags = vec![Tag(0x0008,0x1030), Tag(0x0008,0x103E), Tag(0x0008,0x1090)];
    for t in &preserve_text_tags { vr_whitelist.push(*t); }

    for tag in &clear_tags {
        if let Ok(elem) = obj.element(*tag) {
            let vr = elem.vr();
            if vr == VR::SQ {
                let _ = obj.remove_element(*tag);
            } else {
                let _ = obj.put_str(*tag, vr, "");
            }
        }
    }

    // Remove any overlay groups (60xx) and all annotation/presentation state groups (0x0070)
    // to ensure burned-in overlay/annotation features are removed by default.
    let mut dyn_remove: Vec<Tag> = Vec::new();
    for el in obj.iter() {
        let t = el.tag();
        if (t.0 >= 0x6000 && t.0 <= 0x60FF) || t.0 == 0x0070 {
            dyn_remove.push(t);
        }
    }
    for t in dyn_remove {
        let _ = obj.remove_element(t);
    }

    for (tag, vr, val) in &defaults {
        let _ = obj.put_str(*tag, *vr, val);
    }
    for tag in &date_tags {
        if let Ok(elem) = obj.element(*tag) {
            if let Ok(s) = elem.to_str() {
                if s.len() >= 8 {
                    let date_part = &s[0..8];
                    if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                        let shifted = dt + Duration::days(shift_days);
                        let new = shifted.format("%Y%m%d").to_string();
                        let _ = obj.put_str(*tag, VR::DA, &new);
                    }
                }
            }
        }
    }

    if let Ok(_) = obj.element(Tag(0x0040,0xA730)) {
        let _ = obj.update_value(Tag(0x0040,0xA730), |v| {
            if let Some(items) = v.items_mut() {
                    for item in items.iter_mut() {
                    process_inmem_top(item, &study_uid, preserve_private, seed, &clear_tags, &date_tags, &mut map, &text_vrs, &vr_whitelist);
                }
            }
        });
    }

    // If preserving private tags, handle MITRA private Global Patient ID at file root
    if preserve_private {
        for t in &private_tags {
            if t.0 == 0x0031 && (t.1 & 0x00FF) == 0x0020 {
                let private_tag_group = (t.1 >> 8) as u16;
                let creator_tag = Tag(0x0031, private_tag_group);
                if let Ok(creator_el) = obj.element(creator_tag) {
                    if let Ok(s) = creator_el.to_str() {
                        if s == "MITRA LINKED ATTRIBUTES 1.0" {
                            if let Ok(el) = obj.element(*t) {
                                let orig = el.to_str().ok().map(|s| s.to_string()).unwrap_or_else(|| String::new());
                                let hb = hash_bytes(orig.as_ref());
                                let new_uid = uid_from_hash_bytes(&hb);
                                let _ = obj.put_str(*t, VR::LO, &new_uid);
                                map.insert(format!("UID:{}", orig), new_uid.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    process_file(&mut obj, &study_uid, preserve_private, seed, &clear_tags, &date_tags, &mut map, &text_vrs, &vr_whitelist);

    // (private tags restoration will be applied below)

    // Sanitize preserved descriptive text fields for likely PHI tokens
    if let Ok(orig_name_el) = obj.element(Tag(0x0010,0x0010)) {
        let orig_name = orig_name_el.to_str().ok().map(|s| s.to_string()).unwrap_or_else(|| String::new());
        for tag in &preserve_text_tags {
            if let Ok(el) = obj.element(*tag) {
                    if let Ok(s) = el.to_str() {
                        let s_owned = s.to_string();
                        let vr = el.vr();
                        let _ = el;
                        let cleaned = sanitize_text_field(&s_owned, &orig_name, &orig_patient_id, shift_days);
                        let _ = obj.put_str(*tag, vr, &cleaned);
                    }
                }
        }
    }

    let date_tags = vec![Tag(0x0008,0x0020), Tag(0x0008,0x0021), Tag(0x0008,0x0022), Tag(0x0008,0x0023), Tag(0x0010,0x0030)];
    for tag in date_tags {
        if let Ok(elem) = obj.element(tag) {
            if let Ok(s) = elem.to_str() {
                if s.len() >= 8 {
                    let date_part = &s[0..8];
                    if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                        let shifted = dt + Duration::days(shift_days);
                        let new = shifted.format("%Y%m%d").to_string();
                        let _ = obj.put_str(tag, VR::DA, &new);
                    }

                        // Ensure common time tags are shifted as well (StudyTime, SeriesTime, etc.)
                        let time_tags = vec![Tag(0x0008,0x0030), Tag(0x0008,0x0031), Tag(0x0008,0x0032), Tag(0x0008,0x0033), Tag(0x0008,0x0034), Tag(0x0008,0x0035), Tag(0x0010,0x0032), Tag(0x0040,0xA122)];
                        for tag in time_tags {
                            if let Ok(elem) = obj.element(tag) {
                                if let Ok(s) = elem.to_str() {
                                    let patterns = ["%H%M%S", "%H%M", "%H:%M:%S"];
                                    let mut parsed: Option<NaiveTime> = None;
                                    for p in &patterns {
                                        if let Ok(tm) = NaiveTime::parse_from_str(&s, p) {
                                            parsed = Some(tm);
                                            break;
                                        }
                                    }
                                    if let Some(tm) = parsed {
                                        let minutes = minute_offset_by_study(&study_uid, seed);
                                        let shifted_time = tm + Duration::minutes(minutes);
                                        let secs = shifted_time.num_seconds_from_midnight();
                                        let new = NaiveTime::from_num_seconds_from_midnight_opt(secs, 0).map(|t| t.format("%H%M%S").to_string()).unwrap_or_else(|| s.to_string());
                                        let _ = obj.put_str(tag, VR::TM, &new);
                                    }
                                }
                            }
                        }
                }
            }
        }
    }

    fs::create_dir_all(output_dir).map_err(|e| format!("mkdir failed: {}", e))?;
    let fname = input.file_name().ok_or_else(|| "invalid filename".to_string())?;
    let out_path = output_dir.join(fname);

    obj.write_to_file(&out_path).map_err(|e| format!("write failed: {}", e))?;

    let fname = fname.to_string_lossy();
    let map_fname = format!("{}.anon_map.json", fname);
    let map_path = output_dir.join(map_fname);
    match File::create(&map_path) {
        Ok(f) => {
            if let Err(e) = serde_json::to_writer_pretty(f, &map) {
                eprintln!("Failed to write anon map: {}", e);
            }
        }
        Err(e) => eprintln!("Failed to create anon map file: {}", e),
    }

    if remove_original {
        let _ = fs::remove_file(input);
    }

    // Indicate that the dataset has been de-identified and that re-identification is
    // not supported (irreversible anonymization).
    let _ = obj.put_str(Tag(0x0012, 0x0062), VR::CS, "YES");
    let _ = obj.put_str(Tag(0x0012, 0x0063), VR::LO, "dicor-rs: irreversible; re-identification not supported");

    Ok(out_path)
}
