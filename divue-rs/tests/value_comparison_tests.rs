use divue_rs::values_are_same;
use std::collections::HashMap;

fn sample_metadata() -> Vec<(String, HashMap<String, String>)> {
    let mut map1 = HashMap::new();
    map1.insert("PatientName".to_string(), "John Doe".to_string());
    map1.insert("PatientID".to_string(), "123456".to_string());
    map1.insert("Modality".to_string(), "CT".to_string());

    let mut map2 = HashMap::new();
    map2.insert("PatientName".to_string(), "John Doe".to_string());
    map2.insert("PatientID".to_string(), "654321".to_string());
    map2.insert("Modality".to_string(), "MRI".to_string());
    map2.insert("StudyDate".to_string(), "2026-03-17".to_string());

    vec![
        ("file1.dcm".to_string(), map1),
        ("file2.dcm".to_string(), map2),
    ]
}

#[test]
fn test_values_are_same_identical() {
    let mut map1 = HashMap::new();
    map1.insert("Key".to_string(), "Value".to_string());

    let mut map2 = HashMap::new();
    map2.insert("Key".to_string(), "Value".to_string());

    let comps = vec![
        ("file1".to_string(), map1),
        ("file2".to_string(), map2),
    ];

    assert!(values_are_same("Key", &comps));
}

#[test]
fn test_values_are_same_different() {
    let comps = sample_metadata();
    assert!(!values_are_same("PatientID", &comps)); // 123456 vs 654321
}

#[test]
fn test_values_are_same_missing_in_one_file() {
    let comps = sample_metadata();
    assert!(!values_are_same("StudyDate", &comps)); // Missing in file1
}

#[test]
fn test_values_are_same_single_file() {
    let mut map = HashMap::new();
    map.insert("Key".to_string(), "Value".to_string());
    let comps = vec![("file".to_string(), map)];

    assert!(values_are_same("Key", &comps));
}
