fn main() {
    // Add the crate-local lib/ directory to the linker's search path
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-search=native={}/lib", manifest_dir);
    // Link against the dynamic vosk library
    println!("cargo:rustc-link-lib=dylib=vosk");
    // Add an rpath so the built binary will find the bundled lib at runtime
    // $ORIGIN/../lib means: relative to the binary location, look in ../lib
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
}
