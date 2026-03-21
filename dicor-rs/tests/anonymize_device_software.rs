use dicor_rs::anonymize_file;
use dicom_object::{FileMetaTableBuilder, InMemDicomObject, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

#[test]
fn device_and_software_tags_are_anonymised() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("device.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0008, 0x0070), VR::LO, "Acme Medical"); // Manufacturer
    let _ = obj.put_str(Tag(0x0008, 0x1090), VR::LO, "Model X"); // ManufacturerModelName
    let _ = obj.put_str(Tag(0x0018, 0x1000), VR::LO, "SN-12345"); // DeviceSerialNumber
    let _ = obj.put_str(Tag(0x0018, 0x1020), VR::LO, "v1.2.3"); // SoftwareVersions

    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.7777");

    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
            .media_storage_sop_instance_uid("2.25.7777"),
    ).expect("with_meta");
    file_obj.write_to_file(&in_path).expect("write input");

    // sanity: input contains strings
    let in_bytes = std::fs::read(&in_path).expect("read input");
    let in_text = String::from_utf8_lossy(&in_bytes).to_lowercase();
    for s in &["acme medical", "model x", "sn-12345", "v1.2.3"] {
        assert!(in_text.contains(s), "input should contain '{}'", s);
    }

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None).expect("anonymize");
    let out_bytes = std::fs::read(&res).expect("read out");
    let out_text = String::from_utf8_lossy(&out_bytes).to_lowercase();

    // ManufacturerModelName (0008,1090) is preserved (but sanitized), the others should be removed
    assert!(out_text.contains("model x"), "expected model x to be preserved/sanitized");
    for s in &["acme medical", "sn-12345", "v1.2.3"] {
        assert!(!out_text.contains(s), "found '{}' in anonymised output", s);
    }
}
