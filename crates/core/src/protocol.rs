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
    Hello {
        /// Pairing token shared by hub and clients.
        token: &'a str,
        /// Non-empty PC name.
        name: &'a str,
    },
    /// Makes `name` the PC that owns the headset output.
    Activate {
        /// Pairing token shared by hub and clients.
        token: &'a str,
        /// Non-empty PC name; it may not be connected yet.
        name: &'a str,
    },
    /// Asks the hub for a [`Packet::Status`] without registering.
    Query {
        /// Pairing token shared by hub and clients.
        token: &'a str,
    },
    /// Hub state, sent in reply to every control packet and on every switch.
    Status {
        /// PC that owns the headset output, if any.
        active: Option<&'a str>,
        /// Connected PCs, sorted by name.
        clients: Vec<&'a str>,
    },
    /// One Opus frame.
    Audio {
        /// Wrapping sequence number that lets the receiver conceal lost frames.
        seq: u16,
        /// Opus packet for one 10 ms mono frame.
        payload: &'a [u8],
    },
}

impl<'a> Packet<'a> {
    /// Serializes the packet into `out`, replacing its contents.
    ///
    /// # Errors
    /// Fails when a string is longer than 255 bytes, a PC name is empty, or there are more
    /// than 255 clients.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        match self {
            Packet::Hello { token, name } => {
                out.push(HELLO);
                put_str(out, token)?;
                put_name(out, name)?;
            }
            Packet::Activate { token, name } => {
                out.push(ACTIVATE);
                put_str(out, token)?;
                put_name(out, name)?;
            }
            Packet::Query { token } => {
                out.push(QUERY);
                put_str(out, token)?;
            }
            Packet::Status { active, clients } => {
                out.push(STATUS);
                // An empty name on the wire means "no active PC", so real names are never empty.
                match active {
                    Some(name) => put_name(out, name)?,
                    None => put_str(out, "")?,
                }
                let count = u8::try_from(clients.len())
                    .with_context(|| format!("too many clients: {}", clients.len()))?;
                out.push(count);
                for client in clients {
                    put_name(out, client)?;
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
                name: take_name(&mut rest)?,
            },
            ACTIVATE => Packet::Activate {
                token: take_str(&mut rest)?,
                name: take_name(&mut rest)?,
            },
            QUERY => Packet::Query {
                token: take_str(&mut rest)?,
            },
            STATUS => {
                let active = Some(take_str(&mut rest)?).filter(|name| !name.is_empty());
                let (&count, tail) = rest.split_first().context("truncated status")?;
                rest = tail;
                let clients = (0..count)
                    .map(|_| take_name(&mut rest))
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

/// PC names are never empty: an empty string means "no active PC" in [`Packet::Status`].
fn put_name(out: &mut Vec<u8>, name: &str) -> Result<()> {
    ensure!(!name.is_empty(), "empty PC name");
    put_str(out, name)
}

fn take_name<'a>(rest: &mut &'a [u8]) -> Result<&'a str> {
    let name = take_str(rest)?;
    ensure!(!name.is_empty(), "empty PC name");
    Ok(name)
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
            &[HELLO, 0, 0],
            &[ACTIVATE, 1, b't', 0],
            &[STATUS, 0, 1, 0],
            &[QUERY, 0, 0],
            &[AUDIO, 1],
        ] {
            assert!(Packet::decode(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn refuses_to_encode_empty_names() {
        let packets = [
            Packet::Hello {
                token: "t",
                name: "",
            },
            Packet::Status {
                active: Some(""),
                clients: vec![],
            },
            Packet::Status {
                active: None,
                clients: vec![""],
            },
        ];
        for packet in packets {
            assert!(
                packet.encode(&mut Vec::new()).is_err(),
                "encoded {packet:?}"
            );
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
