use interprocess::local_socket::LocalSocketStream;
use std::io::Write;

fn main() {
    // Attempt to connect to the per-user IPC socket and send 'loaded'.
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| format!("pid{}", std::process::id()));
    let ipc_name = format!("uploader_rs_{}", user);
    match LocalSocketStream::connect(ipc_name.as_str()) {
        Ok(mut s) => {
            let _ = s.write_all(b"loaded");
            println!("sent loaded");
        }
        Err(e) => {
            eprintln!("failed to connect to ipc socket: {:?}", e);
        }
    }
}
