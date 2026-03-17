use dicor_rs::anonymize_file;
use tempfile::tempdir;
use std::io::Write;

#[test]
fn non_dicom_file_errors() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("not_a_dicom.txt");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut f = std::fs::File::create(&in_path).expect("create file");
    writeln!(f, "this is plain text, not a DICOM").expect("write");

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None);
    assert!(res.is_err(), "Expected anonymize_file to error for non-DICOM file");
    // Ensure no output file was produced
    let entries = std::fs::read_dir(&out_dir).unwrap().count();
    assert_eq!(entries, 0, "output directory should be empty on error");
}

#[test]
fn empty_file_errors() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("empty.bin");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    // create empty file
    std::fs::File::create(&in_path).expect("create empty");

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None);
    assert!(res.is_err(), "Expected anonymize_file to error for empty file");
    let entries = std::fs::read_dir(&out_dir).unwrap().count();
    assert_eq!(entries, 0, "output directory should be empty on error");
}

#[test]
fn truncated_file_errors() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("truncated.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    // write a few bytes that cannot form a valid DICOM
    std::fs::write(&in_path, &[0u8, 1u8, 2u8, 3u8]).expect("write truncated");

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None);
    assert!(res.is_err(), "Expected anonymize_file to error for truncated file");
    let entries = std::fs::read_dir(&out_dir).unwrap().count();
    assert_eq!(entries, 0, "output directory should be empty on error");
}
