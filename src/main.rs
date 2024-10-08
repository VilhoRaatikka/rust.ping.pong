use chrono::prelude::*;
use serde::{Serialize, Deserialize};
use std::env;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::io::{BufReader, AsyncBufReadExt};
use tokio::sync::mpsc;

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    serial_number: u32,
    timestamp: DateTime<Utc>,
    content: String,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <port> <target_port>", args[0]);
        return;
    }

    let port = &args[1];
    let target_port = &args[2];
    let address = format!("127.0.0.1:{}", port);
    let target_address = format!("127.0.0.1:{}", target_port);

    // Kääritään socket Arc-kääreeseen
    let socket = Arc::new(UdpSocket::bind(&address).await.expect("Couldn't bind to address"));

    println!("Type your message and press Enter to send...");

    // Luodaan kanava lähettäjän ja syötteen käsittelyn välille
    let (tx, mut rx) = mpsc::channel::<String>(100);

    // Kopioidaan socket eri tehtäviä varten
    let send_socket = Arc::clone(&socket);
    let recv_socket = Arc::clone(&socket);

    // Lähettäjän tehtävä
    let send_task = tokio::spawn(async move {
        let mut serial_number = 1;

        while let Some(msg) = rx.recv().await {
            // Luodaan uusi viesti
            let message = Message {
                serial_number,
                timestamp: Utc::now(),
                content: msg,
            };

            // Serialisoidaan viesti JSON-muotoon
            let serialized = serde_json::to_string(&message).expect("Failed to serialize message");

            if let Err(e) = send_socket.send_to(serialized.as_bytes(), &target_address).await {
                eprintln!("Failed to send message: {}", e);
            } else {
                println!("Sent message {}: {}", serial_number, message.content);
                serial_number += 1;
            }
        }
    });

    // Käyttäjän syötteen käsittely
    let stdin_task = tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut stdin_lines = BufReader::new(stdin).lines();

        while let Ok(Some(line)) = stdin_lines.next_line().await {
            let msg = line.trim().to_string();
            if !msg.is_empty() {
                if let Err(e) = tx.send(msg).await {
                    eprintln!("Failed to send message to channel: {}", e);
                    break;
                }
            }
        }

        // Suljetaan lähetyskanava, kun stdin päättyy
        drop(tx);
    });

    // Vastaanottajan tehtävä
    let recv_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match recv_socket.recv_from(&mut buf).await {
                Ok((n, src)) => {
                    let received = String::from_utf8_lossy(&buf[..n]).to_string();

                    // Yritetään deserialisoida viesti
                    if let Ok(message) = serde_json::from_str::<Message>(&received) {
                        println!("Received message from {}:", src);
                        println!("  Serial Number: {}", message.serial_number);
                        println!("  Timestamp: {}", message.timestamp);
                        println!("  Content: {}", message.content);

                        // Lähetetään kuittaus takaisin
                        let ack = format!("ACK:{}", message.serial_number);
                        if let Err(e) = recv_socket.send_to(ack.as_bytes(), &src).await {
                            eprintln!("Failed to send ACK: {}", e);
                        } else {
                            println!("Sent ACK to {}: {}", src, message.serial_number);
                        }
                    } else if received.starts_with("ACK:") {
                        // Käsitellään kuittaus
                        let ack_number = &received[4..];
                        println!("Received ACK from {}: {}", src, ack_number);
                    } else {
                        eprintln!("Failed to deserialize message: Invalid format");
                    }
                }
                Err(e) => {
                    eprintln!("Failed to receive: {}", e);
                    break;
                }
            }
        }
    });

    // Odotetaan, että kaikki tehtävät päättyvät
    if let Err(e) = tokio::try_join!(stdin_task, send_task, recv_task) {
        eprintln!("Error: {:?}", e);
    }
}
