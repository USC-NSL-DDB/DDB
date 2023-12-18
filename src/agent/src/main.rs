use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use std::str;
use std::process::{Command, Child};

fn start_process() -> Child {
    let child = Command::new("gdbserver")
        .arg("127.0.0.1:7879")
        .arg("../bin/hello_world")
        .spawn()
        .expect("Failed to start process");

    child
}

fn handle_client(mut stream: TcpStream) {
    let mut buffer = [0; 1024];

    // Read data from the stream
    match stream.read(&mut buffer) {
        Ok(_) => {
            // Convert bytes to string and process the message
            let msg = str::from_utf8(&buffer).unwrap();
            println!("Received: {}", msg);

            // You can write back to the stream if needed
            // stream.write_all(b"Message received\n").unwrap();
        }
        Err(e) => println!("Failed to read from connection: {}", e),
    }
}

fn main() {
    let mut child = start_process();
    child.kill().expect("Failed to gdbserver");

    // ctrlc::set_handler(move || {
    //     println!("Start kill");
    // }).expect("Failed to set handler");

    // Bind the listener to localhost on port 7878
    let listener = TcpListener::bind("127.0.0.1:7878").unwrap();

    println!("Server listening on port 7878");

    // Accept connections and process them
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // Handle the connection in a separate function
                handle_client(stream);
            }
            Err(e) => {
                println!("Connection failed: {}", e);
            }
        }
    }
}
