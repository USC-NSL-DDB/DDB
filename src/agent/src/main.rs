use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write, BufReader, BufRead};
use std::time::Duration;
use std::{str, thread};
use std::process::{Command, Child};
use std::sync::mpsc;

fn start_process() -> Child {
    let child = Command::new("gdbserver")
        .arg("127.0.0.1:7879")
        .arg("../bin/hello_world")
        .spawn()
        .expect("Failed to start process");

    child
}

fn handle_connection(mut client_stream: TcpStream) {
    let mut server_stream = TcpStream::connect("127.0.0.1:7879")
                                        .expect("Failed to establish connection to gdbserver");
    println!("Connected to the gdbserver");

    let mut client_reader = client_stream.try_clone().unwrap();
    let mut server_reader = server_stream.try_clone().unwrap();

    let client_to_server = std::thread::spawn(move || {
        if let Err(e) = std::io::copy(&mut client_reader, &mut server_stream) {
            eprintln!("Error copying client to server: {:?}", e);
        }
    });

    let server_to_client = std::thread::spawn(move || {
        if let Err(e) = std::io::copy(&mut server_reader, &mut client_stream) {
            eprintln!("Error copying server to client: {:?}", e);
        }
    });

    client_to_server.join().unwrap();
    server_to_client.join().unwrap();

    println!("Finished io printing.");
}

fn handle_client(stream: &mut TcpStream, conn_stream: &mut TcpStream, sender: mpsc::Sender<&str>) {
    let mut buffer = [0; 40960];

    // let mut data_buf = vec![0u8; 0];
    // stream.read_to_end(&mut data_buf).expect("failed to read");

    // let msg = str::from_utf8(&data_buf).unwrap();
    // println!("AGENT Received: {}", msg);
    // let mut msg = String::from("");

    loop {
        // Read data from the stream
        match stream.read(&mut buffer) {
            Ok(size) => {
                if size == 0 { break }

                println!("Size is not zero. Keep reading.");

                // Convert bytes to string and process the message

                let msg = str::from_utf8(&buffer).unwrap();
                if msg.starts_with("+") { continue }

                // msg += curr_msg;

                println!("AGENT Received: {}", msg);

                conn_stream.write_all(&buffer).expect("Failed to write");
                conn_stream.flush().expect("Failed to flush");

                let mut conn_buf = [0u8; 40960];
                match conn_stream.read(&mut conn_buf) {
                    Ok(size) => {
                        let msg = str::from_utf8(&conn_buf).unwrap();
                        println!("gdbserver returns: {}", msg);
                        stream.write_all(&conn_buf).unwrap();
                        stream.flush().unwrap();
                    },
                    Err(e) => eprintln!("failed to read from gdbserver buffer {}", e),
                }
                // conn_stream.read(&mut conn_buf).expect("Failed");

                // for byte in buffer {
                //     // if byte == 0 { break }

                //     print!("{} ", byte);
                // }
                // println!();

                // for c in msg.chars() {
                //     print!("{} ", c as u8);
                // }
                // println!();

                // You can write back to the stream if needed
                // stream.write_all(b"Message received\n").unwrap();
            }
            Err(e) => println!("Failed to read from connection: {}", e),
        }
    }
    // println!("FINAL MSG: {msg}");
    println!("Stream finished.")

}

fn main() {
    // let mut child = start_process();
    // // child.kill().expect("Failed to gdbserver");
    // thread::sleep(Duration::from_secs(3));
    let (tx, rx) = mpsc::channel();
    tx.send("hello");

    
    // Bind the listener to localhost on port 7878
    let listener = TcpListener::bind("127.0.0.1:7878").unwrap();

    println!("Server listening on port 7878");

    // Accept connections and process them
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // Handle the connection in a separate function
                // handle_client(&mut stream, &mut conn_stream, tx.clone());
                handle_connection(stream);
            }
            Err(e) => {
                println!("Connection failed: {}", e);
            }
        }
    }
}
