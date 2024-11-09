use tokio::net::UdpSocket;
use serde::{Serialize, Deserialize};
use chrono::prelude::*;
use std::env;

#[derive(Serialize, Deserialize, Debug)]
enum MsgType {
    Hello,
    Normal,
    ServerList,
    Ping,
    PingResponse,
}

#[derive(Serialize, Deserialize, Debug)]
enum Command {
    Put,
    Get,
    Delete,
    Exists,
    List,
}

#[derive(Serialize, Deserialize, Debug)]
enum MessageRole {
    Command,
    Response,
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    msg_type: MsgType,
    serial_number: Option<u32>,
    timestamp: String,
    content: Option<String>,
    command: Option<Command>,
    servers: Option<Vec<String>>,
    forwarded: bool,
    role: MessageRole,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <server_port> <command> [key] [value]", args[0]);
        eprintln!("Commands:");
        eprintln!("  put <key> <value>");
        eprintln!("  get <key>");
        eprintln!("  delete <key>");
        eprintln!("  exists <key>");
        eprintln!("  list");
        return;
    }

    let server_port = &args[1];
    let command = &args[2].to_lowercase();
    let server_addr = format!("127.0.0.1:{}", server_port);

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("Couldn't bind client socket");

    let message = match command.as_str() {
        "put" => {
            if args.len() != 5 {
                eprintln!("Put command requires key and value");
                return;
            }
            let content = serde_json::json!({
                "key": args[3],
                "value": args[4]
            }).to_string();
            Message {
                msg_type: MsgType::Normal,
                serial_number: Some(1),
                timestamp: Utc::now().to_rfc3339(),
                content: Some(content),
                command: Some(Command::Put),
                servers: None,
                forwarded: false,
                role: MessageRole::Command,
            }
        },
        "get" => {
            if args.len() != 4 {
                eprintln!("Get command requires key");
                return;
            }
            Message {
                msg_type: MsgType::Normal,
                serial_number: Some(1),
                timestamp: Utc::now().to_rfc3339(),
                content: Some(args[3].clone()),
                command: Some(Command::Get),
                servers: None,
                forwarded: false,
                role: MessageRole::Command,
            }
        },
        "delete" => {
            if args.len() != 4 {
                eprintln!("Delete command requires key");
                return;
            }
            Message {
                msg_type: MsgType::Normal,
                serial_number: Some(1),
                timestamp: Utc::now().to_rfc3339(),
                content: Some(args[3].clone()),
                command: Some(Command::Delete),
                servers: None,
                forwarded: false,
                role: MessageRole::Command,
            }
        },
        "exists" => {
            if args.len() != 4 {
                eprintln!("Exists command requires key");
                return;
            }
            Message {
                msg_type: MsgType::Normal,
                serial_number: Some(1),
                timestamp: Utc::now().to_rfc3339(),
                content: Some(args[3].clone()),
                command: Some(Command::Exists),
                servers: None,
                forwarded: false,
                role: MessageRole::Command,
            }
        },
        "list" => {
            if args.len() != 3 {
                eprintln!("List command doesn't take any arguments");
                return;
            }
            Message {
                msg_type: MsgType::Normal,
                serial_number: Some(1),
                timestamp: Utc::now().to_rfc3339(),
                content: Some("".to_string()),
                command: Some(Command::List),
                servers: None,
                forwarded: false,
                role: MessageRole::Command,
            }
        },
        _ => {
            Message {
                msg_type: MsgType::Normal,
                serial_number: Some(1),
                timestamp: Utc::now().to_rfc3339(),
                content: Some(command.to_string()),
                command: None,
                servers: None,
                forwarded: false,
                role: MessageRole::Command,
            }
        }
    };
    println!("DEBUG: Sending message: {:?}", message);
    let serialized = serde_json::to_string(&message).expect("Failed to serialize message");
    if let Err(e) = socket.send_to(serialized.as_bytes(), &server_addr).await {
        eprintln!("Failed to send message: {}", e);
    } else {
        println!("Sent command: {}", command);
    }

    // Odotetaan vastausta
    let mut buf = [0u8; 1024];
    match socket.recv_from(&mut buf).await {
        Ok((len, _)) => {
            let response = String::from_utf8_lossy(&buf[..len]);
            println!("Received response: {}", response);
        },
        Err(e) => eprintln!("Failed to receive response: {}", e),
    }
} 