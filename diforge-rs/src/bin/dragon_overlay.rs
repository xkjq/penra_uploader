use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

fn main() {
    // Simple cross-platform overlay helper (testing prototype).
    // Connects to 127.0.0.1:54231 and listens for JSON commands.
    // On `{"cmd":"show_overlay","text":"..."}` it will print the text and allow
    // the user to type a response which will be sent back to the app as a newline-terminated message.

    let addr = "127.0.0.1:54231";
    println!("Connecting to app at {}...", addr);
    let mut stream = match TcpStream::connect(addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect: {}", e);
            return;
        }
    };

    // Make a reader clone
    let mut reader = match stream.try_clone() {
        Ok(r) => BufReader::new(r),
        Err(e) => {
            eprintln!("Failed to clone stream: {}", e);
            return;
        }
    };

    println!("Connected. Waiting for commands...");

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("Connection closed by server");
                break;
            }
            Ok(_) => {
                let txt = line.trim();
                if txt.is_empty() {
                    continue;
                }
                // try parse as JSON
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(txt) {
                    if let Some(cmd) = v.get("cmd").and_then(|c| c.as_str()) {
                        match cmd {
                            "show_overlay" => {
                                let body = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                println!("--- Overlay text from app ---\n{}\n------------------------------", body);
                                println!("Type a response to send back to the app. End with an empty line.");
                                // read lines from stdin until an empty line
                                let stdin = io::stdin();
                                let mut resp = String::new();
                                for l in stdin.lock().lines() {
                                    match l {
                                        Ok(l) => {
                                            if l.trim().is_empty() {
                                                break;
                                            }
                                            resp.push_str(&l);
                                            resp.push('\n');
                                        }
                                        Err(_) => break,
                                    }
                                }
                                if !resp.is_empty() {
                                    let _ = stream.write_all(resp.as_bytes());
                                }
                            }
                            "set_overlay_position" => {
                                let x = v.get("x").and_then(|n| n.as_i64()).unwrap_or(0);
                                let y = v.get("y").and_then(|n| n.as_i64()).unwrap_or(0);
                                let w = v.get("w").and_then(|n| n.as_i64()).unwrap_or(0);
                                let h = v.get("h").and_then(|n| n.as_i64()).unwrap_or(0);
                                println!("Received overlay position: x={}, y={}, w={}, h={}", x, y, w, h);
                                // No real overlay on terminal; just acknowledge
                                let ack = format!("overlay_position_ack {} {} {} {}\n", x, y, w, h);
                                let _ = stream.write_all(ack.as_bytes());
                            }
                            other => {
                                println!("Unknown command: {}", other);
                            }
                        }
                    }
                } else {
                    // treat plain text as incoming message
                    println!("Message: {}", txt);
                }
            }
            Err(e) => {
                eprintln!("Read error: {}", e);
                break;
            }
        }
        line.clear();
    }
}
