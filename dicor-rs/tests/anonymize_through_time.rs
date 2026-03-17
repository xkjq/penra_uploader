use dicor_rs::anonymize_file;
use dicom_object::{InMemDicomObject, FileMetaTableBuilder, Tag, open_file};
use dicom_core::header::{VR, Header};
use dicom_dictionary_std::uids;
use tempfile::tempdir;

fn make_minimal(path: &std::path::Path) {
    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0010,0x0010), VR::PN, "TWO^TIME");
    let _ = obj.put_str(Tag(0x0010,0x0020), VR::LO, "TID");
    let _ = obj.put_str(Tag(0x0008,0x0020), VR::DA, "19740101");
    let _ = obj.put_str(Tag(0x0008,0x0030), VR::TM, "121558");
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.4");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.9999");

    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.4")
            .media_storage_sop_instance_uid("2.25.9999"),
    ).expect("with_meta");

    file_obj.write_to_file(path).expect("write minimal");
}

fn datasets_equal(a: &dicom_object::FileDicomObject<InMemDicomObject>, b: &dicom_object::FileDicomObject<InMemDicomObject>) -> bool {
    for el in a.iter() {
        let t = el.tag();
        if let Ok(be) = b.element(t) {
            if el.to_str().ok() != be.to_str().ok() {
                return false;
            }
        } else {
            return false;
        }
    }
    true
}

#[test]
fn dataset_anonymizes_same_with_same_seed() {
    let tmp = tempdir().expect("tempdir");
    let a = tmp.path().join("a.dcm");
    let b = tmp.path().join("b.dcm");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();

    make_minimal(&a);
    make_minimal(&b);

    let oa = anonymize_file(&a, &out, false, false, Some("SOME_FIXED_SEED")).expect("anonymize a");
    let ob = anonymize_file(&b, &out, false, false, Some("SOME_FIXED_SEED")).expect("anonymize b");

    let ra = open_file(&oa).expect("open a");
    let rb = open_file(&ob).expect("open b");

    assert!(datasets_equal(&ra, &rb));
}
