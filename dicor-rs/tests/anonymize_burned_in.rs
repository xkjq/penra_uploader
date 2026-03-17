use dicor_rs::anonymize_file;
use dicom_object::{open_file, FileMetaTableBuilder, InMemDicomObject, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;
use std::fs::copy as copy_file;

fn copy_vendored_fixture(fname: &str, dest: &std::path::Path) {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(fname);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    copy_file(&fixture_path, dest).expect("copy fixture");
}

#[test]
fn burned_in_annotation_is_error_by_default() {
    // If a dataset indicates Burned In Annotation == YES, anonymisation
    // should fail by default (unless explicitly permitted). This test is
    // based on the dicognito `test_burned_in_annotation_fail` behaviour.
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("burned_in_yes.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_vendored_fixture("burned_in_yes.dcm", &in_path);

    // expect an error (library to be updated to return Err on burned-in)
    let res = anonymize_file(&in_path, &out_dir, false, false, false, None);
    assert!(res.is_err(), "Expected anonymize_file to error for burned-in image");
}

// Additional tests to cover allowed and missing/other values.

#[test]
fn burned_in_annotation_when_permitted_allows_anonymisation() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("burned_in_yes.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_vendored_fixture("burned_in_yes.dcm", &in_path);

    // permit_burned_in = true should allow anonymisation to proceed
    let res = anonymize_file(&in_path, &out_dir, false, false, true, None);
    assert!(res.is_ok(), "Expected anonymize_file to succeed when burned-in is permitted");
    let out_path = res.unwrap();
    assert!(out_path.exists(), "output file should exist");
}

#[test]
fn burned_in_annotation_no_allows_anonymisation() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("burned_in_no.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_vendored_fixture("burned_in_no.dcm", &in_path);

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None);
    assert!(res.is_ok(), "Expected anonymize_file to succeed for burned_in=NO");
    let out_path = res.unwrap();
    assert!(out_path.exists());
}

#[test]
fn burned_in_annotation_missing_allows_anonymisation() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("burned_in_missing.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_vendored_fixture("burned_in_missing.dcm", &in_path);

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None);
    assert!(res.is_ok(), "Expected anonymize_file to succeed when Burned In tag is missing");
    let out_path = res.unwrap();
    assert!(out_path.exists());
}

#[test]
fn burned_in_annotation_other_value_allows_anonymisation() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("burned_in_other.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    copy_vendored_fixture("burned_in_other.dcm", &in_path);

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None);
    assert!(res.is_ok(), "Expected anonymize_file to succeed for non-YES burned-in values");
    let out_path = res.unwrap();
    assert!(out_path.exists());
}
