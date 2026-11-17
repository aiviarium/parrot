//! Where a session's bytes come from and go to.
//!
//! A session needs a [`ByteSource`] (output from the application) and a [`ByteSink`] (input to
//! it, and resizes). A local PTY is one implementation ([`crate::pty`]); a remote host's byte
//! stream, or a test, is another ([`channel`]).

use std::future::Future;
use std::io;
use std::pin::Pin;

use tokio::sync::mpsc;

use crate::screen::Size;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Output from the application.
pub trait ByteSource: Send + 'static {
    /// The next chunk of output, or `None` at the end.
    fn read(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>>;
}

/// Input to the application.
pub trait ByteSink: Send + Sync + 'static {
    fn write<'a>(&'a self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>>;
    /// Tells the application the terminal size changed.
    fn resize(&self, size: Size) -> BoxFuture<'_, io::Result<()>>;
}

impl ByteSource for Box<dyn ByteSource> {
    fn read(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        (**self).read()
    }
}

impl ByteSink for Box<dyn ByteSink> {
    fn write<'a>(&'a self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        (**self).write(data)
    }
    fn resize(&self, size: Size) -> BoxFuture<'_, io::Result<()>> {
        (**self).resize(size)
    }
}

impl ByteSink for std::sync::Arc<dyn ByteSink> {
    fn write<'a>(&'a self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        (**self).write(data)
    }
    fn resize(&self, size: Size) -> BoxFuture<'_, io::Result<()>> {
        (**self).resize(size)
    }
}

/// A source fed from a channel.
pub struct ChannelSource(pub mpsc::Receiver<Vec<u8>>);

impl ByteSource for ChannelSource {
    fn read(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move { Ok(self.0.recv().await) })
    }
}

/// What a [`ChannelSink`] forwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SinkEvent {
    Data(Vec<u8>),
    Resize(Size),
}

/// A sink that forwards to a channel.
pub struct ChannelSink(pub mpsc::UnboundedSender<SinkEvent>);

impl ByteSink for ChannelSink {
    fn write<'a>(&'a self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        let r = self
            .0
            .send(SinkEvent::Data(data.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "sink closed"));
        Box::pin(async move { r })
    }

    fn resize(&self, size: Size) -> BoxFuture<'_, io::Result<()>> {
        let r = self
            .0
            .send(SinkEvent::Resize(size))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "sink closed"));
        Box::pin(async move { r })
    }
}

/// A connected source and sink over channels, for remote streams and tests. Send output with
/// the returned sender; read what the session wrote from the returned receiver.
pub fn channel() -> (
    ChannelSource,
    ChannelSink,
    mpsc::Sender<Vec<u8>>,
    mpsc::UnboundedReceiver<SinkEvent>,
) {
    let (out_tx, out_rx) = mpsc::channel(256);
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    (ChannelSource(out_rx), ChannelSink(in_tx), out_tx, in_rx)
}
