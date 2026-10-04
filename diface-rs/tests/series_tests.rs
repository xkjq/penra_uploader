//! End-to-end tests: synthesize a DICOM series, load it, deface it and read
//! the result back.

use std::path::{Path, PathBuf};

use dicom_core::value::PrimitiveValue;
use dicom_core::VR;
use dicom_object::mem::InMemElement;
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, Tag};
use dicom_pixeldata::PixelDecoder;

use diface_rs::geometric::{GeometricBackend, GeometricParams};
use diface_rs::series::{collect_dicom_files, group_by_series, load_series, write_defaced_series};
use diface_rs::DefacingBackend;

const CT_SOP_CLASS: &str = "1.2.840.10008.5.1.4.1.1.2";
const EXPLICIT_LE: &str = "1.2.840.10008.1.2.1";

fn put_u16(obj: &mut DefaultDicomObject, tag: Tag, v: u16) {
    let _ = obj.put(InMemElement::new(tag, VR::US, PrimitiveValue::new_u16(v)));
}

fn put_i16_slice(obj: &mut DefaultDicomObject, data: &[i16]) {
    let elem: InMemElement = InMemElement::new(
        Tag(0x7FE0, 0x0010),
        VR::OW,
        PrimitiveValue::I16(data.to_vec().into()),
    );
    let _ = obj.put(elem);
}

/// Write one 32x32 CT slice with a synthetic head and nose.
#[allow(clippy::too_many_arguments)]
fn write_slice(
    dir: &Path,
    series_uid: &str,
    study_uid: &str,
    instance: u16,
    z_world: f64,
    spacing: f64,
    rows: usize,
    cols: usize,
) -> PathBuf {
    write_slice_oriented(
        dir,
        series_uid,
        study_uid,
        instance,
        z_world,
        spacing,
        rows,
        cols,
        "1\\0\\0\\0\\1\\0",
    )
}

/// Like [`write_slice`] but with an explicit ImageOrientationPatient, used to
/// build series that mix slice orientations.
#[allow(clippy::too_many_arguments)]
fn write_slice_oriented(
    dir: &Path,
    series_uid: &str,
    study_uid: &str,
    instance: u16,
    z_world: f64,
    spacing: f64,
    rows: usize,
    cols: usize,
    iop: &str,
) -> PathBuf {
    let sop_uid = format!("1.2.826.0.1.3680043.10.99.{}.{}", series_uid, instance);
    let meta = FileMetaTableBuilder::new()
        .media_storage_sop_class_uid(CT_SOP_CLASS)
        .media_storage_sop_instance_uid(&sop_uid)
        .transfer_syntax(EXPLICIT_LE)
        .build()
        .unwrap();
    let mut obj = DefaultDicomObject::new_empty_with_meta(meta);

    let half_x = cols as f64 * spacing / 2.0;
    let half_y = rows as f64 * spacing / 2.0;

    let mut data = vec![-1000i16; rows * cols];
    for y in 0..rows {
        for x in 0..cols {
            let wx = -half_x + x as f64 * spacing;
            let wy = -half_y + y as f64 * spacing;
            let wz = z_world;
            let e = (wx / 14.0).powi(2) + (wy / 14.0).powi(2) + (wz / 18.0).powi(2);
            let mut v = -1000i16;
            if e <= 1.0 {
                v = 40;
            }
            if wy <= -14.0 && wy >= -18.0 && wx.abs() <= 2.0 && wz.abs() <= 3.0 {
                v = 40;
            }
            data[y * cols + x] = v;
        }
    }

    let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, CT_SOP_CLASS);
    let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, &sop_uid);
    let _ = obj.put_str(Tag(0x0008, 0x0060), VR::CS, "CT");
    let _ = obj.put_str(Tag(0x0008, 0x0020), VR::DA, "20240101");
    let _ = obj.put_str(Tag(0x0020, 0x000D), VR::UI, study_uid);
    let _ = obj.put_str(Tag(0x0020, 0x000E), VR::UI, series_uid);
    let _ = obj.put_str(Tag(0x0020, 0x0013), VR::IS, &instance.to_string());
    let _ = obj.put_str(
        Tag(0x0020, 0x0032),
        VR::DS,
        &format!("{:.4}\\{:.4}\\{:.4}", -half_x, -half_y, z_world),
    );
    let _ = obj.put_str(Tag(0x0020, 0x0037), VR::DS, iop);
    let _ = obj.put_str(
        Tag(0x0028, 0x0030),
        VR::DS,
        &format!("{spacing:.4}\\{spacing:.4}"),
    );
    put_u16(&mut obj, Tag(0x0028, 0x0002), 1);
    let _ = obj.put_str(Tag(0x0028, 0x0004), VR::CS, "MONOCHROME2");
    put_u16(&mut obj, Tag(0x0028, 0x0010), rows as u16);
    put_u16(&mut obj, Tag(0x0028, 0x0011), cols as u16);
    put_u16(&mut obj, Tag(0x0028, 0x0100), 16);
    put_u16(&mut obj, Tag(0x0028, 0x0101), 16);
    put_u16(&mut obj, Tag(0x0028, 0x0102), 15);
    put_u16(&mut obj, Tag(0x0028, 0x0103), 1);
    let _ = obj.put_str(Tag(0x0028, 0x1052), VR::DS, "0");
    let _ = obj.put_str(Tag(0x0028, 0x1053), VR::DS, "1");
    let _ = obj.put_str(Tag(0x0018, 0x0050), VR::DS, &format!("{spacing:.4}"));
    let _ = obj.put_str(Tag(0x0018, 0x0088), VR::DS, &format!("{spacing:.4}"));
    put_i16_slice(&mut obj, &data);

    let path = dir.join(format!("slice_{instance:03}.dcm"));
    obj.write_to_file(&path).unwrap();
    path
}

fn make_series(root: &Path) -> (String, usize, usize) {
    let series_uid = "1.2.826.0.1.3680043.10.99.1";
    let study_uid = "1.2.826.0.1.3680043.10.99.0";
    let spacing = 2.0f64;
    let rows = 32usize;
    let cols = 32usize;
    let nz = 16usize;
    for z in 0..nz {
        let z_world = z as f64 * spacing - (nz as f64 * spacing) / 2.0;
        write_slice(
            root,
            series_uid,
            study_uid,
            z as u16 + 1,
            z_world,
            spacing,
            rows,
            cols,
        );
    }
    (series_uid.to_string(), rows, cols)
}

#[test]
fn synthetic_series_loads_defaces_and_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("in");
    let output = tmp.path().join("out");
    std::fs::create_dir_all(&input).unwrap();
    let (series_uid, rows, cols) = make_series(&input);

    let files = collect_dicom_files(&input, false).unwrap();
    assert_eq!(files.len(), 16);

    let groups = group_by_series(&files).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].uid, series_uid);

    let (volume, entries) = load_series(&groups[0].files).unwrap();
    assert_eq!(volume.dims, [cols, rows, 16]);
    assert_eq!(volume.modality, "CT");
    assert_eq!(entries.len(), 16);
    // Sorted by position: first slice is the most inferior.
    assert!(entries[0].sort_position < entries[15].sort_position);

    let backend = GeometricBackend::new(GeometricParams::default());
    let mask = backend.compute_mask(&volume).unwrap();
    assert!(mask.count_removed() > 0, "expected facial removal");

    let written = write_defaced_series(&entries, &volume, &mask, -2000.0, &output).unwrap();
    assert_eq!(written, 16);

    // Read a middle output slice and confirm the nose is blanked while deep
    // tissue survives, and metadata is preserved.
    let out_slice = output.join("slice_009.dcm");
    let obj = dicom_object::open_file(&out_slice).unwrap();
    assert_eq!(
        obj.meta().transfer_syntax().trim_end_matches(['\0', ' ']),
        EXPLICIT_LE
    );
    let suid = obj
        .element(Tag(0x0020, 0x000E))
        .unwrap()
        .to_str()
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(suid, series_uid);

    let decoded = obj.decode_pixel_data().unwrap();
    let pixels: Vec<i16> = decoded.to_vec().unwrap();
    assert_eq!(pixels.len(), rows * cols);

    // World origin is (-32, -32); with 2mm spacing the head centre (world 0,0)
    // maps to pixel index 16 in both axes.
    // Nose at world (0, -16) -> x index 16, y index 8.
    let nose = pixels[8 * cols + 16];
    assert_eq!(nose, -2000, "nose should be blanked to the fill value");

    // Deep posterior tissue at world (0, 4) -> y index 18, x index 16.
    let deep = pixels[18 * cols + 16];
    assert_eq!(deep, 40, "deep tissue should be untouched");
}

#[test]
fn deface_dir_in_place_overwrites_files_preserving_names() {
    use diface_rs::{deface_dir_in_place, DefaceOptions};

    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("anon");
    std::fs::create_dir_all(&input).unwrap();
    let (_uid, _rows, cols) = make_series(&input);

    let before: Vec<PathBuf> = collect_dicom_files(&input, false).unwrap();
    let before_len = before.len();

    let report = deface_dir_in_place(&input, &DefaceOptions::default()).unwrap();
    assert_eq!(report.series.len(), 1);
    assert!(report.series[0].removed_voxels > 0);

    // Same files still exist (names preserved).
    let after: Vec<PathBuf> = collect_dicom_files(&input, false).unwrap();
    assert_eq!(after.len(), before_len);
    assert_eq!(after, before, "in-place defacing must not rename files");

    // The nose is now blanked in the defaced middle slice.
    let obj = dicom_object::open_file(&input.join("slice_009.dcm")).unwrap();
    let pixels: Vec<i16> = obj.decode_pixel_data().unwrap().to_vec().unwrap();
    assert_eq!(
        pixels[8 * cols + 16],
        -1000,
        "nose blanked to fill (volume min)"
    );
    assert_eq!(pixels[18 * cols + 16], 40, "deep tissue preserved");
}

#[test]
fn mixed_orientation_series_uses_largest_consistent_group() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("in");
    std::fs::create_dir_all(&input).unwrap();
    let series_uid = "1.2.826.0.1.3680043.10.99.42";
    let study_uid = "1.2.826.0.1.3680043.10.99.40";
    let spacing = 2.0f64;

    // 10 slices share the axial orientation; 2 use a different (coronal) one.
    for z in 0..10u16 {
        let z_world = z as f64 * spacing - 10.0;
        write_slice_oriented(
            &input,
            series_uid,
            study_uid,
            z + 1,
            z_world,
            spacing,
            32,
            32,
            "1\\0\\0\\0\\1\\0",
        );
    }
    for i in 0..2u16 {
        write_slice_oriented(
            &input,
            series_uid,
            study_uid,
            100 + i,
            0.0,
            spacing,
            32,
            32,
            "1\\0\\0\\0\\0\\1",
        );
    }

    let files = collect_dicom_files(&input, false).unwrap();
    assert_eq!(files.len(), 12);
    let groups = group_by_series(&files).unwrap();
    assert_eq!(groups.len(), 1);

    // Loading must not fail: the two odd-orientation slices are dropped and the
    // largest consistent group is returned.
    let (volume, entries) = load_series(&groups[0].files).unwrap();
    assert_eq!(
        entries.len(),
        10,
        "only the largest orientation group is kept"
    );
    assert_eq!(volume.dims[2], 10);

    // And the resulting volume can be defaced.
    let backend = GeometricBackend::new(GeometricParams::default());
    let mask = backend.compute_mask(&volume).unwrap();
    assert!(mask.count_removed() > 0);
}
