use super::*;
use tempfile::tempdir;
use std::fs;
use std::env;
use dicom_object::{InMemDicomObject, FileMetaTableBuilder, Tag};
use dicom_core::header::VR;

const EXPLICIT_VR_LE_UID: &str = "1.2.840.10008.1.2.1";

// Use httpmock to simulate server endpoints for hash-check and upload
use httpmock::MockServer;
use httpmock::Method::POST;

fn make_minimal_dcm(path: &std::path::Path, sop_instance: &str, patient: &str) {
    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, sop_instance);
    let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, patient);
    let file_obj = obj
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(EXPLICIT_VR_LE_UID)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid(sop_instance),
        )
        .expect("with_meta");
    file_obj.write_to_file(path).expect("write dcm");
}

#[test]
fn test_scan_and_upload_with_mock_server() {
    // isolate home/config paths
    let td = tempdir().unwrap();
    env::set_var("HOME", td.path());

    // create anon dir and a single .dcm file
    let anon = td.path().join("anon_dir");
    fs::create_dir_all(&anon).unwrap();
    let file_path = anon.join("test.dcm");
    make_minimal_dcm(&file_path, "2.25.777", "Doe^John");

    // start mock server
    let server = MockServer::start();

    // mock the hash check endpoint (scan_for_upload will POST here)
    let _m_check = server.mock(|when, then| {
        when.method(POST).path("/api/atlas/check_image_hashes/");
        then.status(200).body("{}");
    });

    // mock the upload endpoint
    let _m_upload = server.mock(|when, then| {
        when.method(POST).path("/api/atlas/upload_dicom");
        then.status(200).json_body_obj(&serde_json::json!({
            "uploaded": [["test.dcm", "fakehash"]],
            "duplicates": [],
            "failed": [],
            "duplicate_series": []
        }));
    });

    // point the uploader to the mock server
    env::set_var("UPLOADER_BASE_URL", server.url(""));

    // run upload_anon_dir and verify results
    let res = upload_anon_dir(&anon, None, None).expect("upload failed");
    assert_eq!(res.uploaded.len(), 1);
    assert_eq!(res.uploaded[0].0, "test.dcm");

    // file should be deleted after successful upload
    assert!(!file_path.exists());
}
