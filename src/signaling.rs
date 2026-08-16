//! Embedded WebRTC signaling server.
//!
//! When gopher64 is launched with `--netplay-host`, this spins up an
//! in-process full-mesh signaling rendezvous so the other players connect
//! directly to this machine — no external server and no third-party service.
//! The signaling channel only brokers the initial WebRTC handshake
//! (SDP/ICE exchange); once peers are introduced, all gameplay traffic flows
//! directly peer-to-peer, so the host machine is not in the latency path.
//!
//! This is a self-contained port of the full-mesh topology from
//! `matchbox_server` 0.14.0 (Johan Helsing, MIT OR Apache-2.0), trimmed to the
//! single topology gopher64 netplay uses. The client side already depends on
//! `matchbox_socket` 0.14, so protocol versions match by construction.

use async_trait::async_trait;
use axum::{Error, extract::ws::Message};
use futures::StreamExt;
use matchbox_protocol::{JsonPeerEvent, PeerId, PeerRequest};
use matchbox_signaling::{
    ClientRequestError, NoCallbacks, SignalingError, SignalingServerBuilder, SignalingState,
    SignalingTopology, WsStateMeta,
    common_logic::{self, StateObj, parse_request},
};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};
use tokio::sync::mpsc::UnboundedSender;

/// A room is identified by the websocket path (e.g. `/gauntlet`). All peers
/// that request the same room see each other in a full mesh.
#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq, Hash)]
struct RoomId(String);

/// A room request. `next` is an optional matchmaking-by-count hint; gopher64
/// does not use it (peers join a named room and count each other), but it is
/// preserved so the server behaves identically to upstream matchbox.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RequestedRoom {
    id: RoomId,
    next: Option<usize>,
}

/// A connected peer and the channel used to push signaling messages to it.
#[derive(Debug, Clone)]
struct Peer {
    uuid: PeerId,
    room: RequestedRoom,
    sender: UnboundedSender<Result<Message, Error>>,
}

/// Shared server state. Every field is an `Arc<Mutex<..>>` (`StateObj`) so the
/// per-connection state machines can mutate it concurrently.
#[derive(Default, Debug, Clone)]
struct ServerState {
    clients_waiting: StateObj<HashMap<SocketAddr, RequestedRoom>>,
    clients_in_queue: StateObj<HashMap<PeerId, RequestedRoom>>,
    clients: StateObj<HashMap<PeerId, Peer>>,
    rooms: StateObj<HashMap<RequestedRoom, HashSet<PeerId>>>,
    matched_by_next: StateObj<HashSet<Vec<PeerId>>>,
}
impl SignalingState for ServerState {}

impl ServerState {
    /// Record a client that has requested a room but not yet been assigned an id.
    fn add_waiting_client(&mut self, origin: SocketAddr, room: RequestedRoom) {
        self.clients_waiting.lock().unwrap().insert(origin, room);
    }

    /// Promote a waiting client to the id queue once the server assigns its id.
    fn assign_id_to_waiting_client(&mut self, origin: SocketAddr, peer_id: PeerId) {
        let room = {
            let mut lock = self.clients_waiting.lock().unwrap();
            lock.remove(&origin).expect("waiting client")
        };
        self.clients_in_queue.lock().unwrap().insert(peer_id, room);
    }

    /// Remove a queued peer, returning the room it requested.
    fn remove_waiting_peer(&mut self, peer_id: PeerId) -> RequestedRoom {
        self.clients_in_queue
            .lock()
            .unwrap()
            .remove(&peer_id)
            .expect("waiting peer")
    }

    /// Add a peer to its room, returning the peers already present (so the new
    /// peer can be announced to them for a full mesh).
    fn add_peer(&mut self, peer: Peer) -> Vec<PeerId> {
        let peer_id = peer.uuid;
        let room = peer.room.clone();
        self.clients.lock().unwrap().insert(peer.uuid, peer);

        let mut rooms = self.rooms.lock().unwrap();
        let peers = rooms.entry(room.clone()).or_default();
        let prev_peers = peers.iter().cloned().collect();

        match room.next {
            None => {
                peers.insert(peer_id);
            }
            Some(num_players) => {
                if peers.len() == num_players - 1 {
                    let mut matched_by_next = self.matched_by_next.lock().unwrap();
                    let mut updated_peers = peers.clone();
                    updated_peers.insert(peer_id);
                    matched_by_next.insert(updated_peers.into_iter().collect());
                    peers.clear(); // room is complete
                } else {
                    peers.insert(peer_id);
                }
            }
        };

        prev_peers
    }

    /// Remove a peer that was matched via the `next` mechanism, returning the
    /// remaining members of its match group.
    fn remove_matched_peer(&mut self, peer: PeerId) -> Vec<PeerId> {
        let mut matched_by_next = self.matched_by_next.lock().unwrap();
        let mut peers = vec![];
        matched_by_next.retain(|group| {
            if group.contains(&peer) {
                peers = group.clone();
                return false;
            }
            true
        });

        peers.retain(|p| p != &peer);
        if !peers.is_empty() {
            matched_by_next.insert(peers.clone());
        }
        peers
    }

    /// Look up a connected peer.
    fn get_peer(&self, peer_id: &PeerId) -> Option<Peer> {
        self.clients.lock().unwrap().get(peer_id).cloned()
    }

    /// List the peers currently in a room.
    fn get_room_peers(&self, room: &RequestedRoom) -> Vec<PeerId> {
        self.rooms
            .lock()
            .unwrap()
            .get(room)
            .map(|room_peers| room_peers.iter().copied().collect::<Vec<PeerId>>())
            .unwrap_or_default()
    }

    /// Remove a peer from the state, returning it if it existed.
    #[must_use]
    fn remove_peer(&mut self, peer_id: &PeerId) -> Option<Peer> {
        let peer = self.clients.lock().unwrap().remove(peer_id);
        if let Some(ref peer) = peer {
            // Best effort to remove the peer from its room.
            if let Some(room) = self.rooms.lock().unwrap().get_mut(&peer.room) {
                room.remove(peer_id);
            }
        }
        peer
    }

    /// Non-blocking send of a signaling message to a peer.
    fn try_send(&self, id: PeerId, message: Message) -> Result<(), SignalingError> {
        let clients = self.clients.lock().unwrap();
        match clients.get(&id) {
            Some(peer) => Ok(common_logic::try_send(&peer.sender, message)?),
            None => Err(SignalingError::UnknownPeer),
        }
    }
}

/// Full-mesh topology: every peer in a room is introduced to every other peer,
/// which is exactly the mesh gopher64's GGRS session expects.
#[derive(Debug, Default)]
struct FullMeshTopology;

#[async_trait]
impl SignalingTopology<NoCallbacks, ServerState> for FullMeshTopology {
    async fn state_machine(upgrade: WsStateMeta<NoCallbacks, ServerState>) {
        let WsStateMeta {
            peer_id,
            sender,
            mut receiver,
            mut state,
            ..
        } = upgrade;

        let room = state.remove_waiting_peer(peer_id);
        let peer = Peer {
            uuid: peer_id,
            sender: sender.clone(),
            room,
        };

        // Announce this new peer to everyone already in the room.
        let peers = state.add_peer(peer);
        let event = Message::Text(JsonPeerEvent::NewPeer(peer_id).to_string().into());
        for other in peers {
            if let Err(e) = state.try_send(other, event.clone()) {
                eprintln!("signaling: error announcing {peer_id:?} to {other:?}: {e:?}");
            }
        }

        // Relay signaling traffic for the lifetime of this websocket.
        while let Some(request) = receiver.next().await {
            let request = match parse_request(request) {
                Ok(request) => request,
                Err(e) => match e {
                    ClientRequestError::Axum(_) => break, // connection reset or similar
                    ClientRequestError::Close => break,
                    ClientRequestError::Json(_) | ClientRequestError::UnsupportedType(_) => {
                        eprintln!("signaling: bad request from {peer_id:?}: {e:?}");
                        continue; // recoverable
                    }
                },
            };

            match request {
                PeerRequest::Signal { receiver, data } => {
                    let event = Message::Text(
                        JsonPeerEvent::Signal {
                            sender: peer_id,
                            data,
                        }
                        .to_string()
                        .into(),
                    );
                    if let Some(peer) = state.get_peer(&receiver) {
                        if let Err(e) = peer.sender.send(Ok(event)) {
                            eprintln!("signaling: error relaying signal: {e:?}");
                        }
                    }
                }
                // KeepAlive exists only to keep idle proxies from dropping the
                // socket; nothing to do.
                PeerRequest::KeepAlive => {}
            }
        }

        // The peer disconnected: drop it and tell the room.
        if let Some(removed_peer) = state.remove_peer(&peer_id) {
            let room = removed_peer.room;
            let event = Message::Text(JsonPeerEvent::PeerLeft(removed_peer.uuid).to_string().into());
            let matched = state.remove_matched_peer(peer_id);
            if !matched.is_empty() {
                for other in matched {
                    if let Err(e) = state.try_send(other, event.clone()) {
                        eprintln!("signaling: failed to send peer-left: {e:?}");
                    }
                }
            } else {
                for other in state
                    .get_room_peers(&room)
                    .into_iter()
                    .filter(|other_id| *other_id != peer_id)
                {
                    if let Err(e) = state.try_send(other, event.clone()) {
                        eprintln!("signaling: failed to send peer-left: {e:?}");
                    }
                }
            }
        }
    }
}

/// Start an in-process signaling server bound to `addr`, serving on the ambient
/// tokio runtime. Binding happens synchronously so a bind failure (e.g. the
/// port is already in use) is reported immediately and the local client is not
/// left racing an unbound server. Serving then runs as a background task for
/// the lifetime of the process.
pub fn spawn(addr: SocketAddr) {
    let mut state = ServerState::default();
    let mut server = SignalingServerBuilder::new(addr, FullMeshTopology, state.clone())
        .on_connection_request({
            let mut state = state.clone();
            move |connection| {
                let room_id = RoomId(connection.path.clone().unwrap_or_default());
                let next = connection
                    .query_params
                    .get("next")
                    .and_then(|next| next.parse::<usize>().ok());
                state.add_waiting_client(connection.origin, RequestedRoom { id: room_id, next });
                Ok(true) // accept all clients
            }
        })
        .on_id_assignment(move |(origin, peer_id)| {
            state.assign_id_to_waiting_client(origin, peer_id);
        })
        .build();

    match server.bind() {
        Ok(bound) => {
            eprintln!("Embedded signaling server listening on {bound}");
            tokio::spawn(async move {
                if let Err(e) = server.serve().await {
                    eprintln!("Embedded signaling server stopped: {e}");
                }
            });
        }
        Err(e) => {
            eprintln!("Could not start embedded signaling server on {addr}: {e}");
        }
    }
}
