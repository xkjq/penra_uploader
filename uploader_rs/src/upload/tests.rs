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

#[test]
fn test_calculate_pixel_hash_jpegls_matches_uncompressed() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../test_dicoms/head/IMG_000.dcm");
    if !src.exists() {
        eprintln!("Sample DICOM not found at {:?}, skipping test", src);
        return;
    }

    let td = tempdir().unwrap();
    let anon_path = dicor_rs::anonymize_file(&src, td.path(), false, false, false, None)
        .expect("anonymize and compress to jpegls");

    // Check that anonymized file has JPEG-LS transfer syntax
    let anon_obj = open_file(&anon_path).expect("open anon");
    let ts = anon_obj.meta().transfer_syntax();
    assert_eq!(ts, "1.2.840.10008.1.2.4.80", "Transfer syntax should be JPEG-LS Lossless");

    // Check compression ratio: compressed file should be significantly smaller
    let orig_size = std::fs::metadata(&src).unwrap().len();
    let anon_size = std::fs::metadata(&anon_path).unwrap().len();
    assert!(anon_size < orig_size, "Compressed size ({}) should be < original size ({})", anon_size, orig_size);

    // Calculate pixel hash on original uncompressed vs compressed
    let raw_hash = calculate_pixel_hash(&src).expect("raw pixel hash");
    let jpegls_hash = calculate_pixel_hash(&anon_path).expect("jpegls pixel hash");

    assert_eq!(raw_hash, jpegls_hash, "Pixel hash of uncompressed and JPEG-LS compressed must match");
}

#[test]
fn test_pixel_hash_cache_validates_size_and_mtime() {
    let td = tempdir().unwrap();
    let f = td.path().join("cache_probe.bin");
    fs::write(&f, b"hello").unwrap();

    let md = std::fs::metadata(&f).unwrap();
    let size = md.len();
    let mtime = file_mtime_secs(&md);

    assert!(get_cached_pixel_hash(&f, size, mtime).is_none());

    cache_pixel_hash(&f, "abc123");
    assert_eq!(get_cached_pixel_hash(&f, size, mtime).as_deref(), Some("abc123"));

    // Any change to size or mtime must invalidate the entry.
    assert!(get_cached_pixel_hash(&f, size + 1, mtime).is_none());
    assert!(get_cached_pixel_hash(&f, size, mtime + 1).is_none());

    evict_cached_pixel_hash(&f);
    assert!(get_cached_pixel_hash(&f, size, mtime).is_none());
}

#[test]
fn test_ready_meta_cache_fast_path() {
    let td = tempdir().unwrap();
    let f = td.path().join("meta_cache.dcm");
    make_minimal_dcm(&f, "2.25.9999", "Doe^John");

    // Populate the metadata cache from an already-open object.
    let obj = open_file(&f).expect("open");
    cache_ready_file_from_obj(&f, &obj, Some("deadbeef".to_string()));

    let md = std::fs::metadata(&f).unwrap();
    let size = md.len();
    let mtime = file_mtime_secs(&md);

    let cached = get_cached_ready_meta(&f, size, mtime).expect("cached meta");
    assert_eq!(cached.hash, "deadbeef");
    assert_eq!(cached.series_uid, "NO_SERIES");
    // A different size/mtime must invalidate the cached entry.
    assert!(get_cached_ready_meta(&f, size + 1, mtime).is_none());

    // The scan's upsert should take the fast path and still succeed.
    upsert_ready_file_internal(&f).expect("upsert from cache");

    // Eviction clears the entry.
    evict_ready_meta(&f);
    assert!(get_cached_ready_meta(&f, size, mtime).is_none());
}

#[test]
fn test_upload_chunk_parses_response() {
    let td = tempdir().unwrap();
    let f1 = td.path().join("a.dcm");
    let f2 = td.path().join("b.dcm");
    fs::write(&f1, b"x").unwrap();
    fs::write(&f2, b"y").unwrap();

    let server = MockServer::start();
    let _m = server.mock(|when, then| {
        when.method(POST).path("/api/atlas/upload_dicom");
        then.status(200).json_body_obj(&serde_json::json!({
            "uploaded": [["a.dcm", "h1"], ["b.dcm", "h2"]],
            "duplicates": [],
            "failed": [],
            "duplicate_series": []
        }));
    });

    let client = make_client(None).expect("client");
    let endpoint = format!("{}/api/atlas/upload_dicom", server.url(""));
    let pairs = vec![
        (f1, "a.dcm".to_string()),
        (f2, "b.dcm".to_string()),
    ];

    let out = upload_chunk(&client, &endpoint, &pairs);
    assert!(out.succeeded, "chunk should succeed");
    assert_eq!(out.uploaded.len(), 2);
    assert!(out.duplicates.is_empty());
    assert!(!out.saw_timeout);
}


