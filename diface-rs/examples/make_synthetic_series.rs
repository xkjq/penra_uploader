//! Generates a small synthetic CT series so `diface --view` can be tried
//! without real patient data.
//!
//! ```bash
//! cargo run --example make_synthetic_series -- /tmp/demo_in
//! cargo run --release -- --view /tmp/demo_in /tmp/demo_out
//! ```

use std::path::Path;

use dicom_core::value::PrimitiveValue;
use dicom_core::VR;
use dicom_object::mem::InMemElement;
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, Tag};

const CT_SOP_CLASS: &str = "1.2.840.10008.5.1.4.1.1.2";
const EXPLICIT_LE: &str = "1.2.840.10008.1.2.1";

fn put_u16(obj: &mut DefaultDicomObject, tag: Tag, v: u16) {
    let _ = obj.put(InMemElement::new(tag, VR::US, PrimitiveValue::new_u16(v)));
}

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
) {
    let sop_uid = format!("{series_uid}.{instance}");
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
            let e = (wx / 14.0).powi(2) + (wy / 14.0).powi(2) + (z_world / 18.0).powi(2);
            let mut v = -1000i16;
            if e <= 1.0 {
                v = 40;
            }
            if wy <= -14.0 && wy >= -18.0 && wx.abs() <= 2.0 && z_world.abs() <= 3.0 {
                v = 40;
            }
            data[y * cols + x] = v;
        }
    }

    let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, CT_SOP_CLASS);
    let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, &sop_uid);
    let _ = obj.put_str(Tag(0x0008, 0x0060), VR::CS, "CT");
    let _ = obj.put_str(Tag(0x0020, 0x000D), VR::UI, study_uid);
    let _ = obj.put_str(Tag(0x0020, 0x000E), VR::UI, series_uid);
    let _ = obj.put_str(Tag(0x0020, 0x0013), VR::IS, &instance.to_string());
    let _ = obj.put_str(
        Tag(0x0020, 0x0032),
        VR::DS,
        &format!("{:.4}\\{:.4}\\{:.4}", -half_x, -half_y, z_world),
    );
    let _ = obj.put_str(Tag(0x0020, 0x0037), VR::DS, "1\\0\\0\\0\\1\\0");
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

    let elem: InMemElement = InMemElement::new(
        Tag(0x7FE0, 0x0010),
        VR::OW,
        PrimitiveValue::I16(data.into()),
    );
    let _ = obj.put(elem);

    let path = dir.join(format!("slice_{instance:03}.dcm"));
    obj.write_to_file(&path).unwrap();
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "synthetic_series".to_string());
    let out = Path::new(&out);
    std::fs::create_dir_all(out).unwrap();

    let spacing = 2.0f64;
    let (rows, cols, nz) = (32usize, 32usize, 16usize);
    let series_uid = "1.2.826.0.1.3680043.10.99.1";
    let study_uid = "1.2.826.0.1.3680043.10.99.0";
    for z in 0..nz {
        let z_world = z as f64 * spacing - (nz as f64 * spacing) / 2.0;
        write_slice(
            out,
            series_uid,
            study_uid,
            z as u16 + 1,
            z_world,
            spacing,
            rows,
            cols,
        );
    }
    println!("wrote {nz} slices to {}", out.display());
}
