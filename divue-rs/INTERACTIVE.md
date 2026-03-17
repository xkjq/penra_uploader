# Divue-rs - Interactive DICOM Metadata Viewer

A modern Rust application for comparing DICOM metadata across multiple files with an interactive GUI featuring file picker and drag-and-drop support.

## New Features

### ✨ Interactive Mode (Default)

When run without arguments, divue-rs launches in interactive mode:

```bash
./divue
```

This provides an intuitive GUI with:
- **File Picker Button** - Click "📁 Add Files..." to select DICOM files
- **Folder Picker Button** - Click "📂 Add Folder..." to load all .dcm files from a directory
- **Drag & Drop Support** - Simply drag files or folders onto the window
- **File List Display** - Shows all selected files with individual remove buttons
- **Bulk Actions** - "🔍 Compare" button to launch comparison view or "🗑️ Clear All" to reset

### 💡 Smart File Detection

The application automatically:
- Filters for .dcm files when adding folders
- Prevents duplicate file additions
- Displays file names for easy identification
- Shows total count of selected files

### 🔄 Seamless Navigation

- Click "🔍 Compare Metadata" to view the comparison
- Click "← Back to File Selection" from the comparison view to add more files
- Maintain full filtering and metadata comparison functionality

## Usage

### Interactive Mode (Recommended)

```bash
./divue
```

Steps:
1. Add files using buttons or drag-and-drop
2. Review selected files in the list
3. Click "Compare" to open side-by-side comparison
4. Use filter box to search metadata
5. Click values to view full text
6. Return to file selection to compare different files

### Command Line Mode (Direct Comparison)

Skip the file picker and go straight to comparison:

```bash
./divue /path/to/file1.dcm /path/to/file2.dcm
```

## Building

```bash
cd divue-rs
cargo build --release
```

The binary will be at `target/release/divue`.

## Implementation Details

### Architecture

The new implementation uses a unified `DivueApp` state that manages:
- **File Selection** - `Vec<PathBuf>` for selected files
- **Comparison View** - `Vec<(String, HashMap<String, String>)>` for loaded metadata
- **Filter State** - String for current search filter
- **UI Mode** - Boolean flag to switch between views

### Key Components

1. **`run_interactive()`** - Entry point for interactive mode with file picker
2. **`run_meta_viewer(paths)`** - Original entry point for direct comparison
3. **`DivueApp`** - Unified app state managing both views
4. **`MetaApp`** - Original comparison display (kept for compatibility)

### File Format Support

- Primary: `.dcm` - DICOM medical image files
- Configurable filters in file dialog for other formats

## Testing

All 18 existing tests pass without modification:

```bash
cargo test                      # Run all tests
cargo test --test filter_tests  # Test specific suite
```

Additional test coverage for interactive features can be added in `tests/interactive_tests.rs`.

## Known Limitations

- Drag-and-drop behavior depends on OS/platform (tested on Linux)
- Very large folders may take time to load
- Metadata loading from file system is synchronous (UI may appear frozen on slow drives)

## Future Enhancements

Potential improvements:
- Async file loading to prevent UI freezing
- Recent files list
- File history/cache
- Export comparison results to CSV/JSON
- Settings for metadata display preferences
- Batch operations on multiple file sets
