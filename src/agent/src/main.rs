use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write, BufReader, BufRead};
use std::time::Duration;
use std::{str, thread};
use std::process::{Command, Child};

fn start_process() -> Child {
    let child = Command::new("gdbserver")
        .arg("127.0.0.1:7879")
        .arg("../bin/hello_world")
        .spawn()
        .expect("Failed to start process");

    child
}

fn handle_client(stream: &mut TcpStream, conn_stream: &mut TcpStream) {
    let mut buffer = [0; 200];

    let read_buf = BufReader::new(stream);

    for line in read_buf.lines() {
        let line = line.expect("error");
        println!("Received: {}", line);
        // You can process the line here
    }


    // let mut data_buf = vec![0u8; 0];
    // stream.read_to_end(&mut data_buf).expect("failed to read");

    // let msg = str::from_utf8(&data_buf).unwrap();
    // println!("AGENT Received: {}", msg);

    return;

    // Read data from the stream
    match stream.read_exact(&mut buffer) {
        Ok(_) => {
            // Convert bytes to string and process the message
            let msg = str::from_utf8(&buffer).unwrap();
            println!("AGENT Received: {}", msg);

            conn_stream.write_all(&buffer).expect("Failed to pass data to gdbserver.");
            conn_stream.flush().expect("Failed to flush");
            println!("Finished writing");
            let mut gdbserver_buf = [0; 200];
            match conn_stream.read_exact(&mut gdbserver_buf) {
                Ok(_) => {
                    let msg = str::from_utf8(&gdbserver_buf).unwrap();
                    println!("gdbserver Returned: {}", msg);
                    stream.write_all(&gdbserver_buf).expect("Failed to pass data to gdb.");
                    stream.flush().expect("Failed to flush");
                },
                Err(e) => eprintln!("Failed to receive data: {}", e),
            }

            // You can write back to the stream if needed
            // stream.write_all(b"Message received\n").unwrap();
        }
        Err(e) => println!("Failed to read from connection: {}", e),
    }
}

fn main() {
    // let mut child = start_process();
    // // child.kill().expect("Failed to gdbserver");
    // thread::sleep(Duration::from_secs(3));

    let mut conn_stream = TcpStream::connect("127.0.0.1:7879")
                                        .expect("Failed to establish connection to gdbserver");
    println!("Connected to the gdbserver");
    
    // Bind the listener to localhost on port 7878
    let listener = TcpListener::bind("127.0.0.1:7878").unwrap();

    println!("Server listening on port 7878");

    // Accept connections and process them
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                // Handle the connection in a separate function
                handle_client(&mut stream, &mut conn_stream);
            }
            Err(e) => {
                println!("Connection failed: {}", e);
            }
        }
    }
}
