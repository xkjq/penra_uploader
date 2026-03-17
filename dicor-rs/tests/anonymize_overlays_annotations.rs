use dicor_rs::anonymize_file;
use dicom_object::{FileMetaTableBuilder, InMemDicomObject, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

#[test]
fn overlays_and_annotations_removed_by_default() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("overlay_ann.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut obj = InMemDicomObject::new_empty();

    // overlay group example (6000)
    let _ = obj.put_str(Tag(0x6000, 0x0010), VR::LO, "10"); // OverlayRows (as string)
    let _ = obj.put_str(Tag(0x6000, 0x3000), VR::OB, "overlaydata"); // OverlayData

    // annotation/presentation-like group (0070)
    let _ = obj.put_str(Tag(0x0070, 0x0001), VR::LO, "GraphicAnnotation");

    // Content Sequence tag included in clear_tags
    let _ = obj.put_str(Tag(0x0040, 0xA730), VR::LO, "ContentSeqValue");

    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.9999");

    let file_obj = obj.with_meta(
        FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
            .media_storage_sop_instance_uid("2.25.9999"),
    ).expect("with_meta");
    file_obj.write_to_file(&in_path).expect("write input");

    // Sanity: input contains these strings
    let in_bytes = std::fs::read(&in_path).expect("read input");
    let in_text = String::from_utf8_lossy(&in_bytes).to_lowercase();
    for s in &["overlaydata", "graphicannotation", "contentseqvalue"] {
        assert!(in_text.contains(s), "input should contain '{}'", s);
    }

    // Run anonymiser
    let res = anonymize_file(&in_path, &out_dir, false, false, false, None).expect("anonymize");
    let out_bytes = std::fs::read(&res).expect("read out");
    let out_text = String::from_utf8_lossy(&out_bytes).to_lowercase();

    // Ensure overlays/annotations/content sequence values not present in output
    for s in &["overlaydata", "graphicannotation", "contentseqvalue"] {
        assert!(!out_text.contains(s), "found '{}' in anonymised output", s);
    }
}
