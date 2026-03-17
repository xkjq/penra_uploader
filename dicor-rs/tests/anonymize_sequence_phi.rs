use dicor_rs::anonymize_file;
use dicom_object::{FileMetaTableBuilder, InMemDicomObject, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

#[test]
fn sequence_contained_phi_is_removed() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("seqphi.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut obj = InMemDicomObject::new_empty();

    // Put nested/person-name style tags that commonly appear in SR/sequence contexts
    let _ = obj.put_str(Tag(0x0040, 0xA123), VR::PN, "Confidential^Alice");
    let _ = obj.put_str(Tag(0x0040, 0xA124), VR::PN, "Nested^Name");

    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.8888");

    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
            .media_storage_sop_instance_uid("2.25.8888"),
    ).expect("with_meta");
    file_obj.write_to_file(&in_path).expect("write input");

    // confirm input contains PHI
    let in_bytes = std::fs::read(&in_path).expect("read input");
    let in_text = String::from_utf8_lossy(&in_bytes).to_lowercase();
    assert!(in_text.contains("confidential^alice"), "input should contain PHI");

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None).expect("anonymize");
    let out_bytes = std::fs::read(&res).expect("read out");
    let out_text = String::from_utf8_lossy(&out_bytes).to_lowercase();

    assert!(!out_text.contains("confidential^alice"), "PHI remained in output");
}
