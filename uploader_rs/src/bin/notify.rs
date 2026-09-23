use interprocess::local_socket::{prelude::*, ConnectOptions};
#[cfg(not(windows))]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use std::io::Write;
use std::path::PathBuf;

#[cfg(not(windows))]
fn local_socket_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

fn main() {
    // initialize basic tracing for this short-lived notifier (logs to stderr)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .with_target(false)
        .try_init();

    // Attempt to connect to the per-user IPC socket and send 'loaded'.
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| format!("pid{}", std::process::id()));
    let ipc_name = format!("uploader_rs_{}", user);
    let connect = {
        #[cfg(windows)]
        let name = ipc_name
            .as_str()
            .to_ns_name::<GenericNamespaced>();
        #[cfg(not(windows))]
        let name = ipc_name
            .as_str();
        #[cfg(not(windows))]
        let name = local_socket_path(name)
            .to_string_lossy()
            .into_owned()
            .to_fs_name::<GenericFilePath>();
        name.and_then(|name| ConnectOptions::new().name(name).connect_sync())
    };
    match connect {
        Ok(mut s) => {
            let _ = s.write_all(b"loaded");
            tracing::info!("sent loaded");
        }
        Err(e) => {
            tracing::error!("failed to connect to ipc socket: {:?}", e);
        }
    }
}
