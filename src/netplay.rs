use crate::device;
use crate::ui;
use sha2::digest::Digest;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RtcIceServerConfig {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

pub struct GgrsConfig;
impl ggrs::Config for GgrsConfig {
    type Input = ui::input::InputData;
    type InputPredictor = ggrs::PredictRepeatLast;
    type State = i32;
    type Address = matchbox_socket::PeerId;
}

pub struct MatchboxChannel {
    channel: matchbox_socket::WebRtcChannel,
    // Byte counters shared with the Netplay struct so the overlay can report
    // real throughput. GGRS 0.13 no longer exposes kbps, so we measure it here
    // at the actual transport (the GGRS input traffic channel).
    bytes_sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
    bytes_recv: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl ggrs::NonBlockingSocket<matchbox_socket::PeerId> for MatchboxChannel {
    fn send_to(&mut self, msg: &ggrs::Message, addr: &matchbox_socket::PeerId) {
        let encoded = postcard::to_stdvec(msg).expect("serialization failed");
        self.bytes_sent
            .fetch_add(encoded.len() as u64, std::sync::atomic::Ordering::Relaxed);
        let _ = self.channel.try_send(encoded.into(), *addr);
    }

    fn receive_all_messages(&mut self) -> Vec<(matchbox_socket::PeerId, ggrs::Message)> {
        let received = self.channel.receive();
        let mut out = Vec::with_capacity(received.len());
        for (peer, packet) in received.iter() {
            self.bytes_recv
                .fetch_add(packet.len() as u64, std::sync::atomic::Ordering::Relaxed);
            if let Ok(msg) = postcard::from_bytes::<ggrs::Message>(packet) {
                out.push((*peer, msg));
            }
        }
        out
    }
}

pub struct NetplayConfig {
    pub server_addr: String,
    /// The 0-based player slots this machine owns locally. Each slot is driven
    /// by the physical controller of the same index (slot == N64 channel ==
    /// controller index), so a machine with two controllers owns e.g. [0, 1].
    /// Remote peers see every one of these slots as belonging to this peer.
    pub local_players: Vec<usize>,
    pub number_of_players: usize,
    pub input_delay: usize,
    pub ice_config_path: std::path::PathBuf,
}

pub struct Netplay {
    pub disconnected: bool,
    pub session: ggrs::P2PSession<GgrsConfig>,
    pub reliable_channel: matchbox_socket::WebRtcChannel,
    pub peers: Vec<matchbox_socket::PeerId>,
    /// Slots this machine owns locally (see NetplayConfig::local_players).
    pub local_players: Vec<usize>,
    pub connected: [bool; 4],
    pub input_delay: usize,
    pub messages: std::collections::HashMap<String, Vec<u8>>,
    pub received_data: std::collections::VecDeque<Vec<u8>>,
    pub inputs: Vec<(ui::input::InputData, ggrs::InputStatus)>,
    pub requests: std::collections::VecDeque<ggrs::GgrsRequest<GgrsConfig>>,
    pub incoming_message: Vec<u8>,
    pub ice_config_path: std::path::PathBuf,
    // Overlay statistics (throughput measured at the transport, plus a rolling
    // 1-second sampling window).
    pub number_of_players: usize,
    pub bytes_sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub bytes_recv: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub stats_timer: std::time::Instant,
    pub last_bytes_sent: u64,
    pub last_bytes_recv: u64,
}

impl Netplay {
    /// True if the given player slot (== N64 channel) is driven by a local
    /// controller on this machine.
    pub fn owns_slot(&self, slot: usize) -> bool {
        self.local_players.contains(&slot)
    }

    /// The host is whichever single machine owns slot 0. Save-state authority,
    /// RNG/RTC seeding, etc. are anchored to the host so exactly one machine
    /// decides them, independent of how many local players each machine has.
    pub fn is_host(&self) -> bool {
        self.owns_slot(0)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NetplayMessage {
    name: String,
    data: Vec<u8>,
}

fn send_message(netplay: &mut Netplay, message: NetplayMessage) {
    let data = postcard::to_stdvec(&message).unwrap();
    let chunks = data.chunks(16384).collect::<Vec<&[u8]>>();
    for peer in netplay.peers.iter() {
        for chunk in chunks.iter() {
            if let Err(e) = netplay
                .reliable_channel
                .try_send(chunk.to_vec().into(), *peer)
            {
                eprintln!("Failed to send message: {}", e);
            }
        }
    }
}

fn process_reliable_messages(netplay: &mut Netplay) {
    netplay.received_data.extend(
        netplay
            .reliable_channel
            .receive()
            .iter()
            .map(|(_, data)| data.to_vec()),
    );

    while !netplay.received_data.is_empty() {
        if let Some(data) = netplay.received_data.pop_front() {
            netplay.incoming_message.extend(data);

            if let Ok(decoded_message) =
                postcard::from_bytes::<NetplayMessage>(&netplay.incoming_message)
            {
                netplay
                    .messages
                    .insert(decoded_message.name, decoded_message.data);
                netplay.incoming_message.clear();
                check_input_delay(netplay);
                check_disconnect(netplay);
            }
        }
    }
}

fn receive_message(netplay: &mut Netplay, name: &str) -> Vec<u8> {
    let timeout = std::time::Duration::from_secs(10);
    let now = std::time::Instant::now();

    loop {
        process_reliable_messages(netplay);
        if let Some(data) = netplay.messages.remove(name) {
            return data;
        }

        if now.elapsed() > timeout {
            panic!("Could not receive message for {name}");
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn send_player_number(
    channel: &mut matchbox_socket::WebRtcChannel,
    peers: &[matchbox_socket::PeerId],
    local_players: &[usize],
) {
    // Announce every slot this machine owns. The payload is a list of u64
    // slot indices (one machine may own several), so a peer learns all of our
    // local slots from a single message.
    let mut data = Vec::with_capacity(local_players.len() * 8);
    for slot in local_players {
        data.extend_from_slice(&(*slot as u64).to_be_bytes());
    }
    let message = NetplayMessage {
        name: "player_number".to_string(),
        data,
    };
    let data = postcard::to_stdvec(&message).unwrap();
    for peer in peers {
        if let Err(e) = channel.try_send(data.clone().into(), *peer) {
            eprintln!("Failed to send message: {}", e);
        }
    }
}

fn get_player_numbers(
    channel: &mut matchbox_socket::WebRtcChannel,
    player_numbers: &mut std::collections::BTreeMap<usize, Option<matchbox_socket::PeerId>>,
) {
    for (peer, data) in channel.receive() {
        let message = postcard::from_bytes::<NetplayMessage>(&data).unwrap();
        if message.name == "player_number" {
            // The payload is a concatenation of 8-byte slot indices; record
            // every slot as owned by this peer.
            for slot_bytes in message.data.chunks_exact(8) {
                let slot = u64::from_be_bytes(slot_bytes.try_into().unwrap()) as usize;
                player_numbers.insert(slot, Some(peer));
            }
        }
    }
}

pub fn send_rtc(netplay: &mut Netplay, rtc: i64) {
    let message = NetplayMessage {
        name: "rtc".to_string(),
        data: rtc.to_be_bytes().to_vec(),
    };
    send_message(netplay, message);
}

pub fn receive_rtc(netplay: &mut Netplay) -> i64 {
    let message = receive_message(netplay, "rtc");

    i64::from_be_bytes(message.try_into().unwrap())
}

pub fn send_rng(netplay: &mut Netplay, seed: u64) {
    let message = NetplayMessage {
        name: "rng".to_string(),
        data: seed.to_be_bytes().to_vec(),
    };
    send_message(netplay, message);
}

pub fn receive_rng(netplay: &mut Netplay) -> u64 {
    let message = receive_message(netplay, "rng");
    u64::from_be_bytes(message.try_into().unwrap())
}

pub fn send_save(netplay: &mut Netplay, save_type: &str, save_data: &[u8]) {
    let message = NetplayMessage {
        name: save_type.to_string(),
        data: save_data.to_vec(),
    };
    send_message(netplay, message);
}

pub fn receive_save(netplay: &mut Netplay, save_type: &str, save_data: &mut Vec<u8>) {
    let message = receive_message(netplay, save_type);
    *save_data = message;
}

pub fn send_input_delay(netplay: &mut Netplay, input_delay: usize) {
    if input_delay < 1 {
        return;
    }

    let message = NetplayMessage {
        name: "input_delay".to_string(),
        data: (input_delay as u64).to_be_bytes().to_vec(),
    };
    send_message(netplay, message);
    change_input_delay(netplay, input_delay);
}

fn change_input_delay(netplay: &mut Netplay, input_delay: usize) {
    netplay.input_delay = input_delay;
    for handle in netplay.session.local_player_handles() {
        if let Err(e) = netplay.session.set_input_delay(handle, input_delay) {
            eprintln!("Error setting input delay: {}", e);
        } else {
            ui::video::onscreen_message(
                &format!("Input delay: {}", input_delay),
                ui::video::MESSAGE_LENGTH_MESSAGE_VERY_SHORT,
            );
        }
    }
}

fn check_input_delay(netplay: &mut Netplay) {
    if let Some(data) = netplay.messages.remove("input_delay") {
        let input_delay = u64::from_be_bytes(data.try_into().unwrap()) as usize;
        if input_delay != netplay.input_delay {
            change_input_delay(netplay, input_delay);
        }
    }
}

fn check_disconnect(netplay: &mut Netplay) {
    if !netplay.disconnected && netplay.messages.remove("disconnect").is_some() {
        netplay.disconnected = true;
        ui::video::onscreen_message(
            "Player disconnected, session has ended",
            ui::video::MESSAGE_LENGTH_MESSAGE_LONG,
        );
    }
}

fn pending_frames(netplay: &Netplay) -> usize {
    netplay
        .requests
        .iter()
        .filter(|r| matches!(r, ggrs::GgrsRequest::AdvanceFrame { .. }))
        .count()
}

pub fn in_rollback(netplay: Option<&Netplay>) -> bool {
    if let Some(netplay) = netplay {
        pending_frames(netplay) != 0
    } else {
        false
    }
}

pub fn process_requests(
    device: &mut device::Device,
) -> Vec<(ui::input::InputData, ggrs::InputStatus)> {
    loop {
        if let Some(request) = device.netplay.as_mut().unwrap().requests.pop_front() {
            match request {
                ggrs::GgrsRequest::SaveGameState { cell, frame } => {
                    //savestates::create_savestate(device, true, Some(frame));

                    let mut hasher = sha2::Sha256::new();
                    for reg in device.cpu.cop0.regs.as_ref() {
                        hasher.update(reg.to_be_bytes());
                    }
                    let hash = u128::from_be_bytes(hasher.finalize()[..16].try_into().unwrap());
                    cell.save(frame, Some(frame), Some(hash));
                }
                ggrs::GgrsRequest::LoadGameState { cell, frame: _ } => {
                    eprintln!("attempting to load game state");
                    if let Some(_frame) = cell.load() {
                        //    savestates::load_savestate(device, true, Some(frame));
                    }
                }
                ggrs::GgrsRequest::AdvanceFrame { inputs } => {
                    return inputs;
                }
            }
        } else {
            process_netplay(device);
        }
    }
}

fn poll_clients(netplay: &mut Netplay) {
    netplay.session.poll_remote_clients();
    for event in netplay.session.events() {
        match event {
            ggrs::GgrsEvent::Synchronizing { .. } => {}
            ggrs::GgrsEvent::Synchronized { .. } => {}
            ggrs::GgrsEvent::Disconnected { .. } => {
                if !netplay.disconnected {
                    netplay.disconnected = true;
                    ui::video::onscreen_message(
                        "Lost connection to peer(s)",
                        ui::video::MESSAGE_LENGTH_MESSAGE_LONG,
                    );
                }
            }
            ggrs::GgrsEvent::NetworkInterrupted { .. } => {
                println!("network interrupted");
            }
            ggrs::GgrsEvent::NetworkResumed { .. } => {
                println!("network resumed");
            }
            ggrs::GgrsEvent::WaitRecommendation { skip_frames } => {
                println!("wait recommendation: skip_frames={}", skip_frames);
            }
            ggrs::GgrsEvent::DesyncDetected { .. } => {
                eprintln!("desync detected");
                ui::video::onscreen_message(
                    "Desync detected",
                    ui::video::MESSAGE_LENGTH_MESSAGE_LONG,
                );
            }
        }
    }
}

fn process_netplay(device: &mut device::Device) {
    let netplay = device.netplay.as_mut().unwrap();

    poll_clients(netplay);
    process_reliable_messages(netplay);
    advance_frame(device);
}

fn advance_frame(device: &mut device::Device) {
    let netplay = device.netplay.as_mut().unwrap();
    // GGRS requires an input for EVERY local handle each frame. A handle equals
    // its player slot equals its N64 controller channel, so each local player
    // reads its own distinct physical controller via ui::input::get(ui, slot).
    let past_prediction = netplay.session.current_frame() > netplay.session.max_prediction() as i32;
    let local_handles = netplay.session.local_player_handles();
    for handle in local_handles {
        let local_input = if past_prediction {
            ui::input::get(&mut device.ui, handle)
        } else {
            // workaround for disabled rollback
            ui::input::InputData::default()
        };
        netplay
            .session
            .add_local_input(handle, local_input)
            .unwrap();
    }

    // avoid rollback
    while !netplay.disconnected
        && netplay.session.current_frame() > netplay.session.confirmed_frame()
        && netplay.session.confirmed_frame() != ggrs::NULL_FRAME
    {
        poll_clients(netplay);
    }

    if netplay.disconnected {
        netplay.requests.push_back(ggrs::GgrsRequest::AdvanceFrame {
            inputs: vec![
                (
                    ui::input::InputData::default(),
                    ggrs::InputStatus::Disconnected
                );
                4
            ],
        });
        return;
    }

    match netplay.session.advance_frame() {
        Ok(requests) => {
            netplay.requests.extend(requests);
        }
        Err(ggrs::GgrsError::PredictionThreshold) => {
            println!("prediction threshold reached");
        }
        Err(e) => panic!("{e}"),
    }
}

fn verify_peers(
    peers: &[matchbox_socket::PeerId],
    player_numbers: &std::collections::BTreeMap<usize, Option<matchbox_socket::PeerId>>,
) -> bool {
    for peer in player_numbers.values() {
        if let Some(peer) = peer
            && !peers.contains(peer)
        {
            return false;
        }
    }
    true
}

fn create_socket(builder: matchbox_socket::WebRtcSocketBuilder) -> matchbox_socket::WebRtcSocket {
    let (socket, loop_fut) = builder.build();
    tokio::spawn(async move {
        if let Err(e) = loop_fut.await {
            eprintln!("WebRTC loop failed: {}", e);
        }
    });
    socket
}

pub fn init(
    device: &mut device::Device,
    netplay_config: &NetplayConfig,
    pal: bool,
) -> Option<Netplay> {
    let mut builder = matchbox_socket::WebRtcSocketBuilder::new(&netplay_config.server_addr)
        .add_unreliable_channel()
        .add_reliable_channel();

    if let Ok(ice_config) = std::fs::read(&netplay_config.ice_config_path)
        && let Ok(ice_config) = serde_json::from_slice::<RtcIceServerConfig>(&ice_config)
    {
        builder = builder.ice_server(matchbox_socket::RtcIceServerConfig {
            urls: ice_config.urls,
            username: ice_config.username,
            credential: ice_config.credential,
        });
    } else {
        eprintln!("Using default ICE config");
    }

    let mut socket = create_socket(builder.clone());

    let mut now = std::time::Instant::now();
    let mut message_timer = now;
    let socket_timeout = std::time::Duration::from_secs_f64(rand::random_range(8.0..10.0));
    // Seed the ownership map with every slot this machine owns locally (value
    // None == "mine"). Remote peers' slots are filled in during the handshake.
    let mut player_numbers: std::collections::BTreeMap<usize, Option<matchbox_socket::PeerId>> =
        netplay_config
            .local_players
            .iter()
            .map(|slot| (*slot, None))
            .collect();

    ui::video::onscreen_message(
        "Connecting to netplay peers...\nPlease wait...",
        ui::video::MESSAGE_LENGTH_MESSAGE_SHORT,
    );

    device.cpu.running = true;
    while device.cpu.running {
        if socket
            .update_peers()
            .iter()
            .any(|(_, peer_state)| *peer_state == matchbox_socket::PeerState::Disconnected)
        {
            // if someone has disconnected, reset the timeout
            now = std::time::Instant::now();
        }

        let connected_peers = socket
            .connected_peers()
            .collect::<Vec<matchbox_socket::PeerId>>();

        // A machine may own several slots, so we can't assume the peer count
        // equals number_of_players - 1. Instead, announce our slots and learn
        // our peers' slots whenever any peer is connected, and finish once every
        // slot has a known owner and all owning peers are connected.
        if !connected_peers.is_empty() {
            send_player_number(
                socket.channel_mut(1),
                &connected_peers,
                &netplay_config.local_players,
            );
            get_player_numbers(socket.channel_mut(1), &mut player_numbers);
            if player_numbers.len() == netplay_config.number_of_players
                && verify_peers(&connected_peers, &player_numbers)
            {
                break;
            }
        }
        if now.elapsed() > socket_timeout {
            socket.close();
            player_numbers.retain(|_, peer| peer.is_none());
            socket = create_socket(builder.clone());
            now = std::time::Instant::now();
        }

        if message_timer.elapsed() > std::time::Duration::from_secs(4) {
            ui::video::onscreen_message(
                &format!(
                    "Still connecting to {} netplay peer(s)...\nPlease wait...",
                    netplay_config.number_of_players - player_numbers.len()
                ),
                ui::video::MESSAGE_LENGTH_MESSAGE_SHORT,
            );
            message_timer = std::time::Instant::now();
        }

        ui::video::render_frame();
        ui::video::update_screen();
        std::thread::sleep(std::time::Duration::from_millis(10));
        ui::video::check_callback(device);
    }
    if !device.cpu.running {
        // user closed the window
        return None;
    }
    device.cpu.running = false;

    let mut session_builder = ggrs::SessionBuilder::<GgrsConfig>::new()
        .with_num_players(netplay_config.number_of_players)
        .unwrap()
        .with_input_delay(netplay_config.input_delay)
        .with_fps(if pal { 50 } else { 60 })
        .unwrap()
        .with_desync_detection_mode(ggrs::DesyncDetection::On { interval: 60 })
        .with_max_prediction_window(16)
        .with_disconnect_timeout(std::time::Duration::from_secs(if cfg!(debug_assertions) {
            10
        } else {
            5
        }));

    let mut peers = vec![];
    for (i, peer) in player_numbers.iter() {
        if let Some(peer) = peer {
            session_builder = session_builder
                .add_player(ggrs::PlayerType::Remote(*peer), *i)
                .unwrap();
            // A peer that owns several slots appears once per slot here; keep
            // `peers` de-duplicated so reliable messages are sent to it once.
            if !peers.contains(peer) {
                peers.push(*peer);
            }
        } else {
            session_builder = session_builder
                .add_player(ggrs::PlayerType::Local, *i)
                .unwrap();
        }
    }

    let bytes_sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let bytes_recv = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let matchbox_channel = MatchboxChannel {
        channel: socket.take_channel(0).unwrap(),
        bytes_sent: bytes_sent.clone(),
        bytes_recv: bytes_recv.clone(),
    };
    let reliable_channel = socket.take_channel(1).unwrap();

    if matchbox_channel.channel.config().max_retransmits != Some(0)
        || matchbox_channel.channel.config().ordered
    {
        eprintln!("Sending GGRS traffic over reliable channel");
    }

    let mut session = session_builder.start_p2p_session(matchbox_channel).unwrap();

    let now = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(10);
    while session.current_state() != ggrs::SessionState::Running {
        session.poll_remote_clients();
        if now.elapsed() > timeout {
            eprintln!("Could not start netplay session");
            return None;
        }
        unsafe { sdl3_sys::events::SDL_PumpEvents() };
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    Some(Netplay {
        disconnected: false,
        incoming_message: vec![],
        input_delay: netplay_config.input_delay,
        session,
        reliable_channel,
        peers,
        local_players: netplay_config.local_players.clone(),
        connected: [
            netplay_config.number_of_players > 0,
            netplay_config.number_of_players > 1,
            netplay_config.number_of_players > 2,
            netplay_config.number_of_players > 3,
        ],
        inputs: Vec::new(),
        requests: std::collections::VecDeque::new(),
        received_data: std::collections::VecDeque::new(),
        messages: std::collections::HashMap::new(),
        ice_config_path: netplay_config.ice_config_path.clone(),
        number_of_players: netplay_config.number_of_players,
        bytes_sent,
        bytes_recv,
        stats_timer: std::time::Instant::now(),
        last_bytes_sent: 0,
        last_bytes_recv: 0,
    })
}

/// Build the netplay overlay line once per second (throughput measured here,
/// latency and frame-lead from GGRS). Returns None between samples so the
/// caller can poll it every frame cheaply.
pub fn overlay_stats(netplay: &mut Netplay) -> Option<String> {
    use std::sync::atomic::Ordering::Relaxed;
    let elapsed = netplay.stats_timer.elapsed().as_secs_f64();
    if elapsed < 1.0 {
        return None;
    }

    let sent = netplay.bytes_sent.load(Relaxed);
    let recv = netplay.bytes_recv.load(Relaxed);
    // bytes/sec * 8 bits / 1000 => kbit/s over the elapsed window.
    let up_kbps =
        (sent.saturating_sub(netplay.last_bytes_sent) as f64 * 8.0 / 1000.0 / elapsed) as u64;
    let down_kbps =
        (recv.saturating_sub(netplay.last_bytes_recv) as f64 * 8.0 / 1000.0 / elapsed) as u64;
    netplay.last_bytes_sent = sent;
    netplay.last_bytes_recv = recv;
    netplay.stats_timer = std::time::Instant::now();

    // Aggregate the worst (highest) ping and lag across all remote players.
    // network_stats() errors for local handles and before ~1s of data, which
    // we simply skip.
    let mut ping: u128 = 0;
    let mut behind: i32 = 0;
    for handle in 0..netplay.number_of_players {
        if let Ok(stats) = netplay.session.network_stats(handle) {
            ping = ping.max(stats.ping);
            behind = behind.max(stats.local_frames_behind);
        }
    }
    let ahead = netplay.session.frames_ahead();

    let line = format!(
        "Ping: {ping}ms  Up: {up_kbps} kb/s  Down: {down_kbps} kb/s  Lead: {ahead}  Lag: {behind}"
    );
    // Optional headless logging for latency measurement / debugging.
    if std::env::var("GOPHER_NETSTATS_LOG").is_ok() {
        eprintln!("[NETSTATS] {line}");
    }
    Some(line)
}

pub fn close(netplay: &mut Netplay) {
    if !netplay.disconnected {
        let message = NetplayMessage {
            name: "disconnect".to_string(),
            data: vec![],
        };
        send_message(netplay, message);
        std::thread::sleep(std::time::Duration::from_millis(200)); // give the message time to be sent
    }

    let _ = std::fs::remove_file(&netplay.ice_config_path);
}
