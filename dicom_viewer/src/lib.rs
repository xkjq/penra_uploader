use dicom_object::open_file;
use dicom_object::Tag;
use dicom_core::header::Header;
use dicom_core::VR;
use std::path::Path;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataReadMode {
    /// Fast, text-focused extraction used by default in the UI.
    Simple,
    /// In-depth extraction that iterates all available tags and provides
    /// fallback representations for non-text values.
    InDepth,
}

/// Diagnostic information about extraction issues encountered during metadata reading.
#[derive(Clone, Debug, Default)]
pub struct ExtractionDiagnostics {
    /// Sequences we encountered but couldn't fully descend into, with reason.
    pub failed_sequences: Vec<(String, String)>,
    /// Elements that had errors during extraction, with reason.
    pub failed_elements: Vec<(String, String)>,
    /// Count of successfully extracted elements.
    pub successful_count: usize,
    /// Human-readable summary of issues.
    pub summary: String,
}

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
    read_metadata_with_mode(path, MetadataReadMode::Simple)
}

/// Read an in-depth set of metadata from the file.
/// This mode iterates available tags and includes non-text fields using
/// concise, diff-friendly placeholder representations.
pub fn read_metadata_in_depth(path: &Path) -> Result<HashMap<String, String>, String> {
    read_metadata_with_mode(path, MetadataReadMode::InDepth)
}

/// Recursively extract metadata including nested elements within sequences.
/// Keys are formatted as "GGGG,EEEE" at top level, and "parent_key[index]/GGGG,EEEE" for nested items.
fn extract_deep_metadata(
    obj: &dicom_object::InMemDicomObject,
    prefix: &str,
    out: &mut HashMap<String, String>,
    diagnostics: &mut ExtractionDiagnostics,
) -> Result<(), String> {
    for elem in obj.iter() {
        let tag = elem.tag();
        let key = if prefix.is_empty() {
            format!("{:04X},{:04X}", tag.group(), tag.element())
        } else {
            format!("{}/{:04X},{:04X}", prefix, tag.group(), tag.element())
        };

        // Handle sequences specially by attempting to extract nested items
        if elem.vr() == VR::SQ {
            out.insert(key.clone(), "<SQ sequence>".to_string());
            
            // Try to access sequence items
            match elem.items() {
                Some(items) => {
                    for (item_idx, item) in items.iter().enumerate() {
                        let item_prefix = format!("{}[{}]", key, item_idx);
                        // Attempt to recurse; if it fails, log it
                        if let Err(e) = extract_deep_metadata(item, &item_prefix, out, diagnostics) {
                            diagnostics.failed_sequences.push((
                                item_prefix.clone(),
                                format!("Recursion failed: {}", e),
                            ));
                        }
                    }
                }
                None => {
                    diagnostics.failed_sequences.push((
                        key.clone(),
                        "No items() iterator available for sequence".to_string(),
                    ));
                }
            }
        } else {
            // Non-sequence: extract value
            let value = if let Ok(s) = elem.to_str() {
                s.to_string()
            } else if let Ok(bytes) = elem.to_bytes() {
                let preview_len = bytes.len().min(16);
                let mut preview = String::new();
                for (i, b) in bytes.iter().take(preview_len).enumerate() {
                    if i > 0 {
                        preview.push(' ');
                    }
                    preview.push_str(&format!("{:02X}", b));
                }
                if bytes.len() > preview_len {
                    preview.push_str(" ...");
                }
                format!("<{:?} binary: {} bytes [{}]>", elem.vr(), bytes.len(), preview)
            } else {
                format!("<{:?} non-text>", elem.vr())
            };

            out.insert(key.clone(), value);
            diagnostics.successful_count += 1;
        }
    }
    Ok(())
}


/// Read metadata from the file using a selectable extraction mode.
pub fn read_metadata_with_mode(path: &Path, mode: MetadataReadMode) -> Result<HashMap<String, String>, String> {
    let obj = open_file(path).map_err(|e| format!("open_file error: {}", e))?;
    let mut out: HashMap<String, String> = HashMap::new();
    let mut _diagnostics = ExtractionDiagnostics::default();

    match mode {
        MetadataReadMode::Simple => {
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
        }
        MetadataReadMode::InDepth => {
            // Iterate all available elements for maximum comparison coverage,
            // including nested elements within sequences.
            extract_deep_metadata(&obj, "", &mut out, &mut _diagnostics)?;
        }
    }

    Ok(out)
}

/// Read metadata from the file with diagnostic information about extraction issues.
/// Returns both the metadata map and detailed diagnostics about any sequences or elements
/// that couldn't be fully descended into.
pub fn read_metadata_with_diagnostics(path: &Path, mode: MetadataReadMode) -> Result<(HashMap<String, String>, ExtractionDiagnostics), String> {
    let obj = open_file(path).map_err(|e| format!("open_file error: {}", e))?;
    let mut out: HashMap<String, String> = HashMap::new();
    let mut diagnostics = ExtractionDiagnostics::default();

    match mode {
        MetadataReadMode::Simple => {
            // Best-effort extraction: probe common groups and element ranges.
            let groups: &[u16] = &[0x0008, 0x0010, 0x0020, 0x0028, 0x0040, 0x7FE0];
            for &g in groups {
                for el in 0x0000u16..=0xFFFFu16 {
                    let tag = Tag(g, el);
                    if let Ok(elem) = obj.element(tag) {
                        if let Ok(s) = elem.to_str() {
                            out.insert(format!("{:04X},{:04X}", g, el), s.to_string());
                            diagnostics.successful_count += 1;
                        }
                    }
                    if out.len() > 2000 { break; }
                }
                if out.len() > 2000 { break; }
            }
        }
        MetadataReadMode::InDepth => {
            // Iterate all available elements with full diagnostic logging.
            extract_deep_metadata(&obj, "", &mut out, &mut diagnostics)?;
        }
    }

    // Build summary message
    let mut summary = String::new();
    if !diagnostics.failed_sequences.is_empty() {
        summary.push_str(&format!(
            "Failed to descend into {} sequence(s)\n",
            diagnostics.failed_sequences.len()
        ));
    }
    if !diagnostics.failed_elements.is_empty() {
        summary.push_str(&format!(
            "Failed to extract {} element(s)\n",
            diagnostics.failed_elements.len()
        ));
    }
    if summary.is_empty() {
        summary = format!(
            "Successfully extracted {} element(s) with no descent issues",
            diagnostics.successful_count
        );
    }
    diagnostics.summary = summary;

    Ok((out, diagnostics))
}
