//! UDP wire format shared by hub and clients.
//!
//! Every datagram starts with a one-byte tag. Strings are prefixed by a one-byte length,
//! so names and tokens are limited to 255 bytes.

use anyhow::{Context, Result, bail, ensure};

/// Default UDP port of the hub.
pub const DEFAULT_PORT: u16 = 7700;

/// Largest datagram either side sends; Opus frames of 10 ms stay far below it.
pub const MAX_PACKET: usize = 1500;

const HELLO: u8 = 1;
const ACTIVATE: u8 = 2;
const QUERY: u8 = 3;
const STATUS: u8 = 4;
const AUDIO: u8 = 5;

/// One datagram, borrowing from the receive buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet<'a> {
    /// Client heartbeat: registers `name` at the sender address. Sent every second.
    Hello { token: &'a str, name: &'a str },
    /// Makes `name` the PC that owns the headset output.
    Activate { token: &'a str, name: &'a str },
    /// Asks the hub for a [`Packet::Status`] without registering.
    Query { token: &'a str },
    /// Hub state, sent in reply to every control packet and on every switch.
    Status {
        active: Option<&'a str>,
        clients: Vec<&'a str>,
    },
    /// One Opus frame. `seq` wraps and lets the receiver conceal lost frames.
    Audio { seq: u16, payload: &'a [u8] },
}

impl<'a> Packet<'a> {
    /// Serializes the packet into `out`, replacing its contents.
    ///
    /// # Errors
    /// Fails when a string is longer than 255 bytes.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        match self {
            Packet::Hello { token, name } => {
                out.push(HELLO);
                put_str(out, token)?;
                put_str(out, name)?;
            }
            Packet::Activate { token, name } => {
                out.push(ACTIVATE);
                put_str(out, token)?;
                put_str(out, name)?;
            }
            Packet::Query { token } => {
                out.push(QUERY);
                put_str(out, token)?;
            }
            Packet::Status { active, clients } => {
                out.push(STATUS);
                put_str(out, active.unwrap_or(""))?;
                ensure!(clients.len() <= usize::from(u8::MAX), "too many clients");
                out.push(clients.len() as u8);
                for client in clients {
                    put_str(out, client)?;
                }
            }
            Packet::Audio { seq, payload } => {
                out.push(AUDIO);
                out.extend_from_slice(&seq.to_be_bytes());
                out.extend_from_slice(payload);
            }
        }
        Ok(())
    }

    /// Parses one datagram.
    ///
    /// # Errors
    /// Fails on unknown tags, truncated fields, or non UTF-8 strings.
    pub fn decode(buf: &'a [u8]) -> Result<Self> {
        let (&tag, mut rest) = buf.split_first().context("empty packet")?;
        let packet = match tag {
            HELLO => Packet::Hello {
                token: take_str(&mut rest)?,
                name: take_str(&mut rest)?,
            },
            ACTIVATE => Packet::Activate {
                token: take_str(&mut rest)?,
                name: take_str(&mut rest)?,
            },
            QUERY => Packet::Query {
                token: take_str(&mut rest)?,
            },
            STATUS => {
                let active = Some(take_str(&mut rest)?).filter(|name| !name.is_empty());
                let (&count, tail) = rest.split_first().context("truncated status")?;
                rest = tail;
                let clients = (0..count)
                    .map(|_| take_str(&mut rest))
                    .collect::<Result<_>>()?;
                Packet::Status { active, clients }
            }
            AUDIO => {
                ensure!(rest.len() >= 2, "truncated audio");
                let (seq, payload) = rest.split_at(2);
                return Ok(Packet::Audio {
                    seq: u16::from_be_bytes([seq[0], seq[1]]),
                    payload,
                });
            }
            other => bail!("unknown packet tag {other}"),
        };
        ensure!(rest.is_empty(), "trailing bytes in packet tag {tag}");
        Ok(packet)
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) -> Result<()> {
    let len =
        u8::try_from(s.len()).with_context(|| format!("string too long: {} bytes", s.len()))?;
    out.push(len);
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn take_str<'a>(rest: &mut &'a [u8]) -> Result<&'a str> {
    let (&len, tail) = rest.split_first().context("truncated string length")?;
    ensure!(tail.len() >= usize::from(len), "truncated string");
    let (s, tail) = tail.split_at(usize::from(len));
    *rest = tail;
    Ok(std::str::from_utf8(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_packet_round_trips() {
        let packets = [
            Packet::Hello {
                token: "t0k",
                name: "mac",
            },
            Packet::Activate {
                token: "t0k",
                name: "linux-1",
            },
            Packet::Query { token: "" },
            Packet::Status {
                active: Some("mac"),
                clients: vec!["mac", "linux-1"],
            },
            Packet::Status {
                active: None,
                clients: vec![],
            },
            Packet::Audio {
                seq: 65535,
                payload: &[1, 2, 3],
            },
            Packet::Audio {
                seq: 0,
                payload: &[],
            },
        ];
        let mut buf = Vec::new();
        for packet in packets {
            packet.encode(&mut buf).expect("encode");
            assert_eq!(Packet::decode(&buf).expect("decode"), packet);
        }
    }

    #[test]
    fn rejects_malformed_packets() {
        for bad in [
            &[][..],
            &[9],
            &[HELLO, 5, b'a'],
            &[QUERY, 0, 0],
            &[AUDIO, 1],
        ] {
            assert!(Packet::decode(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn rejects_names_over_255_bytes() {
        let name = "x".repeat(256);
        let err = Packet::Hello {
            token: "",
            name: &name,
        }
        .encode(&mut Vec::new());
        assert!(err.is_err());
    }
}
