Project: uploader (Python + Rust)

Critical policy: duplicate detection hashing
- Duplicate detection must use PixelData-only hashing.
- Do not use full-file hashing for duplicate checks.
- Reason: metadata may differ between exports while image pixels are identical; full-file hashes would miss true duplicates.
- Canonical implementation location: uploader_rs/src/upload.rs (`calculate_pixel_hash`).

Purpose
- Anonymiser + uploader for DICOM files. Rust port (`uploader_rs`) aims for parity with Python dicognito-based anonymiser and the existing Nice uploader flow.

Important paths
- Root: uploader/
  - anonymiser.py             (Python wrapper using dicognito)
  - scripts/compare_anonymizers.py  (runs dicognito and Rust anon binary and compares cleared fields)
  - test_dicoms/              (sample DICOMs used for tests)
  - .venv/                    (project virtualenv for Python testing)
- Rust project: uploader/uploader_rs
  - src/anonymizer.rs         (Rust anonymiser core)
  - src/main.rs               (CLI + GUI skeleton)
  - src/upload.rs             (upload multipart logic)
  - tests/anonymizer_tests.rs (integration tests invoking the binary)
  - Cargo.toml                (Rust deps and dev-deps)

High-level design (Rust anonymiser)
- Deterministic pseudonymization:
  - `PatientName` -> `ANON-<hex>` (blake3-derived)
  - `PatientID` -> `ID-<hex>`
- UID remapping:
  - Remap to decimal-format UIDs `2.25.<decimal>` using blake3 -> BigUint
  - Applied to top-level UIDs and recursively to UI VRs within sequences
  - Audit map JSON written next to outputs (`<file>.anon_map.json`)
- Date/time shifting:
  - Study-level deterministic shift derived from StudyInstanceUID (optional seed)
  - Shift `DA` (YYYYMMDD), `DT` (leading YYYYMMDD), `TM` (rotate by offset modulo 24h)
- Clearing rules:
  - Remove private-group tags (odd group)
  - `clear_tags` list: conservative set of free-text and demographic tags removed/cleared
  - Blanket-clear textual VRs (UT, LT, SH, LO, PN) except whitelist for `PatientName` and `PatientID`
  - For SQ elements in `clear_tags`, remove the sequence rather than writing empty strings
- SR handling:
  - Do not remove Content Sequence `(0040,A730)` globally; instead recurse items and:
    - clear narrative/text VRs and PN
    - remap UID/UIDREF
    - shift DA/DT/TM inside items

Tags & behavior (summary)
- UIDs (remapped): Instance Creator UID `(0008,0014)`, SOP Instance UID `(0008,0018)`, Referenced SOP `(0008,1155)`, Study/Series UIDs `(0020,000D)/(0020,000E)`, Frame of Reference UIDs, SR UIDREFs, Storage Media File-set UID `(0088,0140)`, and other UI VRs recursively.
- Dates/times (shifted): Study/Series dates `(0008,0020..0023)`, Patient Birth Date/Time `(0010,0030)/(0010,0032)`, DT/DA inside sequences, and TM values rotated by offset.
- Pseudonymized: `PatientName` `(0010,0010)`, `PatientID` `(0010,0020)`.
- Cleared/removed: Accession Number `(0008,0050)`, Institution Name/Address, Referring Physician data, Study/Series descriptions, Device Serial Number, StudyID `(0020,0010)`, OtherPatientIDs/Names, demographics (Sex, Age, Size, Weight, Ethnic Group, Occupation), Protocol Name, Image Comments, RequestAttributesSequence `(0040,0275)` removed, and other tags in `clear_tags`.
- SR Content Sequence `(0040,A730)`: scrubbed (structure preserved; PHI fields cleared/remapped/shifted).

Implementation notes
- Uses `dicom-object` / `dicom-core` crates (0.6), `blake3`, `chrono`, `num-bigint`, `serde_json`.
- Mutation-safe iteration pattern: collect `to_remove` and `puts` during iteration, apply changes after loop; mutate SQ items via `update_value` and `items_mut()`.
- Be careful with string types (`Cow<str>`) when parsing dates/times.

Testing & validation
- Python validation harness: `scripts/compare_anonymizers.py` runs dicognito anonymiser and the Rust `--anon` binary, compares cleared fields using `pydicom`.
- Rust integration test: `uploader_rs/tests/anonymizer_tests.rs` invokes the binary against a sample DICOM from `test_dicoms` and asserts anonymisation outcomes.

Build & run
- Build Rust: `cd uploader/uploader_rs && cargo build`
- Run anonymiser: `./target/debug/uploader_rs --anon <input.dcm> <output.dcm>`
- Compare with dicognito (from repo root): `PYTHONPATH=. .venv/bin/python scripts/compare_anonymizers.py test_dicoms`
- Run Rust tests: `cd uploader/uploader_rs && cargo test`

Next recommended work
- CLI options: `--seed`, `--anon-map <path>`, `--clear-text-vr` (configurable behaviour)
- Add more unit tests: SR content checks, nested UID remap, date/time shift correctness
- Template-aware SR handling (if clinical SR utility must be preserved)
- Documentation (README) describing anonymisation policy and audit map format
- Commit changes and package for Linux

Recorded: 2026-03-16
