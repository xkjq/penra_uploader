use dicor_rs::anonymize_file;
use dicom_object::{InMemDicomObject, FileMetaTableBuilder, Tag, open_file};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

fn make_instance_file(path: &std::path::Path, patient: u32, study: u32, series: u32, instance: u32) {
    let mut obj = InMemDicomObject::new_empty();

    let study_uid = format!("1.3.6.1.4.1.5962.20040827145012.5458.{}.{}", patient, study);
    let series_uid = format!("{}.{}", study_uid, series);
    let sop_uid = format!("{}.{}", series_uid, instance);

    let _ = obj.put_str(Tag(0x0010,0x0020), VR::LO, &format!("4MR{}", patient)); // PatientID
    let _ = obj.put_str(Tag(0x0010,0x0010), VR::PN, &format!("CompressedSamples^MR{}", patient));
    let _ = obj.put_str(Tag(0x0020,0x000D), VR::UI, &study_uid);
    let _ = obj.put_str(Tag(0x0020,0x000E), VR::UI, &series_uid);
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, &sop_uid);
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.4");

    // instance meta
    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.4")
            .media_storage_sop_instance_uid(&sop_uid),
    ).expect("with_meta");

    file_obj.write_to_file(path).expect("write instance");
}

#[test]
fn test_one_series_two_instances_anonymize_consistently() {
    let tmp = tempdir().expect("tempdir");
    let in1 = tmp.path().join("p1_s1_ser1_i1.dcm");
    let in2 = tmp.path().join("p1_s1_ser1_i2.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    make_instance_file(&in1, 1, 1, 1, 1);
    make_instance_file(&in2, 1, 1, 1, 2);

    // anonymize with same seed to ensure deterministic mapping across files
    let out1 = anonymize_file(&in1, &out_dir, false, false, Some("")).expect("anonymize1");
    let out2 = anonymize_file(&in2, &out_dir, false, false, Some("")).expect("anonymize2");

    let o1 = open_file(&out1).expect("open out1");
    let o2 = open_file(&out2).expect("open out2");

    // patient/study/series fields should match
    let p1 = o1.element(Tag(0x0010,0x0010)).unwrap().to_str().unwrap();
    let p2 = o2.element(Tag(0x0010,0x0010)).unwrap().to_str().unwrap();
    assert_eq!(p1, p2);

    let study1 = o1.element(Tag(0x0020,0x000D)).unwrap().to_str().unwrap();
    let study2 = o2.element(Tag(0x0020,0x000D)).unwrap().to_str().unwrap();
    assert_eq!(study1, study2);

    let series1 = o1.element(Tag(0x0020,0x000E)).unwrap().to_str().unwrap();
    let series2 = o2.element(Tag(0x0020,0x000E)).unwrap().to_str().unwrap();
    assert_eq!(series1, series2);

    // instance UIDs should differ
    let i1 = o1.element(Tag(0x0008,0x0018)).unwrap().to_str().unwrap();
    let i2 = o2.element(Tag(0x0008,0x0018)).unwrap().to_str().unwrap();
    assert_ne!(i1, i2);
}
