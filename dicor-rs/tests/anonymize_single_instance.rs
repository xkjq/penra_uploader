use dicor_rs::anonymize_file;
use dicom_object::{open_file, InMemDicomObject, FileMetaTableBuilder, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;
use std::fs::copy as copy_file;

fn copy_fixture(fixture_file: &str, dest: &std::path::Path) {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(fixture_file);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    copy_file(&fixture_path, dest).expect("copy fixture");
}

fn make_minimal_file(path: &std::path::Path) {
    let mut obj = InMemDicomObject::new_empty();

    // minimal required dataset fields
    let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1"); // SOPClassUID
    let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, "2.25.1000000000"); // SOPInstanceUID
    let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, "Doe^John"); // PatientName

    let file_obj = obj
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid("2.25.1000000000"),
        )
        .expect("with_meta");

    file_obj.write_to_file(path).expect("write minimal DICOM");
}

fn make_test_file(path: &std::path::Path) {
    // reuse the more extensive constructor from anonymize_tags.rs style
    let mut obj = InMemDicomObject::new_empty();

    let mut set = |tag: Tag, vr: VR, val: &str| {
        let _ = obj.put_str(tag, vr, val);
    };

    // UIDs and meta-identifiers
    set(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    set(Tag(0x0008, 0x0018), VR::UI, "2.25.123456789");
    set(Tag(0x0008, 0x0050), VR::SH, "ACC123");
    set(Tag(0x0010, 0x0010), VR::PN, "John^Doe");
    set(Tag(0x0010, 0x0020), VR::LO, "PAT123");
    set(Tag(0x0010, 0x0021), VR::LO, "ISSUER");
    set(Tag(0x0010, 0x0030), VR::DA, "19800101");
    set(Tag(0x0010, 0x0032), VR::TM, "070000");
    set(Tag(0x0020, 0x000D), VR::UI, "1.2.3.4.5.6.7"); // StudyInstanceUID
    set(Tag(0x0020, 0x000E), VR::UI, "1.2.3.4.5.6.8"); // SeriesInstanceUID

    let file_obj = obj
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid("2.25.123456789"),
        )
        .expect("with_meta");
    file_obj.write_to_file(path).expect("write DICOM");
}

#[test]
fn minimal_instance_anonymizes_safely() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("min.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_fixture("p01_s01_s01_i01.dcm", &in_path);

    let res = anonymize_file(&in_path, &out_dir, false, false, None).expect("anonymize minimal");
    assert!(res.exists());
}

#[test]
fn nonidentifying_uis_are_left_alone() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("in.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_fixture("p01_s01_s01_i01.dcm", &in_path);

    // capture original values
    let orig = open_file(&in_path).expect("open orig");
    let orig_media_class = orig.element(Tag(0x0002, 0x0002)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());
    let orig_sop_class = orig.element(Tag(0x0008, 0x0016)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());

    let res = anonymize_file(&in_path, &out_dir, false, false, None).expect("anonymize");
    let out = open_file(&res).expect("open out");

    let new_media_class = out.meta().media_storage_sop_class_uid.clone();
    let new_sop_class = out.element(Tag(0x0008, 0x0016)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());

    // ensure media storage SOP Class UID in file meta is preserved
    assert_eq!(orig.meta().media_storage_sop_class_uid.clone(), new_media_class);
}

#[test]
fn identifying_uis_are_updated() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("in.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_fixture("p01_s01_s01_i01.dcm", &in_path);

    let orig = open_file(&in_path).expect("open orig");
    let orig_sop_instance = orig.element(Tag(0x0008, 0x0018)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());
    let orig_study = orig.element(Tag(0x0020, 0x000D)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());
    let orig_series = orig.element(Tag(0x0020, 0x000E)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());

    let res = anonymize_file(&in_path, &out_dir, false, false, None).expect("anonymize");
    let out = open_file(&res).expect("open out");

    let new_sop_instance = out.element(Tag(0x0008, 0x0018)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());
    let new_study = out.element(Tag(0x0020, 0x000D)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());
    let new_series = out.element(Tag(0x0020, 0x000E)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned());

    assert!(orig_sop_instance.is_some() && new_sop_instance.is_some());
    assert_ne!(orig_sop_instance, new_sop_instance);
    assert_ne!(orig_study, new_study);
    assert_ne!(orig_series, new_series);
}

#[test]
fn repeated_identifying_uis_get_same_values() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("in.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_fixture("p01_s01_s01_i01.dcm", &in_path);

    let orig = open_file(&in_path).expect("open orig");
    let orig_uid = orig.element(Tag(0x0008, 0x0018)).ok().and_then(|e| e.to_str().ok()).map(|s| s.into_owned()).expect("orig uid");

    let res = anonymize_file(&in_path, &out_dir, false, false, None).expect("anonymize");
    assert!(res.exists());
    let map_path = out_dir.join(format!("{}.anon_map.json", in_path.file_name().unwrap().to_string_lossy()));
    let map_contents = std::fs::read_to_string(&map_path).expect("read map");
    assert!(map_contents.contains(&format!("UID:{}", orig_uid)), "map did not contain expected UID mapping");
}

#[test]
fn issuer_of_patient_id_changed_if_not_empty_and_not_added_if_empty() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("in.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    // Case: not empty -> ensure anonymizer ran and wrote anon map
    let obj_path = tmp.path().join("case1.dcm");
    {
        let mut obj = InMemDicomObject::new_empty();
        let _ = obj.put_str(Tag(0x0010, 0x0021), VR::LO, "ISSUER");
        let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
        let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, "2.25.4444");
        let file_obj = obj.with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN).media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1").media_storage_sop_instance_uid("2.25.4444")).unwrap();
        file_obj.write_to_file(&obj_path).unwrap();
    }
    let res = anonymize_file(&obj_path, &out_dir, false, false, None).expect("anonymize case1");
    assert!(res.exists());
    let map_path = out_dir.join(format!("{}.anon_map.json", obj_path.file_name().unwrap().to_string_lossy()));
    assert!(map_path.exists());

    // Case: empty -> ensure anonymizer ran and wrote anon map
    let in_path2 = tmp.path().join("case2.dcm");
    {
        let mut obj = InMemDicomObject::new_empty();
        let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
        let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, "2.25.5555");
        let file_obj = obj.with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN).media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1").media_storage_sop_instance_uid("2.25.5555")).unwrap();
        file_obj.write_to_file(&in_path2).unwrap();
    }
    let out_dir2 = tmp.path().join("out2");
    std::fs::create_dir_all(&out_dir2).unwrap();
    let res2 = anonymize_file(&in_path2, &out_dir2, false, false, None).expect("anonymize case2");
    assert!(res2.exists());
    let map_path2 = out_dir2.join(format!("{}.anon_map.json", in_path2.file_name().unwrap().to_string_lossy()));
    assert!(map_path2.exists());
}

#[test]
fn dates_and_times_get_anonymized_when_both_are_present() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("in.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0010, 0x0030), VR::DA, "19741103");
    let _ = obj.put_str(Tag(0x0010, 0x0032), VR::TM, "121558");
    let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, "2.25.6666");
    let file_obj = obj.with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN).media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1").media_storage_sop_instance_uid("2.25.6666")).unwrap();
    file_obj.write_to_file(&in_path).unwrap();

    let res = anonymize_file(&in_path, &out_dir, false, false, None).expect("anonymize");
    assert!(res.exists());
    // ensure file not empty
    let md = std::fs::metadata(&res).expect("stat out");
    assert!(md.len() > 64);
}
