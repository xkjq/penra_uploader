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

/// Write a small defaceable CT series (geometry + nose) into `dir`.
fn write_defaceable_series(dir: &std::path::Path) {
    use dicom_core::value::PrimitiveValue;
    use dicom_object::{DefaultDicomObject, Tag};
    let (rows, cols, nz) = (32usize, 32usize, 16usize);
    let spacing = 2.0f64;
    for z in 0..nz {
        let instance = z as u16 + 1;
        let sop = format!("1.2.826.0.1.3680043.10.55.{instance}");
        let meta = FileMetaTableBuilder::new()
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.2")
            .media_storage_sop_instance_uid(&sop)
            .transfer_syntax(EXPLICIT_VR_LE_UID)
            .build()
            .unwrap();
        let mut obj = DefaultDicomObject::new_empty_with_meta(meta);
        let half_x = cols as f64 * spacing / 2.0;
        let half_y = rows as f64 * spacing / 2.0;
        let z_world = z as f64 * spacing - (nz as f64 * spacing) / 2.0;
        let mut data = vec![-1000i16; rows * cols];
        for y in 0..rows {
            for x in 0..cols {
                let wx = -half_x + x as f64 * spacing;
                let wy = -half_y + y as f64 * spacing;
                let e = (wx / 14.0).powi(2) + (wy / 14.0).powi(2) + (z_world / 18.0).powi(2);
                let mut v = -1000i16;
                if e <= 1.0 {
                    v = 40;
                }
                if wy <= -14.0 && wy >= -18.0 && wx.abs() <= 2.0 && z_world.abs() <= 3.0 {
                    v = 40;
                }
                data[y * cols + x] = v;
            }
        }
        let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.2");
        let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, &sop);
        let _ = obj.put_str(Tag(0x0008, 0x0060), VR::CS, "CT");
        let _ = obj.put_str(Tag(0x0020, 0x000D), VR::UI, "1.2.826.0.1.3680043.10.55.0");
        let _ = obj.put_str(Tag(0x0020, 0x000E), VR::UI, "1.2.826.0.1.3680043.10.55.1");
        let _ = obj.put_str(Tag(0x0020, 0x0013), VR::IS, &instance.to_string());
        let _ = obj.put_str(
            Tag(0x0020, 0x0032),
            VR::DS,
            &format!("{:.4}\\{:.4}\\{:.4}", -half_x, -half_y, z_world),
        );
        let _ = obj.put_str(Tag(0x0020, 0x0037), VR::DS, "1\\0\\0\\0\\1\\0");
        let _ = obj.put_str(Tag(0x0028, 0x0030), VR::DS, &format!("{spacing}\\{spacing}"));
        let _ = obj.put(dicom_object::mem::InMemElement::new(Tag(0x0028, 0x0002), VR::US, PrimitiveValue::new_u16(1)));
        let _ = obj.put_str(Tag(0x0028, 0x0004), VR::CS, "MONOCHROME2");
        let _ = obj.put(dicom_object::mem::InMemElement::new(Tag(0x0028, 0x0010), VR::US, PrimitiveValue::new_u16(rows as u16)));
        let _ = obj.put(dicom_object::mem::InMemElement::new(Tag(0x0028, 0x0011), VR::US, PrimitiveValue::new_u16(cols as u16)));
        let _ = obj.put(dicom_object::mem::InMemElement::new(Tag(0x0028, 0x0100), VR::US, PrimitiveValue::new_u16(16)));
        let _ = obj.put(dicom_object::mem::InMemElement::new(Tag(0x0028, 0x0101), VR::US, PrimitiveValue::new_u16(16)));
        let _ = obj.put(dicom_object::mem::InMemElement::new(Tag(0x0028, 0x0102), VR::US, PrimitiveValue::new_u16(15)));
        let _ = obj.put(dicom_object::mem::InMemElement::new(Tag(0x0028, 0x0103), VR::US, PrimitiveValue::new_u16(1)));
        let _ = obj.put_str(Tag(0x0028, 0x1052), VR::DS, "0");
        let _ = obj.put_str(Tag(0x0028, 0x1053), VR::DS, "1");
        let _ = obj.put_str(Tag(0x0018, 0x0088), VR::DS, &format!("{spacing}"));
        let pixel: dicom_object::mem::InMemElement =
            dicom_object::mem::InMemElement::new(Tag(0x7FE0, 0x0010), VR::OW, PrimitiveValue::I16(data.into()));
        let _ = obj.put(pixel);
        obj.write_to_file(dir.join(format!("slice_{instance:03}.dcm")))
            .unwrap();
    }
}

#[test]
fn defacing_changes_pixels_but_keeps_original_cached_hash() {
    let td = tempdir().unwrap();
    let dir = td.path();
    write_defaceable_series(dir);

    let sample = dir.join("slice_009.dcm");
    let original_hash = calculate_pixel_hash(&sample).expect("original pixel hash");
    // Prime the cache as the anonymise step does before defacing.
    cache_pixel_hash(&sample, &original_hash);

    // Deface in place.
    let opts = diface_rs::DefaceOptions {
        subdir_by_series: false,
        remove_original: false,
        ..diface_rs::DefaceOptions::default()
    };
    let report = diface_rs::deface_dir_in_place(dir, &opts).expect("deface");
    assert_eq!(report.series.len(), 1);
    assert!(report.series[0].removed_voxels > 0);

    // Pixels changed, so a fresh hash differs...
    let new_hash = calculate_pixel_hash(&sample).expect("new pixel hash");
    assert_ne!(original_hash, new_hash, "defacing must change the pixels");

    // Recompress the defaced file to JPEG-LS (as the uploader does).
    {
        let mut obj = open_file(&sample).expect("open defaced");
        let compressed = dicor_rs::compress_to_jpegls(&mut obj).expect("compress");
        assert!(compressed, "defaced file should compress");
        obj.write_to_file(&sample).expect("write compressed");
        let re = open_file(&sample).expect("reopen");
        assert_eq!(
            re.meta().transfer_syntax().trim_end_matches(['\0', ' ']),
            "1.2.840.10008.1.2.4.80",
            "should be JPEG-LS after recompression"
        );
    }
    // Recompression is lossless, so the defaced pixels hash is unchanged.
    assert_eq!(
        calculate_pixel_hash(&sample).expect("hash"),
        new_hash,
        "JPEG-LS recompression must be lossless"
    );

    // ...and after re-caching the original, the duplicate-detection hash is
    // still the original pixels' hash.
    cache_pixel_hash(&sample, &original_hash);
    let cached = cached_pixel_hash(&sample).expect("cached hash");
    assert_eq!(cached, original_hash);

    // The ready-file upsert keeps the cached (original) hash too.
    let obj = open_file(&sample).expect("open defaced");
    let info = build_ready_info(&sample, &obj, cached.clone(), 0);
    assert_eq!(info.hash, original_hash);
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


