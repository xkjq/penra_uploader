Project: penra_uploader (Python + Rust)

Critical policy: duplicate detection hashing
- Duplicate detection must use PixelData-only hashing.
- Do not use full-file hashing for duplicate checks.
- Reason: metadata may differ between exports while image pixels are identical; full-file hashes would miss true duplicates.
- Canonical implementation location: uploader_rs/src/upload.rs (`calculate_pixel_hash`).

Purpose
- Anonymiser + uploader for DICOM files. Rust port (`uploader_rs`) aims for
  parity with Python dicognito-based anonymiser and the existing Nice uploader
  flow, and now also defaces faces in the export pipeline.
- The defacing toolchain is `diface-rs` (pixel-level facial removal, metadata
  untouched) plus `diviz-rs` (viewer with in-viewer defacing for tuning).

Important paths
- Root: penra_uploader/
  - anonymiser.py             (Python wrapper using dicognito)
  - scripts/compare_anonymizers.py  (runs dicognito and Rust anon binary and compares cleared fields)
  - test_dicoms/              (sample DICOMs used for tests)
  - .venv/                    (project virtualenv for Python testing)
- Rust project: penra_uploader/uploader_rs
  - src/anonymizer.rs         (Rust anonymiser core)
  - src/main.rs               (CLI + GUI skeleton)
  - src/upload.rs             (upload multipart logic)
  - tests/anonymizer_tests.rs (integration tests invoking the binary)
  - Cargo.toml                (Rust deps and dev-deps)
- Related Rust crates in the same repo:
  - diface-rs/                (facial defacing; see below)
  - diviz-rs/                 (DICOM viewer; in-viewer defacing)
  - dicor-rs/                 (metadata anonymisation / compression)
  - divue-rs/, diforge-rs/, launcher/, dicom_viewer/
- Real validation data lives outside the repo in `~/dicoms` (case 546 CT,
  case 554 MR x6, case_811 MR x4, case/series_2079-2081).
- Toolchain: nightly (`rust-toolchain.toml`).

High-level design (Rust anonymiser)
- Deterministic pseudonymization:
  - `PatientName` -> `ANON-<hex>` (blake3-derived)
  - `PatientID` -> `ID-<hex>`
- UID remapping:
  - Remap to decimal-format UIDs `2.25.<decimal>` using blake3 -> BigUint
  - Applied to top-level UIDs and recursively to UI VRs within sequences
  - Remapping is deterministic and irreversible; no audit map file is written
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
- Uses `dicom-object` / `dicom-core` 0.10 (also `dicom-pixeldata` 0.10 with
  `charls` for JPEG-LS), `blake3`, `chrono`, `num-bigint`, `serde_json`.
- Mutation-safe iteration pattern: collect `to_remove` and `puts` during iteration, apply changes after loop; mutate SQ items via `update_value` and `items_mut()`.
- Be careful with string types (`Cow<str>`) when parsing dates/times.

Testing & validation
- Python validation harness: `scripts/compare_anonymizers.py` runs dicognito anonymiser and the Rust `--anon` binary, compares cleared fields using `pydicom`.
- Rust integration test: `uploader_rs/tests/anonymizer_tests.rs` invokes the binary against a sample DICOM from `test_dicoms` and asserts anonymisation outcomes.

Build & run
- Build Rust: `cd uploader_rs && cargo build`
- Run anonymiser: `./target/debug/uploader_rs --anon <input.dcm> <output.dcm>`
- Compare with dicognito (from repo root): `PYTHONPATH=. .venv/bin/python scripts/compare_anonymizers.py test_dicoms`
- Run Rust tests: `cd uploader_rs && cargo test`

Facial anonymisation crate (diface-rs)
- Root: penra_uploader/diface-rs
  - src/lib.rs         High-level API: `deface()`, `deface_dir_in_place()`,
                       `DefaceOptions`, reports; re-exports `CutReference`.
  - src/backend.rs     `DefacingBackend` trait + `BackendKind` selector
  - src/geometric.rs   Default geometric backend (head stats + preserve ellipsoid)
  - src/geometry.rs    Vec3/percentile helpers (LPS patient coords)
  - src/volume.rs      `Volume` + `Mask`
  - src/series.rs      DICOM discovery, loading, defaced writing
  - src/segmentation.rs In-house head/brain segmentation + vault/cavity
  - src/atlas.rs       `.dfatlas` atlas format + registration backend
  - src/viewer.rs      `--view` launcher for diviz-rs
  - src/main.rs        `diface` CLI
  - tests/             Synthetic-volume + end-to-end DICOM round-trip tests
                       (37 tests across geometric/segmentation/atlas/series/viewer)
- Purpose: pixel-level removal of identifiable facial anatomy from multi-file
  CT/MR series. Metadata is left untouched so it composes with dicor-rs.
- Design: pluggable backends. Default `geometric` thresholds the volume, keeps
  the largest connected component, estimates the head's principal (S-I) axis
  and extents, then blanks foreground voxels anterior to a preserve ellipsoid
  (biased posterior/superior) so brain is protected. Refuses masks above
  `max_removed_fraction` (default 0.6).
- Robustness: slices that disagree on ImageOrientationPatient are split into
  orientation-consistent groups and the largest group is defaced (previously the
  whole series was rejected). Verified on real data in ~/dicoms: CT head
  (case 546) and six MR brain series (case 554) all give `mask match: OK`,
  `wrong-fill: 0`, and 94-97% of removed voxels anterior of the head centre.
- Debug helpers (all under `diface-rs`): `inspect_series <dir>` lists per-slice
  geometry; `verify_defacing <orig> <defaced>` compares the output against a
  freshly computed mask (anterior/posterior split, ASCII axial previews);
  `seg_debug <dir> [--ascii] [--protect MM]` prints the segmentation, vault/cavity
  stats and per-region removal (0 brain / 0 vault expected); `atlas_debug <dir>`.
  The diviz-rs example `deface_debug <dir>` prints the viewer's volume geometry,
  estimated head axes, and the anterior/posterior split, and cross-checks against
  `diface_rs::series::load_series` (`diface-rs ref:` line; dims/counts must match).
- Output pixel data: uncompressed Explicit VR Little Endian; run dicor-rs after
  to compress/anonymise metadata.
- Viewer integration: `--view` opens the defaced output in `diviz-rs`
  (src/viewer.rs). Resolution order: `DIVACE_VIEWER` env override, `diviz-rs`
  on PATH, then workspace build outputs (`diviz-rs/target/{debug,release}`).
  Spawned as a subprocess, so the library keeps no GUI dependencies.
- In-viewer defacing: diviz-rs depends on diface_rs and exposes a "🙈 Deface"
  toolbar button. It builds a `diface_rs::Volume` in memory (build_deface_volume
  in diviz-rs/src/lib.rs), runs the geometric backend, blanks the mask in a
  copy of the active series, groups it as `<uid>-DEFACED`, and opens it in a
  second viewport next to the original. Clicking again refreshes the copy
  (always rebuilt from the base series, so the uid never gets double-suffixed).
  `build_deface_volume` mirrors `diface_rs::series::load_series`: it requires
  identical dimensions, groups slices by `ImageOrientationPatient` (1e-3) and
  defaces only the **largest** orientation-consistent group, so a mixed series
  (localisers under one SeriesInstanceUID) previews exactly what the CLI/uploader
  produces. Test `build_deface_volume_keeps_the_largest_orientation_group`.
  MPR volumes are **per series** (`mpr_volumes: Vec<Option<MprVolume>>`), so
  the original and the defaced copy each have their own volume and can be
  viewed in MPR side by side. `refresh_mpr_volumes` rebuilds them all;
  `mpr_series_for(viewport)` returns the viewport's *own* series (so a defaced
  viewport uses the defaced volume and the source uses the original), and
  `mpr_slice_for`/`mpr_index_for_point`/`viewport_anchor_point` resolve the
  volume through that helper.
- Viewport sync (per viewport): each `ViewportState` has `sync_group:
  Option<u32>`. The toolbar "🔗 Sync" checkbox + group combo apply to the active
  viewport; viewports sharing a group stay in lockstep, independent ones do not.
  Each frame `autosync_active` detects which viewport was navigated
  (per-viewport `last_navs` snapshots) and `sync_from` moves the other members
  of its group to the matching patient-space position. Works for Stack (nearest
  image by `dot(pos, normal)`) and MPR (nearest plane index along the patient
  axis). Defacing auto-assigns the two panes to a new shared group.
  Series/plane/mode changes re-baseline instead of propagating, and `last_navs`
  is refreshed after every propagation to prevent echo. Synced cells show a
  "🔗 Group N" badge.
- MPR orientation & gantry tilt: `MprPlane::u_sign()`/`v_sign()` control the
  image flip so planes follow the radiological convention (axial: anterior top;
  coronal: superior top; sagittal: superior top, anterior left). `extract_plane`
  flips by reversing the sample order. `MprVolume::sample_trilinear` samples
  each slice using its own `ImagePositionPatient` and in-plane axes
  (`slice_positions`/`slice_col_dirs`/`slice_row_dirs`), so a sheared stack
  (CT gantry tilt, e.g. `case 546`) is de-tilted instead of assuming a
  rectangular grid. The resample grid spacing per patient axis is the source
  voxel's *projected extent* (sum of absolute edge projections), so a tilted
  stack is neither oversampled (previously the Z step collapsed to the in-plane
  pixel size) nor undersampled. Manual check:
  `cargo run --example mpr_preview -- <dir> <plane>`.
- Defacing axis assignment (`head_statistics` in diface-rs/src/geometric.rs):
  raw PCA is unreliable on wide-FOV / partially segmented scans (a head CT that
  also imaged neck/table has more in-plane than S-I variance), which rotated
  case 546 by ~90 degrees. The axes are now assigned from the acquisition
  geometry: `superior` = the slice axis when the stack is axial (`|dir[2]·Z| >
  0.6`), otherwise the eigenvector most aligned with world Z; `anterior`/`left`
  from the in-plane row/column axes, orthogonalised. Verified: case 546 now
  76.6% anterior (was 36%), brain series 80-100% anterior. Diagnose with
  `cargo run --example deface_debug -- <series_dir>` from `diviz-rs` (it prints
  the viewer's volume geometry, estimated axes, and anterior/posterior split).
- Manual defacing controls (`GeometricParams`): `axis_override` (explicit unit
  frame), `yaw_deg` (rotate about superior), `anterior_offset_mm` (cut depth),
  and `extent_scale` (per-axis preserve-ellipsoid multipliers). Exposed as CLI
  flags `--yaw/--depth/--extent` and as the diviz-rs toolbar **🎛 Align** panel
  (`deface_alignment_ui`), which edits `DicomViewApp::deface_params` and
  re-applies via `deface_active_series`.
- Segmentation backend (`diface_rs::segmentation`): in-house head+brain
  segmentation — threshold -> morphological open/close on a per-axis coarse grid
  -> isolate the head (largest foreground component near the volume centre,
  cropped to a head-sized sphere) -> estimate brain (CT: detect + morphologically
  close the skull via `bone_hu`/`skull_close_radius`, grow soft tissue from an
  interior seed bounded by it, tolerating small skull gaps; MR: intensity-band
  grow) -> clamp to a brain-sized ellipsoid -> cut a single plane in front of the
  brain. `SegBackendParams::region` selects `Face` (default; flat cut) or
  `ExternalSoftTissue`/"deflesh" (remove everything **outside the cranial
  vault**; keep region = **`cavity ∪ vault`**, so it never extends into the
  brain or vault; non-CT fallback = closed cavity). Removal is gated anteriorly
  by `SegBackendParams::deflesh_posterior_mm` (default 0 = external tissue
  posterior of the brain centre is **kept**, preserving neck/back-of-head; set
  `f64::INFINITY` / CLI `--deflesh-posterior all` to remove all external
  tissue). CLI `--seg-region face|deflesh`; viewer **Region** selector +
  **Keep back-of-head after** slider (`deface_deflesh_posterior_mm`).
  `cut_reference` selects `BrainFront` (default) or `SkullFront`; the viewer
  exposes it as the **Cut at** selector (`deface_cut_reference`, Face region),
  and a **Safety band** slider (`deface_brain_protect_mm`, default 2 mm)
  mirrors the CLI `--brain-protect`. Both live-update the preview, and
  **↺ Auto** resets them together with `deface_params`.
  **Vault autosegmentation** (`vault_from_cavity`): the brain grow is re-run
  without the ellipsoid clamp to produce `cavity` (brain + CSF to the inner
  table); `vault` = the closed skull shell surrounding the cavity
  (`vault_shell_radius`). This separates the calvarium from the facial skeleton
  even when they fuse into one connected bone component — connectivity alone
  (the old topmost-bone `component_of_extreme`) could not, which is why the
  vault used to reach into the face. `Segmentation` now carries `cavity` and a
  true `vault`; `vault_at`/`cavity_at` accessors; `SkullFront` cuts in front of
  the vault's frontal bone (`most_anterior_vault`). Viewer `🗺 Areas` now draws
  head(blue)/brain(green)/vault(orange)/cavity(cyan) — labels 1..4.
  Note: `erode` treats out-of-bounds neighbours as foreground so morphological
  closings never clip cells at the volume border.
  `brain_protect_mm` is the hard safety band (mm) dilated around the protected
  core (`cavity ∪ vault`); nothing inside it is removed by either region. It is
  the run-level `--brain-protect` CLI flag (default 2); the same value lives on
  `SegBackendParams`, and the ambiguous `DefaceOptions.brain_protect_mm` was
  removed (it was never read). `seg_debug --protect MM` exercises it.
  Deflesh **boundary feathering**: `SegBackendParams::deflesh_smooth_mm`
  (default 3, CLI `--deflesh-smooth`, viewer **Smooth** slider,
  uploader `DefaceConfig::deflesh_smooth_mm`) blends the defleshed edge instead
  of a hard wall. `feather_boundary` runs a 6-connected multi-source BFS from the
  removed/kept boundary (seeded from kept neighbours, the volume border, and the
  coarse `core` keep cells) and writes a per-voxel `Mask::weight` that ramps
  0 → 1 over `deflesh_smooth_mm`. The binary `remove` flags are unchanged, so the
  0 brain / 0 vault guarantee holds; `Mask::weight` (empty = binary) is consumed
  by `series::write_defaced_series` and `deface_volume`, which compute
  `orig + (fill - orig) * w`. The **in-viewer** defaced copy applies the same
  blend via `DefaceGeometry::removal_weight_native` (previously it hard-wrote the
  fill and ignored the weight, so the slider looked inert in the viewer). The red
  👁 Preview overlay stays binary on purpose (it shows *which* voxels are removed,
  not the blend). Tests
  `deflesh_smooth_feathers_the_boundary_without_changing_removal`,
  `deflesh_smoothing_blends_the_in_viewer_defaced_copy`.
  Types:
  `SegParams`, `Segmentation`, `segment()`, `SegBackendParams`,
  `SegmentationBackend`; `BackendKind::Segmentation`; CLI `--backend
  segmentation`; viewer Backend row (3-way). Tests: diface-rs/tests/
  segmentation_tests.rs (skull-bounded phantom; `vault_is_a_shell_that_excludes_the_face`,
  `deflesh_removes_external_tissue_and_keeps_brain` asserts 0 brain **and** 0 vault removed,
  `brain_protect_band_shrinks_removal_and_spares_the_core`).
- Head-stats stabilisation: `geometric::head_statistics_best` runs the
  in-house segmentation and restricts the principal-axis point cloud to the
  segmented head (via `head_filter`), falling back to the plain estimate if
  segmentation fails. Both `GeometricBackend::compute_mask` and
  `AtlasBackend::fit` use it, so wide-FOV CT (head + neck/table) no longer
  skews the head frame. `head_statistics_masked` exposes the masked form.
- Atlas backend (`diface_rs::atlas`): `Atlas` (self-describing `.dfatlas`
  format, `to_bytes`/`from_bytes`, built-in `synthetic`), `AtlasBackend`
  (9-DOF affine fitted from head stats, removes foreground in front of the
  registered brain, never behind its centre), `AtlasParams`, `AtlasSpec`
  (Synthetic/File/InMemory). Exposed via `BackendKind::Atlas`, CLI
  `--backend atlas` / `--atlas <file>`, and the viewer **Backend** row +
  **Load atlas…**. Registration is coarse (no true brain segmentation), so it
  is a cross-check rather than the default.
- Algorithms (`diface_rs::DefaceAlgorithm`): `Ellipsoid` (default),
  `Plane` (single anterior plane), `CurvedFront` (parabolic cut that recedes
  toward the vertex/laterally). Selected in `compute_mask` via `remove_at`;
  CLI `--algorithm`; viewer **Algorithm** dropdown (live preview). On case 546
  they remove 10.2% / 4.0% / 6.5% respectively. `apply_preset` never changes the
  algorithm.
- Presets (`diface_rs::DefacePreset`): `Conservative` / `Balanced` / `Thorough`,
  applied via `GeometricParams::apply_preset` (keeps manual yaw/axis override).
  `GeometricParams::default()` now equals `Balanced` (the old default
  preserve_fraction 0.72 trimmed too much brain). `matching_preset()` reports
  the active preset. CLI `--preset`; viewer **Preset** buttons live-update the
  preview. Measured on case 546: conservative 3.7%, balanced 10.2%, thorough
  14.5% of voxels removed.
- Removal preview: `toggle_deface_preview`/`refresh_deface_preview` compute the
  mask without writing and store a `DefacePreview` = 3D volume mask (`volume_flags`
  + `dims`) plus per-image flags (`per_image`). **Stack** viewports look up
  `per_image` for the displayed slice; **MPR** viewports use
  `DefacePreview::plane_flags(plane, index)` (matching `MprVolume::extract_plane`
  axis mapping + u/v flips) so the red overlay works in MPR too. Both tint via
  `tint_removed` (`👁 Preview` toggle in the Align panel; live-updates as sliders
  change). Cleared once the defaced copy is written.
- MPR sagittal orientation: `MprPlane::Sagittal` uses `u_sign = +1` (u axis = +Y
  posterior, unflipped) so **anterior is on the left** (standard radiological
  convention), superior at the top. Covered by
  `sagittal_puts_anterior_on_the_left`.
  `🧭 Reset view` in the Align panel resets the active viewport's
  zoom/rotation/pan and window/level and clears the preview.
  `🗺 Areas` toggle (`deface_show_segmentation`) draws the in-house segmentation
  in the overlay (head blue / brain green / vault orange / cavity cyan) via
  `DefacePreview::volume_labels`/`per_image_labels`/`plane_labels` and
  `tint_segmentation`.
  Preview is on by default (`deface_preview_on`, `toggle_deface_preview` flips
  the preference) but **only auto-computes while the Align panel is open**
  (`deface_panel_open`); closing the panel clears the overlay.
  Performance: the preview path is expensive (the volume build re-decodes every
  voxel to `f32`; the segmentation backend re-runs `segment()`; the Areas overlay
  segments again). Three caches/deferrals avoid redoing it on every drag frame:
  - `deface_volume_cache: Option<(String, DefaceGeometry)>` — reconstructed
    volume + geometry, keyed by base series UID (`deface_volume_for`), so a
    slider change does not re-decode. Invalidated in `load_files`.
  - `deface_seg_cache: Option<(String, diface_rs::Segmentation)>`
    (`segmentation_for`), shared by the mask and the Areas overlay. The new
    `SegmentationBackend::compute_mask_from_segmentation` builds the mask from an
    existing segmentation, so only the cut plane is recomputed (CT ~340 ms ->
    ~55 ms per adjustment).
  - **Debounce**: a changed parameter sets `deface_preview_dirty` and a timestamp;
    `flush_pending_deface_preview` (end of `update`, `DEFACE_PREVIEW_DEBOUNCE`
    = 120 ms) recomputes once the drag settles instead of per intermediate value.
  Tests: `deface_volume_and_segmentation_are_cached_across_param_changes`,
  `deface_preview_debounce_coalesces_and_flushes`.
  The Align panel shows **only the controls the active backend reads**
  (per-backend gating in `deface_alignment_ui`): Geometric = Preset / Algorithm /
  Yaw / Depth / Extent / Preserve; Segmentation = Preset / Depth / Preserve /
  Region / Cut at / Safety band (no Yaw/Extent/Algorithm); Atlas = no manual
  params, only **Load atlas…** and the template dims. Depth/Preset map onto the
  Segmentation backend via `SegBackendParams::brain_margin_mm`. Tested by
  `deface_panel_renders_for_every_backend`.
  **Manual brush** (`🖌 Brush` in the Align panel): correct over/under-segmentation
  by painting the mask. `ManualEdits` holds sparse `BrushEdits`
  (`force_remove` / `force_keep` voxel-index sets) per base-series UID; **Add**
  fills missed tissue (under-segmentation), **Erase** protects wrongly removed
  tissue (over-segmentation; wins if a voxel is in both). Size (mm) + `Slices ±`
  stamp a disc across neighbouring slices. `apply_manual_edits` overlays the edits
  on the automatic mask in `refresh_deface_preview` **and**
  `deface_active_series`, so the red overlay and the defaced copy both reflect
  them. Painting uses `stamp_brush`/`paint_brush_at` with `screen_to_pixel`
  (inverse of `pixel_to_screen`) to map the pointer to base-series voxels.
  **Stack viewports only** (MPR painting unsupported). Session-only (not
  persisted). Tests: `manual_brush_add_and_erase_override_the_auto_mask`
  (incl. preview reflects the paint), `brush_paint_marks_voxels_in_the_base_volume`.
- Uploader integration (uploader_rs):
  - `diface_rs::deface_dir_in_place(dir, opts)` defaces each series found in a
    directory **overwriting files in place** (names preserved), used after the
    anonymise step when the "Deface faces" setting is on.
  - After defacing, `/home/ross/penra_uploader/uploader_rs/src/main.rs`
    recompresses each output to **JPEG-LS lossless** via
    `dicor_rs::compress_to_jpegls` (defacing writes uncompressed), in parallel.
    Recompression is lossless, so the defaced pixels' hash is unchanged.
  - CRITICAL: duplicate detection must keep using the **original (pre-deface)**
    pixel hash. The anonymise loop computes `calculate_pixel_hash_from_obj` on
    the input object and records `(output_path, original_hash)`. After
    deface+recompress, each output is re-cached with `cache_pixel_hash(path,
    original_hash)` (re-stats the new size/mtime) and `request_scan` rebuilds
    ready metadata; `upsert_ready_file_internal` prefers the cached hash, so the
    defaced pixels never change the duplicate key. Tested by
    `defacing_changes_pixels_but_keeps_original_cached_hash` in
    uploader_rs/src/upload/tests.rs (also asserts the output is JPEG-LS after
    recompression and that this is lossless).
  - The settings are carried as `DefaceConfig` (`enabled`, `backend`
    Geometric/Segmentation, segmentation `region`, `brain_protect_mm`) via
    `QueueItem.deface` -> `enqueue_export_processing`; `DefaceConfig::to_options()`
    builds the `diface_rs::DefaceOptions` (always in place, `min_slices: 3`).
    Kept in sync with the Settings controls through
    `AppState.shared_deface: Arc<Mutex<DefaceConfig>>` for the IPC ("loaded")
    path. The Settings panel exposes the Deface checkbox + Backend/Region/Safety
    band. Tested by `deface_config_maps_to_backend_options` in
    uploader_rs/src/processing_workflow_tests.rs.
- diviz-rs tests: `mod tests` in diviz-rs/src/lib.rs (67 tests). Covers MPR
  construction (incl. gantry tilt), defacing (incl. largest-orientation-group
  selection), viewport sync, geometry helpers, file discovery, real on-disk
  DICOM decode, and headless `eframe` UI frames via `run_headless_frame[_with]`
  (drives `App::ui` through `ctx.run_ui`). Coverage: `cargo llvm-cov --lib` ->
  ~80% regions / 86% functions; the remainder is UI interaction closures and the
  window-launching `run_viewer*` entry points.
- Recent-folders history: `load_files(paths, record_recent, ctx)` only records a
  recent folder when `record_recent` is true. User-initiated opens (Open
  Files/Folder dialog, drag-and-drop, Recent menu) pass `true`;
  **programmatic loads pass `false`** — CLI arguments, `diface --view`
  (`run_viewer_with_files` sets `pending_load = Some((path_bufs, false))`) and
  tests. `pending_load: Option<(Vec<PathBuf>, bool)>` carries the flag. Tested by
  `programmatic_loads_do_not_touch_recent_history`.
- Per-viewport controls are split into separate `egui::Area` overlays
  (`viewport_controls_overlay` + `viewport_area`), each in its own position:
  sync top-left, view mode + plane top-right, window/level + presets
  bottom-right, series selector bottom-left, fit/zoom/rotate mid-left. The
  Switching Stack <-> MPR keeps the current window/level preset (only changing
  the series resets it). The window/level fields are hidden until the W/L overlay is double-clicked
  (`wl_editor_open[idx]`). The image rect per viewport is recorded during the
  draw pass in `viewport_image_rects`. The toolbar keeps only global controls
  (open, viewport add/remove, series for the active viewport, and Deface).
- Wheel scrolling: both the single- and multi-viewport paths go through
  `wheel_step_slices`. It accumulates `smooth_scroll_delta` (egui sends a
  decaying tail per notch) and advances at most one slice per call once
  `WHEEL_SLICE_THRESHOLD` (48) is reached, then resets. A per-viewport
  `wheel_owner` discards partial movement when the hovered cell changes.
- Demo: `cargo run --example make_synthetic_series -- /tmp/demo_in` then
  `cargo run --release -- --view /tmp/demo_in /tmp/demo_out`.
- Build: `cd diface-rs && cargo build --release`; test: `cargo test`
- Run: `cargo run --release -- [--recursive] [--dry-run] [--view] <INPUT> <OUTPUT>`
- See diface-rs/README.md for options, limitations and backend extension.

Current status
- The diface-rs crate and its diviz-rs/uploader_rs integration are **committed**
  (see `git log --oneline`). Working tree is clean apart from ignored build
  artifacts.
- Test counts: diface-rs 37, diviz-rs 67, uploader_rs 12 (11 unit + 1
  anonymiser integration). All green.
- Recent fixes worth knowing:
  - `build_deface_volume` groups by `ImageOrientationPatient` and keeps the
    largest group, so the viewer matches the CLI/uploader on mixed series.
  - `--brain-protect` (segmentation safety band) is wired; the dead
    `DefaceOptions.brain_protect_mm` field was removed.
  - The viewer Align panel exposes segmentation **Cut at** and **Safety band**.
  - The uploader Settings panel exposes backend/region/safety band/deflesh
    smooth via `DefaceConfig`.
  - `.github/workflows/build-windows.yml` builds and uploads the `diface.exe`
    CLI artifact alongside diviz/dicor/divue/uploader.

Next recommended work (defacing)
- Packaging: `build.py` (PyInstaller) is Python-only and never bundled the Rust
  binaries; if a single shippable bundle is wanted, collect `uploader_rs`,
  `diviz-rs`, `divue` and `diface` together (the launcher/uploader currently
  resolve `diviz-rs` on PATH then via workspace `target/{debug,release}`).
- Validation: keep re-running `seg_debug` / `deface_debug` over `~/dicoms` as new
  MR series arrive; MR has no detectable skull, so `vault` is empty and
  `SkullFront`/deflesh fall back to the brain/closed-cavity path.
- Higher-assurance option: a registration/atlas or model (ONNX/HD-BET) backend
  behind the existing `DefacingBackend` trait.

Next recommended work (metadata anonymiser)
- CLI options: `--seed`, `--clear-text-vr` (configurable behaviour)
- Add more unit tests: SR content checks, nested UID remap, date/time shift correctness
- Template-aware SR handling (if clinical SR utility must be preserved)
- Documentation (README) describing anonymisation policy

Recorded: 2026-03-16 (metadata-anonymiser notes)
Updated: 2026-10-04 (diface-rs + diviz/uploader defacing integration)
