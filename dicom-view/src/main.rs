use std::env;
use dicom_view_app::{run_viewer, run_viewer_with_files};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        let paths = args[1..].iter().cloned().collect::<Vec<String>>();
        run_viewer_with_files(paths);
    } else {
        run_viewer();
    }
}
