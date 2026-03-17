use dicor_rs::anonymize_file;
use dicom_object::{InMemDicomObject, FileMetaTableBuilder, Tag, open_file};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

fn make_instance(path: &std::path::Path, patient: u32, study: u32, series: u32, instance: u32) {
    let mut obj = InMemDicomObject::new_empty();
    let study_uid = format!("1.2.3.4.5.{}.{}", patient, study);
    let series_uid = format!("{}.{}", study_uid, series);
    let sop_uid = format!("{}.{}", series_uid, instance);

    let _ = obj.put_str(Tag(0x0010,0x0010), VR::PN, &format!("Patient^{}", patient));
    let _ = obj.put_str(Tag(0x0010,0x0020), VR::LO, &format!("PID{}", patient));
    let _ = obj.put_str(Tag(0x0020,0x000D), VR::UI, &study_uid);
    let _ = obj.put_str(Tag(0x0020,0x000E), VR::UI, &series_uid);
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, &sop_uid);
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.4");

    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.4")
            .media_storage_sop_instance_uid(&sop_uid),
    ).expect("with_meta");

    file_obj.write_to_file(path).expect("write instance");
}

#[test]
fn one_patient_two_studies() {
    let tmp = tempdir().expect("tempdir");
    let a = tmp.path().join("a.dcm");
    let b = tmp.path().join("b.dcm");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();

    make_instance(&a, 1, 1, 1, 1);
    make_instance(&b, 1, 2, 1, 1);

    let oa = anonymize_file(&a, &out, false, false, false, Some("")).expect("anonymize a");
    let ob = anonymize_file(&b, &out, false, false, false, Some("")).expect("anonymize b");

    let ra = open_file(&oa).expect("open a");
    let rb = open_file(&ob).expect("open b");

    // patient fields should match
    assert_eq!(ra.element(Tag(0x0010,0x0020)).unwrap().to_str().unwrap(), rb.element(Tag(0x0010,0x0020)).unwrap().to_str().unwrap());
    assert_eq!(ra.element(Tag(0x0010,0x0010)).unwrap().to_str().unwrap(), rb.element(Tag(0x0010,0x0010)).unwrap().to_str().unwrap());

    // study/series/instance should differ
    assert_ne!(ra.element(Tag(0x0020,0x000D)).unwrap().to_str().unwrap(), rb.element(Tag(0x0020,0x000D)).unwrap().to_str().unwrap());
    assert_ne!(ra.element(Tag(0x0020,0x000E)).unwrap().to_str().unwrap(), rb.element(Tag(0x0020,0x000E)).unwrap().to_str().unwrap());
    assert_ne!(ra.element(Tag(0x0008,0x0018)).unwrap().to_str().unwrap(), rb.element(Tag(0x0008,0x0018)).unwrap().to_str().unwrap());
}
