//! Debug helper: list geometry metadata for every DICOM file under a root.
//!
//! ```bash
//! cargo run --example inspect_series -- /path/to/series
//! ```

use std::path::Path;

use dicom_object::{open_file, Tag};

fn get(obj: &dicom_object::DefaultDicomObject, tag: Tag) -> String {
    obj.element(tag)
        .ok()
        .and_then(|e| e.to_str().ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("usage: inspect_series <dir>");
    let files = diface_rs::series::collect_dicom_files(Path::new(&root), true).unwrap();
    println!("{} candidate files", files.len());
    for path in files {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        match open_file(&path) {
            Ok(obj) => {
                let modality = get(&obj, Tag(0x0008, 0x0060));
                let series = get(&obj, Tag(0x0020, 0x000E));
                let iop = get(&obj, Tag(0x0020, 0x0037));
                let ipp = get(&obj, Tag(0x0020, 0x0032));
                let rows = get(&obj, Tag(0x0028, 0x0010));
                let cols = get(&obj, Tag(0x0028, 0x0011));
                let inst = get(&obj, Tag(0x0020, 0x0013));
                let desc = get(&obj, Tag(0x0008, 0x103E));
                let body = get(&obj, Tag(0x0018, 0x0015));
                let pos = get(&obj, Tag(0x0018, 0x5100));
                println!(
                    "{name}\n  mod={modality} series={series} {rows}x{cols} inst={inst}\n  desc={desc} body={body} pos={pos}\n  iop={iop}\n  ipp={ipp}"
                );
            }
            Err(e) => println!("{name}: OPEN FAILED: {e}"),
        }
    }
}
