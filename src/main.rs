use tokio::net::UdpSocket;
use tokio::io::{BufReader, AsyncBufReadExt};
use tokio::sync::mpsc;
use std::env;
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use chrono::prelude::*;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration, Instant};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug)]
enum MsgType {
    Hello,
    Normal,
    ServerList,
    Ping,
    PingResponse,
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    msg_type: MsgType,
    serial_number: Option<u32>,
    timestamp: String,
    content: Option<String>,
    servers: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug)]
struct ContentData {
    key: String,
    value: String,
}

#[tokio::main]
async fn main() {
    // 1. Parsitaan komentoriviparametrit
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("Usage: {} <own_port> [<first_server_port>]", args[0]);
        return;
    }

    let own_port = &args[1];
    let own_address = format!("127.0.0.1:{}", own_port);
    let own_address = Arc::new(own_address);

    let first_server_address = if args.len() == 3 {
        Some(format!("127.0.0.1:{}", args[2]))
    } else {
        None
    };

    // 2. Bindataan UdpSocket omaan osoitteeseen
    let socket = Arc::new(UdpSocket::bind(&*own_address).await.expect("Couldn't bind to address"));

    // 3. Alustetaan tunnetut palvelimet - listaa ylläpidetään lukitusmekanismin avulla
    let known_servers = Arc::new(RwLock::new(Vec::<String>::new()));

    // Lisätään oma osoite listalle
    {
        let mut servers = known_servers.write().await;
        servers.push((*own_address).clone());
    }

    // Jos on annettu ensimmäisen palvelimen osoite, lisätään se tunnettuun listaan
    if let Some(ref first_server) = first_server_address {
        let mut servers = known_servers.write().await;
        if !servers.contains(first_server) {
            servers.push(first_server.clone());
        }
    }

    println!("Type your message and press Enter to send...");

    // 4. Luodaan kanava käyttäjän syötteen välittämiseksi lähetys-tehtävälle
    let (tx, mut rx) = mpsc::channel::<String>(100);

    // 5. Klonataan Arcit eri tehtäviä varten
    let send_socket = Arc::clone(&socket);
    let recv_socket = Arc::clone(&socket);
    let known_servers_send = Arc::clone(&known_servers);
    let own_address_send = Arc::clone(&own_address);

    // 6. Lähetys-tehtävä: vastaanottaa käyttäjän syötteen ja lähettää viestejä
    let send_task = tokio::spawn(async move {
        let mut serial_number = 1;
        let mut last_send_times: HashMap<String, Instant> = HashMap::new();

        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    let message = Message {
                        msg_type: MsgType::Normal,
                        serial_number: Some(serial_number),
                        timestamp: Utc::now().to_rfc3339(),
                        content: Some(msg.clone()),
                        servers: None,
                    };

                    let serialized = serde_json::to_string(&message).expect("Failed to serialize message");
                    let servers = known_servers_send.read().await.clone();
                    
                    for server in &servers {
                        if server != &*own_address_send {
                            if let Err(e) = send_socket.send_to(serialized.as_bytes(), &server).await {
                                eprintln!("Failed to send message to {}: {}", server, e);
                            } else {
                                println!("Sent message {}: {}", serial_number, msg);
                                last_send_times.insert(server.clone(), Instant::now());
                            }
                        }
                    }

                    serial_number += 1;
                }
                _ = sleep(Duration::from_millis(100)) => {
                    let servers = known_servers_send.read().await.clone();
                    let now = Instant::now();
                    
                    for server in &servers {
                        if server != &*own_address_send {
                            let last_send = last_send_times.get(server).copied().unwrap_or(Instant::now() - Duration::from_secs(2));
                            
                            if now.duration_since(last_send) >= Duration::from_secs(1) {
                                let ping_message = Message {
                                    msg_type: MsgType::Ping,
                                    serial_number: None,
                                    timestamp: Utc::now().to_rfc3339(),
                                    content: None,
                                    servers: None,
                                };
                                
                                let serialized = serde_json::to_string(&ping_message).expect("Failed to serialize ping message");
                                if let Err(e) = send_socket.send_to(serialized.as_bytes(), &server).await {
                                    eprintln!("Failed to send ping to {}: {}", server, e);
                                } else {
                                    last_send_times.insert(server.clone(), now);
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    // 7. Klonataan Arcit stdin-tehtävää varten
    let tx_clone = tx.clone();
    // Stdin-tehtävä: lukee käyttäjän syötteen ja lähettää sen lähetys-tehtävälle
    let stdin_task = tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut stdin_lines = BufReader::new(stdin).lines();
        let mut serial_number = 1;

        while let Ok(Some(line)) = stdin_lines.next_line().await {
            let input = line.trim();
            
            if let Some((key, value)) = input.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                
                if !key.is_empty() && !value.is_empty() {
                    // Luodaan JSON-objekti key-value -parista
                    let content = serde_json::json!({
                        "key": key,
                        "value": value
                    }).to_string();

                    let message = Message {
                        msg_type: MsgType::Normal,
                        serial_number: Some(serial_number),
                        timestamp: Utc::now().to_rfc3339(),
                        content: Some(content),
                        servers: None,
                    };

                    if let Err(e) = tx_clone.send(serde_json::to_string(&message).expect("Failed to serialize message")).await {
                        eprintln!("Failed to send message to channel: {}", e);
                        break;
                    }
                    serial_number += 1;
                } else {
                    eprintln!("Invalid input format. Use: key=value");
                }
            } else {
                eprintln!("Invalid input format. Use: key=value");
            }
        }

        drop(tx_clone);
    });

    // 8. Klonataan Arcit vastaanotto-tehtävää varten
    let known_servers_recv = Arc::clone(&known_servers);
    let own_address_recv = Arc::clone(&own_address);
    let recv_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match recv_socket.recv_from(&mut buf).await {
                Ok((n, src)) => {
                    let received = String::from_utf8_lossy(&buf[..n]).to_string();

                    // Yritetään deserialisoida viesti
                    match serde_json::from_str::<Message>(&received) {
                        Ok(message) => {
                            match message.msg_type {
                                MsgType::Hello => {
                                    println!("Received Hello from {} at {}", src, message.timestamp);

                                    // Lisätään lähettäjä tunnettuun listaan
                                    {
                                        let mut servers = known_servers_recv.write().await;
                                        if !servers.contains(&src.to_string()) {
                                            servers.push(src.to_string());
                                            println!("Added server: {}", src);
                                        }
                                    }

                                    // Luo päivitetty ServerList
                                    let servers = known_servers_recv.read().await.clone();
                                    let server_list_message = Message {
                                        msg_type: MsgType::ServerList,
                                        serial_number: None,
                                        timestamp: Utc::now().to_rfc3339(),
                                        content: None,
                                        servers: Some(servers),
                                    };

                                    let serialized = serde_json::to_string(&server_list_message)
                                        .expect("Failed to serialize server list");

                                    // Lähetetään ServerList kaikille tunnetuille palvelimille
                                    let servers = known_servers_recv.read().await.clone();
                                    for server in &servers {
                                        if server != &*own_address_recv { // Vältetään lähettäminen itselle
                                            if let Err(e) = recv_socket.send_to(serialized.as_bytes(), &server).await {
                                                eprintln!("Failed to send ServerList to {}: {}", server, e);
                                            } else {
                                                println!("Sent ServerList to {}", server);
                                            }
                                        }
                                    }
                                },
                                MsgType::Normal => {
                                    // Tarkista, onko lähettäjä tunnettu
                                    let sender = src.to_string();
                                    let is_known = {
                                        let servers = known_servers_recv.read().await;
                                        servers.contains(&sender)
                                    };

                                    if !is_known {
                                        println!("Unknown sender: {}. Sending Hello to request ServerList.", src);
                                        // Lähetetään Hello viestin lähettäjälle
                                        let send_socket_hello = Arc::clone(&recv_socket);
                                        let sender_clone = sender.clone();
                                        tokio::spawn(async move {
                                            let hello_message = Message {
                                                msg_type: MsgType::Hello,
                                                serial_number: None,
                                                timestamp: Utc::now().to_rfc3339(),
                                                content: None,
                                                servers: None,
                                            };
                                            let serialized = serde_json::to_string(&hello_message).expect("Failed to serialize Hello message");
                                            if let Err(e) = send_socket_hello.send_to(serialized.as_bytes(), &sender_clone).await {
                                                eprintln!("Failed to send Hello to {}: {}", sender_clone, e);
                                            } else {
                                                println!("Sent Hello to {}", sender_clone);
                                            }
                                        });
                                    }

                                    if let Some(serial_number) = message.serial_number {
                                        println!("Received message from {}:", src);
                                        println!("  Serial Number: {}", serial_number);
                                        println!("  Timestamp: {}", message.timestamp);
                                        println!("  Content: {}", message.content.unwrap_or_default());

                                        // Lähetetään ACK
                                        let ack = format!("ACK:{}", serial_number);
                                        if let Err(e) = recv_socket.send_to(ack.as_bytes(), &src).await {
                                            eprintln!("Failed to send ACK to {}: {}", src, e);
                                        } else {
                                            println!("Sent ACK to {}: {}", src, serial_number);
                                        }
                                    }
                                },
                                MsgType::ServerList => {
                                    if let Some(servers) = message.servers {
                                        println!("Received ServerList from {} at {}", src, message.timestamp);
                                        // Lisätään kaikki palvelimet listalle
                                        {
                                            let mut known = known_servers_recv.write().await;
                                            for server in servers {
                                                if &server != &*own_address_recv && !known.contains(&server) {
                                                    known.push(server.clone());
                                                    println!("Added server from ServerList: {}", server);
                                                }
                                            }
                                        }
                                    }
                                },
                                MsgType::Ping => {
                                    // Vastataan ping-viestiin
                                    let ping_response = Message {
                                        msg_type: MsgType::PingResponse,
                                        serial_number: None,
                                        timestamp: Utc::now().to_rfc3339(),
                                        content: None,
                                        servers: None,
                                    };
                                    
                                    let serialized = serde_json::to_string(&ping_response)
                                        .expect("Failed to serialize ping response");
                                        
                                    if let Err(e) = recv_socket.send_to(serialized.as_bytes(), &src).await {
                                        eprintln!("Failed to send ping response to {}: {}", src, e);
                                    } else {
                                        //println!("Received ping from {} and sent response", src);
                                    }
                                },
                                MsgType::PingResponse => {
                                    //println!("Received ping response from {}", src);
                                },
                            }
                        },
                        Err(_) => {
                            // Jos viesti ei ole validi JSON, tarkistetaan onko se ACK
                            if received.starts_with("ACK:") {
                                let ack_number = &received[4..];
                                println!("Received ACK from {}: {}", src, ack_number);
                            } else {
                                eprintln!("Failed to deserialize message from {}: {}", src, received);
                            }
                        }
                    }
                },
                Err(e) => {
                    eprintln!("Failed to receive: {}", e);
                    break;
                }
            }
        }
    });

    // 9. Jos ei ole ensimmäinen palvelin, lähetetään Hello-viesti ensimmäiselle palvelimelle
    if let Some(first_server) = first_server_address.clone() {
        // Lähetetään Hello-viesti ensimmäiselle palvelimelle
        let send_socket_hello = Arc::clone(&socket);
        tokio::spawn(async move {
            let hello_message = Message {
                msg_type: MsgType::Hello,
                serial_number: None,
                timestamp: Utc::now().to_rfc3339(),
                content: None,
                servers: None,
            };
            let serialized = serde_json::to_string(&hello_message).expect("Failed to serialize Hello message");
            if let Err(e) = send_socket_hello.send_to(serialized.as_bytes(), &first_server).await {
                eprintln!("Failed to send Hello to {}: {}", first_server, e);
            } else {
                println!("Sent Hello to {}", first_server);
            }
        });
    }

    // 10. Odotetaan, että kaikki tehtävät päättyvät
    tokio::select! {
        _ = send_task => (),
        _ = stdin_task => (),
        _ = recv_task => (),
    }
}
