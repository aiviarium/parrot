//! Noise sessions over a carrier's byte stream (spec 24.2, 24.3, E9).
//!
//! The dialling side sends a short plaintext hello naming the mode, then runs
//! `Noise_XX_25519_ChaChaPoly_BLAKE2s`, or `Noise_XXpsk3_…` with the invite's secret when pairing.
//! The hello is part of the Noise prologue, so a carrier that changes it breaks the handshake.
//! Outside pairing, each side accepts only a pinned key: the dialler checks the responder's key
//! before revealing its own, and the responder checks the dialler's against its pins.

use crate::LinkError;
use crate::frame::{self, Assembler, MAX_MESSAGE, MIN_MESSAGE, NOISE_MAX};
use crate::keys::{Pins, PublicKey, StaticKey};
use crate::pairing::{InviteCode, Invites, Secret, Side};
use serde::{Deserialize, Serialize};
use snow::{Builder, HandshakeState, TransportState};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

/// The pattern of every connection after pairing.
pub const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// The pattern of a pairing with a code.
pub const PAIRING_PATTERN: &str = "Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s";
/// How long an unauthenticated candidate may take to finish the handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

const PROLOGUE: &[u8] = b"parrot.link.1\n";
const HELLO_MAX: usize = 512;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    parrot: String,
    version: u32,
    mode: Mode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invite: Option<String>,
    /// The largest Noise message the dialer's carrier takes, when it is under 64 KiB: a
    /// message-based carrier delivers each message whole, so the accepting side keeps to it too.
    /// The hello is in the prologue, so this is authenticated with the handshake.
    #[serde(rename = "maxMessage", default, skip_serializing_if = "Option::is_none")]
    max_message: Option<usize>,
}

impl Hello {
    fn new(mode: Mode, invite: Option<String>, max_message: usize) -> Hello {
        Hello {
            parrot: "link".into(),
            version: 1,
            mode,
            invite,
            max_message: (max_message < NOISE_MAX).then_some(max_message.max(MIN_MESSAGE)),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Mode {
    /// `XX` with pinned keys.
    Xx,
    /// `XXpsk3` with an invite's secret.
    Pair,
}

/// The authenticated peer of a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub key: PublicKey,
    /// Derived from `key`, so it can't be claimed.
    pub runtime: String,
}

/// An authenticated, encrypted link connection carrying JSON-RPC messages.
pub struct LinkConnection<S> {
    pub reader: LinkReader<S>,
    pub writer: LinkWriter<S>,
    pub peer: Peer,
    pub side: Side,
    /// The invite this connection paired with; the runtime pins `peer.key` for it.
    pub paired: Option<String>,
}

impl<S: AsyncRead + AsyncWrite> LinkConnection<S> {
    pub async fn send(&mut self, msg: &[u8]) -> Result<(), LinkError> {
        self.writer.send(msg).await
    }

    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        self.reader.recv().await
    }

    pub async fn send_json(&mut self, msg: &serde_json::Value) -> Result<(), LinkError> {
        self.writer
            .send(&serde_json::to_vec(msg).expect("JSON values serialize"))
            .await
    }

    /// The next message, parsed. `None` when the peer closed the connection.
    pub async fn recv_json(&mut self) -> Result<Option<serde_json::Value>, LinkError> {
        match self.reader.recv().await? {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| LinkError::Protocol(format!("not JSON: {e}"))),
        }
    }
}

/// The receiving half of a connection.
pub struct LinkReader<S> {
    io: ReadHalf<S>,
    noise: Arc<Mutex<TransportState>>,
    asm: Assembler,
    buf: Vec<u8>,
}

impl<S: AsyncRead> LinkReader<S> {
    /// The next whole message. `None` when the peer closed between messages.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        loop {
            let Some(cipher) = frame::read_noise(&mut self.io).await? else {
                return Ok(None);
            };
            let n = self
                .noise
                .lock()
                .expect("noise lock")
                .read_message(&cipher, &mut self.buf)?;
            if let Some(msg) = self.asm.push(&self.buf[..n])? {
                return Ok(Some(msg));
            }
        }
    }
}

/// The sending half of a connection.
pub struct LinkWriter<S> {
    io: WriteHalf<S>,
    noise: Arc<Mutex<TransportState>>,
    part_len: usize,
    buf: Vec<u8>,
}

impl<S> LinkWriter<S> {
    /// The largest plaintext part one Noise message carries on this connection.
    pub fn part_len(&self) -> usize {
        self.part_len
    }
}

impl<S: AsyncWrite> LinkWriter<S> {
    /// Sends one message, split into as many Noise messages as it needs.
    pub async fn send(&mut self, msg: &[u8]) -> Result<(), LinkError> {
        if msg.len() > MAX_MESSAGE {
            return Err(LinkError::TooLarge(msg.len()));
        }
        for part in frame::split(msg, self.part_len) {
            let n = self
                .noise
                .lock()
                .expect("noise lock")
                .write_message(&part, &mut self.buf)?;
            frame::write_noise(&mut self.io, &self.buf[..n]).await?;
        }
        self.io.flush().await?;
        Ok(())
    }

    /// Closes the sending direction.
    pub async fn shutdown(&mut self) -> Result<(), LinkError> {
        self.io.shutdown().await?;
        Ok(())
    }
}

/// Dials peers: runs the initiator's side of the handshake over a carrier's stream.
pub struct Dialer {
    key: Arc<StaticKey>,
    max_message: usize,
    timeout: Duration,
}

impl Dialer {
    pub fn new(key: Arc<StaticKey>) -> Dialer {
        Dialer {
            key,
            max_message: NOISE_MAX,
            timeout: HANDSHAKE_TIMEOUT,
        }
    }

    /// The carrier's `maxMessage`, for message-based carriers.
    pub fn max_message(mut self, max_message: usize) -> Dialer {
        self.max_message = max_message;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Dialer {
        self.timeout = timeout;
        self
    }

    /// Connects to the peer whose key is pinned as `expected`. Any other key fails with
    /// [`LinkError::Key`] before this runtime reveals its own key.
    pub async fn dial<S>(&self, stream: S, expected: &PublicKey) -> Result<LinkConnection<S>, LinkError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let hello = Hello::new(Mode::Xx, None, self.max_message);
        self.run(stream, hello, expected, None).await
    }

    /// Pairs with the inviter of `code`, checking it holds the key in the code.
    pub async fn pair<S>(&self, stream: S, code: &InviteCode) -> Result<LinkConnection<S>, LinkError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let hello = Hello::new(Mode::Pair, Some(code.invite.clone()), self.max_message);
        let mut conn = self.run(stream, hello, &code.key, Some(&code.secret)).await?;
        conn.paired = Some(code.invite.clone());
        Ok(conn)
    }

    async fn run<S>(
        &self,
        stream: S,
        hello: Hello,
        expected: &PublicKey,
        psk: Option<&Secret>,
    ) -> Result<LinkConnection<S>, LinkError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let work = initiate(&self.key, stream, hello, expected, psk, self.max_message);
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| LinkError::Timeout)?
    }
}

async fn initiate<S>(
    key: &StaticKey,
    mut stream: S,
    hello: Hello,
    expected: &PublicKey,
    psk: Option<&Secret>,
    max_message: usize,
) -> Result<LinkConnection<S>, LinkError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let hello_bytes = serde_json::to_vec(&hello).expect("the hello serializes");
    let prologue = prologue(&hello_bytes);
    let mut hs = builder(key, &prologue, psk)?.build_initiator()?;
    frame::write_noise(&mut stream, &hello_bytes).await?;
    let mut buf = vec![0u8; NOISE_MAX];
    let mut payload = vec![0u8; NOISE_MAX];
    let n = hs.write_message(&[], &mut buf)?;
    frame::write_noise(&mut stream, &buf[..n]).await?;
    stream.flush().await?;

    let msg = read_handshake(&mut stream).await?;
    hs.read_message(&msg, &mut payload)?;
    let remote = remote_key(&hs)?;
    if remote != *expected {
        return Err(LinkError::Key {
            got: remote.runtime_id(),
        });
    }
    let n = hs.write_message(&[], &mut buf)?;
    frame::write_noise(&mut stream, &buf[..n]).await?;
    stream.flush().await?;
    connection(stream, hs, remote, Side::Dialled, None, max_message)
}

/// Accepts link candidates: runs the responder's side of the handshake on every stream the
/// runtime's link endpoint accepts.
pub struct Listener {
    key: Arc<StaticKey>,
    pins: Arc<dyn Pins>,
    invites: Arc<dyn Invites>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    max_message: usize,
    timeout: Duration,
}

impl Listener {
    pub fn new(key: Arc<StaticKey>, pins: Arc<dyn Pins>, invites: Arc<dyn Invites>) -> Listener {
        Listener {
            key,
            pins,
            invites,
            clock: Arc::new(system_ms),
            max_message: NOISE_MAX,
            timeout: HANDSHAKE_TIMEOUT,
        }
    }

    /// The clock invites expire by, in milliseconds since the Unix epoch.
    pub fn clock(mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Listener {
        self.clock = clock;
        self
    }

    pub fn max_message(mut self, max_message: usize) -> Listener {
        self.max_message = max_message;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Listener {
        self.timeout = timeout;
        self
    }

    /// Authenticates one inbound stream. The result's `peer` is pinned, or `paired` names the
    /// invite the peer just used.
    pub async fn accept<S>(&self, stream: S) -> Result<LinkConnection<S>, LinkError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        tokio::time::timeout(self.timeout, self.respond(stream))
            .await
            .map_err(|_| LinkError::Timeout)?
    }

    async fn respond<S>(&self, mut stream: S) -> Result<LinkConnection<S>, LinkError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let hello_bytes = read_handshake(&mut stream).await?;
        if hello_bytes.len() > HELLO_MAX {
            return Err(LinkError::Protocol("hello is too long".into()));
        }
        let hello: Hello = serde_json::from_slice(&hello_bytes)
            .map_err(|e| LinkError::Protocol(format!("bad hello: {e}")))?;
        if hello.parrot != "link" || hello.version != 1 {
            return Err(LinkError::Protocol("unsupported link version".into()));
        }
        let secret = match (hello.mode, &hello.invite) {
            (Mode::Xx, None) => None,
            (Mode::Pair, Some(invite)) => Some(
                self.invites
                    .secret(invite, (self.clock)())
                    .ok_or_else(|| LinkError::Pairing("the code is unknown, used or expired".into()))?,
            ),
            _ => return Err(LinkError::Protocol("bad hello".into())),
        };
        let prologue = prologue(&hello_bytes);
        let mut hs = builder(&self.key, &prologue, secret.as_ref())?.build_responder()?;
        let mut buf = vec![0u8; NOISE_MAX];
        let mut payload = vec![0u8; NOISE_MAX];

        let msg = read_handshake(&mut stream).await?;
        hs.read_message(&msg, &mut payload)?;
        let n = hs.write_message(&[], &mut buf)?;
        frame::write_noise(&mut stream, &buf[..n]).await?;
        stream.flush().await?;
        let msg = read_handshake(&mut stream).await?;
        hs.read_message(&msg, &mut payload)?;
        let remote = remote_key(&hs)?;

        let paired = match hello.invite {
            None if self.pins.is_pinned(&remote) => None,
            None => {
                return Err(LinkError::Key {
                    got: remote.runtime_id(),
                });
            }
            Some(invite) => {
                if !self.invites.claim(&invite, (self.clock)()) {
                    return Err(LinkError::Pairing("the code was used or expired".into()));
                }
                Some(invite)
            }
        };
        // The smaller of this side's limit and the one the dialer's carrier stated.
        let max_message = hello.max_message.map_or(self.max_message, |m| {
            m.clamp(MIN_MESSAGE, NOISE_MAX).min(self.max_message)
        });
        connection(stream, hs, remote, Side::Accepted, paired, max_message)
    }
}

fn prologue(hello: &[u8]) -> Vec<u8> {
    let mut prologue = PROLOGUE.to_vec();
    prologue.extend_from_slice(hello);
    prologue
}

fn builder<'a>(
    key: &'a StaticKey,
    prologue: &'a [u8],
    psk: Option<&'a Secret>,
) -> Result<Builder<'a>, LinkError> {
    let pattern = if psk.is_some() { PAIRING_PATTERN } else { PATTERN };
    let mut b = Builder::new(pattern.parse()?)
        .local_private_key(key.private())?
        .prologue(prologue)?;
    if let Some(psk) = psk {
        b = b.psk(3, &psk.0)?;
    }
    Ok(b)
}

async fn read_handshake<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, LinkError> {
    frame::read_noise(stream)
        .await?
        .ok_or_else(|| LinkError::Protocol("closed during the handshake".into()))
}

fn remote_key(hs: &HandshakeState) -> Result<PublicKey, LinkError> {
    hs.get_remote_static()
        .and_then(PublicKey::from_slice)
        .ok_or_else(|| LinkError::Protocol("no remote static key".into()))
}

fn connection<S: AsyncRead + AsyncWrite>(
    stream: S,
    hs: HandshakeState,
    key: PublicKey,
    side: Side,
    paired: Option<String>,
    max_message: usize,
) -> Result<LinkConnection<S>, LinkError> {
    let noise = Arc::new(Mutex::new(hs.into_transport_mode()?));
    let (r, w) = tokio::io::split(stream);
    Ok(LinkConnection {
        reader: LinkReader {
            io: r,
            noise: noise.clone(),
            asm: Assembler::new(MAX_MESSAGE),
            buf: vec![0u8; NOISE_MAX],
        },
        writer: LinkWriter {
            io: w,
            noise,
            part_len: frame::part_len(max_message),
            buf: vec![0u8; NOISE_MAX],
        },
        peer: Peer {
            runtime: key.runtime_id(),
            key,
        },
        side,
        paired,
    })
}

fn system_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
