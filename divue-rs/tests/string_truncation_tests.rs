use divue_rs::truncate_string;

#[test]
fn test_truncate_string_no_truncation_needed() {
    let text = "Short text";
    let result = truncate_string(text, 20);
    assert_eq!(result, "Short text");
}

#[test]
fn test_truncate_string_truncate() {
    let text = "This is a long text that needs to be truncated";
    let result = truncate_string(text, 20);
    // Characters 0-19 are "This is a long text" (with trailing space at position 19)
    assert!(result.starts_with("This is a long text"));
    assert!(result.ends_with("..."));
    assert!(result.len() < text.len());
}

#[test]
fn test_truncate_string_empty() {
    let text = "";
    let result = truncate_string(text, 10);
    assert_eq!(result, "");
}

#[test]
fn test_truncate_string_exact_length() {
    let text = "Exact";
    let result = truncate_string(text, 5);
    assert_eq!(result, "Exact");
}

#[test]
fn test_truncate_string_one_char_over() {
    let text = "Exact!";
    let result = truncate_string(text, 5);
    assert_eq!(result, "Exact...");
}
