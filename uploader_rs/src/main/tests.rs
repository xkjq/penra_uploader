use super::*;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

#[test]
fn handle_done_resets_progress() {
    let mut s = AppState::default();
    s.processing_step = Some("Something".to_string());
    s.processing_progress = 0.5;
    s.handle_message("done");
    assert_eq!(s.last_msg, "Processing complete");
    assert!(s.processing_step.is_none());
    assert_eq!(s.processing_progress, 0.0);
}

#[test]
fn handle_proc_step_and_prog_update() {
    let mut s = AppState::default();
    s.handle_message("PROC:STEP:Testing step");
    assert_eq!(s.processing_step.as_deref(), Some("Testing step"));
    assert_eq!(s.last_msg, "Testing step");
    s.handle_message("PROC:PROG:0.42");
    assert!((s.processing_progress - 0.42).abs() < f32::EPSILON);
}

#[test]
fn handle_scan_written_parses_last_scan() {
    // create a temp dir and write .last_scan.json in CWD
    let td = tempdir().unwrap();
    let _cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(td.path()).unwrap();

    // build a fake SeriesInfo JSON
    let series = vec![SeriesInfo {
        series_uid: "S1".to_string(),
        files: vec![],
        duplicate_series_urls: vec![],
        patient_name: Some("P".to_string()),
        examination: None,
        patient_id: None,
        study_date: None,
        modality: None,
        series_description: None,
        series_number: None,
        file_count: 0,
        total_bytes: 0,
    }];
    let mut f = File::create(".last_scan.json").unwrap();
    let j = serde_json::to_string(&series).unwrap();
    f.write_all(j.as_bytes()).unwrap();

    let mut s = AppState::default();
    s.handle_message("scan_written");
    assert_eq!(s.ready_series.len(), 1);
    assert_eq!(s.last_msg, "Ready-to-upload refreshed");
}

#[test]
fn handle_login_user_message() {
    let mut s = AppState::default();
    s.login_open = true;
    s.handle_message("LOGIN_USER:alice");
    assert_eq!(s.logged_in_user.as_deref(), Some("alice"));
    assert_eq!(s.login_open, false);
    assert!(s.last_msg.contains("alice"));
}

#[test]
fn handle_duplicates_cleared_message() {
    let mut s = AppState::default();
    s.handle_message("duplicates_cleared:3");
    assert_eq!(s.last_msg, "Cleared 3 duplicate files");
}

#[test]
fn handle_generic_message_pushes_processed() {
    let mut s = AppState::default();
    s.handle_message("SOME_LOG_ENTRY");
    assert_eq!(s.last_msg, "SOME_LOG_ENTRY");
    assert_eq!(s.processed.last().map(|v| v.as_str()), Some("SOME_LOG_ENTRY"));
}
