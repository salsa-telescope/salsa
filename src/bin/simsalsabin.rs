use clap::Parser;
use std::io::prelude::*;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process;

fn handle(request: &[u8]) -> [u8; 12] {
    if request
        == [
            0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6F,
        ]
    {
        println!("Got direction request");
        // ACK
        [
            0x58, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20,
        ]
    } else if request
        == [
            0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0F,
        ]
    {
        println!("Got stop request");
        // ACK
        [
            0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20,
        ]
    } else {
        println!("Unknown request. Data: {:02X?}", request);
        // FIXME: Is this a proper error
        [
            0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]
    }
}

fn controller_connection(mut stream: TcpStream) {
    loop {
        let mut command_buffer = [0; 13];
        match stream.read(&mut command_buffer) {
            Ok(0) => {
                println!("Client closed connection.");
                break;
            }
            Ok(13) => {
                eprintln!("Client sent: {:02X?}", command_buffer);
                let response = handle(&command_buffer[0..12]);
                // FIXME: Error handling
                stream.write_all(&response).unwrap();
            }
            Ok(n) => {
                println!(
                    "Client sent {} bytes, expected 13. Data: {:02X?}",
                    n, command_buffer
                );
            }
            _ => {
                // FIXME: Handle these errors more gracefully.
                println!("Something went wrong!");
            }
        };
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    port: Option<u16>,
}

fn main() {
    let args = Args::parse();
    let addr = if let Some(port) = args.port {
        SocketAddr::from(([0, 0, 0, 0], port))
    } else {
        SocketAddr::from(([0, 0, 0, 0], 3001))
    };
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(err) => {
            println!("Failed to bind to address {} ({})", addr, err);
            process::exit(1);
        }
    };
    if let Some(port) = args.port
        && port == 0
    {
        // Tests need to know which port to connect to.
        println!("port:{}", listener.local_addr().unwrap().port());
    }
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => controller_connection(stream),
            Err(err) => {
                println!("Failed to accept connection ({})", err);
            }
        }
    }
}
