use super::*;
use tempfile::tempdir;
use dicom_object::{InMemDicomObject, FileMetaTableBuilder, Tag};
use dicom_core::header::VR;
// use explicit VR little endian UID string directly to avoid adding new dependency
const EXPLICIT_VR_LE_UID: &str = "1.2.840.10008.1.2.1";
use std::time::{Duration, Instant};

fn make_minimal_dcm(path: &std::path::Path, sop_instance: &str, patient: &str) {
    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, sop_instance);
    let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, patient);
    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(EXPLICIT_VR_LE_UID)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
            .media_storage_sop_instance_uid(sop_instance),
    ).expect("with_meta");
    file_obj.write_to_file(path).expect("write dcm");
}

#[test]
fn test_export_processing_enqueues_and_anonymizes() {
    // Setup temporary workspace (export + anon)
    let tmp = tempdir().expect("tempdir");
    let export = tmp.path().join("export");
    let anon = tmp.path().join("anon");
    std::fs::create_dir_all(&export).unwrap();
    std::fs::create_dir_all(&anon).unwrap();

    // create a minimal DICOM file in export
    let in_path = export.join("test1.dcm");
    make_minimal_dcm(&in_path, "2.25.999999", "Doe^Jane");

    // Prepare AppState and channel to receive messages
    let mut app = AppState::default();
    app.export_dir = export.clone();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    app.tx = Some(tx.clone());

    // Trigger processing (this will move export->processing and enqueue)
    app.trigger_process_export();

    // Collect messages for up to 20s
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_anonymized = false;
    let mut saw_cleaned = false;
    while Instant::now() < deadline {
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(500)) {
            if m.contains("Anonymized:") { saw_anonymized = true; }
            if m.contains("Cleaned processing dir") { saw_cleaned = true; }
            if saw_anonymized && saw_cleaned { break; }
        }
    }

    // Verify anonymized file and anon map exist in anon dir
    let anon_file = anon.join("test1.dcm");
    let anon_map = anon.join("test1.dcm.anon_map.json");
    assert!(anon_file.exists(), "anonymized file missing: {}", anon_file.display());
    assert!(anon_map.exists(), "anon map missing: {}", anon_map.display());

    // Ensure processing directory was cleaned up
    let processing_parent = tmp.path().join("processing");
    // processing parent may or may not exist; ensure if exists it's empty
    if processing_parent.exists() {
        let mut has_child = false;
        if let Ok(mut it) = std::fs::read_dir(&processing_parent) { has_child = it.next().is_some(); }
        assert!(!has_child, "processing directory not empty");
    }

    assert!(saw_anonymized, "did not see anonymized message in logs");
    assert!(saw_cleaned, "did not see cleaned processing dir message in logs");
}

#[test]
fn test_multiple_files_and_repeated_exports() {
    let tmp = tempdir().expect("tempdir");
    let export = tmp.path().join("export");
    let anon = tmp.path().join("anon");
    std::fs::create_dir_all(&export).unwrap();
    std::fs::create_dir_all(&anon).unwrap();

    // Batch A
    let a1 = export.join("a1.dcm");
    let a2 = export.join("a2.dcm");
    make_minimal_dcm(&a1, "2.25.A1", "Alice^A");
    make_minimal_dcm(&a2, "2.25.A2", "Alice^B");

    // Prepare AppState and channel
    let mut app = AppState::default();
    app.export_dir = export.clone();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    app.tx = Some(tx.clone());

    // Enqueue first batch
    app.trigger_process_export();

    // shortly after, place Batch B and enqueue again
    std::thread::sleep(Duration::from_millis(300));
    let b1 = export.join("b1.dcm");
    let b2 = export.join("b2.dcm");
    make_minimal_dcm(&b1, "2.25.B1", "Bob^A");
    make_minimal_dcm(&b2, "2.25.B2", "Bob^B");
    app.trigger_process_export();

    // Wait up to 30s for anonymizations
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut anon_count = 0usize;
    while Instant::now() < deadline {
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(500)) {
            if m.contains("Anonymized:") { anon_count += 1; }
            if anon_count >= 4 { break; }
        }
    }

    // Verify all four anonymized files present
    for name in &["a1.dcm", "a2.dcm", "b1.dcm", "b2.dcm"] {
        let f = anon.join(name);
        let map = anon.join(format!("{}.anon_map.json", name));
        assert!(f.exists(), "missing anonymized file {}", f.display());
        assert!(map.exists(), "missing anon map for {}", name);
    }

    // Ensure processing directory was cleaned up
    // Wait for processing directory to be removed/cleaned (best-effort)
    let processing_parent = tmp.path().join("processing");
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !processing_parent.exists() {
            break;
        }
        if let Ok(mut it) = std::fs::read_dir(&processing_parent) {
            if it.next().is_none() {
                // empty — consider cleaned
                break;
            }
        }
        if Instant::now() >= wait_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if processing_parent.exists() {
        let mut has_child = false;
        if let Ok(mut it) = std::fs::read_dir(&processing_parent) { has_child = it.next().is_some(); }
        assert!(!has_child, "processing directory not empty after repeated exports");
    }

    assert!(anon_count >= 4, "expected at least 4 anonymized events, got {}", anon_count);
}
