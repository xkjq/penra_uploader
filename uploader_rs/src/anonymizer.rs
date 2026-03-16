use dicom_object::{open_file, FileDicomObject, Tag};
use blake3;
use chrono::{NaiveDate, Duration};
use dicom_core::header::{VR, Header};
use num_bigint::BigUint;
use std::path::{Path, PathBuf};
use std::fs;

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
    ];
    // Physician and operator names
    clear_tags.push(Tag(0x0008,0x0090)); // ReferringPhysicianName
    clear_tags.push(Tag(0x0008,0x1050)); // PerformingPhysicianName
    clear_tags.push(Tag(0x0008,0x1070)); // OperatorsName
    // IDs and other patient identifiers
    clear_tags.push(Tag(0x0008,0x0050)); // AccessionNumber
    clear_tags.push(Tag(0x0020,0x0010)); // StudyID
    clear_tags.push(Tag(0x0018,0x1000)); // DeviceSerialNumber
    clear_tags.push(Tag(0x0010,0x1000)); // OtherPatientIDs
    clear_tags.push(Tag(0x0010,0x1002)); // OtherPatientIDsSequence
    for tag in clear_tags {
        // write empty string with a reasonable VR; fall back to LO
        let _ = obj.put_str(tag, VR::LO, "");
    }

    // Remap UIDs: StudyInstanceUID (0020,000D), SeriesInstanceUID (0020,000E), SOPInstanceUID (0008,0018)
    if let Ok(e) = obj.element(Tag(0x0020,0x000D)) {
        if let Ok(s) = e.to_str() {
            let h = blake3::hash(s.as_bytes());
            let uid = uid_from_hash_bytes(&h.as_bytes()[..16]);
            let _ = obj.put_str(Tag(0x0020,0x000D), VR::UI, &uid);
        }
    }
    if let Ok(e) = obj.element(Tag(0x0020,0x000E)) {
        if let Ok(s) = e.to_str() {
            let h = blake3::hash(s.as_bytes());
            let uid = uid_from_hash_bytes(&h.as_bytes()[..16]);
            let _ = obj.put_str(Tag(0x0020,0x000E), VR::UI, &uid);
        }
    }
    if let Ok(e) = obj.element(Tag(0x0008,0x0018)) {
        if let Ok(s) = e.to_str() {
            let h = blake3::hash(s.as_bytes());
            let uid = uid_from_hash_bytes(&h.as_bytes()[..16]);
            let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, &uid);
        }
    }

    // Strip private tags (any element whose group number is odd)
    // and recurse into Sequence (SQ) elements to anonymize nested items.
    fn process_dataset<D: dicom_object::DataSetMut>(ds: &mut D, study_uid: &str, seed: Option<&str>) {
        use dicom_core::value::Value;
        use dicom_object::mem::InMemDicomObject;

        // collect odd-group private tags to remove after iteration
        let mut to_remove: Vec<Tag> = Vec::new();
        // collect sequence tags to walk
        let mut seq_tags: Vec<Tag> = Vec::new();

        for el in ds.iter() {
            let t = el.tag();
            let group = t.0;
            if (group & 1) == 1 {
                to_remove.push(t);
                continue;
            }
            if el.vr() == VR::SQ {
                seq_tags.push(t);
            }
        }

        // remove private tags
        for t in to_remove {
            let _ = ds.remove_element(t);
        }

        // walk sequences and recurse into each item
        for t in seq_tags {
            if let Ok(el) = ds.element(t) {
                if let Value::Sequence { items, .. } = el.value() {
                    // items is a slice of InMemDicomObject values; clone and recreate
                    let mut new_items: Vec<InMemDicomObject> = Vec::new();
                    for item in items.iter() {
                        // create a mutable copy to modify
                        let mut item_obj = item.to_owned();
                        process_dataset(&mut item_obj, study_uid, seed);
                        new_items.push(item_obj);
                    }
                    // write back the sequence with anonymized items
                    let _ = ds.put_value(t, VR::SQ, Value::Sequence { items: new_items, size: dicom_core::value::Len::Undefined });
                }
            }
        }
    }

    process_dataset(&mut obj, &study_uid, seed);

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

    if remove_original {
        let _ = fs::remove_file(input);
    }

    Ok(out_path)
}
