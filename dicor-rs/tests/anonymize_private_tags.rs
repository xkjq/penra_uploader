use dicor_rs::anonymize_file;
use dicom_object::{InMemDicomObject, FileMetaTableBuilder, open_file, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;
use std::fs::copy as copy_file;

fn copy_third_party_fixture(rel_path: &str, dest: &std::path::Path) {
    use std::path::Path;
    // Use only the vendored fixture copied into this crate under `tests/fixtures`.
    // Original source: upstream `dicognito` tests —
    // `third_party/dicognito/tests/orig_data/test_mitra_global_patient_id_is_updated/global_patient_id_implicit_vr.dcm`.
    // The file was copied into this repo to avoid submodule dependence.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fname = Path::new(rel_path).file_name().expect("filename");
    let vendored = manifest.join("tests").join("fixtures").join(fname);
    if !vendored.exists() {
        panic!("Vendored fixture not found: {}", vendored.display());
    }
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    copy_file(&vendored, dest).expect("copy vendored fixture");
}

fn make_file_with_element(path: &std::path::Path, tag: Tag, vr: VR, val: &str, sop_uid: &str) {
    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, sop_uid);
    let _ = obj.put_str(tag, vr, val);
    let file_obj = obj.with_meta(FileMetaTableBuilder::new()
        .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
        .media_storage_sop_instance_uid(sop_uid)
    ).unwrap();
    file_obj.write_to_file(path).unwrap();
}

#[test]
fn mitra_global_patient_id_is_updated() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("mitra_in.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    // fixture in the upstream dicognito tree
    copy_third_party_fixture("third_party/dicognito/tests/orig_data/test_mitra_global_patient_id_is_updated/global_patient_id_implicit_vr.dcm", &in_path);

    let _ = anonymize_file(&in_path, &out_dir, false, true, None).expect("anonymize mitra fixture");
    let out_files = std::fs::read_dir(&out_dir).unwrap().collect::<Vec<_>>();
    assert!(!out_files.is_empty());

    let out = open_file(&out_dir.join(in_path.file_name().unwrap())).expect("open out");
    // MITRA global patient id is a private element at group 0x0031, private slot 0x10 -> element 0x1020
    if let Ok(elem) = out.element(Tag(0x0031, 0x1020)) {
        if let Ok(s) = elem.to_str() {
            assert_ne!(s, "GPIYMBB54");
        }
    }
}

#[test]
fn tag_0031_0040_not_updated() {
    let tmp = tempdir().expect("tempdir");
    let obj_path = tmp.path().join("case.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    make_file_with_element(&obj_path, Tag(0x0031, 0x0040), VR::LO, "Some value", "2.25.2000");

    let orig = open_file(&obj_path).expect("open orig");
    let orig_val = orig.element(Tag(0x0031,0x0040)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());

    let res = anonymize_file(&obj_path, &out_dir, false, true, None).expect("anonymize");
    let out = open_file(&res).expect("open out");
    let new_val = out.element(Tag(0x0031,0x0040)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());

    assert_eq!(orig_val, new_val);
}

#[test]
fn private_creator_0031_0020_not_updated() {
    let tmp = tempdir().expect("tempdir");
    let obj_path = tmp.path().join("case2.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    make_file_with_element(&obj_path, Tag(0x0031, 0x0020), VR::LO, "Another value", "2.25.2001");

    let orig = open_file(&obj_path).expect("open orig");
    let orig_val = orig.element(Tag(0x0031,0x0020)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());

    let res = anonymize_file(&obj_path, &out_dir, false, true, None).expect("anonymize");
    let out = open_file(&res).expect("open out");
    let new_val = out.element(Tag(0x0031,0x0020)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());

    assert_eq!(orig_val, new_val);
}

#[test]
fn binary_mitra_global_patient_id_is_updated() {
    let tmp = tempdir().expect("tempdir");
    let obj_path = tmp.path().join("case3.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    // create a private creator and a binary-like value (store as string bytes)
    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0031, 0x0010), VR::LO, "MITRA LINKED ATTRIBUTES 1.0");
    let _ = obj.put_str(Tag(0x0031, 0x1020), VR::OB, "GPIYMBB54");
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.3000");
    let file_obj = obj.with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN).media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1").media_storage_sop_instance_uid("2.25.3000")).unwrap();
    file_obj.write_to_file(&obj_path).unwrap();

    let orig = open_file(&obj_path).expect("open orig");
    let orig_val = orig.element(Tag(0x0031,0x1020)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());

    let res = anonymize_file(&obj_path, &out_dir, false, true, None).expect("anonymize");
    let out = open_file(&res).expect("open out");
    let new_val = out.element(Tag(0x0031,0x1020)).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());

    assert!(orig_val.is_some());
    assert!(new_val.is_some());
    assert_ne!(orig_val, new_val);
}
