use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use dicom_object::{FileMetaTableBuilder, InMemDicomObject, Tag};
use dicor_rs::{anonymize_object_with_profile, AnonymizeProfile};
use tempfile::tempdir;

/// A supplied profile must drive the keep/clear/remove pass, not the compiled
/// defaults: the platform fetches it so upload and site checks cannot drift.
#[test]
fn profile_overrides_keep_clear_and_remove() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("profile.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0008, 0x1030), VR::LO, "Study to clear");
    let _ = obj.put_str(Tag(0x0008, 0x103E), VR::LO, "Series to keep");
    let _ = obj.put_str(Tag(0x0018, 0x1020), VR::LO, "Software 9.9");
    let _ = obj.put_str(Tag(0x0042, 0x0011), VR::OB, "secret");
    let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, "2.25.4242");
    let file_obj = obj
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid("2.25.4242"),
        )
        .expect("with_meta");
    file_obj.write_to_file(&in_path).expect("write input");

    let mut profile = AnonymizeProfile::default();
    profile.keep_tags = vec![
        Tag(0x0008, 0x103E),
        Tag(0x0010, 0x0010),
        Tag(0x0010, 0x0020),
    ];
    profile.clear_tags = vec![Tag(0x0008, 0x1030)];
    profile.remove_tags = vec![Tag(0x0042, 0x0011)];

    let (out_path, _) = anonymize_object_with_profile(
        dicom_object::open_file(&in_path).expect("open"),
        &in_path,
        &out_dir,
        false,
        false,
        false,
        None,
        &profile,
    )
    .expect("anonymize");

    let out = dicom_object::open_file(&out_path).expect("open output");
    let text = |tag| out.element(tag).ok().and_then(|e| e.to_str().ok()).map(|s| s.to_string());
    assert_eq!(text(Tag(0x0008, 0x103E)).as_deref(), Some("Series to keep"));
    assert!(text(Tag(0x0008, 0x1030)).unwrap_or_default().is_empty());
    // SoftwareVersions is text but not in keep_tags, so it is blanked.
    assert!(text(Tag(0x0018, 0x1020)).unwrap_or_default().is_empty());
    // Explicit remove tag is gone.
    assert!(out.element(Tag(0x0042, 0x0011)).is_err());
}
