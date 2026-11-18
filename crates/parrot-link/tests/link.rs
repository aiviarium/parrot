//! Handshakes, transport and pairing over in-memory streams, with fixed made-up keys.

use parrot_link::keys::{PinSet, Pins};
use parrot_link::noise::{Dialer, Listener, PAIRING_PATTERN, PATTERN};
use parrot_link::pairing::{InviteBook, InviteCode, Side};
use parrot_link::{LinkError, PublicKey, StaticKey, runtime_id};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn key(seed: u8) -> Arc<StaticKey> {
    Arc::new(StaticKey::from_private([seed; 32]))
}

struct Pair {
    a: Arc<StaticKey>,
    b: Arc<StaticKey>,
    b_pins: Arc<PinSet>,
    b_invites: Arc<InviteBook>,
    now: Arc<AtomicU64>,
}

impl Pair {
    fn new() -> Pair {
        Pair {
            a: key(0x0a),
            b: key(0x0b),
            b_pins: Arc::new(PinSet::new()),
            b_invites: Arc::new(InviteBook::new()),
            now: Arc::new(AtomicU64::new(1_000_000)),
        }
    }

    fn listener(&self) -> Listener {
        let now = self.now.clone();
        Listener::new(self.b.clone(), self.b_pins.clone(), self.b_invites.clone())
            .clock(Arc::new(move || now.load(Ordering::SeqCst)))
            .timeout(Duration::from_secs(5))
    }
}

#[test]
fn x25519_matches_rfc_7748() {
    let private: [u8; 32] = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let public = StaticKey::from_private(private).public();
    assert_eq!(
        public.to_hex(),
        "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
    );
}

#[test]
fn runtime_id_is_derived_from_the_key() {
    assert_eq!(
        runtime_id(&PublicKey([0x11; 32])),
        "rt_0BA4K8RZQCK7S3SN5TCPH9WY7S"
    );
    let counting: [u8; 32] = std::array::from_fn(|i| i as u8);
    assert_eq!(runtime_id(&PublicKey(counting)), "rt_CC6WTAB6RGSPD48JAH4BQCJV9Z");
    let id = key(1).runtime_id();
    assert_eq!(id.len(), 29);
    assert_ne!(id, key(2).runtime_id());
}

#[test]
fn patterns_are_the_spec_suite() {
    assert_eq!(PATTERN, "Noise_XX_25519_ChaChaPoly_BLAKE2s");
    assert_eq!(PAIRING_PATTERN, "Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s");
}

#[tokio::test]
async fn pinned_handshake_and_transport() {
    let p = Pair::new();
    p.b_pins.pin(p.a.public());
    let (ca, cb) = tokio::io::duplex(1 << 16);
    let listener = p.listener();
    let dialer = Dialer::new(p.a.clone());
    let b_public = p.b.public();
    let (a, b) = tokio::join!(dialer.dial(ca, &b_public), listener.accept(cb));
    let (mut a, mut b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.peer.runtime, p.b.runtime_id());
    assert_eq!(b.peer.runtime, p.a.runtime_id());
    assert_eq!(b.peer.key, p.a.public());
    assert_eq!((a.side, b.side), (Side::Dialled, Side::Accepted));
    assert_eq!(b.paired, None);

    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    a.send_json(&init).await.unwrap();
    assert_eq!(b.recv_json().await.unwrap(), Some(init));
    b.send(b"").await.unwrap();
    assert_eq!(a.recv().await.unwrap(), Some(vec![]));

    // Each direction runs on its own half.
    let (mut ar, mut aw) = (a.reader, a.writer);
    let writer = tokio::spawn(async move {
        for i in 0..50u32 {
            aw.send(format!("{{\"n\":{i}}}").as_bytes()).await.unwrap();
        }
        aw.shutdown().await.unwrap();
    });
    for i in 0..50u32 {
        assert_eq!(b.recv_json().await.unwrap(), Some(json!({ "n": i })));
    }
    assert_eq!(b.recv().await.unwrap(), None, "clean close");
    writer.await.unwrap();
    b.send(b"late").await.unwrap();
    assert_eq!(ar.recv().await.unwrap().as_deref(), Some(&b"late"[..]));
}

#[tokio::test]
async fn large_messages_are_split_and_joined() {
    let p = Pair::new();
    p.b_pins.pin(p.a.public());
    let (ca, cb) = tokio::io::duplex(1 << 20);
    let listener = p.listener();
    let b_public = p.b.public();
    let dialer = Dialer::new(p.a.clone());
    let (a, b) = tokio::join!(dialer.dial(ca, &b_public), listener.accept(cb));
    let (mut a, mut b) = (a.unwrap(), b.unwrap());
    let big = json!({ "blob": "x".repeat(3 * 1024 * 1024 + 17) });
    let send = tokio::spawn(async move {
        a.send_json(&big).await.unwrap();
        let too_big = vec![b' '; parrot_link::frame::MAX_MESSAGE + 1];
        assert!(matches!(a.send(&too_big).await, Err(LinkError::TooLarge(_))));
        big
    });
    let got = b.recv_json().await.unwrap().unwrap();
    assert_eq!(got, send.await.unwrap());
}

#[tokio::test]
async fn unpinned_dialler_is_refused() {
    let p = Pair::new();
    let (ca, cb) = tokio::io::duplex(1 << 16);
    let listener = p.listener();
    let b_public = p.b.public();
    let dialer = Dialer::new(p.a.clone());
    let (a, b) = tokio::join!(dialer.dial(ca, &b_public), listener.accept(cb));
    let err = b.err().expect("the listener refuses an unpinned key");
    assert_eq!(err.reason(), "key", "{err}");
    let mut a = a.expect("the dialler finished its side");
    assert!(a.recv().await.unwrap_or(None).is_none(), "the listener closed");
}

#[tokio::test]
async fn wrong_responder_key_is_refused_before_ours_is_sent() {
    let p = Pair::new();
    p.b_pins.pin(p.a.public());
    let (ca, cb) = tokio::io::duplex(1 << 16);
    let listener = p.listener();
    let someone_else = key(0x0c).public();
    let dialer = Dialer::new(p.a.clone());
    let (a, b) = tokio::join!(dialer.dial(ca, &someone_else), listener.accept(cb));
    let err = a.err().expect("the dialler refuses the wrong key");
    match &err {
        LinkError::Key { got } => assert_eq!(got, &p.b.runtime_id()),
        other => panic!("{other}"),
    }
    assert_eq!(err.data()["reason"], "key");
    // The dialler stopped before message 3, so the listener never learnt its key.
    assert!(matches!(b, Err(LinkError::Protocol(_)) | Err(LinkError::Io(_))));
}

#[tokio::test]
async fn pairing_pins_both_keys() {
    let p = Pair::new();
    let code = p
        .b_invites
        .create(p.b.public(), vec![], 600_000, p.now.load(Ordering::SeqCst));
    let text = code.encode();
    let parsed = InviteCode::parse(&text).unwrap();
    assert_eq!(parsed.runtime(), p.b.runtime_id());

    let (ca, cb) = tokio::io::duplex(1 << 16);
    let listener = p.listener();
    let dialer = Dialer::new(p.a.clone());
    let (a, b) = tokio::join!(dialer.pair(ca, &parsed), listener.accept(cb));
    let (mut a, mut b) = (a.unwrap(), b.unwrap());
    assert_eq!(b.paired.as_deref(), Some(code.invite.as_str()));
    assert_eq!(a.peer.key, p.b.public());
    // The inviter pins what it learnt; the next connection runs plain XX.
    p.b_pins.pin(b.peer.key);
    a.send(b"hi").await.unwrap();
    assert_eq!(b.recv().await.unwrap().as_deref(), Some(&b"hi"[..]));

    let (ca, cb) = tokio::io::duplex(1 << 16);
    let (a2, b2) = tokio::join!(dialer.dial(ca, &a.peer.key), listener.accept(cb));
    assert!(a2.is_ok() && b2.is_ok());

    // The code worked once.
    let (ca, cb) = tokio::io::duplex(1 << 16);
    let (_, b3) = tokio::join!(dialer.pair(ca, &parsed), listener.accept(cb));
    assert_eq!(b3.err().unwrap().reason(), "pairing");
}

#[tokio::test]
async fn expired_code_is_refused() {
    let p = Pair::new();
    let code = p
        .b_invites
        .create(p.b.public(), vec![], 600_000, p.now.load(Ordering::SeqCst));
    p.now.fetch_add(600_001, Ordering::SeqCst);
    let (ca, cb) = tokio::io::duplex(1 << 16);
    let listener = p.listener();
    let dialer = Dialer::new(p.a.clone());
    let (a, b) = tokio::join!(dialer.pair(ca, &code), listener.accept(cb));
    assert_eq!(b.err().unwrap().reason(), "pairing");
    assert!(a.is_err());
    assert!(!p.b_pins.is_pinned(&p.a.public()));
}

#[tokio::test]
async fn wrong_secret_fails_the_handshake() {
    let p = Pair::new();
    let code = p
        .b_invites
        .create(p.b.public(), vec![], 600_000, p.now.load(Ordering::SeqCst));
    let mut forged = code.clone();
    forged.secret = parrot_link::pairing::Secret::from_bytes([0x5a; 32]);
    let (ca, cb) = tokio::io::duplex(1 << 16);
    let listener = p.listener();
    let dialer = Dialer::new(p.a.clone());
    let (_, b) = tokio::join!(dialer.pair(ca, &forged), listener.accept(cb));
    assert!(matches!(b, Err(LinkError::Noise(_))), "{:?}", b.err());
    // A failed attempt doesn't use up the code.
    let (ca, cb) = tokio::io::duplex(1 << 16);
    let (a, b) = tokio::join!(dialer.pair(ca, &code), listener.accept(cb));
    assert!(a.is_ok() && b.is_ok());
}

#[tokio::test]
async fn silent_candidate_times_out() {
    let p = Pair::new();
    let (_ca, cb) = tokio::io::duplex(1 << 16);
    let listener = p.listener().timeout(Duration::from_millis(100));
    assert!(matches!(listener.accept(cb).await, Err(LinkError::Timeout)));
}

#[tokio::test]
async fn tampered_hello_breaks_the_handshake() {
    // A carrier that rewrites the hello (here: drops the invite to downgrade to plain XX) is
    // caught because the hello is in the prologue.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let p = Pair::new();
    p.b_pins.pin(p.a.public());
    let code = p
        .b_invites
        .create(p.b.public(), vec![], 600_000, p.now.load(Ordering::SeqCst));
    let (ca, mut mid_a) = tokio::io::duplex(1 << 16);
    let (mut mid_b, cb) = tokio::io::duplex(1 << 16);
    let relay = tokio::spawn(async move {
        let mut len = [0u8; 2];
        mid_a.read_exact(&mut len).await.unwrap();
        let mut hello = vec![0; u16::from_be_bytes(len) as usize];
        mid_a.read_exact(&mut hello).await.unwrap();
        let fake = br#"{"parrot":"link","version":1,"mode":"xx"}"#;
        mid_b.write_all(&(fake.len() as u16).to_be_bytes()).await.unwrap();
        mid_b.write_all(fake).await.unwrap();
        let (mut ar, mut aw) = tokio::io::split(mid_a);
        let (mut br, mut bw) = tokio::io::split(mid_b);
        let _ = tokio::join!(
            tokio::io::copy(&mut ar, &mut bw),
            tokio::io::copy(&mut br, &mut aw)
        );
    });
    let listener = p.listener().timeout(Duration::from_secs(1));
    let dialer = Dialer::new(p.a.clone());
    let (_, b) = tokio::join!(dialer.pair(ca, &code), listener.accept(cb));
    assert!(b.is_err());
    relay.abort();
}

#[test]
fn key_file_is_private_and_stable() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let first = StaticKey::load_or_create(&state).unwrap();
    let second = StaticKey::load_or_create(&state).unwrap();
    assert_eq!(first.public(), second.public());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = state.join(parrot_link::keys::KEY_FILE);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = StaticKey::load(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(!err.to_string().contains(&first.public().to_hex()));
    }
}

fn hex<const N: usize>(text: &str) -> [u8; N] {
    std::array::from_fn(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
}

#[cfg(unix)]
fn sh_egg(name: &str, script: &str) -> parrot_link::carrier::EggCarrier {
    parrot_link::carrier::EggCarrier {
        name: name.into(),
        title: "Test egg".into(),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into(), name.into()],
        target_schema: json!({ "type": "object" }),
        listens: false,
        max_message: 8 * 1024,
        containment: None,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn carrier_egg_answers_ready_then_carries_bytes() {
    use parrot_link::carrier::{CarrierRef, Carriers, Containment};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut carriers = Carriers::new();
    // Checks it was asked to dial and got the target, then echoes the channel.
    carriers
        .add_egg(sh_egg(
            "echo-egg",
            r#"[ "$1" = dial ] || exit 3; read -r target; case "$target" in *box-7*) ;; *) exit 4;; esac; echo '{"ready":true}'; exec cat"#,
        ))
        .unwrap();
    carriers
        .add_egg(sh_egg(
            "down-egg",
            r#"read -r target; echo '{"error":"no route to box-7"}'"#,
        ))
        .unwrap();
    let r: CarrierRef =
        serde_json::from_value(json!({ "kind": "echo-egg", "target": { "device": "box-7" } })).unwrap();
    let mut ch = carriers.open(&r).await.unwrap();
    assert_eq!(ch.containment, Containment::Unknown);
    assert_eq!(ch.max_message, 8 * 1024);
    ch.stream.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    ch.stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping");

    let down: CarrierRef = serde_json::from_value(json!({ "kind": "down-egg", "target": {} })).unwrap();
    let err = carriers.open(&down).await.err().unwrap();
    assert!(err.to_string().contains("no route"), "{err}");

    // Fallbacks are tried in order.
    let with_fallback = CarrierRef {
        fallback: vec![r.clone()],
        ..down
    };
    let (index, _) = carriers.open_with_fallback(&with_fallback).await.unwrap();
    assert_eq!(index, 1);
}
