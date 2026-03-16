use dicom_object::open_file;
use dicom_object::Tag;
use std::path::Path;
use std::collections::HashMap;

/// Read a small selection of DICOM metadata fields from `path`.
/// Returns a map of `label -> string` for display.
pub fn read_metadata(path: &Path) -> Result<HashMap<String, String>, String> {
    let obj = open_file(path).map_err(|e| format!("open_file error: {}", e))?;
    let mut out: HashMap<String, String> = HashMap::new();

    let lookups: &[(Tag, &str)] = &[
        (Tag(0x0010, 0x0010), "PatientName"),
        (Tag(0x0010, 0x0020), "PatientID"),
        (Tag(0x0008, 0x0020), "StudyDate"),
        (Tag(0x0008, 0x0060), "Modality"),
        (Tag(0x0020, 0x000D), "StudyInstanceUID"),
        (Tag(0x0020, 0x000E), "SeriesInstanceUID"),
        (Tag(0x0008, 0x0018), "SOPInstanceUID"),
        (Tag(0x0010, 0x0030), "PatientBirthDate"),
        (Tag(0x0010, 0x0040), "PatientSex"),
    ];

    for (tag, label) in lookups {
        if let Ok(elem) = obj.element(*tag) {
            if let Ok(s) = elem.to_str() {
                out.insert(label.to_string(), s.to_string());
                continue;
            }
        }
        out.insert(label.to_string(), "".to_string());
    }

    // Also include a compact list of all present tags (Tag -> value) up to a limit.
    // This is best-effort and avoids depending on a full iterator API.
    let mut other = Vec::new();
    for &g in &[0x0008u16, 0x0010u16, 0x0020u16, 0x7FE0u16] {
        for el in 0x0000u16..=0x00FFu16 {
            let tag = Tag(g, el);
            if let Ok(elem) = obj.element(tag) {
                if let Ok(s) = elem.to_str() {
                    other.push((format!("{:04X},{:04X}", tag.group(), tag.element()), s.to_string()));
                }
            }
            if other.len() >= 300 { break; }
        }
        if other.len() >= 300 { break; }
    }

    if !other.is_empty() {
        // serialize other as a single entry (displayed in a scroll area)
        let mut combined = String::new();
        for (t, v) in other {
            combined.push_str(&format!("{}: {}\n", t, v));
        }
        out.insert("AllElements".to_string(), combined);
    }

    Ok(out)
}

/// Read a richer set of metadata from the file. Returns a map of tag/key -> value.
pub fn read_metadata_all(path: &Path) -> Result<HashMap<String, String>, String> {
    let obj = open_file(path).map_err(|e| format!("open_file error: {}", e))?;
    let mut out: HashMap<String, String> = HashMap::new();

    // Best-effort extraction: probe common groups and element ranges.
    let groups: &[u16] = &[0x0008, 0x0010, 0x0020, 0x0028, 0x0040, 0x7FE0];
    for &g in groups {
        for el in 0x0000u16..=0xFFFFu16 {
            let tag = Tag(g, el);
            if let Ok(elem) = obj.element(tag) {
                if let Ok(s) = elem.to_str() {
                    out.insert(format!("{:04X},{:04X}", g, el), s.to_string());
                }
            }
            if out.len() > 2000 { break; }
        }
        if out.len() > 2000 { break; }
    }

    Ok(out)
}
