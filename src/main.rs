use std::{collections::HashMap, fmt, fs, str::FromStr, sync::{Arc, Mutex}};
use anyhow::Result;
use clap::Parser;
use futures_lite::StreamExt;
use iroh::{
    protocol::Router, Endpoint, NodeAddr, NodeId, PublicKey
};
use iroh_gossip::{
    net::{Gossip},
    api::{Event, GossipReceiver},
    proto::TopicId,
};
use serde::{Deserialize, Serialize};
use colored::Colorize;
use copypasta::{ClipboardContext, ClipboardProvider};

/// Chat over iroh-gossip
///
/// This broadcasts unsigned messages over iroh-gossip.
///
/// By default a new node id is created when starting the example.
///
/// By default, we use the default n0 discovery services to dial by `NodeId`.
#[derive(Parser, Debug)]
struct Args {
    /// Set your nickname. Overrides the name chosen in minconfig.json.
    #[clap(short, long)]
    name: Option<String>,
    /// Set the bind port for our socket. By default, a random port will be used.
    #[clap(short, long, default_value = "0")]
    bind_port: u16,
    #[clap(subcommand)]
    command: Command,
}

#[derive(Debug, Deserialize)]
struct MinConfig {
    name: String,
}

#[derive(Parser, Debug)]
enum Command {
    /// Open a chat room for a topic and print a ticket for others to join.
    Open,
    /// Join a chat room from a ticket.
    Join {
        /// The ticket, as base32 string.
        ticket: String,
    },
}

fn topic_bytes_from_str(s: &str) -> [u8; 32] {
    let mut result = [0u8; 32]; // Initialize with zeros
    let bytes = s.as_bytes();
    let len = bytes.len();

    if len > 32 {
        // Handle cases where the string is too long
        // For this example, we'll just copy the first 32 bytes.
        result.copy_from_slice(&bytes[..32]);
    } else {
        result[..len].copy_from_slice(bytes);
    }
    result
}

const MINIMAL_TOPIC: &str = "minimal";

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // parse the cli command
    let topic = TopicId::from_bytes(topic_bytes_from_str(MINIMAL_TOPIC));
    let nodes = match &args.command {
        Command::Open => {
            println!("{}", "> opening chat room...".blue());
            vec![]
        }
        Command::Join { ticket } => {
            let Ticket { nodes } = Ticket::from_str(ticket)?;
            println!("{}", "> joining chat room...".blue());
            nodes
        }
    };

    let mut ctx = ClipboardContext::new(); // Will be necessary to copy the ticket id
    if let Err(ref _error) = ctx {
        println!("{}", "> can't access clipboard, you'll need to copy the ticket manually".yellow());
    }

    let endpoint = Endpoint::builder()
        .discovery_n0()
        .bind().await?;

    println!("{} {}", "> our node id:".blue(), endpoint.node_id().to_string().bold().bright_cyan());
    let gossip = Gossip::builder().spawn(endpoint.clone());

    let router = Router::builder(endpoint.clone())
        .accept(iroh_gossip::ALPN, gossip.clone())
        .spawn();

    // print a ticket that includes our own node id and endpoint addresses
    let ticket = {
        // Get our address information, includes our
        // `NodeId`, our `RelayUrl`, and any direct
        // addresses.
        let me = endpoint.node_addr();
        let nodes = vec![me];
        Ticket { nodes }
    };
    println!("{} {}", "> ticket to join us:".blue(), ticket.to_string().bold().bright_cyan());
    if let Ok(ref mut clipboard) = ctx {
        if let Ok(_result) = clipboard.set_contents(ticket.to_string()) {
            println!("{}", "> copied ticket to clipboard.".blue());
        }
    }

    // read from minconfig.json if it exists
    const CONFIG_PATH: &str = "minconfig.json";
    let minconfig_exists = fs::exists(CONFIG_PATH)?;
    if !minconfig_exists {
        // assuming it does exist, we should be able to read it pretty easily
        // otherwise it will need to be created
        fs::write(CONFIG_PATH, "{\n    \"name\": \"\"\n}")?;
    }
    let minconfig: MinConfig = serde_json::from_str(&fs::read_to_string(CONFIG_PATH)?)?;

    // join the gossip topic by connecting to known nodes, if any
    let node_ids = nodes.iter().map(|p| p.node_id).collect();
    if nodes.is_empty() {
        println!("{}", "> waiting for nodes to join us...".blue());
    } else {
        println!("{}", format!("> trying to connect to {} nodes...", nodes.len().to_string().bold()).blue());
        // add the peer addrs from the ticket to our endpoint's addressbook so that they can be dialed
        for node in nodes.into_iter() {
            endpoint.add_node_addr_with_source(node, "Joining gossip topic")?;
        }
    };
    let (sender, receiver) = gossip.subscribe_and_join(topic, node_ids).await?.split();
    println!("{}", "> connected!".blue());

    // broadcast our name, if set
    let my_nickname = if let Some(argument_name) = args.name {
        Some(argument_name)
    } else if !minconfig.name.is_empty() {
        Some(minconfig.name)
    } else {
        None
    };
    if let Some(name) = my_nickname {
        let message = Message::new(MessageBody::AboutMe {
            from: endpoint.node_id(),
            name,
        });
        sender.broadcast(message.to_vec().into()).await?;
    }

    // variable to keep track of game requests
    let game_request_tracker = Arc::new(Mutex::new(None));
    // subscribe and print loop
    tokio::spawn(subscribe_loop(receiver, game_request_tracker.clone()));
    // something questionable is going on with that `.clone()`

    // spawn an input thread that reads stdin
    // create a multi-provider, single-consumer channel
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel(1);
    // and pass the `sender` portion to the `input_loop`
    std::thread::spawn(move || input_loop(line_tx));

    // broadcast each line we type
    // listen for lines that we have typed to be sent from `stdin`
    while let Some(text) = line_rx.recv().await {
        // create a message from the text
        if text.starts_with("/") {
            let arguments: Vec<_> = text.trim().split(" ").collect();
            if arguments[0] == "/nick" {
                let new_nick = arguments[1..].join(" ");
                let message = Message::new(MessageBody::AboutMe {
                    from: endpoint.node_id(),
                    name: new_nick.to_string(),
                });
                // broadcast the encoded message
                sender.broadcast(message.to_vec().into()).await?;
                // print a confirmation message
                println!("{}", format!("> you changed your nickname to {new_nick}").green());
            } else if arguments[0] == "/quit" {
                break;
            } else if arguments[0] == "/ticket" {
                println!("{} {}", "> ticket to join us:".blue(), ticket.to_string().bold().bright_cyan());
                if let Ok(ref mut clipboard) = ctx {
                    if let Ok(_result) = clipboard.set_contents(ticket.to_string()) {
                        println!("{}", "> copied ticket to clipboard.".blue());
                    }
                }
            } else if arguments[0] == "/min" {
                // lock will be released at end of scope
                match game_request_tracker.lock() {
                    Ok(mut requester) => {
                        match *requester {
                            Some(other_requester) => {
                                let message = Message::new(MessageBody::GameStart {
                                    from: endpoint.node_id(),
                                    orig_sender: other_requester
                                });
                                sender.broadcast(message.to_vec().into()).await?;
                                *requester = None; // no one is in the queue anymore since we are out
                                println!("{}", format!("> yay, we started a game! TODO: implementation").green());
                            }
                            None => {
                                let message = Message::new(MessageBody::GameRequest {
                                    from: endpoint.node_id(),
                                });
                                sender.broadcast(message.to_vec().into()).await?;
                                *requester = Some(endpoint.node_id()); // we are requesting
                                println!("{}", format!("> asking if anyone wants to play a game...").green());
                            }
                        }
                    }
                    Err(err) => {
                        println!("{}", format!("could not acquire lock: {err}").red());
                    }
                } // released here
            } else {
                println!("{}", format!("unknown command: {}", text.trim()).red());
            }
        } else {
            let message = Message::new(MessageBody::Message {
                from: endpoint.node_id(),
                text: text.clone(),
            });
            // broadcast the encoded message
            sender.broadcast(message.to_vec().into()).await?;
        }
    }
    router.shutdown().await?;

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    body: MessageBody,
    nonce: [u8; 16],
}

#[derive(Debug, Serialize, Deserialize)]
enum MessageBody {
    AboutMe { from: NodeId, name: String },
    Message { from: NodeId, text: String },
    GameRequest { from: NodeId },
    GameStart { from: NodeId, orig_sender: NodeId },
}

impl Message {
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(Into::into)
    }

    pub fn new(body: MessageBody) -> Self {
        Self {
            body,
            nonce: rand::random(),
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serde_json::to_vec is infallible")
    }
}

// Handle incoming events
async fn subscribe_loop(mut receiver: GossipReceiver, game_request_tracker: Arc<Mutex<Option<PublicKey>>>) -> Result<()> {
    // keep track of the mapping between `NodeId`s and names
    let mut names = HashMap::new();
    // iterate over all events
    while let Some(event) = receiver.try_next().await? {
        // if the Event is a `GossipEvent::Received`, let's deserialize the message:
        if let Event::Received(msg) = event {
            // deserialize the message and match on the message type:
            match Message::from_bytes(&msg.content)?.body {
                MessageBody::AboutMe { from, name } => {
                    // if it's an `AboutMe` message
                    // check for the old name first
                    let old_name = names
                                .get(&from)
                                .map_or_else(|| from.fmt_short().to_string(), String::to_string);
                    // insert the new name
                    names.insert(from, name.clone());
                    println!("{}", format!("> {} is now known as {}", old_name, name).blue());
                }
                MessageBody::Message { from, text } => {
                    // if it's a `Message` message, get the name from the map and print the message
                    let name = names
                        .get(&from)
                        .map_or_else(|| from.to_string(), String::to_string);
                    println!("{}: {}", name.bold().magenta(), text.trim().cyan());
                }
                MessageBody::GameRequest { from } => {
                    // lock will be released at end of scope
                    match game_request_tracker.lock() {
                        Ok(mut requester) => {
                            *requester = Some(from);
                            let name = names
                                .get(&from)
                                .map_or_else(|| from.to_string(), String::to_string);
                            println!("{}", format!("> {} is in the minimal queue, use /min to join!", name).blue());
                        }
                        Err(err) => {
                            println!("{}", format!("could not acquire lock: {err}").red());
                        }
                    } // released here
                }
                MessageBody::GameStart { from, orig_sender } => {
                    // lock will be released at end of scope
                    match game_request_tracker.lock() {
                        Ok(mut requester) => {
                            *requester = None; // the queue is now empty since a game has started
                            // the reason for including orig_sender is because we might have joined the chat
                            // after the request was sent. currently we don't need to know who is currently
                            // in a game but it could be useful later
                            let accepter_name = names
                                .get(&from)
                                .map_or_else(|| from.to_string(), String::to_string);
                            let sender_name = names
                                .get(&orig_sender)
                                .map_or_else(|| orig_sender.to_string(), String::to_string);
                            println!("{}", format!("> {} started a game with {}!", accepter_name, sender_name).blue());
                        }
                        Err(err) => {
                            println!("{}", format!("could not acquire lock: {err}").red());
                        }
                    } // released here
                }
            }
        }
    }
    Ok(())
}

fn input_loop(line_tx: tokio::sync::mpsc::Sender<String>) -> Result<()> {
    let mut buffer = String::new();
    let stdin = std::io::stdin(); // We get `Stdin` here.
    loop {
        stdin.read_line(&mut buffer)?;
        line_tx.blocking_send(buffer.clone())?;
        buffer.clear();
    }
}

// add the `Ticket` code to the bottom of the main file
#[derive(Debug, Serialize, Deserialize)]
struct Ticket {
    nodes: Vec<NodeAddr>,
}

impl Ticket {
    /// Deserialize from a slice of bytes to a Ticket.
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(Into::into)
    }

    /// Serialize from a `Ticket` to a `Vec` of bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serde_json::to_vec is infallible")
    }
}

// The `Display` trait allows us to use the `to_string` method on `Ticket`.
impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut text = data_encoding::BASE32_NOPAD.encode(&self.to_bytes()[..]);
        text.make_ascii_lowercase();
        write!(f, "{}", text)
    }
}

// The `FromStr` trait allows us to turn a `str` into a `Ticket`
impl FromStr for Ticket {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = data_encoding::BASE32_NOPAD.decode(s.to_ascii_uppercase().as_bytes())?;
        Self::from_bytes(&bytes)
    }
}