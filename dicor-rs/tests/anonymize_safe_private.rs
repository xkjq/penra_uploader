use dicor_rs::anonymize_file;
use dicom_object::{open_file, InMemDicomObject, FileMetaTableBuilder, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

fn make_safe_private_test_file(path: &std::path::Path) {
    let mut obj = InMemDicomObject::new_empty();

    let mut set = |tag: Tag, vr: VR, val: &str| {
        let _ = obj.put_str(tag, vr, val);
    };

    // ensure study UID exists
    set(Tag(0x0020,0x000D), VR::UI, "1.2.3.4.5.6.7.8.9");

    // private creator ID (element in 0x0010..0x00FF)
    set(Tag(0x0011,0x0010), VR::LO, "TEST_CREATOR");

    // safe private attributes (numeric VRs)
    set(Tag(0x0011,0x1010), VR::DS, "1.234");
    set(Tag(0x0011,0x1020), VR::FL, "3.14");

    // unsafe private attribute (binary)
    set(Tag(0x0011,0x1050), VR::OB, "binaryblob");

    // Minimal required UIDs
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.99999");

    let file_obj = obj
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid("2.25.99999"),
        )
        .expect("with_meta");
    let _ = file_obj.write_to_file(path).expect("write DICOM");
}

#[test]
fn safe_private_retention() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("in_safe_private.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    make_safe_private_test_file(&in_path);

    // anonymize preserving private tags
    let res = anonymize_file(&in_path, &out_dir, false, true, false, None).expect("anonymize");

    let obj = open_file(&res).expect("open result");
    let orig = open_file(&in_path).expect("open orig");

    // private creator should be retained
    let creator = Tag(0x0011,0x0010);
    assert!(orig.element(creator).is_ok(), "orig missing creator");
    assert!(obj.element(creator).is_ok(), "creator missing in output");

    // safe DS should be retained
    let ds_tag = Tag(0x0011,0x1010);
    if let Ok(orig_el) = orig.element(ds_tag) {
        let orig_val = orig_el.to_str().unwrap_or_default().to_string();
        if let Ok(new_el) = obj.element(ds_tag) {
            let new_val = new_el.to_str().unwrap_or_default().to_string();
            assert_eq!(orig_val, new_val, "DS safe private changed");
        } else {
            panic!("DS safe private missing in output");
        }
    }

    // safe FL should be retained
    let fl_tag = Tag(0x0011,0x1020);
    assert!(obj.element(fl_tag).is_ok(), "FL safe private missing in output");

    // unsafe OB should be removed
    let ob_tag = Tag(0x0011,0x1050);
    assert!(obj.element(ob_tag).is_err(), "Unsafe OB private tag should be removed");
}
