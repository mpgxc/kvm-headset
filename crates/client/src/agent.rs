//! The long-running client: PC audio to the hub while active, headset mic always.

use std::{
    net::UdpSocket,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use headhub_core::{
    audio::{self, Application, Direction, FrameDecoder, FrameEncoder},
    protocol::{MAX_PACKET, Packet},
};
use tracing::{debug, error, info, warn};

use crate::{Config, ports::Cable};

const HEARTBEAT: Duration = Duration::from_secs(1);
/// Without a status for this long the hub is gone; stop streaming until it is back.
const HUB_TIMEOUT: Duration = Duration::from_secs(3);
/// Without a status for this long the socket is assumed broken and the agent exits.
const RESTART_AFTER: Duration = Duration::from_secs(15);
/// PC audio bitrate: music and calls, mono.
const SPEAKER_BITRATE: i32 = 64_000;

/// Runs until the process is killed; reconnects by itself because UDP has no session.
///
/// # Errors
/// Fails when the virtual devices or the socket cannot be opened.
pub fn run(config: &Config, cable: &Cable) -> Result<()> {
    let speaker = audio::find_device(Some(&cable.speaker), Direction::Input)?;
    let mic = audio::find_device(Some(&cable.mic), Direction::Output)?;
    let (_speaker_stream, frames) =
        audio::capture(&speaker).context("capturando a saída virtual")?;
    let (_mic_stream, queue) = audio::playback(&mic).context("abrindo o microfone virtual")?;

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket
        .connect(&config.hub)
        .with_context(|| format!("resolvendo o hub {}", config.hub))?;
    socket.set_read_timeout(Some(HEARTBEAT))?;
    info!("{} conectando ao hub {}", config.name, config.hub);

    let active = Arc::new(AtomicBool::new(false));

    audio::spawn_or_exit("heartbeat", {
        let socket = socket.try_clone()?;
        let mut hello = Vec::new();
        Packet::Hello {
            token: &config.token,
            name: &config.name,
        }
        .encode(&mut hello)?;
        move || loop {
            if let Err(err) = socket.send(&hello) {
                debug!("heartbeat: {err}");
            }
            thread::sleep(HEARTBEAT);
        }
    });

    audio::spawn_or_exit("speaker", {
        let socket = socket.try_clone()?;
        let active = active.clone();
        move || {
            let mut encoder = FrameEncoder::new(Application::Audio, SPEAKER_BITRATE)?;
            let mut out = Vec::with_capacity(MAX_PACKET);
            for frame in frames {
                // Inactive PC stays muted: its audio is captured and dropped.
                if active.load(Ordering::Relaxed) {
                    encoder.encode(&frame, &mut out)?;
                    if let Err(err) = socket.send(&out) {
                        debug!("envio de áudio: {err}");
                    }
                }
            }
            anyhow::bail!("captura da saída virtual parou")
        }
    });

    let mut decoder = FrameDecoder::new(queue)?;
    let mut buf = [0; MAX_PACKET];
    let mut last_status = Instant::now();
    let mut last_stats = Instant::now();
    loop {
        match socket.recv(&mut buf).map(|len| Packet::decode(&buf[..len])) {
            Ok(Ok(Packet::Audio { seq, payload })) => {
                if let Err(err) = decoder.push(seq, payload) {
                    debug!("áudio inválido do hub: {err}");
                }
                if last_stats.elapsed() >= audio::STATS_INTERVAL {
                    info!("microfone do hub: {}", decoder.take_stats());
                    last_stats = Instant::now();
                }
            }
            Ok(Ok(Packet::Status { active: name, .. })) => {
                last_status = Instant::now();
                let now_active = name == Some(config.name.as_str());
                if active.swap(now_active, Ordering::Relaxed) != now_active {
                    info!(
                        "headset {}",
                        if now_active {
                            "neste PC"
                        } else {
                            "em outro PC"
                        }
                    );
                }
            }
            Ok(other) => debug!("pacote ignorado: {other:?}"),
            // Timeouts, and "connection refused" while the hub restarts.
            Err(err) => debug!("recv: {err}"),
        }
        if last_status.elapsed() > HUB_TIMEOUT && active.swap(false, Ordering::Relaxed) {
            warn!("hub sem resposta; aguardando reconexão");
        }
        // A socket created before macOS grants Local Network access stays blocked for good;
        // exiting lets launchd start a fresh process with a working one.
        if last_status.elapsed() > RESTART_AFTER {
            error!(
                "hub sem resposta há {} s; reiniciando",
                RESTART_AFTER.as_secs()
            );
            std::process::exit(1);
        }
    }
}
