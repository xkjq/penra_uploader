//! Debug helper: inspect the defacing geometry for a series.
//!
//! ```bash
//! cargo run --example deface_debug -- <series_dir>
//! ```
//!
//! Prints the volume axes, orthonormality, and the anterior/posterior split of
//! removed voxels (measured against the true patient axes).

use std::path::PathBuf;

use diviz_rs_app::deface_debug_from_dir;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: deface_debug <series_dir>");
    match deface_debug_from_dir(&PathBuf::from(dir)) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("error: {e}"),
    }
}
