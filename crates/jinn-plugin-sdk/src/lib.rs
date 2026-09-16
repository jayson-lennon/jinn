//! Guest-side SDK for authoring jinn plugins.
//!
//! A plugin is a WASI P2 command component built for `wasm32-wasip2` that
//! speaks NDJSON over stdio with its host. This crate provides the loop:
//! send [`hello`], await [`Welcome`], then push contributions and exit —
//! or keep running to react to future host messages.
//!
//! ```no_run
//! use jinn_plugin_sdk::{hello, welcome, push, PluginOutput};
//! use jinn_plugin_api::{Hello, PluginToHost, PushCitations};
//!
//! fn main() {
//!     let mut out = PluginOutput::stdout();
//!     hello(&mut out, "my-plugin");
//!     let _grants = welcome();
//!     push(
//!         &mut out,
//!         PluginToHost::PushCitations(PushCitations { session_id: String::new(), citations: vec![] }),
//!     );
//! }
//! ```

use std::io::{BufRead as _, Write};

use jinn_plugin_api::{
    Envelope, HostToPlugin, PROTOCOL_VERSION, PluginToHost, PluginToHostOrHostToPlugin,
};
use serde_json::from_str;

/// Writes the handshake opener on the given sink.
///
/// # Errors
///
/// Returns the underlying write error if the sink fails.
pub fn hello<W: Write>(out: &mut W, name: &str) -> std::io::Result<()> {
    hello_inner(out, name, &[])
}

/// Writes the handshake opener declaring host→guest event subscriptions.
///
/// Each subscription tag names a host event kind the guest wants forwarded
/// to its stdin (currently `"tool_call"`, `"tool_result"`, `"turn_end"`;
/// unknown tags are ignored by the host). Plugins that only push
/// contributions use [`hello`] instead.
///
/// # Errors
///
/// Returns the underlying write error if the sink fails.
pub fn hello_with_subscriptions<W: Write>(
    out: &mut W,
    name: &str,
    subscriptions: &[&str],
) -> std::io::Result<()> {
    hello_inner(out, name, subscriptions)
}

/// Shared handshake-opener implementation.
fn hello_inner<W: Write>(out: &mut W, name: &str, subscriptions: &[&str]) -> std::io::Result<()> {
    let envelope = Envelope::for_plugin(
        PluginToHost::Hello(jinn_plugin_api::Hello {
            protocol_version: PROTOCOL_VERSION,
            name: name.to_owned(),
            subscriptions: subscriptions.iter().map(|s| (*s).to_owned()).collect(),
        }),
        0,
        now_ms(),
    );
    write_line(out, &envelope)
}

/// Reads and returns the host's [`jinn_plugin_api::Welcome`].
///
/// Blocks until one arrives; a non-`Welcome` first message is skipped.
///
/// # Errors
///
/// Returns the underlying read error, or [`std::io::ErrorKind::InvalidData`]
/// for an undecodable line.
pub fn welcome() -> Result<jinn_plugin_api::Welcome, std::io::Error> {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        let n = lock.read_until(b'\n', &mut bytes)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "host closed stdin before Welcome",
            ));
        }
        let Ok(line) = std::str::from_utf8(bytes.trim_ascii_end()) else {
            continue;
        };
        let Ok(envelope) = from_str::<Envelope>(line) else {
            continue;
        };
        if let PluginToHostOrHostToPlugin::Host(HostToPlugin::Welcome(w)) = envelope.msg {
            return Ok(w);
        }
    }
}

/// Pushes one contribution to the host on the given sink.
///
/// # Errors
///
/// Returns the underlying write error if the sink fails.
pub fn push<W: Write>(out: &mut W, msg: PluginToHost) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let envelope = Envelope::for_plugin(msg, seq, now_ms());
    write_line(out, &envelope)
}

/// Serialize + write one envelope line.
fn write_line<W: Write>(out: &mut W, envelope: &Envelope) -> std::io::Result<()> {
    let mut json = serde_json::to_string(envelope)
        .map_err(|e| std::io::Error::other(format!("encode: {e}")))?;
    json.push('\n');
    out.write_all(json.as_bytes())?;
    out.flush()
}

/// Unix epoch milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The plugin's standard output — the wire to its host.
///
/// A newtype so scaffolded `main` functions cannot accidentally pass some
/// other writer where the host expects the wire.
pub struct PluginOutput<W: Write> {
    inner: W,
}

impl PluginOutput<std::io::Stdout> {
    /// The process's stdout (the wire).
    #[must_use]
    pub fn stdout() -> Self {
        Self {
            inner: std::io::stdout(),
        }
    }
}

impl<W: Write> Write for PluginOutput<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
