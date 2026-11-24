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
fn deface_config_maps_to_backend_options() {
    // Default: disabled, segmentation, anonymisation defaults (orbits/ears and
    // vertex strip on, deflesh-all, hard deflesh edge), defacing in place.
    let cfg = DefaceConfig::default();
    assert!(!cfg.enabled);
    assert_eq!(cfg.backend, DefaceBackendChoice::Segmentation);
    assert!(cfg.include_orbits);
    assert!(cfg.include_ears);
    assert!(!cfg.include_zygoma);
    assert!(cfg.vertex_strip);
    assert!(cfg.deflesh_all);
    assert_eq!(cfg.deflesh_smooth_mm, 0.0);
    let opts = cfg.to_options();
    assert!(matches!(opts.backend, diface_rs::BackendKind::Segmentation(_)));
    assert!(!opts.subdir_by_series, "defacing must overwrite in place");
    assert!(!opts.remove_original);
    assert_eq!(opts.min_slices, 3);

    // Segmentation carries the region, the safety band and every flag.
    let cfg = DefaceConfig {
        enabled: true,
        backend: DefaceBackendChoice::Segmentation,
        region: diface_rs::MaskRegion::ExternalSoftTissue,
        include_orbits: true,
        include_ears: false,
        include_zygoma: true,
        vertex_strip: false,
        brain_protect_mm: 7.5,
        deflesh_all: false,
        deflesh_smooth_mm: 5.0,
    };
    match cfg.to_options().backend {
        diface_rs::BackendKind::Segmentation(p) => {
            assert_eq!(p.region, diface_rs::MaskRegion::ExternalSoftTissue);
            assert!(p.include_orbits);
            assert!(!p.include_ears);
            assert!(p.include_zygoma);
            assert!(!p.vertex_strip);
            assert!((p.brain_protect_mm - 7.5).abs() < 1e-9);
            assert_eq!(p.deflesh_posterior_mm, 0.0);
            assert!((p.deflesh_smooth_mm - 5.0).abs() < 1e-9);
        }
        _ => panic!("expected a segmentation backend"),
    }
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

    // Verify anonymized file exists in anon dir
    let anon_file = anon.join("test1.dcm");
    assert!(anon_file.exists(), "anonymized file missing: {}", anon_file.display());

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
    make_minimal_dcm(&a1, "2.25.1001", "Alice^A");
    make_minimal_dcm(&a2, "2.25.1002", "Alice^B");

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
    make_minimal_dcm(&b1, "2.25.2001", "Bob^A");
    make_minimal_dcm(&b2, "2.25.2002", "Bob^B");
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
        assert!(f.exists(), "missing anonymized file {}", f.display());
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

#[test]
fn undefaced_files_are_moved_out_of_the_upload_directory() {
    let tmp = tempdir().expect("tempdir");
    let anon = tmp.path().join("anon");
    std::fs::create_dir_all(anon.join("nested")).unwrap();
    let kept = anon.join("keep.dcm");
    let skipped = anon.join("skip.dcm");
    let nested = anon.join("nested").join("skip2.dcm");
    std::fs::write(&kept, b"keep").unwrap();
    std::fs::write(&skipped, b"skip").unwrap();
    std::fs::write(&nested, b"skip2").unwrap();

    let (dir, moved, errors) =
        quarantine_undefaced_files(&anon, &[skipped.clone(), nested.clone()]);

    assert!(errors.is_empty(), "unexpected quarantine errors: {errors:?}");
    assert_eq!(moved, 2);
    assert!(kept.exists(), "defaced files must stay in the upload directory");
    assert!(!skipped.exists(), "skipped file must leave the upload directory");
    assert!(!nested.exists(), "skipped nested file must leave the upload directory");
    let quarantined = dir.join("skip.dcm");
    assert!(quarantined.exists(), "skipped file moved to quarantine");
    assert!(dir.join("nested").join("skip2.dcm").exists());
    let manifest = std::fs::read_to_string(dir.join("skipped.txt")).unwrap();
    assert!(manifest.contains("skip.dcm"));
    assert!(manifest.contains("skip2.dcm"));
}

#[test]
fn descriptive_review_requires_explicit_confirmation_per_series() {
    let mk = |uid: &str, flagged: bool| SeriesInfo {
        series_uid: uid.to_string(),
        files: vec![],
        duplicate_series_urls: vec![],
        study_uid: None,
        loaded_order: 0,
        duplicate_checked_files: 0,
        patient_name: None,
        examination: None,
        patient_id: None,
        study_date: None,
        modality: None,
        series_description: None,
        series_number: None,
        protocol_name: None,
        manufacturer_model_name: None,
        file_count: 0,
        total_bytes: 0,
        burned_in_annotation_detected: false,
        descriptive_review_required: flagged,
    };
    let mut app = AppState::default();
    app.ready_series = vec![mk("S1", true), mk("S2", false)];
    assert_eq!(
        app.unconfirmed_descriptive_series(),
        vec!["S1".to_string()]
    );

    app.descriptive_series_confirmed.insert("S1".to_string());
    assert!(app.unconfirmed_descriptive_series().is_empty());

    // A series that is no longer flagged loses its stale approval.
    app.ready_series[0].descriptive_review_required = false;
    app.sync_descriptive_confirmations();
    assert!(app.descriptive_series_confirmed.is_empty());
}
