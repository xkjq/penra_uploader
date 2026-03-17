use divue_rs::filter_keys;
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
fn test_filter_keys_empty_filter() {
    let comps = sample_metadata();
    let keys = vec!["PatientName".to_string(), "PatientID".to_string(), "Modality".to_string(), "StudyDate".to_string()];
    let filtered = filter_keys(&keys, &comps, "");

    assert_eq!(filtered.len(), keys.len());
}

#[test]
fn test_filter_keys_by_key_name() {
    let comps = sample_metadata();
    let keys = vec!["PatientName".to_string(), "PatientID".to_string(), "Modality".to_string(), "StudyDate".to_string()];
    let filtered = filter_keys(&keys, &comps, "patient");

    // Should match PatientName and PatientID (case-insensitive)
    assert_eq!(filtered.len(), 2);
    assert!(filtered.contains(&"PatientName".to_string()));
    assert!(filtered.contains(&"PatientID".to_string()));
}

#[test]
fn test_filter_keys_by_value() {
    let comps = sample_metadata();
    let keys = vec!["PatientName".to_string(), "PatientID".to_string(), "Modality".to_string(), "StudyDate".to_string()];
    let filtered = filter_keys(&keys, &comps, "ct");

    // Should match Modality with value "CT"
    assert!(filtered.len() > 0);
    assert!(filtered.contains(&"Modality".to_string()));
}

#[test]
fn test_filter_keys_case_insensitive() {
    let comps = sample_metadata();
    let keys = vec!["PatientName".to_string(), "PatientID".to_string(), "Modality".to_string(), "StudyDate".to_string()];

    let filtered_lower = filter_keys(&keys, &comps, "modality");
    let filtered_upper = filter_keys(&keys, &comps, "MODALITY");

    assert_eq!(filtered_lower, filtered_upper);
    assert!(filtered_lower.contains(&"Modality".to_string()));
}

#[test]
fn test_filter_keys_no_match() {
    let comps = sample_metadata();
    let keys = vec!["PatientName".to_string(), "PatientID".to_string(), "Modality".to_string(), "StudyDate".to_string()];
    let filtered = filter_keys(&keys, &comps, "nonexistent");

    assert_eq!(filtered.len(), 0);
}
