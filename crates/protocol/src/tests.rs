use super::*;
use std::path::PathBuf;

fn samples() -> Vec<(&'static str, Message)> {
    vec![
        (
            "hello",
            Message::Hello(Hello {
                proto_ver: PROTOCOL_VERSION,
                client_kind: ClientKind::Web,
                features: Features::PEN | Features::HOVER | Features::TILT,
                screen_w: 2560,
                screen_h: 1600,
                dpi: 274,
                codecs: vec!["avc1.42E01F".into(), "avc1.42E033".into()],
            }),
        ),
        (
            "pen",
            Message::Pen(Pen {
                x: 32768,
                y: 1000,
                pressure: 40000,
                tilt_x: -30,
                tilt_y: 45,
                tool: Tool::Eraser,
                buttons: buttons::BARREL,
                contact: true,
                in_range: true,
            }),
        ),
        ("ping", Message::Ping { t: 0x0102_0304_0506_0708 }),
        ("request_keyframe", Message::RequestKeyframe),
        ("ack", Message::Ack { pts_us: 123_456_789 }),
        (
            "stream_config",
            Message::StreamConfig(StreamConfig {
                width: 1920,
                height: 1200,
                codec: "avc1.42E033".into(),
            }),
        ),
        (
            "video",
            Message::Video(Video {
                pts_us: 16_667,
                keyframe: true,
                data: vec![0, 0, 0, 1, 0x67, 0x42],
            }),
        ),
        ("pong", Message::Pong { t: 42 }),
    ]
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(format!("{name}.hex"))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn round_trip() {
    for (name, msg) in samples() {
        assert_eq!(Message::decode(&msg.encode()).as_ref(), Ok(&msg), "{name}");
    }
}

/// Byte layout must match the committed fixtures, which the web (and later
/// native) client tests decode as well. Regenerate with `TABULA_BLESS=1`.
#[test]
fn golden_fixtures() {
    let bless = std::env::var_os("TABULA_BLESS").is_some();
    for (name, msg) in samples() {
        let hex = to_hex(&msg.encode());
        let path = fixture_path(name);
        if bless {
            std::fs::write(&path, format!("{hex}\n")).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (run with TABULA_BLESS=1)", path.display()));
        assert_eq!(hex, expected.trim(), "{name}");
    }
}

#[test]
fn rejects_bad_input() {
    assert_eq!(Message::decode(&[]), Err(DecodeError::Empty));
    assert_eq!(Message::decode(&[0x7f]), Err(DecodeError::UnknownTag(0x7f)));
    assert_eq!(Message::decode(&[tag::PING, 1, 2]), Err(DecodeError::Truncated));
    assert_eq!(
        Message::decode(&[tag::STREAM_CONFIG, 0, 1, 0, 1, 5, b'a']),
        Err(DecodeError::Truncated)
    );
}

#[test]
fn long_codec_string_is_truncated_on_char_boundary() {
    let codec = "é".repeat(200);
    let msg = Message::StreamConfig(StreamConfig { width: 1, height: 1, codec });
    let Ok(Message::StreamConfig(s)) = Message::decode(&msg.encode()) else {
        panic!("decode failed");
    };
    assert_eq!(s.codec.len(), 254);
}
