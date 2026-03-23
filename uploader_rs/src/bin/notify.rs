use interprocess::local_socket::LocalSocketStream;
use std::io::Write;

fn main() {
    // initialize basic tracing for this short-lived notifier (logs to stderr)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .with_target(false)
        .try_init();

    // Attempt to connect to the per-user IPC socket and send 'loaded'.
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| format!("pid{}", std::process::id()));
    let ipc_name = format!("uploader_rs_{}", user);
    match LocalSocketStream::connect(ipc_name.as_str()) {
        Ok(mut s) => {
            let _ = s.write_all(b"loaded");
            tracing::info!("sent loaded");
        }
        Err(e) => {
            tracing::error!("failed to connect to ipc socket: {:?}", e);
        }
    }
}
