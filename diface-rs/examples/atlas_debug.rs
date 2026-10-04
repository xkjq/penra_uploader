//! Debug the atlas registration for a series.
//!
//! ```bash
//! cargo run --example atlas_debug -- <series_dir>
//! ```

use diface_rs::atlas::{Atlas, AtlasBackend, AtlasParams};
use diface_rs::backend::DefacingBackend;
use diface_rs::geometric::{choose_threshold, head_statistics, Threshold};
use diface_rs::series::{collect_dicom_files, load_series};

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: atlas_debug <series_dir>");
    let files = collect_dicom_files(std::path::Path::new(&dir), true).expect("list");
    let (volume, _entries) = load_series(&files).expect("load");

    let th = choose_threshold(&volume, &Threshold::Auto);
    if let Some(stats) = head_statistics(&volume, th, 160) {
        println!("centroid {:?}", stats.centroid);
        println!("anterior {:?}", stats.anterior);
        println!("superior {:?}", stats.superior);
        println!("half {:?}", stats.half);
    } else {
        println!("no head stats");
    }

    let atlas = Atlas::synthetic([128, 160, 140], 1.0);
    let backend = AtlasBackend::new(atlas, AtlasParams::default());
    let mask = backend.compute_mask(&volume).expect("mask");
    println!(
        "atlas removed {} of {} voxels",
        mask.count_removed(),
        mask.remove.len()
    );

    // Anterior/posterior split of removed voxels.
    let (mut ant, mut post) = (0usize, 0usize);
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                if mask.is_removed(x, y, z) {
                    if volume.world(x, y, z)[1] < 0.0 {
                        ant += 1;
                    } else {
                        post += 1;
                    }
                }
            }
        }
    }
    let total = (ant + post).max(1);
    println!(
        "removed anterior {:.1}% posterior {:.1}%",
        100.0 * ant as f64 / total as f64,
        100.0 * post as f64 / total as f64
    );
}
