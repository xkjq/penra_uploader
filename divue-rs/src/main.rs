use divue_rs::run_meta_viewer;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <file1> [file2] ...", args.get(0).unwrap_or(&"divue".to_string()));
        std::process::exit(1);
    }
    
    let paths = args[1..].to_vec();
    run_meta_viewer(paths);
}
