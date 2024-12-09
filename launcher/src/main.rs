use nng::{Protocol, Socket, Error};
use std::process::Command;
//use rfd::MessageDialog;
//use serde::Deserialize;
//use std::fs;
//use std::path::PathBuf;

//#[derive(Debug, Deserialize)]
//struct Settings {
//    cris_tools_path: PathBuf,
//    port: toml::Value,
//}

//fn load_settings() -> Result<Settings, toml::de::Error> {
//    // Read the entire contents of the file
//    let contents = fs::read_to_string("uploader.toml")
//        .expect("Failed to read settings file");
//
//    // Parse the file contents and deserialize into the Settings struct
//    toml::from_str(&contents)
//}

fn main() {
    //let settings = match load_settings() {
    //    Ok(settings) => settings,
    //    Err(e) => {
    //        eprintln!("Failed to load settings: {:?}", e);
    //        std::process::exit(1);
    //    }
    //};

    // Create a socket of type REQ (request)
    let socket = Socket::new(Protocol::Pair0).expect("Failed to create socket");

    // Connect to the NNG server
    //match socket.dial(format!("tcp://localhost:{}", settings.port).as_str()) {
    match socket.dial(format!("tcp://localhost:9976").as_str()) {
        Ok(_) => {
            println!("Connected to server");
        }
        Err(e) => {
            println!("Failed to connect to server: {:?}", e);
            println!("Launching uploader tool");

            let output = Command::new("Uploader.exe")
                .spawn();

            std::process::exit(1);
        }
    }

    // Get the command line argument
    //let arg = std::env::args().nth(1).expect("Missing argument");
    //let arg = match std::env::args().nth(1) {
    //    Some(arg) => arg,
    //    None => {
    //        eprintln!("Missing argument");
    //        std::process::exit(1);
    //    }
    //};

    // Send the argument to the server
    //let message = "run/".to_string() + &arg;
    let message = "loaded".to_string();
    println!("Send message: {}", message);
    socket.send(message.as_bytes()).expect("Failed to send message");

    // Receive the response from the server
    match socket.recv() {
        Ok(response) => {
            let response = String::from_utf8(response.to_vec()).expect("Failed to receive response");
            println!("Response: {}", response);
        }
        Err(Error::TimedOut) => {
            println!("Failed to receive response: Timeout");
        }
        Err(e) => {
            println!("Failed to receive response: {:?}", e);
        }
    }
}