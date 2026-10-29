//! CONTROL payload vocabulary (additive v1 extension — packet layout and
//! payload types are unchanged).
//!
//! The `Control` payload type carries small UTF-8 JSON messages. This module
//! defines the wire vocabulary both ends speak, with forward compatibility
//! built in: unknown `t` tags and unknown fields fail parsing to `None`
//! instead of erroring the stream, and every message is optional — a peer
//! that never sends control traffic works exactly as before.
//!
//! | Message     | Direction      | Meaning                                        |
//! |-------------|----------------|------------------------------------------------|
//! | `hello`     | sender→receiver| First packet of a session; sender capabilities |
//! | `hello-ack` | receiver→sender| Receiver capabilities, answer to `hello`       |
//! | `ping`      | receiver→sender| Liveness probe (receiver emits every 1 s)      |
//! | `pong`      | sender→receiver| Answer to `ping`; also refreshes receiver liveness |
//! | `bye`       | either         | The sender is ending the session               |
//!
//! Control packets share the audio sequence space of their direction (one
//! counter per direction across all payload types, per the canonical spec).
//! Any received packet — audio or control — proves the peer is alive.

use serde::{Deserialize, Serialize};

/// Protocol version announced in `hello` / `hello-ack`. Stays 1 for the
/// frozen v1 packet format; a future format change raises it so peers can
/// detect the mismatch instead of misparsing audio.
pub const CONTROL_PROTO: u8 = 1;

/// Upper bound for a CONTROL payload we are willing to parse. Our vocabulary
/// is a handful of small objects; anything larger is treated as not-a-control
/// message (and still counts as received traffic).
const MAX_CONTROL_PAYLOAD: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum ControlMsg {
    /// First packet a sender transmits on a new session. `codecs` lists the
    /// payload types the sender can produce; `rate` is the sample rate.
    Hello {
        app: String,
        proto: u8,
        #[serde(default)]
        codecs: Vec<String>,
        #[serde(default)]
        rate: u32,
    },
    /// The receiver's answer to `hello`, with its own capabilities.
    HelloAck {
        app: String,
        proto: u8,
        #[serde(default)]
        codecs: Vec<String>,
        #[serde(default)]
        rate: u32,
    },
    /// Liveness probe from the receiver.
    Ping,
    /// Answer to a `ping`.
    Pong,
    /// The sender is ending the session (receiver may log it; never fatal).
    Bye,
}

/// Parse a CONTROL payload. Returns `None` for anything that is not a
/// control message from this vocabulary (foreign/older/newer JSON, trailing
/// garbage, non-UTF-8, oversized) — callers count the packet as received and
/// move on; a control message is never fatal.
pub fn parse_control(payload: &[u8]) -> Option<ControlMsg> {
    if payload.len() > MAX_CONTROL_PAYLOAD {
        return None;
    }
    serde_json::from_slice(payload).ok()
}

/// Serialize a control message into payload bytes.
pub fn control_payload(msg: &ControlMsg) -> Vec<u8> {
    serde_json::to_vec(msg).expect("ControlMsg serialization cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_is_exact_two_field_json() {
        let wire = control_payload(&ControlMsg::Ping);
        assert_eq!(wire, b"{\"t\":\"ping\"}");
        assert_eq!(parse_control(&wire), Some(ControlMsg::Ping));
    }

    #[test]
    fn pong_bye_roundtrip() {
        assert_eq!(parse_control(b"{\"t\":\"pong\"}"), Some(ControlMsg::Pong));
        assert_eq!(
            parse_control(&control_payload(&ControlMsg::Bye)),
            Some(ControlMsg::Bye)
        );
        assert_eq!(control_payload(&ControlMsg::Pong), b"{\"t\":\"pong\"}");
    }

    #[test]
    fn hello_full_roundtrip() {
        let msg = ControlMsg::Hello {
            app: "openay-mic".into(),
            proto: CONTROL_PROTO,
            codecs: vec!["pcm".into(), "opus".into()],
            rate: 48_000,
        };
        let wire = control_payload(&msg);
        let s = std::str::from_utf8(&wire).unwrap();
        assert!(s.starts_with("{\"t\":\"hello\""), "got {s}");
        assert!(s.contains("\"app\":\"openay-mic\""));
        assert!(s.contains("\"proto\":1"));
        assert!(s.contains("\"codecs\":[\"pcm\",\"opus\"]"));
        assert!(s.contains("\"rate\":48000"));
        assert_eq!(parse_control(&wire), Some(msg));
    }

    #[test]
    fn hello_optional_fields_default() {
        // An older/leaner sender that omits codecs and rate still parses.
        let parsed = parse_control(b"{\"t\":\"hello\",\"app\":\"x\",\"proto\":1}");
        assert_eq!(
            parsed,
            Some(ControlMsg::Hello {
                app: "x".into(),
                proto: 1,
                codecs: vec![],
                rate: 0,
            })
        );
    }

    #[test]
    fn hello_ack_kebab_tag() {
        let msg = ControlMsg::HelloAck {
            app: "openay-server".into(),
            proto: 1,
            codecs: vec!["opus".into()],
            rate: 48_000,
        };
        let wire = control_payload(&msg);
        let s = std::str::from_utf8(&wire).unwrap();
        assert!(s.starts_with("{\"t\":\"hello-ack\""), "got {s}");
        // app/proto are required; codecs/rate default when omitted.
        assert_eq!(
            parse_control(b"{\"t\":\"hello-ack\",\"app\":\"x\",\"proto\":1}"),
            Some(ControlMsg::HelloAck {
                app: "x".into(),
                proto: 1,
                codecs: vec![],
                rate: 0,
            })
        );
        assert_eq!(parse_control(&wire), Some(msg));
    }

    #[test]
    fn foreign_json_is_not_a_control_message() {
        // The spec's free-form example and unknown tags parse to None.
        assert_eq!(parse_control(b"{\"bye\":true}"), None);
        assert_eq!(parse_control(b"{\"t\":\"future-thing\"}"), None);
        assert_eq!(parse_control(b"not json"), None);
        assert_eq!(parse_control(b""), None);
        // Oversized payloads are rejected without parsing.
        let big = vec![b'x'; MAX_CONTROL_PAYLOAD + 1];
        assert_eq!(parse_control(&big), None);
    }
}
