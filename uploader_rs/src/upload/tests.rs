use super::*;
use tempfile::tempdir;
use std::fs::{self, File};
use std::io::Write;
use std::env;

// Use httpmock to simulate server endpoints for hash-check and upload
use httpmock::MockServer;
use httpmock::Method::POST;

#[test]
fn test_scan_and_upload_with_mock_server() {
    // isolate home/config paths
    let td = tempdir().unwrap();
    env::set_var("HOME", td.path());

    // create anon dir and a single .dcm file
    let anon = td.path().join("anon_dir");
    fs::create_dir_all(&anon).unwrap();
    let file_path = anon.join("test.dcm");
    let mut f = File::create(&file_path).unwrap();
    let _ = f.write_all(b"dummy dicom content");

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
