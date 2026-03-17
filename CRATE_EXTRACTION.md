# DICOM Viewer & Divue-rs - Independent Rust Crates

This extraction creates two standalone Rust crates from the original uploader project:

## Crates

### 1. `dicom_viewer` 
Located at: `/home/ross/penra_uploader/dicom_viewer`

A library for reading DICOM metadata from files.

**Public Functions:**
- `read_metadata(path: &Path) -> Result<HashMap<String, String>, String>` - Read essential DICOM fields
- `read_metadata_all(path: &Path) -> Result<HashMap<String, String>, String>` - Read comprehensive metadata

**Dependencies:**
- `dicom-object` 0.6
- `dicom-core` 0.6

**Building:**
```bash
cd dicom_viewer
cargo build
```

**Usage as a Library:**
Add to your `Cargo.toml`:
```toml
[dependencies]
dicom_viewer = { path = "../dicom_viewer" }
```

Then in your code:
```rust
use dicom_viewer::read_metadata_all;
use std::path::Path;

let metadata = read_metadata_all(Path::new("file.dcm"))?;
for (key, value) in metadata {
    println!("{}: {}", key, value);
}
```

---

### 2. `divue-rs` (DICOM Viewer - Rust)
Located at: `/home/ross/penra_uploader/divue-rs`

A GUI application (and library) for viewing and comparing DICOM metadata from multiple files using egui.

**Public Function:**
- `run_meta_viewer(paths: Vec<String>)` - Launch the metadata viewer GUI

**Dependencies:**
- `eframe` 0.33
- `egui` 0.33
- `dicom_viewer` (local dependency)

**Building:**
```bash
cd divue-rs
cargo build --release
```

**Running as Standalone Application:**
```bash
cd divue-rs
cargo run --release -- /path/to/file1.dcm /path/to/file2.dcm ...
```

Or directly:
```bash
./divue-rs/target/release/divue /path/to/file1.dcm /path/to/file2.dcm ...
```

The GUI allows you to:
- View metadata from multiple DICOM files side-by-side
- Filter metadata by key or value
- Compare values across files (different values highlighted)
- View full metadata values in a popup window

**Usage as a Library:**
Add to your `Cargo.toml`:
```toml
[dependencies]
divue_rs = { package = "divue-rs", path = "../divue-rs" }
```

Then in your code:
```rust
use divue_rs::run_meta_viewer;

let file_paths = vec![
    "/path/to/file1.dcm".to_string(),
    "/path/to/file2.dcm".to_string(),
];

run_meta_viewer(file_paths);
```

---

## Integration with uploader_rs

The `uploader_rs` crate has been updated to use these new crates as dependencies:

```toml
[dependencies]
dicom_viewer = { path = "../dicom_viewer" }
divue_rs = { package = "divue-rs", path = "../divue-rs" }
```

And the imports in `src/main.rs` have been changed from module references to external crate imports:
```rust
use dicom_viewer::{read_metadata, read_metadata_all};
use divue_rs::run_meta_viewer;
```

---

## Future Improvements

These crates can now be:
1. Published to crates.io for public use
2. Used in other projects independently
3. Extended with additional features
4. Tested in isolation from the uploader application

## Build Status

- ✅ `dicom_viewer` - Compiles successfully
- ✅ `divue-rs` - Compiles successfully  
- ✅ Both crates can be used independently
