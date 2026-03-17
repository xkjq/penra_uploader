# Divue-rs Test Suite

The `divue-rs` crate includes a comprehensive test suite covering the core functionality of the metadata viewer. Tests are organized in separate files in the `tests/` directory.

## Running Tests

```bash
cd divue-rs
cargo test                  # Run all tests
cargo test --lib           # Run library tests only
cargo test -- --nocapture  # Show output
cargo test -- --test-threads=1  # Run tests sequentially
```

## Test Organization

Tests are organized into 4 separate test files in `tests/`:

### 1. `key_union_tests.rs` - Key Union Building (4 tests)

- **test_build_key_union_empty**: Verifies behavior with no metadata files
- **test_build_key_union_single_file**: Tests union with a single metadata map
- **test_build_key_union_multiple_files**: Ensures union correctly combines keys from multiple files
- **test_build_key_union_preserves_order**: Verifies that keys are collected in consistent order

Run only these tests:
```bash
cargo test --test key_union_tests
```

### 2. `filter_tests.rs` - Key Filtering (5 tests)

- **test_filter_keys_empty_filter**: Verifies empty filter returns all keys
- **test_filter_keys_by_key_name**: Tests filtering by DICOM tag name (e.g., "PatientID")
- **test_filter_keys_by_value**: Tests filtering by metadata value (e.g., "CT" modality)
- **test_filter_keys_case_insensitive**: Ensures filter matching is case-insensitive
- **test_filter_keys_no_match**: Verifies empty result when no keys match filter

Run only these tests:
```bash
cargo test --test filter_tests
```

### 3. `value_comparison_tests.rs` - Value Comparison (4 tests)

- **test_values_are_same_identical**: Tests detection when all files have identical values
- **test_values_are_same_different**: Tests detection when values differ across files
- **test_values_are_same_missing_in_one_file**: Tests detection when a key is missing in some files
- **test_values_are_same_single_file**: Verifies behavior with a single file

Run only these tests:
```bash
cargo test --test value_comparison_tests
```

### 4. `string_truncation_tests.rs` - String Truncation (5 tests)

- **test_truncate_string_no_truncation_needed**: Short strings are not modified
- **test_truncate_string_truncate**: Long strings are truncated with ellipsis
- **test_truncate_string_empty**: Empty strings remain empty
- **test_truncate_string_exact_length**: Strings matching max length are unchanged
- **test_truncate_string_one_char_over**: Strings exceeding max length by 1 are truncated

Run only these tests:
```bash
cargo test --test string_truncation_tests
```

## Test Functions Covered

The test suite validates these public functions:

- `build_key_union()` - Creates union of metadata keys from multiple files
- `filter_keys()` - Filters keys by search term (matches key names and values)
- `values_are_same()` - Detects if a value is identical across all files
- `truncate_string()` - Truncates strings with ellipsis for display

## Test Data

Tests use a sample metadata structure with two simulated DICOM files:

**File 1: file1.dcm**
- PatientName: "John Doe"
- PatientID: "123456"
- Modality: "CT"

**File 2: file2.dcm**
- PatientName: "John Doe" (same)
- PatientID: "654321" (different)
- Modality: "MRI" (different)
- StudyDate: "2026-03-17" (unique to file2)

This structure allows testing:
- Identical values (PatientName)
- Different values across files (PatientID, Modality)
- Missing keys in some files (StudyDate)

## File Structure

```
divue-rs/
├── src/
│   ├── lib.rs        # Core library code (no tests)
│   └── main.rs       # Binary entry point
├── tests/
│   ├── filter_tests.rs               # 5 tests
│   ├── key_union_tests.rs            # 4 tests
│   ├── string_truncation_tests.rs    # 5 tests
│   └── value_comparison_tests.rs     # 4 tests
└── TESTS.md          # This file
```

## Test Results

All **18 tests** pass:
- ✅ `filter_tests.rs`: 5/5 passed
- ✅ `key_union_tests.rs`: 4/4 passed
- ✅ `string_truncation_tests.rs`: 5/5 passed
- ✅ `value_comparison_tests.rs`: 4/4 passed

## Integration with CI/CD

To add these tests to a CI/CD pipeline:

```bash
# Run tests and generate coverage report
cargo test --test '*' --cov

# Run tests with verbose output
cargo test -- --nocapture --test-threads=1
```

## Adding New Tests

When adding new functionality to `divue-rs`, create a new test file in `tests/`:

```rust
// tests/new_feature_tests.rs
use divue_rs::new_function;
use std::collections::HashMap;

#[test]
fn test_new_functionality() {
    // Setup
    let input = ...;
    
    // Execute
    let result = new_function(input);
    
    // Assert
    assert_eq!(result, expected_value);
}
```
