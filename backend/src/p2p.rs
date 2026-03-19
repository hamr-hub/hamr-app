use anyhow::Result;
use libp2p::{
    gossipsub, identity, kad, mdns, noise, swarm::NetworkBehaviour, tcp, yamux, Swarm,
    SwarmBuilder, PeerId, gossipsub::IdentTopic,
};
use std::time::Duration;
use tokio::sync::mpsc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMessage {
    pub table: String,
    pub operation: String,
    pub record_id: String,
    pub data: serde_json::Value,
    pub version: i64,
    pub device_id: String,
    pub signature: String,
}

#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "CustomEvent")]
pub struct MyBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
}

#[derive(Debug)]
pub enum CustomEvent {
    Gossipsub(gossipsub::Event),
    Mdns(mdns::Event),
    Kademlia(kad::Event),
}

impl From<gossipsub::Event> for CustomEvent {
    fn from(event: gossipsub::Event) -> Self {
        CustomEvent::Gossipsub(event)
    }
}

impl From<mdns::Event> for CustomEvent {
    fn from(event: mdns::Event) -> Self {
        CustomEvent::Mdns(event)
    }
}

impl From<kad::Event> for CustomEvent {
    fn from(event: kad::Event) -> Self {
        CustomEvent::Kademlia(event)
    }
}

pub struct P2PNode {
    pub swarm: Swarm<MyBehaviour>,
    pub peer_id: PeerId,
    pub topic: IdentTopic,
}

impl P2PNode {
    pub async fn new() -> Result<Self> {
        let local_key = identity::Keypair::generate_ed25519();
        let peer_id = PeerId::from(local_key.public());
        
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10))
            .validation_mode(gossipsub::ValidationMode::Strict)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build gossipsub config: {}", e))?;
        
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageId::random,
            gossipsub_config,
            local_key.public(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create gossipsub: {}", e))?;
        
        let mdns = mdns::tokio::Behaviour::new(
            mdns::Config {
                ttl: Duration::from_secs(300),
                query_interval: Duration::from_secs(30),
                ..Default::default()
            },
            peer_id,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create mdns: {}", e))?;
        
        let kad_store = kad::store::MemoryStore::new(peer_id);
        let kademlia = kad::Behaviour::new(peer_id, kad_store);
        
        let swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| anyhow::anyhow!("Failed to setup TCP: {}", e))?
            .with_quic()
            .with_behaviour(|_key| MyBehaviour {
                gossipsub,
                mdns,
                kademlia,
            })
            .map_err(|e| anyhow::anyhow!("Failed to create behaviour: {}", e))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();
        
        let topic = gossipsub::IdentTopic::new("hamr-family-sync");
        
        Ok(Self {
            swarm,
            peer_id,
            topic,
        })
    }
    
    pub async fn start(&mut self, mut rx: mpsc::Receiver<SyncMessage>) -> Result<()> {
        self.swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&self.topic)
            .map_err(|e| anyhow::anyhow!("Failed to subscribe: {}", e))?;
        
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    match event {
                        libp2p::swarm::SwarmEvent::Behaviour(CustomEvent::Mdns(mdns::Event::Discovered(list))) => {
                            for (peer_id, multiaddr) in list {
                                tracing::info!("Discovered peer: {} at {}", peer_id, multiaddr);
                                self.swarm.dial(multiaddr)?;
                            }
                        }
                        libp2p::swarm::SwarmEvent::Behaviour(CustomEvent::Gossipsub(gossipsub::Event::Message {
                            propagation_source: peer_id,
                            message_id: id,
                            message,
                        })) => {
                            tracing::info!("Received message {} from {}", id, peer_id);
                            if let Ok(msg) = serde_json::from_slice::<SyncMessage>(&message.data) {
                                tracing::debug!("Sync message: {:?}", msg);
                            }
                        }
                        libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } => {
                            tracing::info!("Listening on {}", address);
                        }
                        libp2p::swarm::SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            tracing::info!("Connected to peer: {}", peer_id);
                        }
                        libp2p::swarm::SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            tracing::info!("Disconnected from peer: {}", peer_id);
                        }
                        _ => {}
                    }
                }
                
                msg = rx.recv() => {
                    if let Some(msg) = msg {
                        let data = serde_json::to_vec(&msg)?;
                        self.swarm
                            .behaviour_mut()
                            .gossipsub
                            .publish(self.topic.clone(), data)
                            .map_err(|e| anyhow::anyhow!("Failed to publish: {}", e))?;
                        tracing::debug!("Published sync message for table {}", msg.table);
                    }
                }
            }
        }
    }
    
    pub fn publish(&mut self, msg: SyncMessage) -> Result<()> {
        let data = serde_json::to_vec(&msg)?;
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), data)
            .map_err(|e| anyhow::anyhow!("Failed to publish: {}", e))?;
        Ok(())
    }
}
