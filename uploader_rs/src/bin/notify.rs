use nng::{Protocol, Socket};

fn main() {
    let socket = Socket::new(Protocol::Pair0).expect("failed to create socket");
    match socket.dial("tcp://127.0.0.1:9976") {
        Ok(_) => {
            let _ = socket.send(&b"loaded"[..]);
            println!("sent loaded");
        }
        Err(e) => {
            eprintln!("failed to dial: {:?}", e);
        }
    }
}
