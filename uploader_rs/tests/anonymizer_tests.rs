use std::path::PathBuf;
use std::env;
use tempfile::tempdir;
use std::process::Command;

fn find_first_dcm() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let base = manifest.join("../test_dicoms");
    // Ensure a test_dicoms directory exists; if missing, create a minimal DICOM
    // fixture so tests can run in fresh checkouts.
    if !base.exists() {
        if let Err(e) = std::fs::create_dir_all(&base) {
            eprintln!("Failed to create test_dicoms dir: {}", e);
            return None;
        }
        // create a minimal DICOM file
        use dicom_object::{InMemDicomObject, FileMetaTableBuilder, Tag};
        use dicom_core::header::VR;
        const EXPLICIT_VR_LE_UID: &str = "1.2.840.10008.1.2.1";
        let fixture_path = base.join("p01_s01_s01_i01.dcm");
        let mut obj = InMemDicomObject::new_empty();
        let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
        let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, "2.25.1000000");
        let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, "Test^Patient");
        let file_obj = obj.with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(EXPLICIT_VR_LE_UID)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid("2.25.1000000"),
        );
        if let Ok(fm) = file_obj {
            let _ = fm.write_to_file(&fixture_path);
        }
    }
    let mut stack = vec![base];
    while let Some(p) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&p) {
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    if ext.eq_ignore_ascii_case("dcm") {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

#[test]
fn test_anonymize_clears_and_pseudonymizes() {
    let src = find_first_dcm().expect("no sample DICOM found in test_dicoms");
    // read original
    let orig = dicom_object::open_file(&src).expect("open orig");
    let orig_acc = orig.element(dicom_core::Tag(0x0008,0x0050)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
    let orig_study_uid = orig.element(dicom_core::Tag(0x0020,0x000D)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());

    let tmp = tempdir().expect("tempdir");
    let outdir = tmp.path();

    // call anonymizer binary via env var set by Cargo
    let exe = std::env::var("CARGO_BIN_EXE_uploader_rs").unwrap_or_else(|_| {
        // fallback to target path
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("target/debug/uploader_rs");
        p.to_string_lossy().to_string()
    });

    let out_path = outdir.join(src.file_name().unwrap());
    let status = Command::new(exe)
        .arg("--anon")
        .arg(src.as_os_str())
        .arg(&out_path)
        .status()
        .expect("run binary");
    assert!(status.success(), "binary failed");

    let anon = dicom_object::open_file(&out_path).expect("open anon");
    // AccessionNumber should be cleared (empty) if present in clear list
    if let Some(a) = orig_acc {
        let a2 = anon.element(dicom_core::Tag(0x0008,0x0050)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
        assert!(a2.unwrap_or_default().is_empty(), "AccessionNumber not cleared");
    }

    // PatientName should be pseudonymized
    let pn = anon.element(dicom_core::Tag(0x0010,0x0010)).expect("pn present").to_str().map(|c| c.to_string()).unwrap_or_default();
    assert!(pn.starts_with("ANON-"), "PatientName not pseudonymized: {}", pn);

    // StudyInstanceUID should be remapped to 2.25.*
    if let Some(suid) = orig_study_uid {
        let s2 = anon.element(dicom_core::Tag(0x0020,0x000D)).ok().and_then(|e| e.to_str().ok()).unwrap_or_default();
        assert!(s2.starts_with("2.25."), "StudyInstanceUID not remapped: {}", s2);
        assert_ne!(suid, s2, "StudyInstanceUID unchanged");
    }
}
