//! ASCII MPR preview for manual orientation checks.
//!
//! ```bash
//! cargo run --example mpr_preview -- <series_dir> [axial|coronal|sagittal] [index]
//! ```
//!
//! Prints the requested plane at the given index (default: middle). For axial
//! HFS heads, superior/anterior orientation should read naturally: coronal has
//! superior at the top, sagittal has superior at the top and anterior on the
//! left, axial has anterior at the top.

use std::path::PathBuf;

use diviz_rs_app::mpr_preview_from_dir;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: mpr_preview <series_dir> [plane] [index]");
    let plane = args.next().unwrap_or_else(|| "coronal".to_string());
    let index: Option<usize> = args.next().and_then(|s| s.parse().ok());
    match mpr_preview_from_dir(PathBuf::from(dir).as_path(), &plane, index) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("error: {e}"),
    }
}
