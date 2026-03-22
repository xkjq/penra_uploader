use std::env;
use std::io::{self, Read};
use std::net::TcpStream;
use std::io::Write;

fn main() {
    // Usage: dragon_helper [text]
    // If no argument, read stdin.
    let args: Vec<String> = env::args().collect();
    let mut payload = String::new();
    if args.len() > 1 {
        payload = args[1..].join(" ");
    } else {
        let _ = io::stdin().read_to_string(&mut payload);
    }
    if payload.is_empty() {
        return;
    }
    if let Ok(mut s) = TcpStream::connect(("127.0.0.1", 54231)) {
        let _ = s.write_all(payload.as_bytes());
    }
}
