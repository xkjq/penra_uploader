use dicor_rs::anonymize_file;
use dicom_object::{InMemDicomObject, FileMetaTableBuilder, Tag, open_file};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

fn make_instance_full(path: &std::path::Path, patient: u32, study: u32, series: u32, instance: u32) {
    let mut obj = InMemDicomObject::new_empty();

    let study_uid = format!("1.2.3.4.5.{}.{}", patient, study);
    let series_uid = format!("{}.{}", study_uid, series);
    let sop_uid = format!("{}.{}", series_uid, instance);

    let _ = obj.put_str(Tag(0x0010,0x0020), VR::LO, &format!("PAT{:02}", patient));
    let _ = obj.put_str(Tag(0x0010,0x0010), VR::PN, &format!("LAST^FIRST{}", patient));
    let _ = obj.put_str(Tag(0x0010,0x1000), VR::LO, "OTHERIDS");
    let _ = obj.put_str(Tag(0x0010,0x1001), VR::PN, "OtherNames");
    let _ = obj.put_str(Tag(0x0010,0x1040), VR::LO, "ADDRESS");
    let _ = obj.put_str(Tag(0x0010,0x0030), VR::DA, "20000101");
    let _ = obj.put_str(Tag(0x0010,0x0032), VR::TM, "120000");

    let _ = obj.put_str(Tag(0x0020,0x000D), VR::UI, &study_uid);
    let _ = obj.put_str(Tag(0x0008,0x0050), VR::SH, "ACC123");
    let _ = obj.put_str(Tag(0x0008,0x0020), VR::DA, "20000101");
    let _ = obj.put_str(Tag(0x0008,0x0030), VR::TM, "120000");

    let _ = obj.put_str(Tag(0x0020,0x000E), VR::UI, &series_uid);
    let _ = obj.put_str(Tag(0x0008,0x1070), VR::PN, "Operator^One");
    let _ = obj.put_str(Tag(0x0008,0x1050), VR::PN, "Performer^One");
    let _ = obj.put_str(Tag(0x0020,0x0010), VR::SH, "STUDYID");

    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, &sop_uid);

    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.4")
            .media_storage_sop_instance_uid(&sop_uid),
    ).expect("with_meta");

    file_obj.write_to_file(path).expect("write full instance");
}

#[test]
fn one_study_two_serieses_preserve_patient_and_study() {
    let tmp = tempdir().expect("tempdir");
    let a = tmp.path().join("a.dcm");
    let b = tmp.path().join("b.dcm");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();

    // same patient and study, different series
    make_instance_full(&a, 1, 1, 1, 1);
    make_instance_full(&b, 1, 1, 2, 1);

    let oa = anonymize_file(&a, &out, false, false, false, Some("")).expect("anonymize a");
    let ob = anonymize_file(&b, &out, false, false, false, Some("")).expect("anonymize b");

    let ra = open_file(&oa).expect("open a");
    let rb = open_file(&ob).expect("open b");

    // patient/study equal
    assert_eq!(ra.element(Tag(0x0010,0x0020)).unwrap().to_str().unwrap(), rb.element(Tag(0x0010,0x0020)).unwrap().to_str().unwrap());
    assert_eq!(ra.element(Tag(0x0020,0x000D)).unwrap().to_str().unwrap(), rb.element(Tag(0x0020,0x000D)).unwrap().to_str().unwrap());

    // series differ
    assert_ne!(ra.element(Tag(0x0020,0x000E)).unwrap().to_str().unwrap(), rb.element(Tag(0x0020,0x000E)).unwrap().to_str().unwrap());
}

#[test]
fn two_patients_are_anonymized_differently() {
    let tmp = tempdir().expect("tempdir");
    let a = tmp.path().join("p1.dcm");
    let b = tmp.path().join("p2.dcm");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();

    make_instance_full(&a, 1, 1, 1, 1);
    make_instance_full(&b, 2, 1, 1, 1);

    let oa = anonymize_file(&a, &out, false, false, false, None).expect("anonymize a");
    let ob = anonymize_file(&b, &out, false, false, false, None).expect("anonymize b");

    let ra = open_file(&oa).expect("open a");
    let rb = open_file(&ob).expect("open b");

    // patient ids should differ
    assert_ne!(ra.element(Tag(0x0010,0x0020)).unwrap().to_str().unwrap(), rb.element(Tag(0x0010,0x0020)).unwrap().to_str().unwrap());
    // study instance uid should differ because patient differs in our uid hashing
    assert_ne!(ra.element(Tag(0x0020,0x000D)).unwrap().to_str().unwrap(), rb.element(Tag(0x0020,0x000D)).unwrap().to_str().unwrap());
}
