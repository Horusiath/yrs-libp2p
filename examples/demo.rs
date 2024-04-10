use futures::StreamExt;
use libp2p::mdns::tokio::Tokio;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{gossipsub, mdns, noise, swarm::SwarmEvent, tcp, yamux};
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::{Hash, Hasher};
use std::time::Duration;
use tokio::{io, io::AsyncBufReadExt, select};
use tracing_subscriber::EnvFilter;
use yrs::sync::DefaultProtocol;
use yrs::types::ToJson;
use yrs::{Map, Transact};
use yrs_libp2p::behaviour::Behaviour;

#[derive(NetworkBehaviour)]
struct Demo {
    sync: Behaviour<DefaultProtocol>,
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
            // To content-address message, we can take the hash of message and use it as an ID.
            let message_id_fn = |message: &gossipsub::Message| {
                let mut s = DefaultHasher::new();
                message.data.hash(&mut s);
                gossipsub::MessageId::from(s.finish().to_string())
            };

            let sync = Behaviour::default();

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

    println!("Enter command via STDIN");

    // Kick it off
    loop {
        select! {
            Ok(Some(line)) = stdin.next_line() => {
                match Args::from_str(line.as_str()) {
                    Some(args) => {
                        let doc = swarm.behaviour_mut().sync.awareness_mut().doc();
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
                        println!("Document state (local change): {}", map.to_json(&txn))
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
                        swarm.behaviour_mut().sync.remove_peer(peer_id);
                    }
                },
                SwarmEvent::Behaviour(DemoEvent::Sync(yrs_libp2p::behaviour::Event::Message {
                    sender,
                    message
                })) => {
                    let doc = swarm.behaviour().sync.awareness().doc();
                    let map = doc.get_or_insert_map("map");
                    println!("Document state (remote change): {}", map.to_json(&doc.transact()))
                },
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Local node is listening on {address}");
                }
                _ => {}
            }
        }
    }
}

enum Args<'a> {
    Set { key: &'a str, value: &'a str },
    Remove { key: &'a str },
}

impl<'a> Args<'a> {
    fn from_str(s: &'a str) -> Option<Self> {
        if s.starts_with("SET") {
            let mut splits = s.strip_prefix("SET")?.split('=');
            if let Some(key) = splits.next() {
                let key = key.strip_prefix(' ')?.strip_suffix(' ')?;
                if let Some(value) = splits.next() {
                    let value = value.strip_prefix(' ')?.strip_suffix(' ')?;
                    Some(Args::Set { key, value })
                } else {
                    None
                }
            } else {
                None
            }
        } else if s.starts_with("DEL") {
            let s = s.strip_prefix("DEL")?.strip_prefix(' ')?.strip_suffix(' ');
            if let Some(key) = s {
                Some(Args::Remove { key })
            } else {
                None
            }
        } else {
            None
        }
    }
}
