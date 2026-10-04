# diface-rs

Facial anonymisation ("defacing") of DICOM series.

`diface-rs` is a standalone Rust crate in the `penra_uploader` repository, alongside
`dicor-rs` (metadata anonymisation), `dicom_viewer`, `divue-rs` and `diviz-rs`.
It removes identifiable facial anatomy from **pixel data** in multi-file CT and
MR head series. It deliberately does not touch metadata, so it composes cleanly
with `dicor-rs` (which handles tags, dates and UIDs).

> **Important:** defacing is best-effort. The default `geometric` backend is
> fast, offline and deterministic but conservative. It reduces facial
> recognisability; it is **not** a substitute for a validated
> atlas/registration or model-based defacing pipeline for high-assurance use.
> See [Limitations and safety](#limitations-and-safety).

## What it does

1. Discovers DICOM files (`--recursive` optional) and groups them by
   `SeriesInstanceUID`.
2. Reconstructs each group into a 3D volume using `ImageOrientationPatient`,
   `ImagePositionPatient`, `PixelSpacing` and slice position sorting.
3. Decodes pixel data (native and compressed, including JPEG-LS) via
   `dicom-pixeldata`.
4. Asks a pluggable **backend** which voxels contain facial anatomy.
5. Writes each slice back as uncompressed Explicit VR Little Endian, blanking
   the selected voxels to a fill value. All other metadata is preserved.

## Workspace layout

```
diface-rs/
  src/lib.rs        High-level API: deface(), reports, options
  src/backend.rs    DefacingBackend trait + BackendKind selector
  src/geometric.rs  Default geometric backend (head stats, preserve ellipsoid)
  src/geometry.rs   Vec3 / percentile helpers (LPS patient coordinates)
  src/volume.rs     Volume + Mask types
  src/series.rs     DICOM discovery, loading, defaced writing
  src/main.rs       `diface` CLI
  tests/            Synthetic-volume and end-to-end DICOM tests
```

## CLI

```bash
# Deface a directory of slices into out/series
cargo run --release -- /path/to/series /path/to/out

# Recursive scan, custom fill and a stricter safety cap
cargo run --release -- --recursive --fill min --max-removed-fraction 0.4 in out

# Compute masks and stats without writing anything
cargo run --release -- --dry-run in out

# Deface and immediately open the result in diviz-rs
cargo run --release -- --view in out
```

Run `diface --help` for all options:

| Option | Meaning | Default |
| --- | --- | --- |
| `--recursive` | Recurse into subdirectories | off |
| `--backend` | Backend name (`geometric`) | `geometric` |
| `--algorithm` | `ellipsoid`, `plane`, `curved` | `ellipsoid` |
| `--preset` | `conservative`, `balanced`, `thorough` | `balanced` |
| `--threshold` | `auto`, `otsu`, or a number | `auto` |
| `--preserve-fraction` | Preserve-ellipsoid size, `0..1` | `0.82` |
| `--anterior-bias` | Shift ellipsoid posteriorly | `0.05` |
| `--superior-bias` | Shift ellipsoid superiorly | `0.05` |
| `--yaw` | Rotate the head frame about superior (degrees) | `0` |
| `--depth` | Anterior/posterior shift of the cut (mm) | `0` |
| `--extent` | Preserve-ellipsoid scale per axis, `A,L,S` | `1,1,1` |
| `--seg-region` | Segmentation region: `face`, `deflesh` | `face` |
| `--brain-protect` | Segmentation: safety band (mm) kept around the intracranial core | `2` |
| `--deflesh-posterior` | Deflesh: keep external tissue posterior of brain centre + mm, or `all` | `0` |
| `--fill` | `min`, `zero`, or a raw value | `min` |
| `--min-slices` | Skip smaller series | `3` |
| `--max-removed-fraction` | Safety cap | `0.6` |
| `--no-subdir` | Write all series into the output root | off |
| `--dry-run` | Do not write | off |
| `--remove-original` | Delete inputs after writing | off |
| `--view` | Open the defaced output in `diviz-rs` | off |

## Viewing the output

`--view` launches `diviz-rs` on the written slices so the defacing can be
inspected immediately. The viewer binary is resolved in this order:

1. `DIVACE_VIEWER` environment variable (absolute path or command name).
2. `diviz-rs` on `PATH`.
3. Workspace build outputs found by walking up from the current directory, the
   running executable, and `CARGO_MANIFEST_DIR`, looking for
   `diviz-rs/target/{debug,release}/diviz-rs`.

If none are found the defacing still succeeds; a warning is printed. The viewer
is spawned as a separate process, so the `diface-rs` library itself keeps no GUI
dependencies.

Because defacing preserves `SeriesInstanceUID`, viewing the original and the
defaced copy at the same time would merge them into one series; view them
separately (e.g. run `diface --view`, then open the input directly in
`diviz-rs`).

### In-viewer defacing

`diviz-rs` depends on this crate directly and offers an in-memory **🙈 Deface**
toolbar button. It reconstructs a `Volume` from the active stack, runs the
geometric backend, blanks the mask in a copy of the series (grouped as
`<SeriesInstanceUID>-DEFACED`), and opens the result in a second viewport next
to the original for direct comparison — no files or CLI needed. Clicking the
button again refreshes the defaced copy.

### Backends

Three backends exist, selectable with `--backend geometric|atlas|segmentation`
or the **Backend** toggle in the viewer (with live preview):

#### Atlas backend

`AtlasBackend` fits a coarse 9-DOF affine from the subject head to a brain
template (using the head's principal axes and extents), then removes foreground
voxels that map **in front of** the registered brain (with a configurable
anterior margin). It never cuts behind the brain centre, so the occipital/neck
region is left alone.

The atlas is a compact, self-describing file (`.dfatlas`) so nothing large is
bundled:

```text
"DFATLAS1" | u32 version | [u32;3] dims | [f32;3] spacing_mm | [f32;3] origin | u8 mask (1=brain)
```

A built-in synthetic brain ellipsoid is used when no file is given (`--atlas`
omits it). `Atlas::to_bytes`/`from_bytes` read/write the format, and the viewer's
**Load atlas…** button loads one.

> Note: registration is coarse (no true brain segmentation), so the atlas
> backend is currently best used as a cross-check against the geometric
> algorithms. On case 546 it removes ~16.8%.

#### Segmentation backend (in-house)

`SegmentationBackend` builds a head+brain segmentation directly from the image
(no model, no atlas file):

1. threshold, then morphological open/close on a per-axis coarse grid;
2. isolate the **head** as the foreground component nearest the volume's
   central mass, cropped to a head-sized sphere (drops the table/neck);
3. estimate the **brain**: on CT, detect and morphologically close the **skull**
   (`bone_hu`, `skull_close_radius`) and grow soft tissue from an interior seed
   bounded by it (tolerates small skull gaps); on MR, grow within a
   brain-intensity band. The result is clamped to a brain-sized ellipsoid;
4. **autosegment the cranial vault** from the intracranial cavity. The grow in
   step 3 is run *without* the brain clamp to produce the full **cavity**
   (brain + CSF to the inner table); the **vault** is then the closed skull shell
   that surrounds that cavity. Deriving the vault this way separates the
   calvarium from the facial skeleton **even when the two fuse into one connected
   bone component** (connectivity alone cannot), which is what previously made
   the vault reach into the face;
5. cut the mask. `SegBackendParams::region` selects what is removed:
   - `Face` (default) — a single plane in front of the frontal pole (the face
     only). `cut_reference` (`BrainFront` default / `SkullFront`) moves the plane
     to the brain front or further forward, just ahead of the vault;
   - `ExternalSoftTissue` (**"Deflesh"**) — remove everything outside the
     **cranial vault**: scalp, face, table. The keep region is the
     **cavity ∪ vault**, so it stops at the outer table of the calvarium and
     never extends into the brain or the vault. By default removal is also gated
     **anteriorly** (`deflesh_posterior_mm`, default `0`): external tissue
     posterior of the brain centre is **kept**, so the back of the head and neck
     survive and only the face/scalp is stripped. Set `deflesh_posterior_mm` to
     `f64::INFINITY` (CLI `--deflesh-posterior all`) to remove all external
     tissue. Measured 7.4% on case 546 CT (vs 1.8% for `Face`), 7.5% / 3.3% on
     two MR brain series — **0 brain and 0 vault voxels removed** on all.

   `SegBackendParams::brain_margin_mm` widens the margin in front of the cut
   reference; `vault_shell_radius` sets the vault shell thickness (coarse voxels);
   `deflesh_posterior_mm` gates deflesh's posterior extent (keeps the neck).

This backend is deliberately conservative — it only removes tissue between the
face and the brain, so on a tight-FOV head CT it removes a few percent
(measured 2.9% on case 546). Use the geometric or atlas backends for stronger
removal.

It is exposed via `BackendKind::Segmentation` / `--backend segmentation`, and as
the **Segmentation** option in the viewer's Backend row. This is the most
principled of the in-house methods, but still coarse: on CT it relies on an
intact skull to bound the brain grow.

The segmentation also **stabilises the geometric and atlas backends**: their
head-frame estimate (`head_statistics_best`) restricts the point cloud to the
segmented head, so a wide-FOV CT (head + neck/table) no longer skews the
principal axis. It falls back to the unmasked estimate if segmentation fails.

### Algorithms (geometric backend)

Three masking algorithms can be selected (`--algorithm`, or the **Algorithm**
dropdown in the viewer) and previewed live:

| Algorithm | What it does | Notes |
| --- | --- | --- |
| `ellipsoid` (default) | Remove foreground outside a preserve ellipsoid over the head | Tunable, roughly conforms to the head |
| `plane` | Remove everything anterior to a single (tiltable) plane | Simplest/most predictable; sedate by plane position |
| `curved` | Remove anterior to a curved front that follows the brain surface | Recedes toward the vertex and laterally; protects poles better |

Measured on case 546: ellipsoid 10.2%, plane 4.0%, curved 6.5% of voxels
removed — genuinely different masks, so it's quick to compare which suits a
scan.

### Presets

Three brain-safety profiles are provided. They only change the preserve-ellipsoid
size/bias (and never the algorithm or manual orientation):

| Preset | Preserve | Removed (case 546) | Use when |
| --- | --- | --- | --- |
| `conservative` | largest ellipsoid | ~3.7% | you must keep the most brain |
| `balanced` (default) | medium | ~10.2% | general use |
| `thorough` | smallest | ~14.5% | you want the most facial removal |

The default was previously too aggressive against the brain; it now matches
`balanced`. Select with `--preset`, or the **Preset** buttons in the viewer's
Align panel (with live preview).

### Manual alignment (viewer)

The toolbar **🎛 Align** toggle opens manual defacing controls that apply on top
of the automatic head estimate. The panel shows **only the controls the active
backend reads**:

- **Geometric** – Preset, Algorithm, Yaw°, Depth mm, Extent A/L/S, Preserve,
- **Segmentation** – Preset, Depth mm, Preserve, plus Region / Cut at / Safety
  band (below); Yaw/Extent and the Algorithm dropdown are hidden because that
  backend ignores them,
- **Atlas** – no manual parameters (it uses its own registration); only
  **Load atlas…** and the template dimensions appear.

Common controls:

- **Preset** – quickly pick a brain-safety profile (see below); switching
  live-updates the preview and keeps any manual orientation,
- **Depth mm** – shift the cut anteriorly/posteriorly,
- **Yaw°** – rotate the anterior/left frame about the superior axis (geometric),
- **Extent A/L/S** – scale the preserve ellipsoid per anatomical axis (geometric),
- **Preserve** – overall preserve fraction (geometric sizing; segmentation margin),
- **↺ Auto** resets, **✓ Re-apply** rebuilds the defaced copy from the base
  series with the current settings,
- **👁 Preview** is **on by default** and shows a live **red overlay** on the base
  series giving exactly which voxels would be removed, in both **Stack and MPR**
  views. It updates as the orientation/depth/extent sliders (and presets) change,
  and stays on after **✓ Re-apply**, so the cut can be tuned before committing.
- **🗺 Areas** additionally shows the in-house **segmentation** in the overlay:
  **head (blue)**, **brain (green)**, **vault (orange)** and the intracranial
  **cavity (cyan)** — the cavity/vault boundary is exactly the deflesh keep
  region, so you can confirm it never crosses the skull.

With the **Segmentation** backend selected the panel also shows:

- **Region** – `Face` (flat cut) or `Deflesh` (strip external tissue/bone),
  with a **Keep back-of-head after** slider for Deflesh,
- **Cut at** – `Brain front` or `Skull front` (Face region only); the skull
  reference removes the whole face and falls back to the brain front when no
  vault is detected (MR),
- **Safety band** – the hard band (mm) kept around the brain/vault; larger is
  safer. This is the CLI `--brain-protect` option.

The alignment controls (Yaw / Depth / Extent / Preset / Preserve) apply to the
**Segmentation** backend too at the viewer level: Depth and Preset adjust the
safety margin in front of the brain (positive Depth removes more face, higher
Preserve is safer). The CLI exposes the same via `SegBackendParams`.

MPR orientation follows the standard radiological convention: axial anterior at
top, coronal superior at top with patient left on the left, and sagittal
superior at top with **anterior on the left**.

Defacing also links the two panes into a viewer **sync group**, so scrolling or
moving through the original and the defaced copy stays aligned (matched by
patient-space slice position, in both Stack and MPR views).

Sync is **per viewport**: the toolbar "🔗 Sync" checkbox and group selector
apply to the active viewport, viewports sharing a group follow each other, and
independent viewports are unaffected. Synced cells show a "🔗 Group N" badge,
and you can create additional groups or move a viewport between them.

MPR volumes are kept **per series**, so the original and the defaced copy can
each be viewed in MPR (side by side) without their planes being merged. MPR
planes follow the standard radiological orientation (axial: anterior top;
coronal: superior top; sagittal: superior top, anterior left), and CT gantry
tilt is handled by sampling each slice through its own patient-space geometry.

Viewport controls live **on each viewport**, each in its own position:

- **top-left** — sync group,
- **top-right** — Stack/MPR and MPR plane,
- **mid-left** — fit / zoom / rotate,
- **bottom-right** — window/level; **double-click the W/L overlay** to edit the
  WC/WW fields, with presets in the same cluster,
- **bottom-left** — series selector, on its own.

The toolbar keeps only global controls.

## Library usage

```rust
use diface_rs::{deface, DefaceOptions};
use std::path::Path;

let report = deface(
    Path::new("incoming/series"),
    Path::new("defaced"),
    &DefaceOptions::default(),
)?;
for s in &report.series {
    println!(
        "{} [{}]: removed {} / {} voxels ({:.1}%)",
        s.uid, s.modality, s.removed_voxels, s.total_voxels, s.removed_fraction() * 100.0
    );
}
# Ok::<(), String>(())
```

Lower-level entry points are also public:

- `series::collect_dicom_files`, `series::group_by_series`, `series::load_series`
- `series::write_defaced_series`
- `geometric::head_statistics`, `geometric::choose_threshold`
- `compute_mask`, `deface_volume`

## Backends

Backends implement `DefacingBackend`:

```rust
pub trait DefacingBackend: Send + Sync {
    fn name(&self) -> &str;
    fn compute_mask(&self, volume: &Volume) -> Result<Mask, String>;
    fn description(&self) -> String { String::new() }
}
```

The crate is structured so an atlas-registration, ONNX segmentation, or
external-tool (`pydeface` / FSL `mri_deface` / HD-BET) backend can be added
behind the same trait without touching the DICOM I/O. `BackendKind` is the
selector used by the CLI and the library.

### Geometric backend

1. Choose a foreground threshold (`auto` uses `-300 HU` for CT and Otsu
   otherwise).
2. Build a coarse occupancy grid, keep the largest 6-connected component, and
   compute its centroid, covariance and principal axis. The dominant eigenvector
   is the head's superior-inferior axis; anterior/left are derived from patient
   coordinates and orthogonalised against it.
3. Estimate robust half-extents along anterior/left/superior using 1st/99th
   percentiles.
4. Place a symmetric **preserve ellipsoid** over the head, biased slightly
   posteriorly and superiorly (where the brain sits).
5. Blank every foreground voxel that is anterior to the ellipsoid (i.e. the
   protruding face). Intracranial tissue inside the ellipsoid is never touched.

Because the preserve ellipsoid is symmetric and the face is inferred from
protrusion, the default is conservative: some anterior skin may remain rather
than risk clipping frontal cortex. Tune with `--preserve-fraction` and the bias
options.

## Limitations and safety

- Designed for **multi-file, single-frame, grayscale** CT/MR series. It errors
  on multi-frame files, colour data, or series whose slices disagree on
  dimensions/spacing. Slices that disagree on `ImageOrientationPatient` (curved
  reformats, localisers saved under the same series) are split into
  orientation-consistent groups and the **largest group** is defaced; the rest
  are left as-is.
- Output pixel data is re-encoded **uncompressed** Explicit VR Little Endian.
  Run `dicor-rs` afterwards to compress (e.g. JPEG-LS) and to anonymise
  metadata.
- The geometric backend assumes a roughly upright head and a single dominant
  head component. Severe positioning, large foreign objects, or heavy artefact
  can misestimate geometry.
- A mask that would remove more than `--max-removed-fraction` (default 60%) of
  the volume is **rejected** rather than written, so a failed head detection
  cannot silently erase a scan.
- Always review a sample of defaced output before using it operationally.

## Integration with the existing pipeline

`diface-rs` is independent, but a typical combined flow is:

```text
incoming DICOM series
        │
        ├─ diface-rs   (blank facial voxels; metadata preserved)
        │
        └─ dicor-rs    (clear/shift PHI tags, remap UIDs, compress)
```

To depend on it from another crate in this repository:

```toml
[dependencies]
diface_rs = { package = "diface-rs", path = "../diface-rs" }
```

## Build and test

```bash
cd diface-rs
cargo build --release
cargo test
```

Tests cover the geometric backend on synthetic volumes and a full
synthesize → load → deface → read-back round trip.
