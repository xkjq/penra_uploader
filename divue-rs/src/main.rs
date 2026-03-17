use divue_rs::{run_meta_viewer, run_interactive};
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        // Interactive mode: launch file picker UI
        run_interactive();
    } else {
        // Direct comparison mode: use provided file paths
        let paths = args[1..].to_vec();
        run_meta_viewer(paths);
    }
}
