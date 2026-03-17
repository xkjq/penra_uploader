use std::env;
use dicom_view_app::{run_viewer, run_viewer_with_file};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        run_viewer_with_file(args[1].clone());
    } else {
        run_viewer();
    }
}
