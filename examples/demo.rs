use futures::StreamExt;
use libp2p::mdns::tokio::Tokio;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{gossipsub, mdns, noise, swarm::SwarmEvent, tcp, yamux};
use std::error::Error;
use std::hash::Hash;
use std::time::Duration;
use tokio::{io, io::AsyncBufReadExt, select};
use tracing_subscriber::EnvFilter;
use yrs::types::ToJson;
use yrs::{Map, Transact};
use yrs_libp2p::behaviour::{Behaviour, Event, Topic};

#[derive(NetworkBehaviour)]
struct Demo {
    sync: Behaviour,
    mdns: libp2p::mdns::Behaviour<Tokio>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::new(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            // build a gossipsub network behaviour
            let sync = Behaviour::new();

            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;
            Ok(Demo { sync, mdns })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    let topic: Topic = "test-doc".into();

    println!("Enter command via STDIN");

    loop {
        select! {
            Ok(Some(line)) = stdin.next_line() => {
                /* Accepted commands:
                    - `set {key} {value}`
                    - `del {key}`
                */
                match Args::from_str(line.as_str()) {
                    Some(args) => {
                        let doc = swarm.behaviour_mut().sync.awareness(topic.clone()).doc();
                        let map = doc.get_or_insert_map("map");
                        let mut txn = doc.transact_mut();

                        match args {
                            Args::Set { key, value } => {
                                map.insert(&mut txn, key, value);
                            },
                            Args::Remove { key } => {
                                map.remove(&mut txn, key);
                            }
                        }
                    },
                    None => {
                        eprintln!("PARSE ERROR");
                    }
                }
            }
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(DemoEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _multiaddr) in list {
                        println!("mDNS discovered a new peer: {peer_id}");
                        swarm.behaviour_mut().sync.add_peer(peer_id);
                    }
                },
                SwarmEvent::Behaviour(DemoEvent::Mdns(mdns::Event::Expired(list))) => {
                    for (peer_id, multiaddr) in list {
                        println!("mDNS discover peer has expired: {peer_id}");
                        swarm.behaviour_mut().sync.remove_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(DemoEvent::Sync(Event::Message {
                    topic, message,source,
                })) => {
                    let doc = swarm.behaviour_mut().sync.awareness(topic.clone()).doc();
                    let map = doc.get_or_insert_map("map");
                    let source = match source {
                        None => "local".to_string(),
                        Some(peer_id) => format!("`{peer_id}`")};
                    println!("Document `{}` state (from {}): {}", topic, source, map.to_json(&doc.transact()))
                },
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Local node is listening on {address}");
                }
                _ => {}
            }
        }
    }
    // Kick it off
}

enum Args<'a> {
    Set { key: &'a str, value: &'a str },
    Remove { key: &'a str },
}

impl<'a> Args<'a> {
    fn from_str(s: &'a str) -> Option<Self> {
        let mut s = s.split(' ');
        let cmd = s.next()?;
        match cmd {
            "set" => {
                let key = s.next()?;
                let value = s.next()?;
                Some(Args::Set { key, value })
            }
            "del" => {
                let key = s.next()?;
                Some(Args::Remove { key })
            }
            _ => None,
        }
    }
}
