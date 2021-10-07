use std::ops::DerefMut;

use async_trait::async_trait;
use futures_core::{future, Stream};
use futures_util::{future::pending, StreamExt};
use libp2p::{
    core::Network,
    identity::Keypair,
    ping::{Ping, PingConfig, PingEvent, PingFailure},
    swarm::SwarmEvent,
    Multiaddr, PeerId, Swarm,
};
use xtra::{Context, Handler, Message};

#[tokio::main]
async fn main() {
    let mut alice = Actor::new().await;
    let bob = Actor::new().await;

    let alice_multi_address = "/ip4/127.0.0.1/tcp/8888".parse::<Multiaddr>().unwrap();
    alice.swarm.listen_on(alice_multi_address.clone()).unwrap();

    let (alice_addr, alice_context) = Context::new(None);
    tokio::spawn(alice_context.run2(alice));

    let (bob_addr, bob_context) = Context::new(None);
    tokio::spawn(bob_context.run2(bob));

    bob_addr
        .send(Dial {
            address: alice_multi_address,
        })
        .await
        .unwrap();

    pending::<()>().await;
}

struct Dial {
    address: Multiaddr,
}

struct Actor {
    swarm: Swarm<Ping>,
}

impl Actor {
    async fn new() -> Self {
        let id = Keypair::generate_ed25519();
        let transport = libp2p::development_transport(id.clone()).await.unwrap();

        Self {
            swarm: Swarm::new(
                transport,
                Ping::new(PingConfig::new().with_keep_alive(true)),
                id.public().into_peer_id(),
            ),
        }
    }
}

#[async_trait]
impl Handler<NetworkMessage> for Actor {
    async fn handle(&mut self, message: NetworkMessage, ctx: &mut Context<Self>) {
        println!("{:?}", message.0)
    }
}

#[async_trait]
impl Handler<Dial> for Actor {
    async fn handle(&mut self, message: Dial, ctx: &mut Context<Self>) {
        self.swarm.dial_addr(message.address);
    }
}

struct NetworkMessage(SwarmEvent<PingEvent, PingFailure>);

impl Message for NetworkMessage {
    type Result = ();
}

impl Message for Dial {
    type Result = ();
}

impl xtra::Actor for Actor {}

impl Stream for Actor {
    type Item = NetworkMessage;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.swarm
            .poll_next_unpin(cx)
            .map(|o| o.map(NetworkMessage))
    }
}
