//! The socket or pipe to mpv, and the process at the other end of it.
//!
//! mpv's IPC is one JSON object per line in each direction. Lines are read
//! as bytes rather than text, because mpv passes on file names that are not
//! UTF-8 byte for byte, and one such name must not cost the whole stream.

use std::future::Future;
use std::io;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;

use crate::core::error::{Result, SyncPartyError};

/// A line longer than this is dropped. mpv's lines are short — the longest
/// is a property carrying a path — so one this size means something is
/// wrong, and buffering it without limit would only make that worse.
const MAX_LINE: usize = 1 << 20;

/// How often the endpoint is tried while mpv starts. It usually appears
/// within 60 ms.
const CONNECT_INTERVAL: Duration = Duration::from_millis(25);

#[cfg(unix)]
pub(super) type Stream = tokio::net::UnixStream;
#[cfg(windows)]
pub(super) type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Opens a connection to an mpv listening on `endpoint`.
#[cfg(unix)]
pub(super) async fn open(endpoint: &str) -> io::Result<Stream> {
    tokio::net::UnixStream::connect(endpoint).await
}

#[cfg(windows)]
pub(super) async fn open(endpoint: &str) -> io::Result<Stream> {
    tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint)
}

/// Whether a failed connection means mpv is not listening *yet*.
#[cfg(unix)]
fn not_listening_yet(error: &io::Error) -> bool {
    // No socket file yet, or the file is there and nothing accepts on it.
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

#[cfg(windows)]
fn not_listening_yet(error: &io::Error) -> bool {
    // ERROR_PIPE_BUSY: mpv makes a fresh pipe instance after each client it
    // accepts, and between the two every instance is taken.
    const ERROR_PIPE_BUSY: i32 = 231;
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(ERROR_PIPE_BUSY)
}

/// Connects to a freshly started mpv, retrying until it is listening.
///
/// mpv creates its endpoint shortly after it starts, and a bad option makes
/// it exit before it ever does. So every attempt also checks whether the
/// process is still there: an mpv that has exited is reported as that at
/// once rather than as a ten-second wait that ends in "no socket".
pub(super) async fn connect(
    endpoint: &str,
    child: &mut Child,
    patience: Duration,
) -> Result<Stream> {
    let deadline = Instant::now() + patience;

    loop {
        match open(endpoint).await {
            Ok(stream) => return Ok(stream),
            Err(error) if not_listening_yet(&error) => {}
            Err(error) => {
                return Err(SyncPartyError::PlayerFailed(format!(
                    "could not connect to mpv: {error}"
                )))
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(SyncPartyError::PlayerFailed(format!(
                    "mpv exited as soon as it started ({status}). An option it does not \
                     understand — from an mpv older than syncparty needs, or from its \
                     configuration — usually causes this."
                )))
            }
            Ok(None) => {}
            Err(error) => {
                return Err(SyncPartyError::PlayerFailed(format!(
                    "could not tell whether mpv is running: {error}"
                )))
            }
        }

        if Instant::now() >= deadline {
            return Err(SyncPartyError::PlayerFailed(format!(
                "mpv started but did not open its control socket within {} s",
                patience.as_secs()
            )));
        }
        tokio::time::sleep(CONNECT_INTERVAL).await;
    }
}

/// Splits a byte stream into lines without losing anything to cancellation.
///
/// The connection task waits on the next line and on timers in one
/// `select!`, so the read is dropped half-way whenever a timer wins. The only
/// await here is `fill_buf`, which consumes nothing; everything after it runs
/// to completion before the next await. A line cut short by a timer is
/// simply carried on with next time.
pub(super) struct LineReader<R> {
    inner: BufReader<R>,
    line: Vec<u8>,
    overflowed: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    pub(super) fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            line: Vec::new(),
            overflowed: false,
        }
    }

    /// The next complete line, without its `\n`; `None` at end of stream.
    pub(super) async fn next_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            let available = self.inner.fill_buf().await?;
            if available.is_empty() {
                // A line without its terminator at the very end is not a
                // message mpv finished sending.
                return Ok(None);
            }

            let newline = available.iter().position(|&byte| byte == b'\n');
            let chunk = &available[..newline.unwrap_or(available.len())];
            let room = MAX_LINE.saturating_sub(self.line.len());
            if chunk.len() > room {
                self.overflowed = true;
            }
            self.line.extend_from_slice(&chunk[..chunk.len().min(room)]);
            let used = newline.map_or(available.len(), |at| at + 1);
            self.inner.consume(used);

            if newline.is_some() {
                let line = std::mem::take(&mut self.line);
                if std::mem::take(&mut self.overflowed) {
                    tracing::warn!("dropped a line from mpv longer than {MAX_LINE} bytes");
                    continue;
                }
                return Ok(Some(line));
            }
        }
    }
}

/// Writes queued lines to mpv in the order they were queued.
///
/// One writer, fed by a channel, so two commands sent at once can never
/// interleave their bytes. It ends when every sender is gone or a write
/// fails; either way later sends fail, which is how a command learns mpv has
/// gone.
pub(super) async fn write_lines<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut lines: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    while let Some(line) = lines.recv().await {
        let written = async {
            writer.write_all(&line).await?;
            writer.flush().await
        };
        if let Err(error) = written.await {
            tracing::debug!(%error, "could not write to mpv");
            break;
        }
    }
}

/// The process behind the endpoint, as far as the connection task needs it.
///
/// A trait so the driver's tests can play mpv's part over a socket without a
/// process: they decide when "mpv" exits, and with what.
pub(super) trait Process: Send + 'static {
    /// Resolves once the process has exited, with its exit code — `None`
    /// when a signal ended it. Cancel safe.
    fn wait(&mut self) -> impl Future<Output = Option<i32>> + Send;

    /// Asks the process to die. Returns at once; [`Process::wait`] says when
    /// it has.
    fn kill(&mut self);
}

/// A real mpv.
pub(super) struct MpvProcess(pub(super) Child);

impl Process for MpvProcess {
    async fn wait(&mut self) -> Option<i32> {
        match self.0.wait().await {
            Ok(status) => status.code(),
            Err(error) => {
                tracing::debug!(%error, "could not wait for mpv");
                None
            }
        }
    }

    fn kill(&mut self) {
        if let Err(error) = self.0.start_kill() {
            tracing::debug!(%error, "could not kill mpv");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_are_split_on_newlines_and_a_partial_line_waits_for_the_rest() {
        let (mut ours, theirs) = tokio::io::duplex(64);
        let mut lines = LineReader::new(theirs);

        ours.write_all(b"{\"a\":1}\n{\"b\":").await.expect("write");
        assert_eq!(
            lines.next_line().await.expect("read"),
            Some(b"{\"a\":1}".to_vec())
        );

        ours.write_all(b"2}\n").await.expect("write");
        assert_eq!(
            lines.next_line().await.expect("read"),
            Some(b"{\"b\":2}".to_vec())
        );

        drop(ours);
        assert_eq!(lines.next_line().await.expect("read"), None);
    }

    #[tokio::test]
    async fn a_read_cut_short_by_a_timer_loses_nothing() {
        let (mut ours, theirs) = tokio::io::duplex(64);
        let mut lines = LineReader::new(theirs);

        ours.write_all(b"half of ").await.expect("write");
        let cut_short = tokio::time::timeout(Duration::from_millis(20), lines.next_line()).await;
        assert!(cut_short.is_err(), "no complete line yet");

        ours.write_all(b"a line\n").await.expect("write");
        assert_eq!(
            lines.next_line().await.expect("read"),
            Some(b"half of a line".to_vec())
        );
    }

    #[tokio::test]
    async fn an_oversized_line_is_dropped_and_the_next_one_still_arrives() {
        let (mut ours, theirs) = tokio::io::duplex(1 << 16);
        let mut lines = LineReader::new(theirs);

        let writer = tokio::spawn(async move {
            let huge = vec![b'x'; MAX_LINE + 10];
            ours.write_all(&huge).await.expect("write");
            ours.write_all(b"\n{\"ok\":true}\n").await.expect("write");
        });

        assert_eq!(
            lines.next_line().await.expect("read"),
            Some(b"{\"ok\":true}".to_vec())
        );
        writer.await.expect("writer");
    }
}
