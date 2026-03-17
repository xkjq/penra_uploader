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

// The tests below mirror dicognito behaviour and are placeholders to be
// enabled once the library supports the corresponding CLI/config options.

#[test]
#[ignore]
fn burned_in_annotation_when_permitted_allows_anonymisation() {
    // Placeholder: when a "permit burned-in" flag is provided, anonymisation
    // should proceed. To be implemented after adding option to API.
}

#[test]
#[ignore]
fn burned_in_annotation_warn_logs_warning_but_allows() {
    // Placeholder for "warn" behaviour.
}

#[test]
#[ignore]
fn burned_in_annotation_never_allows_all() {
    // Placeholder for "never"/assume-burned settings.
}
