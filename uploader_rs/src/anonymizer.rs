use dicom_object::{open_file, FileDicomObject, Tag};
use blake3;
use chrono::{NaiveDate, Duration, NaiveTime, Timelike};
use dicom_core::header::{VR, Header};
use num_bigint::BigUint;
use std::path::{Path, PathBuf};
use std::fs;
use std::fs::File;
use std::collections::HashMap;

fn hash_bytes(input: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let h = blake3::hash(input.as_bytes());
    out.copy_from_slice(&h.as_bytes()[..16]);
    out
}

fn uid_from_hash_bytes(bytes: &[u8]) -> String {
    // construct a decimal UID using 2.25.<decimal-of-128bit>
    let num = BigUint::from_bytes_be(bytes);
    format!("2.25.{}", num)
}

fn shift_date_by_study(study_uid: &str, seed: Option<&str>) -> i64 {
    // derive a deterministic day offset based on study UID hash
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
    // choose offset in range -3650..+3650 (approx +/-10 years)
    let range = 3650i64 * 2 + 1;
    let offset = (v % (range as u64)) as i64 - 3650;
    offset
}

pub fn anonymize_file(input: &Path, output_dir: &Path, remove_original: bool, seed: Option<&str>) -> Result<PathBuf, String> {
    let mut obj: FileDicomObject<_> = open_file(input).map_err(|e| format!("Failed to open DICOM: {}", e))?;

    // read some tags for study / patient (safe unwraps)
    let study_uid = obj.element(Tag(0x0020, 0x000D)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string()).unwrap_or_else(|| "NO_STUDY_UID".to_string());
    let pat_name = obj.element(Tag(0x0010, 0x0010)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string()).unwrap_or_else(|| "ANON".to_string());

    // deterministic pseudonyms
    let name_hash = hash_bytes(&format!("{}:{}:{}", seed.unwrap_or(""), study_uid, pat_name));
    let pn = format!("ANON-{}", &hex::encode(&name_hash)[..12]);
    // patient id
    let pid_hash = hash_bytes(&format!("{}:{}:{}:id", seed.unwrap_or(""), study_uid, pat_name));
    let pid = format!("ID-{}", &hex::encode(&pid_hash)[..12]);

    // prepare mapping audit
    let mut map: HashMap<String, String> = HashMap::new();
    map.insert(format!("PatientName:{}", pat_name), pn.clone());
    map.insert(format!("PatientID:{}", pid), pid.clone());

    // time shift
    let shift_days = shift_date_by_study(&study_uid, seed);

    // Set patient name and id
    let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, &pn);
    let _ = obj.put_str(Tag(0x0010, 0x0020), VR::LO, &pid);

    // Keep patient name and id pseudonymized, but clear other PHI-heavy fields
    let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, &pn);
    let _ = obj.put_str(Tag(0x0010, 0x0020), VR::LO, &pid);

    // Clear common free-text and ID tags that dicognito typically removes
    let mut clear_tags = vec![
        Tag(0x0008,0x0080), // InstitutionName
        Tag(0x0008,0x0081), // InstitutionAddress
        Tag(0x0008,0x1030), // StudyDescription
        Tag(0x0008,0x103E), // SeriesDescription
        Tag(0x0010,0x1040), // PatientAddress
        Tag(0x0010,0x4000), // PatientComments
        Tag(0x0008,0x0092), // Referring Physician Address Sequence (may be sequence)
        Tag(0x0008,0x0090), // ReferringPhysicianName
        Tag(0x0008,0x1050), // PerformingPhysicianName
        Tag(0x0008,0x1070), // OperatorsName
        Tag(0x0008,0x0050), // AccessionNumber
        Tag(0x0020,0x0010), // StudyID
        Tag(0x0018,0x1000), // DeviceSerialNumber
        Tag(0x0010,0x1000), // OtherPatientIDs
        Tag(0x0010,0x1002), // OtherPatientIDsSequence
        // Additional fields to remove/clear (from policy list)
        Tag(0x0008,0x0094), // ReferringPhysicianTelephoneNumbers
        Tag(0x0008,0x1010), // StationName
        Tag(0x0008,0x1040), // InstitutionalDepartmentName
        Tag(0x0008,0x1048), // PhysiciansOfRecord
        Tag(0x0008,0x1060), // NameOfPhysiciansReadingStudy
        Tag(0x0008,0x1080), // AdmittingDiagnosesDescription
        Tag(0x0008,0x2111), // DerivationDescription
        Tag(0x0010,0x1001), // OtherPatientNames
        Tag(0x0010,0x0040), // PatientSex
        Tag(0x0010,0x1010), // PatientAge
        Tag(0x0010,0x1020), // PatientSize
        Tag(0x0010,0x1030), // PatientWeight
        Tag(0x0010,0x1090), // MedicalRecordLocator
        Tag(0x0010,0x2160), // EthnicGroup
        Tag(0x0010,0x2180), // Occupation
        Tag(0x0010,0x21B0), // AdditionalPatientsHistory
        Tag(0x0018,0x1030), // ProtocolName
        Tag(0x0020,0x4000), // ImageComments
        Tag(0x0040,0x0275), // RequestAttributesSequence
    ];

    // sensible defaults for certain tags (replace rather than blank)
    let defaults: Vec<(Tag, VR, String)> = vec![
        (Tag(0x0008,0x1010), VR::SH, "ANON".to_string()), // StationName
    ];

    // Remap UIDs: StudyInstanceUID (0020,000D), SeriesInstanceUID (0020,000E), SOPInstanceUID (0008,0018)
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

    // Date tags to shift (DA) and some extra date-like tags
    let date_tags = vec![
        Tag(0x0008,0x0020), Tag(0x0008,0x0021), Tag(0x0008,0x0022), Tag(0x0008,0x0023), // Study/Series dates
        Tag(0x0010,0x0030), // PatientBirthDate
        Tag(0x0008,0x002A), // Acquisition DateTime (DT)
    ];

    use dicom_object::InMemDicomObject;

    // text VRs to blanket-clear (unless whitelisted)
    let text_vrs = vec![VR::UT, VR::LT, VR::SH, VR::LO, VR::PN];
    // whitelist tags to keep (we pseudonymize these separately)
    let vr_whitelist = vec![Tag(0x0010,0x0010), Tag(0x0010,0x0020)]; // PatientName, PatientID

    fn process_inmem<D: dicom_core::DataDictionary + Clone>(ds: &mut InMemDicomObject<D>, study_uid: &str, seed: Option<&str>, clear_tags: &Vec<Tag>, date_tags: &Vec<Tag>, map: &mut HashMap<String,String>, text_vrs: &Vec<VR>, vr_whitelist: &Vec<Tag>) {
        use dicom_core::value::Value;

        let mut to_remove: Vec<Tag> = Vec::new();
        let mut seq_tags: Vec<Tag> = Vec::new();
        let mut puts: Vec<(Tag, VR, String)> = Vec::new();

        for el in ds.iter() {
            let t = el.tag();
            let group = t.0;
            if (group & 1) == 1 {
                to_remove.push(t);
                continue;
            }
            if clear_tags.contains(&t) {
                // if this tag is a sequence, remove it; otherwise set to empty using its VR
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
            // blanket-clear text VRs unless whitelisted
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
            // Handle DateTime (DT) values: shift leading YYYYMMDD portion
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
            // Handle Time (TM) values: shift by same day-offset modulo 24h
            if el.vr() == VR::TM || t == Tag(0x0010,0x0032) {
                if let Ok(s) = el.to_str() {
                    // try multiple TM formats
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

        // For each sequence, update its items in-place and recurse
        for t in seq_tags {
            let _ = ds.update_value(t, |v| {
                if let Some(items) = v.items_mut() {
                    for item in items.iter_mut() {
                        process_inmem(item, study_uid, seed, clear_tags, date_tags, map, text_vrs, vr_whitelist);
                    }
                }
            });
        }
    }
    fn process_file<D: dicom_core::DataDictionary + Clone>(ds: &mut dicom_object::FileDicomObject<dicom_object::InMemDicomObject<D>>, study_uid: &str, seed: Option<&str>, clear_tags: &Vec<Tag>, date_tags: &Vec<Tag>, map: &mut HashMap<String,String>, text_vrs: &Vec<VR>, vr_whitelist: &Vec<Tag>) {
        use dicom_core::value::Value;
        let mut to_remove: Vec<Tag> = Vec::new();
        let mut seq_tags: Vec<Tag> = Vec::new();
        let mut puts: Vec<(Tag, VR, String)> = Vec::new();

        for el in ds.iter() {
            let t = el.tag();
            let group = t.0;
            if (group & 1) == 1 {
                to_remove.push(t);
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
            // blanket-clear text VRs unless whitelisted
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
            // Handle DT in file-level elements
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
            // Handle TM in file-level elements
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
                        process_inmem(item, study_uid, seed, clear_tags, date_tags, map, text_vrs, vr_whitelist);
                    }
                }
            });
        }
    }

    // Apply top-level clearing and remapping first
    for tag in &clear_tags {
        if let Ok(elem) = obj.element(*tag) {
            let vr = elem.vr();
            let _ = elem;
            if vr == VR::SQ {
                let _ = obj.remove_element(*tag);
            } else {
                let _ = obj.put_str(*tag, vr, "");
            }
        }
    }

    // Apply sensible defaults (e.g., StationName -> ANON)
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

    // Special-case: scrub Structured Report Content Sequence `(0040,A730)` rather than removing it outright.
    if let Ok(_) = obj.element(Tag(0x0040,0xA730)) {
        let _ = obj.update_value(Tag(0x0040,0xA730), |v| {
            if let Some(items) = v.items_mut() {
                for item in items.iter_mut() {
                    process_inmem(item, &study_uid, seed, &clear_tags, &date_tags, &mut map, &text_vrs, &vr_whitelist);
                }
            }
        });
    }

    process_file(&mut obj, &study_uid, seed, &clear_tags, &date_tags, &mut map, &text_vrs, &vr_whitelist);

    // Shift date fields (basic set)
    let date_tags = vec![Tag(0x0008,0x0020), Tag(0x0008,0x0021), Tag(0x0008,0x0022), Tag(0x0008,0x0023), Tag(0x0010,0x0030)];
    for tag in date_tags {
        if let Ok(elem) = obj.element(tag) {
            if let Ok(s) = elem.to_str() {
                // try parse YYYYMMDD or YYYYMMDDHHMMSS
                if s.len() >= 8 {
                    let date_part = &s[0..8];
                    if let Ok(dt) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                        let shifted = dt + Duration::days(shift_days);
                        let new = shifted.format("%Y%m%d").to_string();
                        let _ = obj.put_str(tag, VR::DA, &new);
                    }
                }
            }
        }
    }

    // Prepare output path
    fs::create_dir_all(output_dir).map_err(|e| format!("mkdir failed: {}", e))?;
    let fname = input.file_name().ok_or_else(|| "invalid filename".to_string())?;
    let out_path = output_dir.join(fname);

    obj.write_to_file(&out_path).map_err(|e| format!("write failed: {}", e))?;

    // write mapping audit next to output file
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
