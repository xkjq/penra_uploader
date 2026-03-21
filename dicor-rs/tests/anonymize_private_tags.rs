use dicor_rs::anonymize_file;
use dicom_object::{open_file, InMemDicomObject, FileMetaTableBuilder, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

fn make_private_test_file(path: &std::path::Path) {
    let mut obj = InMemDicomObject::new_empty();

    let mut set = |tag: Tag, vr: VR, val: &str| {
        let _ = obj.put_str(tag, vr, val);
    };

    // ensure study UID exists so offsets are deterministic
    set(Tag(0x0020,0x000D), VR::UI, "1.2.840.113619.2.55.3.604688432.783.1582036717.467");

    // private-group tags (odd group) with date/time VRs
    set(Tag(0x0011,0x1000), VR::DA, "20200101"); // private date
    set(Tag(0x0011,0x1001), VR::TM, "120000"); // private time

    // Minimal required UIDs
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.123456789");

    let file_obj = obj
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid("2.25.123456789"),
        )
        .expect("with_meta");
    let _ = file_obj.write_to_file(path).expect("write DICOM");
}

#[test]
fn private_tags_preserved_and_shifted() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("in_private.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    make_private_test_file(&in_path);

    // call anonymizer preserving private tags
    let res = anonymize_file(&in_path, &out_dir, false, true, false, None).expect("anonymize");

    let obj = open_file(&res).expect("open result");
    let orig = open_file(&in_path).expect("open orig");

    // private date tag
    let t_date = Tag(0x0011,0x1000);
    if let Ok(orig_el) = orig.element(t_date) {
        let orig_val = orig_el.to_str().map(|s| s.to_string()).unwrap_or_default();
        if let Ok(new_el) = obj.element(t_date) {
            let new_val = new_el.to_str().map(|s| s.to_string()).unwrap_or_default();
            assert!(new_val != orig_val, "Private DA tag was not shifted");
        } else {
            panic!("Private DA tag missing in output");
        }
    }

    // private time tag
    let t_time = Tag(0x0011,0x1001);
    if let Ok(orig_el) = orig.element(t_time) {
        let orig_val = orig_el.to_str().map(|s| s.to_string()).unwrap_or_default();
        if let Ok(new_el) = obj.element(t_time) {
            let new_val = new_el.to_str().map(|s| s.to_string()).unwrap_or_default();
            assert!(new_val != orig_val, "Private TM tag was not shifted");
        } else {
            panic!("Private TM tag missing in output");
        }
    }
}
