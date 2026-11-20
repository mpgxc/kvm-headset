//! Hub that owns the headset: streams its mic to every PC and plays the active PC's audio.

use std::{
    collections::HashMap,
    fs,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc::Receiver},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::Parser;
use headhub_core::{
    audio::{self, Application, Direction, Frame, FrameDecoder, FrameEncoder},
    protocol::{DEFAULT_PORT, MAX_PACKET, Packet},
};
use tracing::{debug, info, warn};

/// A client that missed this many seconds of heartbeats is dropped.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
/// Mic bitrate: voice only, mono.
const MIC_BITRATE: i32 = 32_000;

#[derive(Parser)]
#[command(about = "Hub do head-hub: compartilha o headset USB entre os PCs da rede")]
struct Args {
    /// Token de pareamento; os clientes precisam do mesmo valor.
    #[arg(long, env = "HEADHUB_TOKEN")]
    token: String,
    /// Porta UDP.
    #[arg(long, env = "HEADHUB_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,
    /// Parte do nome ALSA do headset (ex.: "USB"); sem valor usa o dispositivo padrão.
    #[arg(long, env = "HEADHUB_DEVICE")]
    device: Option<String>,
    /// Arquivo que guarda o PC ativo entre reinícios.
    #[arg(long, env = "HEADHUB_STATE", default_value = "/data/active")]
    state: PathBuf,
}

struct Client {
    addr: SocketAddr,
    seen: Instant,
}

struct Hub {
    token: String,
    state: PathBuf,
    clients: HashMap<String, Client>,
    active: Option<String>,
}

impl Hub {
    fn live(&self) -> impl Iterator<Item = (&String, &Client)> {
        self.clients
            .iter()
            .filter(|(_, c)| c.seen.elapsed() < CLIENT_TIMEOUT)
    }

    fn active_addr(&self) -> Option<SocketAddr> {
        let name = self.active.as_ref()?;
        self.live().find(|(n, _)| *n == name).map(|(_, c)| c.addr)
    }

    fn register(&mut self, name: &str, addr: SocketAddr) {
        match self.clients.get_mut(name) {
            Some(client) => {
                if client.addr != addr {
                    info!("{name} reconectou de {addr}");
                    client.addr = addr;
                }
                client.seen = Instant::now();
            }
            None => {
                info!("{name} conectou de {addr}");
                self.clients.insert(
                    name.to_owned(),
                    Client {
                        addr,
                        seen: Instant::now(),
                    },
                );
            }
        }
    }

    fn activate(&mut self, name: &str) {
        info!("PC ativo: {name}");
        self.active = Some(name.to_owned());
        if let Err(err) = fs::write(&self.state, name) {
            warn!("não salvei o PC ativo em {}: {err}", self.state.display());
        }
    }

    fn status(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut clients: Vec<&str> = self.live().map(|(n, _)| n.as_str()).collect();
        clients.sort_unstable();
        Packet::Status {
            active: self.active.as_deref(),
            clients,
        }
        .encode(out)
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();

    let mic = audio::find_device(args.device.as_deref(), Direction::Input)?;
    let speaker = audio::find_device(args.device.as_deref(), Direction::Output)?;
    info!("microfone: {mic}; saída: {speaker}");
    let (_mic_stream, frames) = audio::capture(&mic).context("abrindo o microfone do headset")?;
    let (_speaker_stream, queue) =
        audio::playback(&speaker).context("abrindo a saída do headset")?;

    let socket = UdpSocket::bind(("0.0.0.0", args.port))
        .with_context(|| format!("abrindo a porta UDP {}", args.port))?;
    info!("escutando em UDP {}", args.port);

    let active = fs::read_to_string(&args.state)
        .ok()
        .map(|s| s.trim().to_owned());
    let hub = Arc::new(Mutex::new(Hub {
        token: args.token,
        state: args.state,
        clients: HashMap::new(),
        active: active.filter(|name| !name.is_empty()),
    }));

    audio::spawn_or_exit("mic", {
        let socket = socket.try_clone()?;
        let hub = hub.clone();
        move || stream_mic(&frames, &socket, &hub)
    });

    let mut decoder = FrameDecoder::new(queue)?;
    let mut speaker_source = None;
    let mut last_stats = Instant::now();
    let mut buf = [0; MAX_PACKET];
    let mut out = Vec::with_capacity(MAX_PACKET);
    loop {
        let (len, from) = match socket.recv_from(&mut buf) {
            Ok(received) => received,
            Err(err) => {
                debug!("recv: {err}");
                continue;
            }
        };
        let packet = match Packet::decode(&buf[..len]) {
            Ok(packet) => packet,
            Err(err) => {
                debug!("pacote inválido de {from}: {err}");
                continue;
            }
        };
        let mut hub = hub.lock().expect("hub lock poisoned");
        match packet {
            Packet::Audio { seq, payload } => {
                if hub.active_addr() != Some(from) {
                    continue; // only the active PC reaches the headset
                }
                drop(hub);
                if speaker_source != Some(from) {
                    decoder.reset();
                    decoder.take_stats();
                    speaker_source = Some(from);
                }
                if let Err(err) = decoder.push(seq, payload) {
                    debug!("áudio inválido de {from}: {err}");
                }
                if last_stats.elapsed() >= audio::STATS_INTERVAL {
                    info!("áudio de {from}: {}", decoder.take_stats());
                    last_stats = Instant::now();
                }
            }
            Packet::Hello { token, name } if token == hub.token => {
                hub.register(name, from);
                hub.status(&mut out)?;
                send(&socket, &out, from);
            }
            Packet::Query { token } if token == hub.token => {
                hub.status(&mut out)?;
                send(&socket, &out, from);
            }
            Packet::Activate { token, name } if token == hub.token => {
                hub.activate(name);
                hub.status(&mut out)?;
                // Everyone learns the switch now instead of at the next heartbeat.
                for (_, client) in hub.live() {
                    send(&socket, &out, client.addr);
                }
                send(&socket, &out, from);
            }
            Packet::Hello { .. } | Packet::Query { .. } | Packet::Activate { .. } => {
                warn!("token inválido de {from}");
            }
            Packet::Status { .. } => debug!("status inesperado de {from}"),
        }
    }
}

/// Sends every mic frame to every connected PC (RF5).
fn stream_mic(frames: &Receiver<Frame>, socket: &UdpSocket, hub: &Mutex<Hub>) -> Result<()> {
    let mut encoder = FrameEncoder::new(Application::Voip, MIC_BITRATE)?;
    let mut out = Vec::with_capacity(MAX_PACKET);
    let mut targets = Vec::new();
    for frame in frames {
        encoder.encode(&frame, &mut out)?;
        targets.clear();
        targets.extend(
            hub.lock()
                .expect("hub lock poisoned")
                .live()
                .map(|(_, c)| c.addr),
        );
        for addr in &targets {
            send(socket, &out, *addr);
        }
    }
    anyhow::bail!("captura do microfone parou")
}

/// UDP is best effort: a lost reply is repeated by the next heartbeat, so failures only log.
fn send(socket: &UdpSocket, packet: &[u8], to: SocketAddr) {
    if let Err(err) = socket.send_to(packet, to) {
        debug!("envio para {to}: {err}");
    }
}
