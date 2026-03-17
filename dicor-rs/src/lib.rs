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

    for el in ds.iter() {
        let t = el.tag();
        let group = t.0;

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

        if text_vrs.contains(&el.vr()) && !vr_whitelist.contains(&t) {
            puts.push((t, el.vr(), "".to_string()));
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
                    let minutes = (shift_date_by_study(study_uid, seed) % 1440) as i64;
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

    for el in ds.iter() {
        let t = el.tag();
        let group = t.0;
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
        if text_vrs.contains(&el.vr()) && !vr_whitelist.contains(&t) {
            puts.push((t, el.vr(), "".to_string()));
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

    // Collect private-group tags; if not preserving, remove them now
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
    }

    let shift_days = shift_date_by_study(&study_uid, seed);

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
    let vr_whitelist = vec![Tag(0x0010,0x0010), Tag(0x0010,0x0020)];

    

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
                                        let minutes = (shift_days % 1440) as i64;
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

    Ok(out_path)
}
