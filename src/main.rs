use tokio::net::UdpSocket;
//use tokio::io::{BufReader, AsyncBufReadExt};
//use tokio::sync::mpsc;
use std::env;
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use chrono::prelude::*;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration, Instant};
use std::collections::HashMap;
use std::collections::BTreeMap;
use tokio::sync::RwLockReadGuard;
use std::io::ErrorKind;
use serde_json::Value;

#[derive(Serialize, Deserialize, Debug, Clone)]
enum MsgType {
    Hello,
    Normal,
    ServerList,
    Ping,
    PingResponse,
    InitialState,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum Command {
    Put,
    Get,
    Delete,
    Exists,
    List,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
enum MessageRole {
    Command,
    Response,
    Join,
    CurrentServers,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
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

#[derive(Serialize, Deserialize, Debug)]
struct ContentData {
    key: String,
    value: String,
}

#[derive(Debug)]
struct PingStats {
    sent: u32,
    received: u32,
    last_seen: Instant,
}

#[allow(dead_code)]
struct MessageBuffer {
    next_expected: u32,
    buffer: BTreeMap<u32, Message>,
}

#[allow(dead_code)]
impl MessageBuffer {
    fn new() -> Self {
        MessageBuffer {
            next_expected: 1,
            buffer: BTreeMap::new(),
        }
    }

    fn add_message(&mut self, msg: Message) -> Vec<Message> {
        let mut ready_messages = Vec::new();
        
        if let Some(seq) = msg.serial_number {
            println!("DEBUG: Processing message {} (expecting {})", seq, self.next_expected);
            
            if seq == self.next_expected {
                // Viesti on seuraava odotetussa järjestyksessä
                ready_messages.push(msg);
                self.next_expected += 1;

                // Tarkista onko puskurissa seuraavia viestejä
                while let Some(buffered_msg) = self.buffer.remove(&self.next_expected) {
                    ready_messages.push(buffered_msg);
                    self.next_expected += 1;
                }
            } else if seq > self.next_expected {
                // Viesti on tullut liian aikaisin, puskuroidaan
                println!("DEBUG: Buffering message {} (expecting {})", seq, self.next_expected);
                self.buffer.insert(seq, msg);
            } else {
                println!("DEBUG: Discarding old message {} (expecting {})", seq, self.next_expected);
            }
        }
        
        ready_messages
    }
}

#[allow(dead_code)]
struct ServerState {
    data: HashMap<String, String>,
}

#[allow(dead_code)]
struct ConsensusCollector {
    responses: HashMap<String, String>,
    command_id: String,
    required_count: usize,
    client_addr: String,
    created_at: Instant,
}

impl ConsensusCollector {
    fn new(required_count: usize, command_id: String, client_addr: String) -> Self {
        ConsensusCollector {
            responses: HashMap::new(),
            command_id,
            required_count,
            client_addr,
            created_at: Instant::now(),
        }
    }

    fn add_response(&mut self, server: String, response: String) -> Option<String> {
        println!("DEBUG: Adding response from {}: {}", server, response);
        self.responses.insert(server, response);

        // Odota kunnes required_count vastauksia on saatu
        if self.responses.len() >= self.required_count {
            println!("DEBUG: Got {} responses, required {}", self.responses.len(), self.required_count);
            
            // Jos kaikki vastaukset ovat samat, käytä sitä
            let first_response = self.responses.values().next().unwrap();
            if self.responses.values().all(|r| r == first_response) {
                println!("DEBUG: All responses match: {}", first_response);
                return Some(first_response.clone());
            }
            
            // Muuten käytä enemmistöpäätöstä
            let mut response_counts: HashMap<&String, usize> = HashMap::new();
            for response in self.responses.values() {
                *response_counts.entry(response).or_insert(0) += 1;
            }
            
            if let Some((response, count)) = response_counts
                .iter()
                .max_by_key(|(_, count)| *count) {
                println!("DEBUG: Most common response ({} votes): {}", count, response);
                if *count > self.required_count / 2 {
                    return Some((*response).clone());
                }
            }
            
            // Jos enemmistöä ei löydy, palautetaan virhe
            println!("DEBUG: No consensus reached among responses");
            return Some("Error: No consensus reached among nodes".to_string());
        }

        println!("DEBUG: Waiting for more responses ({}/{})", self.responses.len(), self.required_count);
        None
    }
}

type ConsensusMap = Arc<RwLock<HashMap<String, ConsensusCollector>>>;

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "Usage:\nFirst node: cargo run <port>\nOther nodes: cargo run <port> <first_node_port>"
        ));
    }

    let port = args[1].parse::<u16>().map_err(|e| {
        std::io::Error::new(ErrorKind::InvalidInput, format!("Invalid port number: {}", e))
    })?;

    let first_node_addr = if args.len() == 3 {
        let first_port = args[2].parse::<u16>().map_err(|e| {
            std::io::Error::new(ErrorKind::InvalidInput, format!("Invalid first node port number: {}", e))
        })?;
        Some(format!("127.0.0.1:{}", first_port))
    } else {
        None
    };

    println!("Starting node on port {}", port);
    if let Some(addr) = &first_node_addr {
        println!("Connecting to first node at {}", addr);
    } else {
        println!("Starting as first node");
    }

    let own_address = format!("127.0.0.1:{}", port);
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

    // Lisää tämä first_server_address käsittelyn jälkeen
    if let Some(ref first_server) = first_server_address {
        // Lähetä Hello-viesti ensimmäiselle palvelimelle
        let hello_message = Message {
            msg_type: MsgType::Hello,
            serial_number: None,
            timestamp: Utc::now().to_rfc3339(),
            content: None,
            servers: None,
            forwarded: false,
            command: None,
            role: MessageRole::Command,
        };
        
        let serialized = serde_json::to_string(&hello_message).expect("Failed to serialize hello message");
        if let Err(e) = socket.send_to(serialized.as_bytes(), first_server).await {
            eprintln!("Failed to send hello message: {}", e);
        } else {
            println!("Sent Hello message to {}", first_server);
        }
    }

    // 4. Luodaan kanava käyttäjän syötteen välittämiseksi lähetys-tehtävälle
    //let (tx, mut rx) = mpsc::channel::<String>(100);

    // 5. Klonataan Arcit eri tehtävi varten
    let send_socket = Arc::clone(&socket);
    let recv_socket = Arc::clone(&socket);
    let known_servers_send = Arc::clone(&known_servers);
    let known_servers_recv = Arc::clone(&known_servers);
    let own_address_send = Arc::clone(&own_address);
    let ping_stats = Arc::new(RwLock::new(HashMap::<String, PingStats>::new()));
    let ping_stats_send = Arc::clone(&ping_stats);
    let ping_stats_recv = Arc::clone(&ping_stats);
    //let tx_clone = tx.clone();

    // Main-funktiossa, muiden Arc-muuttujien luonnin yhteydessä
    let message_buffers = Arc::new(RwLock::new(HashMap::<String, MessageBuffer>::new()));
    let _message_buffers_recv = Arc::clone(&message_buffers);

    let _state = Arc::new(RwLock::new(ServerState {
        data: HashMap::new(),
    }));

    // Päätason muuttujien määrittely
    let consensus_collectors: ConsensusMap = Arc::new(RwLock::new(HashMap::new()));

    let _expected_serial = Arc::new(RwLock::new(1u32));
    let _expected_serial_clone = Arc::clone(&_expected_serial);

    // 6. Lähetys-tehtävä: vastaanottaa käyttäjän syötteen ja lähettää viestejä
    let send_task = tokio::spawn(async move {
        //let mut serial_numbers: HashMap<String, u32> = HashMap::new();
        let mut last_send_times: HashMap<String, Instant> = HashMap::new();
        
        loop {
            tokio::select! {
                /*Some(msg) = rx.recv() => {
                    let servers = known_servers_send.read().await.clone();
                    
                    for server in &servers {
                        if server != &*own_address_send {
                            let seq = serial_numbers.entry(server.clone())
                                .and_modify(|n| *n += 1)
                                .or_insert(1);
                                
                            let message = Message {
                                msg_type: MsgType::Normal,
                                serial_number: Some(*seq),
                                timestamp: Utc::now().to_rfc3339(),
                                content: Some(msg.clone()),
                                servers: None,
                                forwarded: false,
                                command: None,
                                role: MessageRole::Command,
                            };

                            let serialized = serde_json::to_string(&message).expect("Failed to serialize message");
                            if let Err(e) = send_socket.send_to(serialized.as_bytes(), &server).await {
                                eprintln!("Failed to send message to {}: {}", server, e);
                            } else {
                                println!("Sent message {} to {}: {}", seq, server, msg);
                            }
                        }
                    }
                }*/
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
                                    forwarded: false,
                                    command: None,
                                    role: MessageRole::Command,
                                };
                                
                                let serialized = serde_json::to_string(&ping_message).expect("Failed to serialize ping message");
                                if let Err(e) = send_socket.send_to(serialized.as_bytes(), &server).await {
                                    eprintln!("Failed to send ping to {}: {}", server, e);
                                } else {
                                    let mut stats = ping_stats_send.write().await;
                                    let stat = stats.entry(server.clone()).or_insert(PingStats {
                                        sent: 0,
                                        received: 0,
                                        last_seen: Instant::now(),
                                    });
                                    stat.sent += 1;
                                    last_send_times.insert(server.clone(), now);
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    // Määritellään recv_task
    let recv_task = async move {
        let mut buf = [0u8; 1024];
        loop {
            match recv_socket.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    println!("DEBUG: Received data from {}", src);
                    match serde_json::from_slice::<Message>(&buf[..len]) {
                        Ok(message) => {
                            println!("DEBUG: Message type: {:?}", message.msg_type);
                            match message.msg_type {
                                MsgType::Hello => {
                                    println!("DEBUG: Processing Hello message from {}", src);
                                    {
                                        let mut servers = known_servers_recv.write().await;
                                        if !servers.contains(&src.to_string()) {
                                            servers.push(src.to_string());
                                            println!("DEBUG: Added new server: {}", src);
                                        }
                                    }

                                    // Lähetä päivitetty lista kaikille tunnetuille nodeille
                                    let servers = known_servers_recv.read().await.clone();
                                    let server_list_message = Message {
                                        msg_type: MsgType::ServerList,
                                        serial_number: None,
                                        timestamp: Utc::now().to_rfc3339(),
                                        content: None,
                                        servers: Some(servers.clone()),
                                        forwarded: false,
                                        command: None,
                                        role: MessageRole::Command,
                                    };

                                    let serialized = serde_json::to_string(&server_list_message)
                                        .expect("Failed to serialize server list");
                                    
                                    // Lähetä lista kaikille nodeille
                                    for server in &servers {
                                        if server != &*own_address {
                                            if let Err(e) = recv_socket.send_to(serialized.as_bytes(), &server).await {
                                                eprintln!("Failed to send server list to {}: {}", server, e);
                                            } else {
                                                println!("DEBUG: Sent server list to {}", server);
                                            }
                                        }
                                    }
                                },
                                MsgType::ServerList => {
                                    println!("DEBUG: Received server list");
                                    if let Some(servers) = message.servers {
                                        let mut known = known_servers_recv.write().await;
                                        println!("DEBUG: Current known servers: {:?}", *known);
                                        for server in servers {
                                            if !known.contains(&server) {
                                                println!("DEBUG: Adding new server from list: {}", server);
                                                known.push(server);
                                            }
                                        }
                                        println!("DEBUG: Updated known servers: {:?}", *known);
                                    }
                                },
                                MsgType::Ping => {
                                    // Päivitä vastaanottotilasto
                                    {
                                        let mut stats = ping_stats_recv.write().await;
                                        let stat = stats.entry(src.to_string()).or_insert(PingStats {
                                            sent: 0,
                                            received: 0,
                                            last_seen: Instant::now(),
                                        });
                                        stat.received += 1;
                                        stat.last_seen = Instant::now();
                                    }

                                    // Lähetä ping-vastaus
                                    let ping_response = Message {
                                        msg_type: MsgType::PingResponse,
                                        serial_number: None,
                                        timestamp: Utc::now().to_rfc3339(),
                                        content: None,
                                        servers: None,
                                        forwarded: false,
                                        command: None,
                                        role: MessageRole::Response,
                                    };
                                    
                                    let serialized = serde_json::to_string(&ping_response)
                                        .expect("Failed to serialize ping response");
                                    
                                    if let Err(e) = recv_socket.send_to(serialized.as_bytes(), &src).await {
                                        eprintln!("Failed to send ping response to {}: {}", src, e);
                                    }
                                },
                                MsgType::Normal => {
                                    match serde_json::from_slice::<Message>(&buf[..len]) {
                                        Ok(parsed_msg) => {
                                            println!("DEBUG: Received Normal message with role: {:?}", parsed_msg.role);
                                            
                                            match parsed_msg.role {
                                                MessageRole::Join => {
                                                    println!("DEBUG: Processing join request from {}", src);
                                                    let mut servers = known_servers_recv.write().await;
                                                    if !servers.contains(&src.to_string()) {
                                                        servers.push(src.to_string());
                                                        println!("Added new server: {}", src);
                                                        
                                                        // Lähetä current_servers vastaus
                                                        let response = Message {
                                                            msg_type: MsgType::Normal,
                                                            serial_number: parsed_msg.serial_number,
                                                            timestamp: parsed_msg.timestamp.clone(),
                                                            content: Some(serde_json::to_string(&*servers).unwrap()),
                                                            command: None,
                                                            role: MessageRole::CurrentServers,
                                                            servers: None,
                                                            forwarded: false,
                                                        };
                                                        
                                                        if let Err(e) = socket.send_to(
                                                            serde_json::to_string(&response).unwrap().as_bytes(),
                                                            &src
                                                        ).await {
                                                            eprintln!("Failed to send current_servers: {}", e);
                                                        }

                                                        // Lähetä nykyinen tila
                                                        let state = _state.read().await;
                                                        let state_msg = Message {
                                                            msg_type: MsgType::InitialState,
                                                            serial_number: Some(1),
                                                            timestamp: chrono::Utc::now().to_rfc3339(),
                                                            content: Some(serde_json::to_string(&state.data).unwrap()),
                                                            command: None,
                                                            role: MessageRole::Response,
                                                            servers: None,
                                                            forwarded: false,
                                                        };

                                                        println!("DEBUG: Sending initial state to new node {}", src);
                                                        if let Err(e) = socket.send_to(
                                                            serde_json::to_string(&state_msg).unwrap().as_bytes(),
                                                            &src
                                                        ).await {
                                                            eprintln!("Failed to send initial state: {}", e);
                                                        }
                                                    }
                                                },
                                                MessageRole::Command => {
                                                    println!("DEBUG: Processing command message from {}", src);
                                                    println!("DEBUG: Message content: {:?}", parsed_msg);
                                                    
                                                    if !parsed_msg.forwarded {
                                                        if let Some(ref cmd) = parsed_msg.command {
                                                            println!("DEBUG: Command type: {:?}", cmd);
                                                            let command_id = format!("{}-{:?}", parsed_msg.timestamp, cmd);
                                                            
                                                            // Käsitellään komento paikallisesti
                                                            let response = handle_command(cmd, &_state, parsed_msg.content.as_deref().unwrap_or("")).await;
                                                            println!("DEBUG: Local response: {}", response);

                                                            let mut consensus_map = consensus_collectors.write().await;
                                                            let servers = known_servers_recv.read().await;
                                                            let other_nodes = servers.iter()
                                                                .filter(|&s| s != &src.to_string())
                                                                .count();
                                                            
                                                            println!("DEBUG: Found {} other nodes", other_nodes);
                                                            
                                                            if other_nodes > 0 {
                                                                // Vaaditaan vastaus kaikilta muilta nodeilta + oma vastaus
                                                                let required_count = other_nodes + 1;
                                                                let mut collector = ConsensusCollector::new(required_count, command_id.clone(), src.to_string());
                                                                
                                                                // Lisää oma vastaus
                                                                collector.add_response("self".to_string(), response.clone());
                                                                consensus_map.insert(command_id.clone(), collector);

                                                                // Välitä komento muille nodeille
                                                                let mut forwarded_msg = parsed_msg.clone();
                                                                forwarded_msg.forwarded = true;
                                                                
                                                                for server in servers.iter() {
                                                                    if server != &src.to_string() {
                                                                        println!("DEBUG: Forwarding command to {}", server);
                                                                        if let Err(e) = socket.send_to(
                                                                            serde_json::to_string(&forwarded_msg).unwrap().as_bytes(),
                                                                            server
                                                                        ).await {
                                                                            eprintln!("Failed to forward command to {}: {}", server, e);
                                                                        }
                                                                    }
                                                                }

                                                                // Aseta aikakatkaisu
                                                                let socket_clone = Arc::clone(&socket);
                                                                let consensus_collectors_clone = Arc::clone(&consensus_collectors);
                                                                let command_id_clone = command_id.clone();
                                                                let src_clone = src.to_string();
                                                                let response_clone = response.clone();

                                                                tokio::spawn(async move {
                                                                    tokio::time::sleep(Duration::from_secs(5)).await;
                                                                    let mut consensus_map = consensus_collectors_clone.write().await;
                                                                    if let Some(collector) = consensus_map.remove(&command_id_clone) {
                                                                        println!("DEBUG: Consensus timed out. Received {}/{} responses", 
                                                                            collector.responses.len(), collector.required_count);
                                                                        let timeout_msg = format!("Warning: Not all nodes responded. Using local response: {}", response_clone);
                                                                        if let Err(e) = socket_clone.send_to(timeout_msg.as_bytes(), &src_clone).await {
                                                                            eprintln!("Failed to send timeout response: {}", e);
                                                                        }
                                                                    }
                                                                });
                                                            } else {
                                                                // Jos olemme ainoa node, lähetä vastaus heti
                                                                if let Err(e) = socket.send_to(response.as_bytes(), &src).await {
                                                                    eprintln!("Failed to send response: {}", e);
                                                                }
                                                            }
                                                        } else {
                                                            println!("DEBUG: Command is None, sending error response");
                                                            let error_response = Message {
                                                                msg_type: MsgType::Normal,
                                                                serial_number: parsed_msg.serial_number,
                                                                timestamp: chrono::Utc::now().to_rfc3339(),
                                                                content: Some("Error: Invalid command format".to_string()),
                                                                command: None,
                                                                role: MessageRole::Response,
                                                                servers: None,
                                                                forwarded: false,
                                                            };

                                                            let response_str = serde_json::to_string(&error_response)
                                                                .expect("Failed to serialize error response");
                                                            println!("DEBUG: Sending error response: {}", response_str);

                                                            if let Err(e) = socket.send_to(
                                                                response_str.as_bytes(),
                                                                &src
                                                            ).await {
                                                                eprintln!("Failed to send error response: {}", e);
                                                            } else {
                                                                println!("DEBUG: Error response sent successfully");
                                                            }
                                                        }
                                                    } else {
                                                        // Käsittele välitetty komento ja lähetä vastaus
                                                        if let Some(ref cmd) = parsed_msg.command {
                                                            let response = handle_command(cmd, &_state, parsed_msg.content.as_deref().unwrap_or("")).await;
                                                            println!("DEBUG: Response to forwarded command: {}", response);
                                                            
                                                            let response_msg = Message {
                                                                msg_type: MsgType::Normal,
                                                                serial_number: parsed_msg.serial_number,
                                                                timestamp: parsed_msg.timestamp,
                                                                content: Some(response),
                                                                command: parsed_msg.command,
                                                                role: MessageRole::Response,
                                                                servers: None,
                                                                forwarded: true,
                                                            };
                                                            
                                                            if let Err(e) = socket.send_to(
                                                                serde_json::to_string(&response_msg).unwrap().as_bytes(),
                                                                &src
                                                            ).await {
                                                                eprintln!("Failed to send response to forwarded command: {}", e);
                                                            }
                                                        }
                                                    }
                                                },
                                                MessageRole::Response => {
                                                    if let Some(ref content) = parsed_msg.content {
                                                        if let Some(ref cmd) = parsed_msg.command {
                                                            println!("DEBUG: Received response for command {:?}: {}", cmd, content);
                                                            let command_id = format!("{}-{:?}", parsed_msg.timestamp, cmd);
                                                            
                                                            let mut consensus_map = consensus_collectors.write().await;
                                                            if let Some(collector) = consensus_map.get_mut(&command_id) {
                                                                if let Some(final_response) = collector.add_response(src.to_string(), content.clone()) {
                                                                    println!("DEBUG: Consensus reached for command {}", command_id);
                                                                    if let Err(e) = socket.send_to(
                                                                        final_response.as_bytes(),
                                                                        &collector.client_addr
                                                                    ).await {
                                                                        eprintln!("Failed to send consensus response: {}", e);
                                                                    }
                                                                    // Poistetaan konsensus-kerääjä kun konsensus on saavutettu
                                                                    consensus_map.remove(&command_id);
                                                                }
                                                            }
                                                        }
                                                    }
                                                },
                                                MessageRole::CurrentServers => {
                                                    println!("DEBUG: Received CurrentServers message");
                                                },
                                            }
                                        },
                                        Err(e) => {
                                            eprintln!("Failed to parse message: {}", e);
                                            let error_response = Message {
                                                msg_type: MsgType::Normal,
                                                serial_number: None,
                                                timestamp: chrono::Utc::now().to_rfc3339(),
                                                content: Some(format!("Error: Failed to parse message: {}", e)),
                                                command: None,
                                                role: MessageRole::Response,
                                                servers: None,
                                                forwarded: false,
                                            };

                                            if let Err(e) = socket.send_to(
                                                serde_json::to_string(&error_response).unwrap().as_bytes(),
                                                &src
                                            ).await {
                                                eprintln!("Failed to send parse error response: {}", e);
                                            }
                                        }
                                    }
                                },
                                MsgType::InitialState => {
                                    println!("DEBUG: Received initial state from {}", src);
                                    if let Some(content) = message.content {
                                        match serde_json::from_str::<HashMap<String, String>>(&content) {
                                            Ok(initial_data) => {
                                                let mut state = _state.write().await;
                                                state.data = initial_data;
                                                println!("DEBUG: Successfully initialized state with {} key-value pairs", state.data.len());
                                            },
                                            Err(e) => eprintln!("Failed to parse initial state: {}", e),
                                        }
                                    }
                                },
                                MsgType::PingResponse => {
                                    // Käsitellään vastaanotettu ping-vastaus
                                    println!("DEBUG: Received ping response from {}", src);
                                    let mut servers = known_servers_recv.write().await;
                                    if !servers.contains(&src.to_string()) {
                                        servers.push(src.to_string());
                                        println!("Added new server from ping response: {}", src);
                                    }
                                },
                            }
                        },
                        Err(e) => eprintln!("Failed to parse message: {}", e),
                    }
                },
                Err(e) => eprintln!("Failed to receive: {}", e),
            }
        }
    };

    // Kommentoidaan pois stdin_task
    /*
    let stdin_task = tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        
        while let Ok(Some(line)) = stdin.next_line().await {
            if let Err(e) = tx_clone.send(line).await {
                eprintln!("Failed to send message: {}", e);
                break;
            }
        }
    });
    */

    // Käynnistetään recv_task ensin
    let recv_handle = tokio::spawn(recv_task);

    // Odotetaan toista nodea
    if first_server_address.is_none() {
        println!("Waiting for other nodes to join the network...");
        loop {
            let servers = known_servers.read().await;
            if servers.len() > 1 {
                println!("Another node joined the network!");
                println!("Type your message and press Enter to send...");
                break;
            }
            drop(servers);
            sleep(Duration::from_millis(100)).await;
        }
    }

    // Käynnistetään muut taskit
    let send_handle = tokio::spawn(send_task);
    //let stdin_handle = tokio::spawn(stdin_task);

    // Lisää tämä muiden taskien luonnin yhteyteen
    let stats_task = {
        let ping_stats = Arc::clone(&ping_stats);
        let known_servers = Arc::clone(&known_servers);
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(10)).await;
                let stats = ping_stats.read().await;
                let servers = known_servers.read().await;
                println!("\nNetwork Statistics:");
                println!("Known servers: {:?}", *servers);
                for (addr, stat) in stats.iter() {
                    println!("Node {}: Sent: {}, Received: {}, Last seen: {:?} ago", 
                        addr, stat.sent, stat.received, stat.last_seen.elapsed());
                }
            }
        })
    };

    // Muokkaa select!-makroa lisäämällä stats_task
    tokio::select! {
        _ = send_handle => (),
        _ = recv_handle => (),
        _ = stats_task => (),
    }

    Ok(())
}

#[allow(dead_code)]
async fn handle_command(cmd: &Command, state: &RwLock<ServerState>, content: &str) -> String {
    match cmd {
        Command::List => {
            let state = state.read().await;
            let mut pairs: Vec<String> = state.data.iter()
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect();
            pairs.sort();
            
            if pairs.is_empty() {
                "No key-value pairs stored".to_string()
            } else {
                format!("Stored key-value pairs:\n{}", pairs.join("\n"))
            }
        },
        Command::Get => {
            let state = state.read().await;
            if let Some(value) = state.data.get(content) {
                format!("Value for key {}: {}", content, value)
            } else {
                format!("Key {} not found", content)
            }
        },
        Command::Exists => {
            let state = state.read().await;
            format!("Key {} exists: {}", content, state.data.contains_key(content))
        },
        Command::Put => {
            println!("DEBUG: Processing PUT command with content: '{}'", content);
            match serde_json::from_str::<Value>(content) {
                Ok(json) => {
                    if let (Some(key), Some(value)) = (
                        json.get("key").and_then(|v| v.as_str()),
                        json.get("value").and_then(|v| v.as_str())
                    ) {
                        let mut state = state.write().await;
                        state.data.insert(key.to_string(), value.to_string());
                        format!("Stored key {} with value {}", key, value)
                    } else {
                        "Invalid JSON format. Expected {\"key\": \"...\", \"value\": \"...\"}".to_string()
                    }
                },
                Err(e) => format!("Failed to parse JSON content: {}", e)
            }
        },
        Command::Delete => {
            let mut state = state.write().await;
            if state.data.remove(content).is_some() {
                format!("Deleted key {}", content)
            } else {
                format!("Key {} not found", content)
            }
        },
    }
}

#[allow(dead_code)]
async fn forward_message<'a>(
    msg: Message, 
    socket: &UdpSocket, 
    known_servers: &'a RwLockReadGuard<'_, Vec<String>>
) {
    let serialized = serde_json::to_string(&msg)
        .expect("Failed to serialize message");
    
    for server in known_servers.iter() {
        if let Err(e) = socket.send_to(serialized.as_bytes(), server).await {
            eprintln!("Failed to forward message to {}: {}", server, e);
        }
    }
}
