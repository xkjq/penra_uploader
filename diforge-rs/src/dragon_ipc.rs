use crossbeam_channel::Sender;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

pub type SharedWriters = Arc<Mutex<Vec<Arc<Mutex<TcpStream>>>>>;

fn handle_stream(stream: TcpStream, tx: Sender<String>, writers: SharedWriters) {
    let writer = Arc::new(Mutex::new(stream.try_clone().unwrap_or_else(|_| stream.try_clone().unwrap_or_else(|_| panic!()))));
    // store writer
    {
        let mut w = writers.lock().unwrap();
        w.push(writer.clone());
    }

    thread::spawn(move || {
        let mut buf = String::new();
        let mut reader = BufReader::new(stream);
        // read until EOF; send as newline-separated messages
        loop {
            buf.clear();
            match reader.read_line(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    for line in buf.split('\n') {
                        let s = line.trim();
                        if !s.is_empty() {
                            let _ = tx.send(s.to_string());
                        }
                    }
                }
            }
        }
        // remove writer on disconnect
        let mut w = writers.lock().unwrap();
        w.retain(|x| !Arc::ptr_eq(x, &writer));
    });
}

/// Start a TCP listener on 127.0.0.1:54231 that forwards incoming text to `tx`.
/// Also keeps writer handles so the app can send commands to connected helpers.
pub fn start_listener(tx: Sender<String>) -> SharedWriters {
    let writers: SharedWriters = Arc::new(Mutex::new(Vec::new()));
    let writers_clone = writers.clone();
    thread::spawn(move || {
        // best-effort bind; if fails, return silently
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", 54231)) {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => handle_stream(s, tx.clone(), writers_clone.clone()),
                    Err(_) => break,
                }
            }
        }
    });
    writers
}

/// Send a JSON command to all connected helpers (best effort).
pub fn send_to_helpers(writers: &SharedWriters, msg: &serde_json::Value) {
    let txt = msg.to_string() + "\n";
    let w = writers.lock().unwrap();
    for ws in w.iter() {
        if let Ok(mut s) = ws.lock() {
            let _ = s.write_all(txt.as_bytes());
        }
    }
}
