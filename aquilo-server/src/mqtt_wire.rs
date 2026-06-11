//! Minimal MQTT 3.x wire codec — just enough to onboard a device.
//!
//! The onboarding modes need the device's *raw* bytes. `learn` tees the proxied
//! stream; `observe` is a tiny stand-in broker. Neither can use the `rumqttc`
//! client API, because the facts we must recover — the CONNECT clientId, username
//! and password — live in the raw CONNECT packet, which a normal client never
//! surfaces. So we decode the wire format directly here.
//!
//! This is deliberately partial: it understands the control packets the Aquilo
//! device actually uses (CONNECT, PUBLISH, SUBSCRIBE, PINGREQ, DISCONNECT) and can
//! encode the handful of replies a broker owes it. It is not a general MQTT stack.
//! CONNECT parsing is version-agnostic (the protocol-name string and level are
//! skipped), so it handles both 3.1 and 3.1.1 clients.

/// A decoded control packet. Packets we frame but don't interpret surface as
/// [`Packet::Other`] so the stream stays in sync.
#[derive(Clone, Debug, PartialEq)]
pub enum Packet {
    Connect(Connect),
    Publish(Publish),
    Subscribe(Subscribe),
    PingReq,
    Disconnect,
    Other { kind: u8 },
}

/// The fields we recover from a CONNECT: the device's identity and credentials.
#[derive(Clone, Debug, PartialEq)]
pub struct Connect {
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Publish {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
    pub retain: bool,
    /// Present only for QoS > 0, where it must be echoed in the PUBACK.
    pub packet_id: Option<u16>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Subscribe {
    pub packet_id: u16,
    pub topics: Vec<String>,
}

/// Streaming framer: bytes in, complete packets out. A partial trailing packet is
/// buffered until the rest of its bytes arrive.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

enum Frame {
    /// A fully framed packet of `total` bytes; `None` when the frame parsed but we
    /// don't model that packet type's body.
    Packet(Option<Packet>, usize),
    /// Not enough bytes yet for a full packet.
    Incomplete,
    /// A malformed length field; the buffer is unrecoverable and gets dropped.
    Corrupt,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds raw bytes and returns every packet that is now complete.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Packet> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            match next_frame(&self.buf) {
                Frame::Packet(pkt, total) => {
                    if let Some(p) = pkt {
                        out.push(p);
                    }
                    self.buf.drain(0..total);
                }
                Frame::Incomplete => break,
                Frame::Corrupt => {
                    self.buf.clear();
                    break;
                }
            }
        }
        out
    }
}

fn next_frame(buf: &[u8]) -> Frame {
    if buf.len() < 2 {
        return Frame::Incomplete;
    }
    let kind = buf[0] >> 4;
    let flags = buf[0] & 0x0f;

    // Remaining-length varint (1–4 bytes), starting at byte 1.
    let mut mult = 1usize;
    let mut len = 0usize;
    let mut i = 1usize;
    loop {
        let Some(&b) = buf.get(i) else {
            return Frame::Incomplete;
        };
        len += (b & 0x7f) as usize * mult;
        mult *= 128;
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
        if i >= 5 {
            return Frame::Corrupt; // varint must terminate within 4 bytes
        }
    }

    let total = i + len;
    if buf.len() < total {
        return Frame::Incomplete;
    }
    let body = &buf[i..total];
    Frame::Packet(decode_body(kind, flags, body), total)
}

fn decode_body(kind: u8, flags: u8, body: &[u8]) -> Option<Packet> {
    match kind {
        1 => decode_connect(body),
        3 => decode_publish(flags, body),
        8 => decode_subscribe(body),
        12 => Some(Packet::PingReq),
        14 => Some(Packet::Disconnect),
        other => Some(Packet::Other { kind: other }),
    }
}

fn decode_connect(body: &[u8]) -> Option<Packet> {
    let mut off = 0;
    read_bytes(body, &mut off)?; // protocol name ("MQTT" / "MQIsdp")
    let _level = *body.get(off)?;
    off += 1;
    let flags = *body.get(off)?;
    off += 1;
    off += 2; // keep-alive
    if off > body.len() {
        return None;
    }

    let client_id = read_str(body, &mut off)?;
    if flags & 0x04 != 0 {
        read_bytes(body, &mut off)?; // will topic
        read_bytes(body, &mut off)?; // will message
    }
    let username = if flags & 0x80 != 0 {
        Some(read_str(body, &mut off)?)
    } else {
        None
    };
    let password = if flags & 0x40 != 0 {
        Some(String::from_utf8_lossy(read_bytes(body, &mut off)?).into_owned())
    } else {
        None
    };

    Some(Packet::Connect(Connect {
        client_id,
        username,
        password,
    }))
}

fn decode_publish(flags: u8, body: &[u8]) -> Option<Packet> {
    let qos = (flags >> 1) & 0x03;
    let retain = flags & 0x01 != 0;
    let mut off = 0;
    let topic = read_str(body, &mut off)?;
    let packet_id = if qos > 0 {
        let hi = *body.get(off)? as u16;
        let lo = *body.get(off + 1)? as u16;
        off += 2;
        Some((hi << 8) | lo)
    } else {
        None
    };
    let payload = body.get(off..)?.to_vec();
    Some(Packet::Publish(Publish {
        topic,
        payload,
        qos,
        retain,
        packet_id,
    }))
}

fn decode_subscribe(body: &[u8]) -> Option<Packet> {
    let mut off = 0;
    let hi = *body.get(off)? as u16;
    let lo = *body.get(off + 1)? as u16;
    off += 2;
    let packet_id = (hi << 8) | lo;

    let mut topics = Vec::new();
    while off < body.len() {
        let topic = read_str(body, &mut off)?;
        off += 1; // per-topic requested QoS byte
        topics.push(topic);
    }
    Some(Packet::Subscribe(Subscribe { packet_id, topics }))
}

/// Reads a 2-byte-length-prefixed byte string, advancing `off` past it.
fn read_bytes<'a>(b: &'a [u8], off: &mut usize) -> Option<&'a [u8]> {
    let hi = *b.get(*off)? as usize;
    let lo = *b.get(*off + 1)? as usize;
    let len = (hi << 8) | lo;
    let start = *off + 2;
    let end = start + len;
    if end > b.len() {
        return None;
    }
    *off = end;
    Some(&b[start..end])
}

fn read_str(b: &[u8], off: &mut usize) -> Option<String> {
    Some(String::from_utf8_lossy(read_bytes(b, off)?).into_owned())
}

// --- encoders: the replies the `observe` stand-in broker owes the device ---

/// CONNACK with return code 0x00 (connection accepted), no session present.
pub fn connack() -> Vec<u8> {
    vec![0x20, 0x02, 0x00, 0x00]
}

/// SUBACK granting QoS 0 for each of `count` subscribed topics.
pub fn suback(packet_id: u16, count: usize) -> Vec<u8> {
    let mut payload = vec![(packet_id >> 8) as u8, packet_id as u8];
    payload.resize(payload.len() + count, 0x00);
    frame(0x90, &payload)
}

pub fn puback(packet_id: u16) -> Vec<u8> {
    vec![0x40, 0x02, (packet_id >> 8) as u8, packet_id as u8]
}

pub fn pingresp() -> Vec<u8> {
    vec![0xd0, 0x00]
}

/// A QoS 0 PUBLISH (no packet id), optionally with the retain bit set.
pub fn publish(topic: &str, payload: &[u8], retain: bool) -> Vec<u8> {
    let header = 0x30 | if retain { 0x01 } else { 0x00 };
    let tb = topic.as_bytes();
    let mut body = Vec::with_capacity(2 + tb.len() + payload.len());
    body.push((tb.len() >> 8) as u8);
    body.push(tb.len() as u8);
    body.extend_from_slice(tb);
    body.extend_from_slice(payload);
    frame(header, &body)
}

/// Prepends the fixed header byte and remaining-length varint to a packet body.
fn frame(header: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![header];
    let mut len = body.len();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a CONNECT packet the way an MQTT 3.1.1 client would, so the decoder
    /// is tested against the real on-wire layout rather than its own assumptions.
    fn connect_bytes(client_id: &str, user: &str, pass: &str) -> Vec<u8> {
        let mut body = Vec::new();
        let str_field = |b: &mut Vec<u8>, s: &str| {
            b.push((s.len() >> 8) as u8);
            b.push(s.len() as u8);
            b.extend_from_slice(s.as_bytes());
        };
        str_field(&mut body, "MQTT");
        body.push(0x04); // level 4 (3.1.1)
        body.push(0xc2); // flags: username + password + clean session
        body.extend_from_slice(&[0x00, 0x3c]); // keep-alive 60s
        str_field(&mut body, client_id);
        str_field(&mut body, user);
        str_field(&mut body, pass);
        frame(0x10, &body)
    }

    #[test]
    fn decodes_a_connect_with_credentials() {
        let bytes = connect_bytes("CieczSensorae83fc", "ae83fc", "48007129");
        let mut dec = Decoder::new();
        let packets = dec.push(&bytes);
        assert_eq!(
            packets,
            vec![Packet::Connect(Connect {
                client_id: "CieczSensorae83fc".into(),
                username: Some("ae83fc".into()),
                password: Some("48007129".into()),
            })]
        );
    }

    #[test]
    fn decodes_a_retained_publish() {
        let bytes = publish("/users/ae83fc/state", br#"{"sensors":[]}"#, true);
        let mut dec = Decoder::new();
        let packets = dec.push(&bytes);
        assert_eq!(
            packets,
            vec![Packet::Publish(Publish {
                topic: "/users/ae83fc/state".into(),
                payload: br#"{"sensors":[]}"#.to_vec(),
                qos: 0,
                retain: true,
                packet_id: None,
            })]
        );
    }

    #[test]
    fn decodes_a_subscribe_with_several_topics() {
        // SUBSCRIBE: header 0x82, packet id 1, two topics each followed by a QoS byte.
        let mut body = vec![0x00, 0x01];
        for t in ["/ping", "/users/ae83fc/state"] {
            body.push((t.len() >> 8) as u8);
            body.push(t.len() as u8);
            body.extend_from_slice(t.as_bytes());
            body.push(0x00);
        }
        let bytes = frame(0x82, &body);
        let mut dec = Decoder::new();
        assert_eq!(
            dec.push(&bytes),
            vec![Packet::Subscribe(Subscribe {
                packet_id: 1,
                topics: vec!["/ping".into(), "/users/ae83fc/state".into()],
            })]
        );
    }

    #[test]
    fn reassembles_packets_split_across_chunks() {
        let bytes = connect_bytes("CieczSensorae83fc", "ae83fc", "48007129");
        let (a, b) = bytes.split_at(5);
        let mut dec = Decoder::new();
        assert!(dec.push(a).is_empty(), "partial packet yields nothing yet");
        let packets = dec.push(b);
        assert_eq!(packets.len(), 1);
        assert!(matches!(packets[0], Packet::Connect(_)));
    }

    #[test]
    fn splits_multiple_packets_in_one_chunk() {
        let mut bytes = publish("/a", b"1", false);
        bytes.extend(pingresp()); // any second packet; decodes as PingReq's cousin
        bytes.extend(publish("/b", b"2", false));
        let mut dec = Decoder::new();
        let packets = dec.push(&bytes);
        assert_eq!(packets.len(), 3);
        assert!(matches!(&packets[0], Packet::Publish(p) if p.topic == "/a"));
        assert!(matches!(&packets[2], Packet::Publish(p) if p.topic == "/b"));
    }

    #[test]
    fn ping_and_disconnect_decode_to_their_variants() {
        let mut dec = Decoder::new();
        assert_eq!(dec.push(&[0xc0, 0x00]), vec![Packet::PingReq]);
        assert_eq!(dec.push(&[0xe0, 0x00]), vec![Packet::Disconnect]);
    }

    #[test]
    fn suback_grants_one_code_per_topic() {
        assert_eq!(suback(7, 3), vec![0x90, 0x05, 0x00, 0x07, 0x00, 0x00, 0x00]);
    }
}
