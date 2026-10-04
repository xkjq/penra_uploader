//! Tests for the optional `diviz-rs` viewer integration.

use std::path::PathBuf;

use diface_rs::viewer::{candidates_for_root, collect_output_files, is_viewable, launch_viewer};
use diface_rs::{DefaceReport, SeriesReport};

#[test]
fn candidate_paths_cover_debug_and_release() {
    let root = PathBuf::from("/repo");
    let cands = candidates_for_root(&root);
    let strings: Vec<String> = cands
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    assert!(
        strings
            .iter()
            .any(|s| s.ends_with("diviz-rs/target/debug/diviz-rs")),
        "expected a debug candidate, got {strings:?}"
    );
    assert!(
        strings
            .iter()
            .any(|s| s.ends_with("diviz-rs/target/release/diviz-rs")),
        "expected a release candidate, got {strings:?}"
    );
}

#[test]
fn viewable_extension_filter() {
    assert!(is_viewable(&PathBuf::from("a.dcm")));
    assert!(is_viewable(&PathBuf::from("a.DICOM")));
    assert!(is_viewable(&PathBuf::from("IM0001")));
    assert!(!is_viewable(&PathBuf::from("readme.txt")));
}

#[test]
fn collect_output_files_filters_and_dedups() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("slice_1.dcm"), b"x").unwrap();
    std::fs::write(dir.join("slice_2.dcm"), b"x").unwrap();
    std::fs::write(dir.join("notes.txt"), b"x").unwrap();

    let report = DefaceReport {
        series: vec![
            SeriesReport {
                uid: "1.2.3".to_string(),
                modality: "CT".to_string(),
                slices: 2,
                removed_voxels: 1,
                total_voxels: 10,
                output_dir: Some(dir.to_path_buf()),
            },
            // A second report pointing at the same directory must not duplicate.
            SeriesReport {
                uid: "1.2.4".to_string(),
                modality: "CT".to_string(),
                slices: 2,
                removed_voxels: 1,
                total_voxels: 10,
                output_dir: Some(dir.to_path_buf()),
            },
        ],
        warnings: Vec::new(),
    };

    let files = collect_output_files(&report);
    assert_eq!(
        files.len(),
        2,
        "expected only the two .dcm files, got {files:?}"
    );
    assert!(files
        .iter()
        .all(|f| f.extension().and_then(|e| e.to_str()) == Some("dcm")));
}

#[test]
fn launch_with_no_files_errors() {
    assert!(launch_viewer(&[]).is_err());
}

#[cfg(unix)]
#[test]
fn launch_honours_viewer_override() {
    // /bin/true accepts any arguments and exits immediately.
    std::env::set_var("DIVACE_VIEWER", "/bin/true");
    let files = vec![PathBuf::from("/tmp/does_not_need_to_exist.dcm")];
    let result = launch_viewer(&files);
    std::env::remove_var("DIVACE_VIEWER");
    assert!(result.is_ok(), "override viewer should launch: {result:?}");
}
