use divue_rs::build_key_union;
use std::collections::HashMap;

#[test]
fn test_build_key_union_empty() {
    let comps: Vec<(String, HashMap<String, String>)> = vec![];
    let keys = build_key_union(&comps);
    assert_eq!(keys.len(), 0);
}

#[test]
fn test_build_key_union_single_file() {
    let mut map = HashMap::new();
    map.insert("Key1".to_string(), "Value1".to_string());
    map.insert("Key2".to_string(), "Value2".to_string());
    let comps = vec![("file.dcm".to_string(), map)];

    let keys = build_key_union(&comps);
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"Key1".to_string()));
    assert!(keys.contains(&"Key2".to_string()));
}

#[test]
fn test_build_key_union_multiple_files() {
    let mut map1 = HashMap::new();
    map1.insert("PatientName".to_string(), "John Doe".to_string());
    map1.insert("PatientID".to_string(), "123456".to_string());
    map1.insert("Modality".to_string(), "CT".to_string());

    let mut map2 = HashMap::new();
    map2.insert("PatientName".to_string(), "John Doe".to_string());
    map2.insert("PatientID".to_string(), "654321".to_string());
    map2.insert("Modality".to_string(), "MRI".to_string());
    map2.insert("StudyDate".to_string(), "2026-03-17".to_string());

    let comps = vec![
        ("file1.dcm".to_string(), map1),
        ("file2.dcm".to_string(), map2),
    ];

    let keys = build_key_union(&comps);

    // Should have union of all unique keys: PatientName, PatientID, Modality, StudyDate
    assert_eq!(keys.len(), 4);
    assert!(keys.contains(&"PatientName".to_string()));
    assert!(keys.contains(&"PatientID".to_string()));
    assert!(keys.contains(&"Modality".to_string()));
    assert!(keys.contains(&"StudyDate".to_string()));
}

#[test]
fn test_build_key_union_preserves_order() {
    let mut map1 = HashMap::new();
    map1.insert("A".to_string(), "1".to_string());
    map1.insert("B".to_string(), "2".to_string());

    let mut map2 = HashMap::new();
    map2.insert("C".to_string(), "3".to_string());

    let comps = vec![
        ("file1".to_string(), map1),
        ("file2".to_string(), map2),
    ];

    let keys = build_key_union(&comps);
    // Should contain A, B, C (order not guaranteed due to HashMap)
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&"A".to_string()));
    assert!(keys.contains(&"B".to_string()));
    assert!(keys.contains(&"C".to_string()));
}
