//! Audio devices, Opus codec and playback jitter buffer.
//!
//! Everything travels as mono 48 kHz in 10 ms frames; devices are downmixed on capture and
//! the mono signal is copied to every channel on playback.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
};

use anyhow::{Context, Result, bail};
use cpal::{
    BufferSize, Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    SupportedBufferSize,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
pub use opus::Application;

use crate::protocol::{MAX_PACKET, Packet};

/// Sample rate of every stream and of the Opus codec.
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples per frame: 10 ms keeps latency low at 100 packets per second.
pub const FRAME: usize = 480;
/// One 10 ms mono frame.
pub type Frame = [f32; FRAME];
/// Mono samples waiting to be played, filled from the network and drained by the device.
pub type Queue = Arc<Mutex<VecDeque<f32>>>;

/// Above this many queued samples (60 ms) something stalled; trim back to the target at once.
const MAX_QUEUED: usize = FRAME * 6;
/// Queue level (20 ms) that absorbs network jitter without adding much delay.
const TARGET_QUEUED: usize = FRAME * 2;
/// Distance from the target (5 ms) before clock drift is corrected.
const DRIFT_BAND: usize = FRAME / 2;
/// Lost frames concealed per gap; longer gaps are a restart, not loss worth concealing.
const MAX_CONCEALED: u16 = 3;
/// Packets up to this many sequence numbers behind the expected one are late duplicates.
const REORDER_WINDOW: u16 = 50;
/// Captured frames buffered for the encoder thread (80 ms) before new ones are dropped.
const CAPTURE_BACKLOG: usize = 8;

/// Which side of a device to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

/// Finds the first device whose name contains `name` (case-insensitive), or the default one.
///
/// # Errors
/// Fails when no device matches; the message lists the available names.
pub fn find_device(name: Option<&str>, direction: Direction) -> Result<Device> {
    let host = cpal::default_host();
    let Some(wanted) = name else {
        let device = match direction {
            Direction::Input => host.default_input_device(),
            Direction::Output => host.default_output_device(),
        };
        return device.context("nenhum dispositivo de áudio padrão");
    };
    let devices: Vec<Device> = match direction {
        Direction::Input => host.input_devices()?.collect(),
        Direction::Output => host.output_devices()?.collect(),
    };
    let wanted_lower = wanted.to_lowercase();
    let names: Vec<String> = devices.iter().map(ToString::to_string).collect();
    match names
        .iter()
        .position(|n| n.to_lowercase().contains(&wanted_lower))
    {
        Some(i) => Ok(devices
            .into_iter()
            .nth(i)
            .expect("index comes from the same list")),
        None => bail!(
            "dispositivo de {direction:?} '{wanted}' não encontrado; disponíveis: {}",
            names.join(", ")
        ),
    }
}

/// Starts capturing `device`; yields mono frames on the returned channel.
///
/// Frames are dropped when the consumer falls more than 80 ms behind.
///
/// # Errors
/// Fails when the device has no 48 kHz f32/i16 configuration or cannot start.
pub fn capture(device: &Device) -> Result<(Stream, Receiver<Frame>)> {
    let (config, format) = stream_config(device, Direction::Input)?;
    let (tx, rx) = sync_channel(CAPTURE_BACKLOG);
    let stream = match format {
        SampleFormat::F32 => build_capture::<f32>(device, config, tx),
        _ => build_capture::<i16>(device, config, tx),
    }?;
    stream.play()?;
    Ok((stream, rx))
}

/// Starts playing on `device` whatever is pushed into the returned queue; silence when empty.
///
/// # Errors
/// Fails when the device has no 48 kHz f32/i16 configuration or cannot start.
pub fn playback(device: &Device) -> Result<(Stream, Queue)> {
    let (config, format) = stream_config(device, Direction::Output)?;
    let queue = Queue::default();
    let stream = match format {
        SampleFormat::F32 => build_playback::<f32>(device, config, queue.clone()),
        _ => build_playback::<i16>(device, config, queue.clone()),
    }?;
    stream.play()?;
    Ok((stream, queue))
}

fn stream_config(device: &Device, direction: Direction) -> Result<(StreamConfig, SampleFormat)> {
    let ranges: Vec<_> = match direction {
        Direction::Input => device.supported_input_configs()?.collect(),
        Direction::Output => device.supported_output_configs()?.collect(),
    };
    let supported = ranges
        .into_iter()
        .filter(|r| matches!(r.sample_format(), SampleFormat::F32 | SampleFormat::I16))
        .filter_map(|r| r.try_with_sample_rate(SAMPLE_RATE))
        .max_by_key(|c| {
            (
                c.sample_format() == SampleFormat::F32,
                std::cmp::Reverse(c.channels()),
            )
        })
        .with_context(|| format!("'{device}' não suporta {SAMPLE_RATE} Hz em f32 ou i16"))?;
    let frame = FRAME as u32;
    let buffer_size = match supported.buffer_size() {
        SupportedBufferSize::Range { min, max } if (*min..=*max).contains(&frame) => {
            BufferSize::Fixed(frame)
        }
        _ => BufferSize::Default,
    };
    let config = StreamConfig {
        buffer_size,
        ..supported.config()
    };
    Ok((config, supported.sample_format()))
}

fn build_capture<T>(device: &Device, config: StreamConfig, tx: SyncSender<Frame>) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let mut frame = [0.0; FRAME];
    let mut filled = 0;
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &_| {
            for chunk in data.chunks(channels) {
                let sum: f32 = chunk.iter().map(|s| f32::from_sample(*s)).sum();
                frame[filled] = sum / channels as f32;
                filled += 1;
                if filled == FRAME {
                    // A full channel means the encoder stalled; dropping keeps latency bounded.
                    let _ = tx.try_send(frame);
                    filled = 0;
                }
            }
        },
        |err| tracing::warn!("erro na captura: {err}"),
        None,
    )?;
    Ok(stream)
}

fn build_playback<T>(device: &Device, config: StreamConfig, queue: Queue) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &_| {
            let mut queue = queue.lock().expect("playback queue lock poisoned");
            for chunk in data.chunks_mut(channels) {
                chunk.fill(T::from_sample(queue.pop_front().unwrap_or(0.0)));
            }
            correct_drift(&mut queue);
        },
        |err| tracing::warn!("erro na reprodução: {err}"),
        None,
    )?;
    Ok(stream)
}

/// Moves the queue one sample toward the target per device callback (at most ~0.2% speed
/// change, inaudible), so sender and headset clocks that disagree never under- or overrun.
fn correct_drift(queue: &mut VecDeque<f32>) {
    let len = queue.len();
    if len > TARGET_QUEUED + DRIFT_BAND {
        queue.pop_front();
    } else if len < TARGET_QUEUED - DRIFT_BAND
        && let Some(&first) = queue.front()
    {
        queue.push_front(first);
    }
}

/// Encodes frames into ready-to-send [`Packet::Audio`] datagrams.
pub struct FrameEncoder {
    opus: opus::Encoder,
    seq: u16,
    scratch: [u8; MAX_PACKET],
}

impl FrameEncoder {
    /// Creates an encoder with in-band FEC, so one lost packet is recovered from the next.
    ///
    /// # Errors
    /// Fails when libopus rejects the settings.
    pub fn new(application: Application, bitrate: i32) -> Result<Self> {
        let mut opus = opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, application)?;
        opus.set_bitrate(opus::Bitrate::Bits(bitrate))?;
        opus.set_inband_fec(true)?;
        opus.set_packet_loss_perc(5)?;
        Ok(Self {
            opus,
            seq: 0,
            scratch: [0; MAX_PACKET],
        })
    }

    /// Writes the datagram for `frame` into `out`.
    ///
    /// # Errors
    /// Fails when libopus fails to encode.
    pub fn encode(&mut self, frame: &Frame, out: &mut Vec<u8>) -> Result<()> {
        let len = self.opus.encode_float(frame, &mut self.scratch)?;
        Packet::Audio {
            seq: self.seq,
            payload: &self.scratch[..len],
        }
        .encode(out)?;
        self.seq = self.seq.wrapping_add(1);
        Ok(())
    }
}

/// How often hub and clients log stream [`Stats`].
pub const STATS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Stream health since the last [`FrameDecoder::take_stats`] (RF8).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub received: u32,
    /// Packets that never arrived (sequence gaps), concealed or not.
    pub lost: u32,
    /// Packets that arrived after a newer one and were dropped.
    pub late: u32,
    /// Loudest decoded sample, 0.0 to 1.0: tells silence from real audio.
    pub peak: f32,
    /// Audio waiting in the playback queue now, in milliseconds.
    pub queued_ms: u32,
}

impl Stats {
    /// Lost packets as a percentage of expected ones.
    #[must_use]
    pub fn loss_percent(&self) -> f32 {
        let expected = self.received + self.lost;
        if expected == 0 {
            0.0
        } else {
            100.0 * self.lost as f32 / expected as f32
        }
    }
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} pacotes, perda {:.1}%, atrasados {}, pico {:.2}, buffer {} ms",
            self.received,
            self.loss_percent(),
            self.late,
            self.peak,
            self.queued_ms
        )
    }
}

/// Decodes [`Packet::Audio`] payloads into a playback [`Queue`], concealing lost frames.
pub struct FrameDecoder {
    opus: opus::Decoder,
    next_seq: Option<u16>,
    frame: Frame,
    queue: Queue,
    stats: Stats,
}

impl FrameDecoder {
    /// # Errors
    /// Fails when libopus cannot create a decoder.
    pub fn new(queue: Queue) -> Result<Self> {
        let opus = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)?;
        Ok(Self {
            opus,
            next_seq: None,
            frame: [0.0; FRAME],
            queue,
            stats: Stats::default(),
        })
    }

    /// Returns the counters gathered since the previous call and starts new ones.
    pub fn take_stats(&mut self) -> Stats {
        let queued = self
            .queue
            .lock()
            .expect("playback queue lock poisoned")
            .len();
        let queued_ms = (queued * 1000 / SAMPLE_RATE as usize) as u32;
        Stats {
            queued_ms,
            ..std::mem::take(&mut self.stats)
        }
    }

    /// Forgets the sequence; call when the sender changes.
    pub fn reset(&mut self) {
        self.next_seq = None;
    }

    /// Decodes one packet, concealing up to 3 lost frames before it and dropping late ones.
    ///
    /// # Errors
    /// Fails on corrupt Opus payloads.
    pub fn push(&mut self, seq: u16, payload: &[u8]) -> Result<()> {
        if let Some(next) = self.next_seq {
            let behind = next.wrapping_sub(seq);
            if (1..=REORDER_WINDOW).contains(&behind) {
                self.stats.late += 1;
                return Ok(());
            }
            let gap = seq.wrapping_sub(next);
            // Bigger gaps are a sender restart or a long outage, not packet loss.
            if gap <= REORDER_WINDOW {
                self.stats.lost += u32::from(gap);
            }
            let lost = gap.min(MAX_CONCEALED);
            if lost > 0 {
                for _ in 1..lost {
                    self.decode(&[], false)?;
                }
                // The last lost frame is rebuilt from the FEC data carried by this packet.
                self.decode(payload, true)?;
            }
        }
        self.stats.received += 1;
        self.decode(payload, false)?;
        self.next_seq = Some(seq.wrapping_add(1));
        Ok(())
    }

    fn decode(&mut self, payload: &[u8], fec: bool) -> Result<()> {
        let len = self.opus.decode_float(payload, &mut self.frame, fec)?;
        let mut queue = self.queue.lock().expect("playback queue lock poisoned");
        queue.extend(&self.frame[..len]);
        let peak = self.frame[..len]
            .iter()
            .fold(0.0_f32, |m, s| m.max(s.abs()));
        self.stats.peak = self.stats.peak.max(peak);
        // Safety net for stalls; regular clock drift is handled by `correct_drift`.
        if queue.len() > MAX_QUEUED {
            let excess = queue.len() - TARGET_QUEUED;
            queue.drain(..excess);
        }
        Ok(())
    }
}

/// Runs `job` on a named thread and exits the process if it fails, so the supervisor restarts us.
pub fn spawn_or_exit(name: &str, job: impl FnOnce() -> Result<()> + Send + 'static) {
    let thread_name = name.to_owned();
    std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            if let Err(err) = job() {
                tracing::error!("thread {thread_name} falhou: {err:#}");
                std::process::exit(1);
            }
        })
        .expect("spawning a thread only fails when the OS is out of resources");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(phase: usize) -> Frame {
        std::array::from_fn(|i| ((phase * FRAME + i) as f32 * 0.05).sin() * 0.5)
    }

    /// Feeds packets in the given order; returns queued samples and the decoder stats.
    fn feed(seqs: &[u16]) -> (usize, Stats) {
        let mut encoder = FrameEncoder::new(Application::Voip, 32_000).expect("encoder");
        let mut packets = Vec::new();
        for i in 0..=*seqs.iter().max().expect("non-empty") {
            let mut out = Vec::new();
            encoder
                .encode(&tone(usize::from(i)), &mut out)
                .expect("encode");
            packets.push(out);
        }
        let queue = Queue::default();
        let mut decoder = FrameDecoder::new(queue.clone()).expect("decoder");
        for &seq in seqs {
            let Ok(Packet::Audio { seq, payload }) = Packet::decode(&packets[usize::from(seq)])
            else {
                panic!("packet {seq} is not audio");
            };
            decoder.push(seq, payload).expect("push");
        }
        let queued = queue.lock().expect("lock").len();
        (queued, decoder.take_stats())
    }

    #[test]
    fn in_order_frames_are_all_played() {
        let (queued, stats) = feed(&[0, 1, 2, 3]);
        assert_eq!(queued, 4 * FRAME);
        assert_eq!((stats.received, stats.lost, stats.late), (4, 0, 0));
        assert!(stats.peak > 0.3, "tone decoded as silence: {stats:?}");
    }

    #[test]
    fn lost_frames_are_concealed_to_keep_timing() {
        let (queued, stats) = feed(&[0, 1, 4]);
        assert_eq!(queued, 5 * FRAME);
        assert_eq!((stats.received, stats.lost), (3, 2));
        assert!((stats.loss_percent() - 40.0).abs() < 0.01);
    }

    #[test]
    fn late_and_duplicate_frames_are_dropped() {
        let (queued, stats) = feed(&[0, 2, 1, 2]);
        assert_eq!(queued, 3 * FRAME);
        assert_eq!((stats.lost, stats.late), (1, 2));
    }

    #[test]
    fn long_gaps_conceal_only_a_few_frames_and_are_not_loss() {
        let (queued, stats) = feed(&[0, 100]);
        assert_eq!(queued, (2 + usize::from(MAX_CONCEALED)) * FRAME);
        assert_eq!(stats.lost, 0);
    }

    #[test]
    fn queue_is_trimmed_when_the_sender_runs_ahead() {
        let (queued, stats) = feed(&(0..20).collect::<Vec<_>>());
        assert!(queued <= MAX_QUEUED, "queue grew to {queued}");
        assert_eq!(
            stats.queued_ms as usize,
            queued * 1000 / SAMPLE_RATE as usize
        );
    }

    #[test]
    fn drift_correction_moves_one_sample_toward_the_target() {
        let level = |len: usize| {
            let mut queue: VecDeque<f32> = (0..len).map(|i| i as f32).collect();
            correct_drift(&mut queue);
            queue.len()
        };
        let (high, low) = (TARGET_QUEUED + DRIFT_BAND, TARGET_QUEUED - DRIFT_BAND);
        assert_eq!(level(high + 1), high, "fast sender loses one sample");
        assert_eq!(level(low - 1), low, "slow sender gains one sample");
        assert_eq!(
            level(TARGET_QUEUED),
            TARGET_QUEUED,
            "inside the band nothing changes"
        );
        assert_eq!(level(0), 0, "underrun stays silent");
    }
}
