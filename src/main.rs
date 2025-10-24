use std::{collections::HashMap, fs, io::{stdout, ErrorKind, Write}, sync::{Arc, Mutex}, time::Duration};
use anyhow::Result;
use clap::Parser;
use crossterm::{cursor::{MoveTo, MoveToNextLine}, event::{DisableMouseCapture, EnableMouseCapture, Event::{Key, Mouse, Resize}, EventStream, KeyCode, MouseEventKind}, execute, style::Stylize, terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, size, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen}};
use futures_lite::StreamExt;
use iroh::{discovery::static_provider::StaticProvider, protocol::Router, Endpoint, NodeAddr, NodeId, PublicKey, SecretKey};
use iroh_gossip::{net::Gossip, api::{Event, GossipReceiver}, proto::TopicId};
use serde::{Deserialize, Serialize};

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
    Join,
}

fn bytes_from_str(s: &str) -> [u8; 32] {
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

const MINIMAL_VERSION: &str = "0.4.0"; // minimal's version, should be consistent with Cargo.toml
const MINIMAL_TOPIC_HEADER: &str = "the-rivulet/minimal/topic/"; // prefix for topics
const MINIMAL_HOST_KEY_KEADER: &str = "the-rivulet/minimal/host/"; // prefix for secret keys
const CONNECTION_TIMEOUT_SECS: u64 = 10; // seconds to wait before assuming network issue

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // parse the cli command
    let topic = TopicId::from_bytes(bytes_from_str(&(MINIMAL_TOPIC_HEADER.to_owned() + MINIMAL_VERSION)));
    let (is_host_node, secret_key) = match &args.command {
        Command::Open => {
            println!("{}", "> opening chat room as host...".blue().dim());
            // set to None because we want to become the host node
            (true, SecretKey::from_bytes(&bytes_from_str(&(MINIMAL_HOST_KEY_KEADER.to_owned() + MINIMAL_VERSION))))
        }
        Command::Join => {
            println!("{}", "> attempting to join chat room...".blue().dim());
            (false, SecretKey::generate(&mut rand::rng()))
        }
    };

    let discovery = StaticProvider::new();
    let endpoint = Endpoint::builder()
        .discovery_n0()
        .add_discovery(discovery.clone())
        .secret_key(secret_key) // if I am hosting then use the dedicated host key. if not, then use a random one
        .bind().await?;

    let gossip = Gossip::builder().spawn(endpoint.clone());

    let router = Router::builder(endpoint.clone())
        .accept(iroh_gossip::ALPN, gossip.clone())
        .spawn();

    // read from minconfig.json if it exists
    const CONFIG_PATH: &str = "minconfig.json";
    let minconfig_exists = fs::exists(CONFIG_PATH)?;
    if !minconfig_exists {
        // assuming it does exist, we should be able to read it pretty easily
        // otherwise it will need to be created
        println!("{}", "> couldn't find minconfig.json, creating a new one".yellow());
        fs::write(CONFIG_PATH, "{\n    \"name\": \"\"\n}")?;
    }
    let minconfig: MinConfig = serde_json::from_str(&fs::read_to_string(CONFIG_PATH)?)?;
    // quick warning if the terminal is too tiny
    let (term_cols, term_rows) = size()?;
    if (term_cols < MIN_TERM_COLS) || (term_rows < MIN_TERM_ROWS) {
        println!("{}", format!("> terminal is too small to play, should be at least {MIN_TERM_COLS} x {MIN_TERM_ROWS}.").yellow());
    }

    println!("{}", "> connecting to the network...".blue().dim());
    let wait_for_online = endpoint.online();
    if let Err(_) = tokio::time::timeout(Duration::from_secs(CONNECTION_TIMEOUT_SECS), wait_for_online).await {
        panic!("{}", std::io::Error::new(
            ErrorKind::NetworkUnreachable,
            format!("couldn't get online within {} seconds", CONNECTION_TIMEOUT_SECS)
        ));
    }
    // join the gossip topic by connecting to known nodes, if any
    let bootstrap_nodes = if is_host_node {
        println!("{}", "> server started, waiting for nodes to join us".blue());
        vec![]
    } else {
        println!("{}", "> trying to reach host node...".blue().dim());
        // mimic the logic used to generate the host key
        let host_key = &bytes_from_str(&(MINIMAL_HOST_KEY_KEADER.to_owned() + MINIMAL_VERSION));
        let host_addr = NodeAddr::new(SecretKey::from_bytes(host_key).public())
            .with_relay_url(endpoint.node_addr().relay_url.ok_or(
                std::io::Error::new(ErrorKind::Other, "node should have a relay_url")
            )?);
        discovery.add_node_info(host_addr.clone());
        // I feel a bit concerned with the amount of `.clone()` here
        vec![host_addr.node_id]
    };
    let sender; let receiver;
    let output = if is_host_node {
        Ok(gossip.subscribe_and_join(topic, bootstrap_nodes).await)
    } else {
        tokio::time::timeout(
            Duration::from_secs(CONNECTION_TIMEOUT_SECS),
            gossip.subscribe_and_join(topic, bootstrap_nodes)
        ).await
    };
    match output {
        Ok(value) => { (sender, receiver) = value?.split(); }
        Err(_) => panic!("{}", std::io::Error::new(
            ErrorKind::NetworkUnreachable,
            format!("couldn't connect to host within {} seconds, maybe try `cargo run open` to start a server?", CONNECTION_TIMEOUT_SECS)
        ))
    }
    println!("{}", "> ready!".blue().bold());

    // broadcast our name, if set
    let my_nickname = if let Some(argument_name) = args.name {
        Some(argument_name)
    } else if !minconfig.name.is_empty() {
        Some(minconfig.name)
    } else {
        None
    };
    if let Some(name) = my_nickname {
        let message = MinimalMessage::new(MinimalMessageType::Chat(ChatMessage::AboutMe {
            from: endpoint.node_id(),
            name,
        }));
        sender.broadcast(message.to_vec().into()).await?;
    }

    // variable to keep track of game requests
    let game_request_tracker = Arc::new(Mutex::new(None));
    let our_id = endpoint.node_id();
    // create an arc to store the gossip because we may need to use it when starting a game
    let gossip_arc = Arc::new(gossip);
    // subscribe and print loop
    tokio::spawn(subscribe_loop(receiver, our_id, gossip_arc.clone(), game_request_tracker.clone()));
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
                let message = MinimalMessage::new(MinimalMessageType::Chat(ChatMessage::AboutMe {
                    from: endpoint.node_id(),
                    name: new_nick.to_string(),
                }));
                // broadcast the encoded message
                sender.broadcast(message.to_vec().into()).await?;
                // print a confirmation message
                println!("{}", format!("> you changed your nickname to {new_nick}").green());
            } else if arguments[0] == "/quit" {
                break;
            } else if arguments[0] == "/min" {
                // lock will be released at end of scope
                let mut requester = game_request_tracker.lock().expect("should be able to acquire lock");
                match *requester {
                    Some(other_requester) => {
                        let game_id = rand::random_range(0.0..=1e9);
                        let message = MinimalMessage::new(MinimalMessageType::Chat(ChatMessage::GameStart {
                            from: endpoint.node_id(),
                            orig_sender: other_requester,
                            game_id: game_id
                        }));
                        sender.broadcast(message.to_vec().into()).await?;
                        *requester = None; // the queue has been emptied
                        println!("{}", "> ok, starting a game!".green());
                        tokio::spawn(begin_game(game_id, gossip_arc.clone(), vec![]));
                    }
                    None => {
                        let message = MinimalMessage::new(MinimalMessageType::Chat(ChatMessage::GameRequest {
                            from: endpoint.node_id(),
                        }));
                        sender.broadcast(message.to_vec().into()).await?;
                        *requester = Some(endpoint.node_id()); // we are requesting
                        println!("{}", format!("> joined the minimal queue!").green());
                    }
                } // released here
            } else {
                println!("{}", format!("unknown command: {}", text.trim()).red());
            }
        } else {
            let message = MinimalMessage::new(MinimalMessageType::Chat(ChatMessage::Message {
                from: endpoint.node_id(),
                text: text.clone(),
            }));
            // broadcast the encoded message
            sender.broadcast(message.to_vec().into()).await?;
        }
    }
    router.shutdown().await?;

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct MinimalMessage {
    body: MinimalMessageType,
    nonce: [u8; 16],
}

#[derive(Debug, Serialize, Deserialize)]
enum MinimalMessageType {
    Chat(ChatMessage),
    Game(GameMessage),
}

#[derive(Debug, Serialize, Deserialize)]
enum ChatMessage {
    AboutMe { from: NodeId, name: String },
    Message { from: NodeId, text: String },
    GameRequest { from: NodeId },
    GameStart { from: NodeId, orig_sender: NodeId, game_id: f64 },
}

#[derive(Debug, Serialize, Deserialize)]
enum GameMessage {
    Aborted {}
}

impl MinimalMessage {
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(Into::into)
    }
    pub fn new(body: MinimalMessageType) -> Self {
        Self { body, nonce: rand::random(), }
    }
    pub fn to_vec(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serde_json::to_vec is infallible")
    }
}

fn get_name(names: &HashMap<PublicKey, String>, from: PublicKey) -> String {
    names
        .get(&from)
        .map_or_else(|| from.fmt_short().to_string(), String::to_string)
}

// Handle incoming events
async fn subscribe_loop(mut receiver: GossipReceiver, our_id: PublicKey, gossip: Arc<Gossip>, game_request_tracker: Arc<Mutex<Option<PublicKey>>>) -> Result<()> {
    // keep track of the mapping between `NodeId`s and names
    let mut names = HashMap::new();
    // iterate over all events
    while let Some(event) = receiver.try_next().await? {
        // if the Event is a `GossipEvent::Received`, let's deserialize the message:
        if let Event::Received(msg) = event {
            // deserialize the message and match on the message type:
            if let MinimalMessageType::Chat(chat_message) = MinimalMessage::from_bytes(&msg.content)?.body {
                match chat_message {
                    ChatMessage::AboutMe { from, name } => {
                        // if it's an `AboutMe` message
                        // check for the old name first
                        let old_name = get_name(&names, from);
                        // insert the new name
                        names.insert(from, name.clone());
                        println!("{}", format!("> {} is now known as {}", old_name, name).blue());
                    }
                    ChatMessage::Message { from, text } => {
                        // if it's a `Message` message, get the name from the map and print the message
                        let name = get_name(&names, from);
                        println!("{}: {}", name.bold().magenta(), text.trim().cyan());
                    }
                    ChatMessage::GameRequest { from } => {
                        // lock will be released at end of scope
                        let mut requester = game_request_tracker.lock().expect("should be able to acquire lock");
                        *requester = Some(from);
                        let name = get_name(&names, from);
                        println!("{}", format!("> {} is in the minimal queue, use /min to join!", name).blue());
                    } // released here
                    ChatMessage::GameStart { from, orig_sender, game_id } => {
                        // lock will be released at end of scope
                        let mut requester = game_request_tracker.lock().expect("should be able to acquire lock");
                        *requester = None; // the queue is now empty since a game has started
                        // the reason for including orig_sender is because we might have joined the chat
                        // after the request was sent. currently we don't need to know who is currently
                        // in a game but it could be useful later
                        let accepter_name = get_name(&names, from);
                        let sender_name = get_name(&names, orig_sender);
                        println!("{}", format!("> {} started a game with {}!", accepter_name, sender_name).blue());
                        if orig_sender == our_id {
                            println!("{}", "> your invite was accepted, starting a game!".green());
                            tokio::spawn(begin_game(game_id, gossip.clone(), vec![from]));
                        } // released here
                    }
                }
            }
        }
    }
    println!("{}", "> chat manager thread was closed.".red());
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

// these are u16 for convenient comparison, they really could be i8 or something
const MIN_TERM_COLS: u16 = 30;
const MIN_TERM_ROWS: u16 = 7;

async fn begin_game(game_id: f64, gossip: Arc<Gossip>, bootstrap: Vec<PublicKey>) -> Result<()> {
    let mut result = [0u8; 32]; // Initialize with zeros
    let bytes = game_id.to_le_bytes();
    let len = bytes.len();
    result[..len].copy_from_slice(&bytes);
    println!("{:?}", result);
    let topic = TopicId::from_bytes(result);
    println!("{:?}", topic);
    println!("{}", "> waiting for other player...".blue().dim());
    let (sender, receiver) = gossip.subscribe_and_join(topic, bootstrap).await?.split();
    // open yet another thread to deal with the sub events
    tokio::spawn(game_subscribe_loop(receiver));
    let (mut term_cols, mut term_rows) = size()?;
    // set up terminal stuff
    let mut event_reader = EventStream::new();
    let mut stdout = stdout();
    stdout.flush()?;
    enable_raw_mode()?;
    execute!(stdout, EnableMouseCapture, EnterAlternateScreen)?;
    // before doing anything else ensure that the terminal is big enough
    // if not, just immediately abort.
    if (term_cols < MIN_TERM_COLS) || (term_rows < MIN_TERM_ROWS) {
        let message = MinimalMessage::new(MinimalMessageType::Game(GameMessage::Aborted {}));
        sender.broadcast(message.to_vec().into()).await?;
        println!("{}", format!("> game aborted due to terminal being too small (should be at least {MIN_TERM_COLS} cols x {MIN_TERM_ROWS} rows).").yellow());
    }
    while let Some(event) = event_reader.try_next().await? {
        if !is_raw_mode_enabled()? {
            // if raw mode was unexpectedly disabled, the game probably ended
            // todo: make sure that we are on the alternate screen (raw mode is expected to be off on the main one)
            // (doesn't need to be done till I implement switching screens in-game)
            println!("{}", "> aborting because opponent quit.".yellow());
            break
        }
        // re-rendering time!! there is no way to avoid redrawing the entire screen iirc, so just do it
        // draw the minimal border
        execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
        write!(stdout, "┌ minimal {}┐", "─".repeat((term_cols - 11).into()))?;
        for _i in 1..(term_rows-1) {
            execute!(stdout, MoveToNextLine(1))?;
            write!(stdout, "│{}│", " ".repeat((term_cols - 2).into()))?;
        }
        execute!(stdout, MoveTo(0, term_rows-1))?;
        write!(stdout, "└{}┘", "─".repeat((term_cols - 2).into()))?;
        execute!(stdout, MoveTo(2, 2))?;
        write!(stdout, "{}", "todo: game goes here".on_dark_cyan())?;
        match event {
            Key(key_event) => {
                if key_event.code == KeyCode::Char('q') {
                    // quit
                    disable_raw_mode()?;
                    execute!(stdout, DisableMouseCapture, LeaveAlternateScreen)?;
                    let message = MinimalMessage::new(MinimalMessageType::Game(GameMessage::Aborted {}));
                    sender.broadcast(message.to_vec().into()).await?;
                    println!("{}", "> game aborted.".yellow());
                    break
                }
            },
            Mouse(mouse_event) => {
                match mouse_event.kind {
                    MouseEventKind::Moved => {
                        execute!(stdout, MoveTo(mouse_event.column + 1, mouse_event.row + 1))?;
                        write!(stdout, "{}", "*".magenta())?;
                        stdout.flush()?;
                    }
                    _ => {}
                }
            },
            Resize(new_cols, new_rows) => {
                term_cols = new_cols;
                term_rows = new_rows;
                if (term_cols < MIN_TERM_COLS) || (term_rows < MIN_TERM_ROWS) {
                    let message = MinimalMessage::new(MinimalMessageType::Game(GameMessage::Aborted {}));
                    sender.broadcast(message.to_vec().into()).await?;
                    println!("{}", format!("> game aborted due to terminal being resized to a too small size (should be at least {MIN_TERM_COLS} cols x {MIN_TERM_ROWS} rows).").yellow());
                }
            }
            _ => {}
        }
    };
    Ok(())
}

async fn game_subscribe_loop(mut receiver: GossipReceiver) -> Result<()> {
    while let Some(event) = receiver.try_next().await? {
        if let Event::Received(msg) = event {
            // deserialize the message and match on the message type:
            if let MinimalMessageType::Game(game_message) = MinimalMessage::from_bytes(&msg.content)?.body {
                match game_message {
                    GameMessage::Aborted {} => {
                        disable_raw_mode()?;
                        execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
                        println!("{}", "> opponent aborted the game.".yellow());
                        break
                    }
                }
            }
        }
    }
    Ok(())
}