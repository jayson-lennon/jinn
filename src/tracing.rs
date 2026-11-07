//! Tracing initialization for jinn.
//!
//! Sets up the global tracing subscriber based on the application's run mode.
//! In TUI mode, traces are written exclusively to a file (to avoid corrupting
//! the terminal in raw mode). In headless mode, traces are written to BOTH the
//! terminal and a file. The file path is resolved at CLI parse time from the
//! `--log-file` flag, defaulting to the XDG `state_dir` (see `AppPaths::log_path`).
//!
//! Filter precedence: `-v`/`-q` (via `clap_verbosity_flag`) controls only the
//! verbosity of `jinn*` crates — [`EnvFilter`] matches targets by string
//! prefix, so `jinn={level}` covers every workspace crate. Third-party crates
//! (wasmtime, trouper, …) sit on a global `warn` floor, so warnings and errors
//! always surface from dependencies; at `-q` the floor steps down to `error`
//! and at `-qq` everything is off. Setting `RUST_LOG` overrides this
//! automatic filter entirely.

use std::{
    env, fmt,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    sync::Arc,
};

use clap_verbosity_flag::{Verbosity, VerbosityFilter, WarnLevel};
use error_stack::{Report, ResultExt};
use jinn_kernel::common::app_info::APP_NAME;
use marked_path::{Absolute, MarkedPath, MarkedPathAccess, MarkedPathBuf};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{
        FmtContext, FormattedFields, format::FormatEvent, format::FormatFields, format::Writer,
        time::FormatTime, time::SystemTime,
    },
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};
use wherror::Error;

/// Error type returned when tracing subscriber initialization fails.
#[derive(Debug, Error)]
#[error(debug)]
pub struct TracingInitError;

/// Decides how the tracing subscriber is configured based on run mode.
///
/// Both variants carry a resolved log file path (caller resolves defaults and
/// overrides before constructing this enum).
#[derive(Debug)]
pub enum TracingMode {
    /// TUI mode: file-only logging to avoid corrupting the terminal.
    Tui {
        /// Resolved path to the log file (e.g. `~/.local/state/jinn/jinn.log`).
        log_path: MarkedPathBuf<Absolute>,
    },
    /// Headless mode: writes to BOTH terminal and file.
    Headless {
        /// Resolved path to the log file (e.g. `~/.local/state/jinn/jinn.log`).
        log_path: MarkedPathBuf<Absolute>,
    },
}

/// Derives the dedicated panic-log path as a sibling of the main `log_path`.
///
/// `jinn.log` -> `jinn-panic.log` in the same directory. The only absolute
/// path with no parent is the filesystem root itself; the panic log then
/// lands at the root beside it.
#[must_use]
fn panic_log_path(log_path: &MarkedPathBuf<Absolute>) -> MarkedPathBuf<Absolute> {
    match log_path.parent() {
        Some(dir) => dir
            .join("jinn-panic.log")
            .expect("absolute parent joins a bare filename"),
        // `log_path` is the filesystem root; write beside it.
        None => log_path
            .join("jinn-panic.log")
            .expect("the root joins a bare filename"),
    }
}

/// Appends one durable panic record to the panic-log file and flushes.
///
/// Each line is `RFC3339 UTC | panic at file:line:col | message`. Extracted as a
/// free function so the append logic is unit-testable without installing a global
/// hook.
fn write_panic_record(
    path: &MarkedPath<'_, Absolute>,
    message: &str,
    location: Option<(&str, u32, u32)>,
) {
    let location_str = match location {
        Some((file, line, col)) => format!("{file}:{line}:{col}"),
        None => "<unknown>".to_owned(),
    };
    let ts = jiff::Timestamp::now();
    let line = format!("{ts} | panic at {location_str} | {message}");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

/// Installs the global panic hook.
///
/// Appends each panic's message + source location to the dedicated panic-log at
/// `panic_path`, then chains to the previously-installed hook so default stderr
/// output (or any earlier hook) is preserved. Extracted from `init` so the chaining
/// behavior is unit-testable without standing up the full tracing subscriber.
fn install_panic_hook(panic_path: MarkedPathBuf<Absolute>) {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &std::panic::PanicHookInfo<'_>| {
        let location = info.location().map(|l| (l.file(), l.line(), l.column()));
        write_panic_record(&panic_path.as_marked_path(), &info.to_string(), location);
        previous_hook(info);
    }));
}

/// Builds the `EnvFilter` directive string from `RUST_LOG` and the CLI verbosity.
///
/// * No `RUST_LOG` → `warn,jinn={v}`: the global `warn` floor keeps warnings
///   and errors visible from third-party crates, while the `jinn={v}`
///   directive (prefix-matched against every `jinn*` workspace crate) is the
///   only thing `-v`/`-q` control. At `-q` the floor steps down to `error`
///   (`error,jinn=error`); at `-qq` the filter is `off` entirely.
/// * Any `RUST_LOG` value → used verbatim as the entire filter; the user has
///   taken manual control of everything.
fn build_filter(rust_log: Option<&str>, verbosity: &Verbosity<WarnLevel>) -> String {
    let Some(log) = rust_log else {
        return format_from_verbosity(verbosity);
    };
    log.to_owned()
}

/// Renders the automatic (no-`RUST_LOG`) filter for the CLI verbosity: a
/// global floor for third-party crates plus the `jinn={v}` directive.
fn format_from_verbosity(verbosity: &Verbosity<WarnLevel>) -> String {
    match verbosity.filter() {
        VerbosityFilter::Off => "off".to_owned(),
        VerbosityFilter::Error => format!("error,{APP_NAME}=error"),
        level => format!("warn,{APP_NAME}={level}"),
    }
}

/// Compact event formatter: shows only the *innermost* span plus a nesting
/// depth marker, with optional ANSI coloring of the structural segments.
///
/// trouper creates one `actor.handle_message` span per actor hop (parented on
/// the caller's span), so a single tell→handle→publish round trip can nest a
/// dozen spans. Rendering the whole chain on every line produces giant,
/// mostly-redundant prefixes. This formatter renders:
///
/// ```text
/// <timestamp> <LEVEL> …×N innermost_span{fields}: target:line: event fields
/// ```
///
/// `…×N` says how deep the event fired without repeating the parents; the
/// innermost span is where the event actually happened (for actor arrivals
/// that's the actor name + message type). Line length is therefore bounded
/// regardless of nesting depth.
///
/// When [`CompactSpans::color`] is set, the timestamp, level, depth marker,
/// span name, and target segments are wrapped in ANSI styles (via
/// `nu-ansi-term`); when unset, output is plain text with no escapes.
struct CompactSpans {
    color: bool,
}

impl Clone for CompactSpans {
    fn clone(&self) -> Self {
        *self
    }
}

impl Copy for CompactSpans {}

impl<S, N> FormatEvent<S, N> for CompactSpans
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        // Timestamp, dimmed when coloring.
        if self.color {
            write_dimmed_timestamp(&mut writer)?;
            writer.write_char(' ')?;
        } else {
            SystemTime.format_time(&mut writer)?;
            writer.write_char(' ')?;
        }

        // Level, padded to width 5 like the default formatters (" INFO") —
        // pad the plain token first so the escape bytes never shift alignment,
        // then wrap the token itself in its per-level color.
        let level: &Level = event.metadata().level();
        let padded = format!("{level:>5}");
        if self.color {
            write!(writer, "{} ", level_color(level).bold().paint(&padded))?;
        } else {
            write!(writer, "{padded} ")?;
        }

        // Depth marker + innermost span only.
        self.write_span_context(ctx, &mut writer)?;

        // target:line — always on, replacing the per-layer display flags.
        let meta = event.metadata();
        if self.color {
            write!(writer, "{}:", DIMMED.paint(meta.target()))?;
        } else {
            write!(writer, "{}:", meta.target())?;
        }
        if let Some(line) = meta.line() {
            write!(writer, "{line}:")?;
        }
        writer.write_char(' ')?;

        // The event's own fields (includes the message text); content stays
        // unstyled so payloads remain greppable.
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// The style with every property unset (mirrors `nu_ansi_term`'s `Default`,
/// which is not `const`); the functional-update syntax builds the styles we
/// actually use in `const` context.
const UNSET: nu_ansi_term::Style = nu_ansi_term::Style {
    foreground: None,
    background: None,
    is_bold: false,
    is_dimmed: false,
    is_italic: false,
    is_underline: false,
    is_blink: false,
    is_reverse: false,
    is_hidden: false,
    is_strikethrough: false,
    prefix_with_reset: false,
};

/// Dimmed style for structural segments (timestamp, depth marker, target).
const DIMMED: nu_ansi_term::Style = nu_ansi_term::Style {
    is_dimmed: true,
    ..UNSET
};

/// Bold style for the innermost span name.
const BOLD: nu_ansi_term::Style = nu_ansi_term::Style {
    is_bold: true,
    ..UNSET
};

/// Maps a tracing level to its conventional terminal color.
const fn level_color(level: &Level) -> nu_ansi_term::Style {
    use nu_ansi_term::Color;
    match *level {
        Level::ERROR => nu_ansi_term::Style {
            foreground: Some(Color::Red),
            ..UNSET
        },
        Level::WARN => nu_ansi_term::Style {
            foreground: Some(Color::Yellow),
            ..UNSET
        },
        Level::INFO => nu_ansi_term::Style {
            foreground: Some(Color::Green),
            ..UNSET
        },
        Level::DEBUG => nu_ansi_term::Style {
            foreground: Some(Color::Blue),
            ..UNSET
        },
        Level::TRACE => nu_ansi_term::Style {
            foreground: Some(Color::Purple),
            ..UNSET
        },
    }
}

/// Renders a system timestamp as dimmed SGR codes around the fixed-width
/// form produced by [`SystemTime`], keeping the colored and plain renderings
/// column-aligned.
fn write_dimmed_timestamp(writer: &mut Writer<'_>) -> fmt::Result {
    // `SystemTime::format_time` writes directly; capture the plain form so it
    // can be wrapped in style codes without reparsing.
    let mut plain = String::new();
    {
        let mut sink = owning_writer(&mut plain);
        SystemTime.format_time(&mut sink)?;
    }
    write!(writer, "{}", DIMMED.paint(plain))
}

/// Adapts a `String` sink into the `tracing_subscriber::fmt::format::Writer`
/// expected by `FormatTime`.
fn owning_writer(sink: &mut String) -> Writer<'_> {
    Writer::new(sink)
}

impl CompactSpans {
    /// Writes the `…×N innermost_span{fields}: ` portion of an event line.
    fn write_span_context<S, N>(
        &self,
        ctx: &FmtContext<'_, S, N>,
        writer: &mut Writer<'_>,
    ) -> fmt::Result
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
        N: for<'writer> FormatFields<'writer> + 'static,
    {
        let Some(scope) = ctx.event_scope() else {
            return Ok(());
        };
        let (depth, innermost) = {
            let mut innermost = None;
            let mut depth = 0;
            // Single pass; avoids buffering the whole span chain.
            for span in scope.from_root() {
                depth += 1;
                innermost = Some(span);
            }
            (depth, innermost)
        };
        let Some(innermost) = innermost else {
            return Ok(());
        };
        if self.color {
            write!(writer, "{} ", DIMMED.paint(format!("…×{depth}")))?;
            write!(writer, "{}", BOLD.paint(innermost.metadata().name()))?;
        } else {
            write!(writer, "…×{depth} ")?;
            write!(writer, "{}", innermost.metadata().name())?;
        }
        let ext = innermost.extensions();
        if let Some(fields) = ext.get::<FormattedFields<N>>()
            && !fields.is_empty()
        {
            if self.color {
                write!(writer, "{}", DIMMED.paint(format!("{{{fields}}}")))?;
            } else {
                write!(writer, "{{{fields}}}")?;
            }
        }
        writer.write_str(": ")
    }
}

/// Initializes the global tracing subscriber.
///
/// Filtering is built by [`build_filter`]: `-v`/`-q` control only the
/// `jinn*` crates while third-party crates sit on a global `warn` floor;
/// setting `RUST_LOG` replaces the whole filter.
///
/// # Arguments
///
/// * `verbosity` - The verbosity level from CLI flags.
/// * `mode` - The [`TracingMode`] controlling where traces are written. The
///   contained `log_path` is opened in append mode; its parent directory is
///   created if it does not exist.
/// * `trace_color` - Whether trace output written to the log file is ANSI
///   colorized.
/// * `log_tail_lines` - The startup log-tail bound from
///   `--log-tail-lines`. `Some(0)` disables the trim entirely (the file
///   grows unbounded); `None` applies the historical default of
///   [`LOG_TAIL_LINES`] lines.
///
/// # Errors
///
/// Returns a [`TracingInitError`] if the log file cannot be opened or its
/// parent directory cannot be created.
///
/// # Panics
///
/// Panics if called more than once or if another tracer has already been set.
pub fn init(
    verbosity: Verbosity<WarnLevel>,
    mode: TracingMode,
    trace_color: bool,
    log_tail_lines: Option<usize>,
) -> Result<(), Report<TracingInitError>> {
    let rust_log = env::var("RUST_LOG").ok();
    let filter = build_filter(rust_log.as_deref(), &verbosity);

    let log_path = match &mode {
        TracingMode::Tui { log_path } | TracingMode::Headless { log_path } => log_path.clone(),
    };

    // Bound the log to its tail before the append handle opens. `0` means
    // keep everything (skip the trim); the flag default falls back to
    // [`LOG_TAIL_LINES`]. Non-fatal by contract: a failed trim is logged and
    // startup proceeds with the oversized file.
    let logfile = prepare_log_file(&log_path, log_tail_lines)?;

    // In TUI mode the terminal enters raw mode shortly after init, so traces
    // (including RUST_LOG output) go to the file only — never the screen. If the
    // user set RUST_LOG expecting to watch startup, point them at the file or the
    // --log-file flag. (Headless mode already prints to the terminal.)
    if matches!(mode, TracingMode::Tui { .. }) && rust_log.is_some() {
        eprintln!("RUST_LOG is set, but TUI mode writes traces to a file, not the terminal.");
        eprintln!("  run tail -f '{log_path}' in another term");
        eprintln!("  or rerun with --log-file <path> to choose a different file.");
    }

    let formatter = CompactSpans { color: trace_color };

    match mode {
        TracingMode::Tui { .. } => {
            let file_layer = tracing_subscriber::fmt::layer()
                .event_format(formatter)
                .with_ansi(trace_color)
                .with_writer(Arc::new(logfile))
                .with_filter(EnvFilter::new(filter));

            tracing_subscriber::registry().with(file_layer).init();
        }
        TracingMode::Headless { .. } => {
            let file_layer: Box<dyn Layer<_> + Send + Sync + 'static> =
                tracing_subscriber::fmt::layer()
                    .event_format(formatter)
                    .with_ansi(trace_color)
                    .with_writer(Arc::new(logfile))
                    .with_filter(EnvFilter::new(filter.clone()))
                    .boxed();

            let terminal_layer = tracing_subscriber::fmt::layer()
                .event_format(formatter)
                .with_ansi(trace_color)
                .with_filter(EnvFilter::new(filter));

            tracing_subscriber::registry()
                .with(file_layer)
                .with(terminal_layer)
                .init();
        }
    }

    // Install the global panic hook: appends each panic's message + source
    // location to a dedicated `jinn-panic.log` alongside the main log, then
    // chains to the previous hook so default stderr output is preserved.
    install_panic_hook(panic_log_path(&log_path));

    tracing::info!("");
    tracing::info!("--- new session started ---");
    tracing::info!("");

    Ok(())
}

/// Bounds the log to its tail, then opens the append handle.
///
/// The single startup path for the log file: the trim runs first so a
/// crash-produced giant log is bounded before anything appends. A
/// `log_tail_lines` of `Some(0)` skips the trim entirely — the contract
/// behind `--log-tail-lines 0` is that the file is never trimmed, so an
/// investigation window spanning restarts keeps every line. A failed trim
/// is non-fatal by contract — it is warned about and startup proceeds
/// against the oversized file; only an `open` failure propagates.
///
/// # Errors
///
/// Returns a [`TracingInitError`] if the log file cannot be opened or its
/// parent directory cannot be created (the trim never blocks startup).
fn prepare_log_file(
    path: &MarkedPathBuf<Absolute>,
    log_tail_lines: Option<usize>,
) -> Result<File, Report<TracingInitError>> {
    if log_tail_lines == Some(0) {
        // --log-tail-lines 0: keep everything, ever.
        return open_log_file(path);
    }
    let keep = log_tail_lines.unwrap_or(LOG_TAIL_LINES);
    if let Err(error) = truncate_log_to_tail_lines(path, keep) {
        tracing::warn!(%error, "failed to trim log tail; leaving the file as-is");
    }
    open_log_file(path)
}

/// Creates the parent directory (if needed) and opens the log file for append.
fn open_log_file(path: &MarkedPathBuf<Absolute>) -> Result<File, Report<TracingInitError>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(TracingInitError)
            .attach_with(|| format!("failed to create log directory '{parent}'"))?;
    }

    File::options()
        .create(true)
        .append(true)
        .open(path)
        .change_context(TracingInitError)
        .attach_with(|| format!("failed to open file '{path}' for tracing"))
}

/// The number of trailing complete lines kept in the log file across startups.
///
/// A hard-coded constant by design: the log is a diagnostic aid, not user
/// data, and it must stay bounded even when the user never touches config.
const LOG_TAIL_LINES: usize = 10_000;

/// Size of each chunk read during the backwards newline scan.
const TAIL_SCAN_CHUNK: usize = 64 * 1024;

/// Bounds the log file to its last [`LOG_TAIL_LINES`] complete lines.
///
/// Runs at startup, before the append handle opens: a crash that produced a
/// giant log cannot skip the trim, so the file stays bounded across any
/// sequence of runs. The scan reads backwards in chunks and counts newlines,
/// so memory use is bounded by [`TAIL_SCAN_CHUNK`] regardless of file size;
/// only the kept tail is buffered and written to a sibling temp file that is
/// renamed over the original. A file at or under the limit is left byte-for-
/// byte untouched. A file whose last line lacks a trailing newline loses that
/// unterminated fragment (it is not a complete line).
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or replaced.
/// The caller treats every error as non-fatal: a failed trim must never
/// block startup.
fn truncate_log_to_tail_lines(
    path: &MarkedPathBuf<Absolute>,
    keep_lines: usize,
) -> std::io::Result<()> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        // First run: nothing to trim.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();

    let start = find_tail_start(
        &mut file,
        len,
        u64::try_from(keep_lines).expect("line counts fit u64") + 1,
    )?;
    if start == len {
        return Ok(()); // at or under the limit; leave the file byte-identical
    }
    // A trim is happening, so an unterminated trailing fragment is dropped:
    // only complete lines survive.
    let end = complete_tail_end(&mut file, len)?;
    let tail = read_chunk(&mut file, start, end - start)?;

    let tmp_path = {
        let mut name = path.as_path().as_os_str().to_os_string();
        name.push(".trim.tmp");
        // The parent exists (the file was opened from it) and the suffix is a
        // bare name, so the join cannot fail.
        path.parent()
            .expect("trim only runs on a log with a parent directory")
            .join(name)
            .expect("bare-name join onto an existing parent cannot fail")
    };
    // Write the tail to a sibling temp file, then swap it into place. The
    // remove-then-rename order is for Windows, where rename fails when the
    // destination exists; the crash window between the two loses the log
    // file, and `open_log_file` recreates it on the next run.
    {
        let mut tmp = File::create(&tmp_path)?;
        tmp.write_all(&tail)?;
        tmp.sync_all().ok();
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        // No file to replace: treat like a plain rename target.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::rename(&tmp_path, path)
}

/// Returns the byte offset where the kept tail starts: just after the
/// `split_on`-th newline counting from the end of the file, or `len` when
/// the file holds fewer than `split_on` newlines (nothing to trim).
///
/// The trailing newline (when the file ends with one) counts as a separator,
/// which makes the arithmetic uniform; the tail's end is repaired separately
/// by [`complete_tail_end`]. Each chunk is scanned in full, so a newline
/// sitting at a chunk boundary is never missed.
fn find_tail_start(file: &mut File, len: u64, split_on: u64) -> std::io::Result<u64> {
    let mut needed = split_on;
    let mut remaining = len;
    while needed > 0 && remaining > 0 {
        let chunk_len = remaining.min(TAIL_SCAN_CHUNK as u64);
        let chunk_start = remaining - chunk_len;
        let chunk = read_chunk(file, chunk_start, chunk_len)?;
        for (i, b) in chunk.iter().enumerate().rev() {
            if *b == b'\n' {
                needed -= 1;
                if needed == 0 {
                    return Ok(chunk_start + i as u64 + 1);
                }
            }
        }
        remaining = chunk_start;
    }
    Ok(len)
}

/// Returns the exclusive end offset of the kept tail: the file length when
/// the file ends with a newline, otherwise the offset just after the last
/// newline (dropping an unterminated trailing fragment).
///
/// Only meaningful when a trim is happening — a final newline is then
/// guaranteed to exist, so the fallback of returning `len` never triggers.
fn complete_tail_end(file: &mut File, len: u64) -> std::io::Result<u64> {
    let mut probe = [0u8; 1];
    file.seek(SeekFrom::Start(len - 1))?;
    file.read_exact(&mut probe)?;
    if probe[0] == b'\n' {
        return Ok(len);
    }
    let mut remaining = len;
    while remaining > 0 {
        let chunk_len = remaining.min(TAIL_SCAN_CHUNK as u64);
        let chunk_start = remaining - chunk_len;
        let chunk = read_chunk(file, chunk_start, chunk_len)?;
        if let Some(i) = chunk.iter().rposition(|b| *b == b'\n') {
            return Ok(chunk_start + i as u64 + 1);
        }
        if chunk_start == 0 {
            break;
        }
        remaining = chunk_start;
    }
    Ok(len)
}

/// Reads exactly `len` bytes starting at `offset` into a fresh buffer.
///
/// `File::seek` + a bounded `read_exact` — the caller sizes the buffer, so a
/// hostile file that grew between the metadata read and this call cannot
/// over-allocate.
fn read_chunk(file: &mut File, offset: u64, len: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A `MakeWriter` capturing formatted output for assertions.
    #[derive(Clone, Default)]
    struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

    impl CapturingWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().expect("poisoned").clone())
                .expect("captured output is utf-8")
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
        type Writer = CapturingSink;

        fn make_writer(&'a self) -> Self::Writer {
            CapturingSink(self.0.clone())
        }
    }

    struct CapturingSink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("poisoned").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Emits an info event inside `depth` nested spans and returns the
    /// captured formatter output.
    fn render_event_at_depth(depth: usize) -> String {
        render_at_depth_with(depth, false)
    }

    /// Emits an info event inside `depth` nested spans, with or without
    /// coloring, returning the captured formatter output.
    fn render_at_depth_with(depth: usize, color: bool) -> String {
        let capture = CapturingWriter::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .event_format(CompactSpans { color })
                .with_ansi(color)
                .with_writer(capture.clone()),
        );

        {
            let _guard = tracing::subscriber::set_default(subscriber);
            let outer = tracing::info_span!("outer_span", hop = "first");
            let _entered_outer = outer.enter();
            let held: Vec<tracing::Span> = {
                let mut spans: Vec<tracing::Span> = Vec::with_capacity(depth);
                for i in 0..depth {
                    let parent = spans.last();
                    let span = match parent {
                        Some(parent) => tracing::info_span!(
                            parent: parent.id(),
                            "actor.handle_message",
                            actor.name = "TestActor"
                        ),
                        None => {
                            tracing::info_span!("actor.handle_message", actor.name = "TestActor")
                        }
                    };
                    spans.push(span);
                    let _ = i;
                }
                spans
            };
            let _guards: Vec<_> = held.iter().map(|span| span.enter()).collect();
            tracing::info!("hello from inside");
        }

        capture.contents()
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    #[test]
    fn formatter_renders_depth_marker_and_innermost_span(#[case] color: bool) {
        // Given a subscriber with the compact formatter and 20 nested spans.
        let output = render_at_depth_with(20, color);

        // When formatting an event inside those spans (rendered above).

        // Then the depth marker counts all spans (outer + 20 nested).
        assert!(
            output
                .replace("\x1b[2m", "")
                .replace("\x1b[0m", "")
                .contains("…×21 "),
            "expected depth marker, got: {output}"
        );
        // And only the innermost span name and fields are rendered.
        assert!(
            output.contains("actor.handle_message"),
            "expected innermost span, got: {output}"
        );
        // And no parent span names leak into the line (field names may carry
        // ANSI styling, so match on the value alone).
        assert!(
            !output.contains("outer_span"),
            "expected no parent-chain text, got: {output}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn formatter_output_prefix_is_bounded() {
        // Given events rendered at depth 1 and depth 20.
        let shallow = render_event_at_depth(1);
        let deep = render_event_at_depth(20);

        // When measuring the span context each line renders (timestamp +
        // level + depth marker + span prefix, i.e. everything before the
        // target).
        let prefix_len = |output: &str| {
            let line = output.lines().next().expect("one line of output");
            line.find("jinn::tracing:").expect("target in output")
        };

        // Then the prefix length does not grow with nesting depth (the depth
        // marker gains one char from "…×2" to "…×21"; that is it).
        let shallow_prefix = prefix_len(&shallow);
        let deep_prefix = prefix_len(&deep);
        let delta = (deep_prefix - shallow_prefix) as i64;
        assert!(
            (-1..=1).contains(&delta),
            "prefix must not depend on nesting depth (shallow={shallow}, deep={deep})"
        );
    }

    #[rstest::rstest]
    #[test]
    fn formatter_renders_event_without_spans() {
        // Given a subscriber with the compact formatter and no active spans.
        let capture = CapturingWriter::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .event_format(CompactSpans { color: false })
                .with_writer(capture.clone()),
        );

        {
            let _guard = tracing::subscriber::set_default(subscriber);
            tracing::info!("spanless event");
        }

        // When formatting the event (rendered above).

        // Then no depth marker or span prefix is emitted.
        let output = capture.contents();
        assert!(
            !output.contains("…×"),
            "expected no depth marker, got: {output}"
        );
        // And the event message and target are still present.
        assert!(
            output.contains("spanless event"),
            "expected event message, got: {output}"
        );
        assert!(
            output.contains("jinn::tracing:"),
            "expected target, got: {output}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn formatter_without_color_emits_no_escapes() {
        // Given the compact formatter with coloring disabled.
        let output = render_at_depth_with(3, false);

        // When formatting an event (rendered above).

        // Then the rendered line contains no ANSI escape sequences.
        assert!(
            !output.contains('\x1b'),
            "expected no escapes, got: {output:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn formatter_with_color_emits_level_color_and_styles() {
        // Given the compact formatter with coloring enabled.
        let output = render_at_depth_with(3, true);

        // When formatting an INFO event (rendered above).

        // Then the level token carries its green foreground code (bold+green
        // renders as `1;32` in a single SGR sequence).
        assert!(
            output.contains("\x1b[1;32m INFO\x1b[0m"),
            "expected colored INFO level, got: {output:?}"
        );
        // And the timestamp and span name are styled.
        assert!(
            output.contains("\x1b[2m"),
            "expected dimmed segments, got: {output:?}"
        );
        assert!(
            output.contains("\x1b[1mactor.handle_message\x1b[0m"),
            "expected bold span name, got: {output:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn formatter_with_color_keeps_level_padding() {
        // Given colored and plain renderings of the same event.
        let plain = render_at_depth_with(3, false);
        let colored = render_at_depth_with(3, true);

        // When locating the level token in each (stripping escapes from the
        // colored one).

        // Then both place the level in the same columns — the escape bytes
        // never shift the padding.
        let strip = |s: &str| {
            let line = s.lines().next().expect("one line");
            let idx = line.find(" INFO").expect("level token in output");
            line[idx..].to_owned()
        };
        assert!(
            strip(&plain).starts_with(" INFO "),
            "plain level alignment broken: {plain:?}"
        );
        let colored_plain = colored
            .replace("\x1b[1;32m", "")
            .replace("\x1b[2m", "")
            .replace("\x1b[0m", "")
            .replace("\x1b[1m", "")
            .replace("\x1b[3m", "");
        assert!(
            strip(&colored_plain).starts_with(" INFO "),
            "colored level alignment broken: {colored:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn filter_without_rust_log_defaults_to_warn_floor() {
        // Given no RUST_LOG and the default verbosity.
        let verbosity = Verbosity::new(0, 0);

        // When building the filter.
        let filter = build_filter(None, &verbosity);

        // Then third-party crates sit on the warn floor and jinn is at the CLI level.
        assert_eq!(filter, "warn,jinn=warn");
    }

    #[rstest::rstest]
    #[test]
    fn filter_verbosity_raises_only_jinn() {
        // Given no RUST_LOG and -vv.
        let verbosity = Verbosity::new(2, 0);

        // When building the filter.
        let filter = build_filter(None, &verbosity);

        // Then jinn rises to debug while the third-party floor stays at warn.
        assert_eq!(filter, "warn,jinn=debug");
    }

    #[rstest::rstest]
    #[test]
    fn filter_quiet_lowers_floor_to_error() {
        // Given no RUST_LOG and -q.
        let verbosity = Verbosity::new(0, 1);

        // When building the filter.
        let filter = build_filter(None, &verbosity);

        // Then the floor steps down to error so quiet quiets dependencies too.
        assert_eq!(filter, "error,jinn=error");
    }

    #[rstest::rstest]
    #[test]
    fn filter_double_quiet_turns_everything_off() {
        // Given no RUST_LOG and -qq.
        let verbosity = Verbosity::new(0, 2);

        // When building the filter.
        let filter = build_filter(None, &verbosity);

        // Then the whole filter is off — silence was requested.
        assert_eq!(filter, "off");
    }

    #[rstest::rstest]
    #[test]
    fn filter_rust_log_overrides_entirely() {
        // Given a global-level RUST_LOG.
        let verbosity = Verbosity::new(1, 0);

        // When building the filter.
        let filter = build_filter(Some("debug"), &verbosity);

        // Then it is used verbatim — no jinn directive appended, no floor injected.
        assert_eq!(filter, "debug");
    }

    #[rstest::rstest]
    #[test]
    fn filter_rust_log_jinn_target_verbatim() {
        // Given a RUST_LOG naming a jinn-prefixed workspace crate.
        let verbosity = Verbosity::new(0, 0);

        // When building the filter.
        let filter = build_filter(Some("jinn_tui=trace"), &verbosity);

        // Then it is used verbatim — the user took control.
        assert_eq!(filter, "jinn_tui=trace");
    }

    #[rstest::rstest]
    #[test]
    fn panic_log_path_is_sibling_of_log_path() {
        // Given a resolved log path in a directory of its own. The path is
        // built from the volume root of the platform temp dir, so it is
        // absolute on every platform without naming a POSIX literal.
        let root = jinn_testutil::volume_root();
        let log_path = root
            .join_all(["x", "jinn.log"])
            .expect("bare-name join keeps the root absolute");

        // When deriving the panic-log path.
        let panic_path = panic_log_path(&log_path);

        // Then it is jinn-panic.log in the same directory.
        assert_eq!(
            panic_path.as_path(),
            root.join_all(["x", "jinn-panic.log"])
                .expect("bare-name join keeps the root absolute")
                .as_path()
        );
    }

    #[rstest::rstest]
    #[test]
    #[cfg(windows)]
    fn panic_log_path_falls_back_to_root_when_log_has_no_parent_windows() {
        // Given the drive root as the log path (no parent on Windows).
        let log_path = MarkedPathBuf::<Absolute>::new(r"C:\").expect("drive root types");

        // When deriving the panic-log path.
        let panic_path = panic_log_path(&log_path);

        // Then the panic log lands at the root.
        assert_eq!(panic_path.to_string_lossy(), r"C:\jinn-panic.log");
    }

    #[cfg(not(windows))]
    #[rstest::rstest]
    #[test]
    fn panic_log_path_falls_back_to_root_when_log_has_no_parent() {
        // Given the filesystem root as the log path (the only absolute
        // path with no parent).
        let log_path = jinn_testutil::volume_root();

        // When deriving the panic-log path.
        let panic_path = panic_log_path(&log_path);

        // Then the panic log lands at the root.
        assert_eq!(
            panic_path.as_path(),
            jinn_testutil::volume_root()
                .join_all(["jinn-panic.log"])
                .expect("bare-name join keeps the root absolute")
                .as_path()
        );
    }

    #[rstest::rstest]
    #[test]
    fn write_panic_record_appends_message_and_location() {
        // Given a temp directory as the panic-log location.
        let dir = jinn_testutil::tempdir();
        let panic_path = dir
            .path
            .join("jinn-panic.log")
            .expect("temp root joins a bare name");

        // When writing a panic record.
        write_panic_record(
            &panic_path.as_marked_path(),
            "boom in actor",
            Some(("src/llm.rs", 42, 7)),
        );

        // Then the file contains a line with the message and the location.
        let content = std::fs::read_to_string(&panic_path).expect("read panic log");
        assert!(
            content.contains("boom in actor"),
            "expected the panic message in the file, got: {content}"
        );
        assert!(
            content.contains("src/llm.rs:42:7"),
            "expected the location in the file, got: {content}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_panic_hook_chains_to_previous_hook() {
        // Given a sentinel previous hook that sets a flag.
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        std::panic::set_hook(Box::new(move |_| {
            called_clone.store(true, Ordering::SeqCst);
        }));

        // When installing our hook (which chains to the sentinel) and then
        // triggering a real panic in a child thread.
        let dir = jinn_testutil::tempdir();
        let panic_path = dir
            .path
            .join("jinn-panic.log")
            .expect("temp root joins a bare name");
        install_panic_hook(panic_path.clone());
        let _ = std::thread::spawn(|| panic!("chain test")).join();

        // Then the sentinel hook was invoked (chaining works).
        assert!(
            called.load(Ordering::SeqCst),
            "expected the previous hook to be called via chaining"
        );
        // And the panic log was written.
        let content = std::fs::read_to_string(&panic_path).expect("read panic log");
        assert!(content.contains("chain test"));

        // Restore the default hook to avoid leaking our hook into other tests.
        let _ = std::panic::take_hook();
    }

    #[rstest::rstest]
    #[test]
    fn truncate_keeps_last_lines() {
        // Given a log file holding 25 complete lines and a keep limit of 10.
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");
        let lines: Vec<String> = (0..25).map(|i| format!("line-{i:02}")).collect();
        std::fs::write(&log_path, lines.join("\n") + "\n").expect("seed log file");

        // When trimming to the last 10 lines.
        truncate_log_to_tail_lines(&log_path, 10).expect("trim succeeds");

        // Then only the last 10 complete lines remain, in order, with no
        // partial line at either end.
        let content = std::fs::read_to_string(&log_path).expect("read trimmed log");
        let kept: Vec<&str> = content.lines().collect();
        assert_eq!(
            kept,
            (15..25).map(|i| format!("line-{i:02}")).collect::<Vec<_>>(),
            "expected exactly the last 10 lines, got: {kept:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn truncate_under_limit_is_noop() {
        // Given a log file holding fewer lines than the keep limit.
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");
        let original = "a\nbb\nccc\n".repeat(3);
        std::fs::write(&log_path, &original).expect("seed log file");

        // When trimming to a limit the file already satisfies.
        truncate_log_to_tail_lines(&log_path, 100).expect("trim succeeds");

        // Then the file is byte-for-byte identical — a true no-op, not a
        // rewrite that happens to preserve the bytes.
        let after = std::fs::read(&log_path).expect("read log");
        assert_eq!(
            std::str::from_utf8(&after).expect("utf-8"),
            original,
            "file under the limit must not be rewritten"
        );
    }

    #[rstest::rstest]
    #[test]
    fn truncate_missing_file_succeeds_without_creating_it() {
        // Given no log file at the path (first run).
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");

        // When trimming.
        truncate_log_to_tail_lines(&log_path, 10).expect("trim succeeds");

        // Then the path is still absent — the trim never creates a log.
        assert!(
            !log_path.exists(),
            "a missing log must not be created by the trim"
        );
    }

    #[rstest::rstest]
    #[test]
    fn truncate_drops_unterminated_last_fragment() {
        // Given a log whose final line has no trailing newline, with more
        // complete lines than the keep limit.
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");
        std::fs::write(&log_path, "l1\nl2\nl3\nl4\nl5\nfragment").expect("seed log file");

        // When trimming to a limit of 4 complete lines.
        truncate_log_to_tail_lines(&log_path, 4).expect("trim succeeds");

        // Then the unterminated fragment is not a complete line and is
        // dropped; exactly the last 4 complete lines survive.
        let content = std::fs::read_to_string(&log_path).expect("read trimmed log");
        assert_eq!(content, "l2\nl3\nl4\nl5\n", "expected 4 complete lines");
    }

    #[rstest::rstest]
    #[test]
    fn truncate_at_limit_with_fragment_is_noop() {
        // Given a log at exactly the keep limit in complete lines, plus an
        // unterminated fragment.
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");
        let original = "l1\nl2\nfragment";
        std::fs::write(&log_path, original).expect("seed log file");

        // When trimming to a limit of 2 complete lines.
        truncate_log_to_tail_lines(&log_path, 2).expect("trim succeeds");

        // Then the file is untouched — the complete-line count satisfies the
        // limit, so the byte-identical no-op wins over fragment removal.
        let after = std::fs::read(&log_path).expect("read log");
        assert_eq!(
            std::str::from_utf8(&after).expect("utf-8"),
            original,
            "at-limit file must not be rewritten"
        );
    }

    #[rstest::rstest]
    #[test]
    fn truncate_scan_spans_multiple_chunks() {
        // Given a log well over one scan-chunk (64 KiB) in size, with the
        // kept tail smaller than a chunk.
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");
        // 6000 lines x 48 bytes = 288 KiB = ~4.4 scan chunks.
        let total = 6_000usize;
        let keep = 500usize;
        let lines: Vec<String> = (0..total)
            .map(|i| format!("{i:06}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n"))
            .collect();
        std::fs::write(&log_path, lines.join("")).expect("seed log file");

        // When trimming to the last 500 lines.
        truncate_log_to_tail_lines(&log_path, keep).expect("trim succeeds");

        // Then exactly the last 500 lines survive — the backwards scan
        // crossed chunk boundaries without losing or duplicating a line.
        let content = std::fs::read_to_string(&log_path).expect("read trimmed log");
        let kept: Vec<&str> = content.lines().collect();
        assert_eq!(kept.len(), keep, "kept the wrong number of lines");
        assert_eq!(
            kept.first().expect("non-empty")[..6]
                .parse::<usize>()
                .expect("index line"),
            total - keep,
            "expected the tail to start at the (total - keep)-th line"
        );
        assert_eq!(
            kept.last().expect("non-empty")[..6]
                .parse::<usize>()
                .expect("index line"),
            total - 1,
            "expected the original final line at the end"
        );
    }

    #[rstest::rstest]
    #[test]
    fn trim_failure_does_not_fail_prepare() {
        // Given a log path whose parent is a regular file, so the trim
        // cannot even read the target (opening fails with a directory error
        // or not-a-directory error, never NotFound).
        let dir = jinn_testutil::tempdir();
        let blocker = dir
            .path
            .join("blocker")
            .expect("temp root joins a bare name");
        std::fs::write(&blocker, "not a directory").expect("seed blocker");
        let log_path = blocker
            .join("jinn.log")
            .expect("file path joins a bare name");

        // When preparing the log file (trim + open).
        let result = prepare_log_file(&log_path, None);

        // Then the failure comes from `open_log_file`, not the trim — the
        // trim error was swallowed as non-fatal (a `NotFound`-shaped error
        // would mean the trim surfaced; a directory/other error means the
        // open did).
        match result {
            Ok(_) => panic!("prepare must fail when the parent is a file"),
            Err(report) => {
                let rendered = format!("{report:?}");
                assert!(
                    rendered.contains("failed to open file")
                        || rendered.contains("failed to create log directory"),
                    "expected an open/directory error, got: {rendered}"
                );
            }
        }
    }

    #[rstest::rstest]
    #[test]
    fn prepare_without_flag_trims_like_before() {
        // Given a log one line over the historical default bound.
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");
        let total = LOG_TAIL_LINES + 1;
        let lines: Vec<String> = (0..total).map(|i| format!("{i}\n")).collect();
        std::fs::write(&log_path, lines.join("")).expect("seed log file");

        // When preparing the log file with the flag absent.
        let file = prepare_log_file(&log_path, None).expect("prepare succeeds");
        drop(file);

        // Then exactly the default bound of complete lines survived — the
        // absent flag behaves byte-identically to the pre-flag behavior.
        let content = std::fs::read_to_string(&log_path).expect("read trimmed log");
        let kept: Vec<&str> = content.lines().collect();
        assert_eq!(kept.len(), LOG_TAIL_LINES, "default bound kept the tail");
        // And the dropped line is the oldest one.
        assert_eq!(
            kept.first()
                .expect("non-empty")
                .parse::<usize>()
                .expect("index line"),
            1,
            "the oldest line (0) was dropped, the tail starts at 1"
        );
    }

    #[rstest::rstest]
    #[test]
    fn prepare_with_zero_keeps_every_line() {
        // Given a modest log and the keep-everything value.
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");
        let total = 25usize;
        let lines: Vec<String> = (0..total).map(|i| format!("{i}\n")).collect();
        let seeded = lines.join("");
        std::fs::write(&log_path, &seeded).expect("seed log file");

        // When preparing the log file with --log-tail-lines 0.
        let file = prepare_log_file(&log_path, Some(0)).expect("prepare succeeds");
        drop(file);

        // Then every seeded line survived the startup untouched: the file is
        // byte-identical to what was on disk, and nothing is ever trimmed.
        let content = std::fs::read_to_string(&log_path).expect("read log");
        assert_eq!(content, seeded, "zero keeps everything");
    }

    #[rstest::rstest]
    #[test]
    fn prepare_with_explicit_bound_uses_it_over_the_default() {
        // Given a modest log and an explicit bound above the file's size.
        let dir = jinn_testutil::tempdir();
        let log_path = dir
            .path
            .join("jinn.log")
            .expect("temp root joins a bare name");
        let total = 25usize;
        let lines: Vec<String> = (0..total).map(|i| format!("{i}\n")).collect();
        let seeded = lines.join("");
        std::fs::write(&log_path, &seeded).expect("seed log file");

        // When preparing the log file with an explicit bound of 20 lines.
        let file = prepare_log_file(&log_path, Some(20)).expect("prepare succeeds");
        drop(file);

        // Then the explicit bound wins: exactly the last 20 lines survive,
        // not the default 10,000 and not the full 25.
        let content = std::fs::read_to_string(&log_path).expect("read trimmed log");
        let kept: Vec<&str> = content.lines().collect();
        assert_eq!(kept.len(), 20, "the explicit bound was applied");
        // And the kept tail starts where the trim left off.
        assert_eq!(
            kept.first()
                .expect("non-empty")
                .parse::<usize>()
                .expect("index line"),
            5,
            "the last 20 lines are lines 5..25"
        );
    }
}
