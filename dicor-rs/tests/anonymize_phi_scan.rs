use dicor_rs::anonymize_file;
use dicom_object::{FileMetaTableBuilder, InMemDicomObject, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

#[test]
fn no_plaintext_phi_in_output() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("phi_in.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    // Create an in-memory DICOM with clear PHI strings
    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, "Confidential^Alice");
    let _ = obj.put_str(Tag(0x0010, 0x0020), VR::LO, "PAT999");
    let _ = obj.put_str(Tag(0x0010, 0x0030), VR::DA, "19900101");
    let _ = obj.put_str(Tag(0x0010, 0x2154), VR::LO, "555-123-4567");
    let _ = obj.put_str(Tag(0x0008, 0x0080), VR::LO, "Hospital X");
    let _ = obj.put_str(Tag(0x0010, 0x1001), VR::PN, "Also^Someone");
    let _ = obj.put_str(Tag(0x0008, 0x0090), VR::PN, "Dr.^Who");
    let _ = obj.put_str(Tag(0x0010, 0x4000), VR::LT, "alice@example.com");

    // ensure SOP UIDs present
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.424242");

    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
            .media_storage_sop_instance_uid("2.25.424242"),
    ).expect("with_meta");

    file_obj.write_to_file(&in_path).expect("write input DICOM");

    // Run anonymiser
    let res = anonymize_file(&in_path, &out_dir, false, false, false, None).expect("anonymize");
    assert!(res.exists());

    // Read output bytes and search for plaintext PHI substrings
    let out_bytes = std::fs::read(&res).expect("read output");
    let out_text = String::from_utf8_lossy(&out_bytes).to_lowercase();

    let forbidden = vec![
        "confidential^alice",
        "pat999",
        "19900101",
        "555-123-4567",
        "alice@example.com",
        "dr.^who",
    ];

    for s in forbidden {
        assert!(!out_text.contains(s), "Found forbidden plaintext '{}' in anonymised output", s);
    }
}
