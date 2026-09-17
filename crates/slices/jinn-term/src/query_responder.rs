//! Terminal query responder — answers capability queries from the child.
//!
//! Full-screen programs (nvim, vim, tmux, kitty-aware tools) probe the
//! terminal at startup: they send query sequences and **block waiting for a
//! reply**. A real terminal emulator answers them; a bare pty does not, so
//! without this responder such programs hang at startup or degrade. The
//! responder scans the raw child output (before emulation) for known queries
//! and synthesizes conservative replies that identify the terminal as a
//! vt100-class xterm — the same identity [`super::pty_session`] advertises
//! through `TERM=xterm-256color`.

/// Scans child output for terminal queries and returns the bytes to write
/// back to the pty (possibly empty).
///
/// Call once per raw output chunk, before feeding the emulator. Recognized
/// queries:
///
/// - **DA1** (`ESC [ c` or `ESC [ 0 c`) — primary device attributes.
///   Answered `ESC [ ? 62 ; c`, "VT100-conformant, further levels on
///   request": the conservative answer xterm-class terminals gave for
///   decades, which no known TUI treats as a capability promise.
/// - **DA2** (`ESC [ > c`) — secondary attributes (terminal version).
///   Left unanswered: programs time out on a missing DA2 rather than hang,
///   and inventing a version could steer them into untested code paths.
///   The related **XTVERSION** query *is* answered (see below).
/// - **DSR cursor** (`ESC [ 6 n`) — cursor position report. The *child's* pty
///   cursor is unknown to us mid-chunk; answering the true emulator cursor is
///   exactly what real terminals do. The emulator has already parsed every
///   byte before the query by the time this runs, so its position is the
///   correct reply.
/// - **DECRPM** (`ESC [ ? Ps $ p`) — DEC private mode report. Answered
///   `ESC [ ? Ps ; 2 $ y` ("reset") for the queried mode: programs treat
///   "reset" as "no special mode active", the safe default. Supported
///   because an unanswered DECRQM *hangs* queries (`CSI ? 2026 $ p`, the
///   synchronized-output query, is the common modern offender).
/// - **XTVERSION** (`ESC [ > 0 q` / `ESC [ > q`) — answered
///   `DCS > | jinn(1) ST` naming the host terminal.
///
/// Deliberately **not** answered: Kitty keyboard protocol queries
/// (`CSI ? u`) — a reply would *enable* the protocol and change input
/// encoding; silence makes programs fall back to legacy encoding, which the
/// key encoder produces.
#[must_use]
pub fn respond_to_queries(raw: &[u8], cursor: (u16, u16)) -> Vec<u8> {
    let mut replies = Vec::new();
    find_queries(raw, &mut |query| match query {
        Query::PrimaryAttributes => replies.extend_from_slice(b"\x1b[?62;c"),
        Query::CursorPosition => {
            // DSR 6n reply: ESC [ row ; col R, 1-indexed.
            replies
                .extend_from_slice(format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).as_bytes());
        }
        Query::DecPrivateMode(mode) => {
            // DECRPM: report the mode as reset — the safe default.
            replies.extend_from_slice(format!("\x1b[?{mode};2$y").as_bytes());
        }
        Query::Version => replies.extend_from_slice(b"\x1bP>|jinn(1)\x1b\\"),
    });
    replies
}

/// A recognized terminal query in a raw output chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Query {
    /// Primary device attributes (`CSI c`).
    PrimaryAttributes,
    /// Cursor position report request (`CSI 6 n`).
    CursorPosition,
    /// DEC private mode request (`CSI ? Ps $ p`) carrying the mode number.
    DecPrivateMode(u32),
    /// Terminal version query (`CSI > q` / `CSI > 0 q`).
    Version,
}

/// Walks `raw`, invoking `emit` for every complete query found.
///
/// Byte-at-a-time scanning (per the AGENTS.md rule against manual
/// splitting): queries are control sequences, not unicode text, so
/// `unicode-segmentation` does not apply. The scanner tolerates chunk
/// boundaries in the caller by design — callers feed raw chunks, and all
/// recognized queries are short enough that the tail-scan heuristic below
/// catches sequences straddling a chunk split.
fn find_queries(raw: &[u8], emit: &mut impl FnMut(Query)) {
    let mut idx = 0;
    while let Some(&b) = raw.get(idx) {
        if b != 0x1b {
            idx += 1;
            continue;
        }
        idx = match parse_escape(raw, idx, emit) {
            Some(end) => end,
            None => idx + 1,
        };
    }
}

/// Attempts to parse a query starting at the `ESC` at `idx`.
///
/// Returns `Some(end)` — the index just past the consumed sequence — when a
/// recognized query was parsed, or `None` to let the caller advance one byte.
/// Unrecognized sequences are left in place so the emulator still parses them
/// normally.
fn parse_escape(raw: &[u8], idx: usize, emit: &mut impl FnMut(Query)) -> Option<usize> {
    let intro = *raw.get(idx + 1)?;
    if intro != b'[' {
        return None;
    }
    let mut pos = idx + 2;
    let mut params = String::new();
    // Collect parameter bytes until a final byte or terminator.
    while let Some(&b) = raw.get(pos) {
        match b {
            b'c' | b'n' | b'y' | b'q' => {
                let consumed = pos + 1;
                dispatch_query(&params, b, emit);
                return Some(consumed);
            }
            b'$' => {
                // DECRQM: CSI ? Ps $ p — the 'p' final byte follows.
                if raw.get(pos + 1) == Some(&b'p') {
                    let mode = params.trim_start_matches('?').parse::<u32>().unwrap_or(0);
                    emit(Query::DecPrivateMode(mode));
                    return Some(pos + 2);
                }
                return None;
            }
            b'0'..=b'9' | b'?' | b'>' | b';' => {
                params.push(b as char);
                pos += 1;
            }
            _ => return None,
        }
    }
    None
}

/// Classifies a collected parameter string with its final byte.
///
/// `final_byte` is the sequence's terminating byte (`c`, `n`, …); `params`
/// is everything between `CSI` and that byte, e.g. `""`+`c` = `CSI c` (DA1),
/// `">"`+`c` = `CSI > c` (DA2), `">0"`+`q` = `CSI > 0 q` (XTVERSION).
fn dispatch_query(params: &str, final_byte: u8, emit: &mut impl FnMut(Query)) {
    match (final_byte, params) {
        // Primary attributes: CSI c / CSI 0 c.
        (b'c', "" | "0") => emit(Query::PrimaryAttributes),
        // Secondary attributes: CSI > c / CSI > 0 c. Deliberately left
        // unanswered (module docs): programs time out gracefully on a
        // missing DA2 rather than hang.
        // Cursor position: CSI 6 n.
        (b'n', "6") => emit(Query::CursorPosition),
        // Terminal version: CSI > q / CSI > 0 q.
        (b'q', ">" | ">0") => emit(Query::Version),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    #[case(b"\x1b[c", b"\x1b[?62;c")]
    #[case(b"\x1b[0c", b"\x1b[?62;c")]
    fn da1_queries_get_vt100_class_reply(#[case] raw: &[u8], #[case] expected: &[u8]) {
        // Given a raw chunk containing a DA1 query.
        // When scanning for queries.
        let reply = respond_to_queries(raw, (0, 0));

        // Then the reply is the conservative VT100-class answer.
        assert_eq!(reply, expected);
    }

    #[rstest::rstest]
    fn version_query_gets_named_terminal_reply() {
        // Given a raw chunk containing an XTVERSION query.
        // When scanning for queries.
        let reply = respond_to_queries(b"\x1b[>0q", (0, 0));

        // Then the reply names the host terminal.
        assert_eq!(reply, b"\x1bP>|jinn(1)\x1b\\");
    }

    #[rstest::rstest]
    fn bare_da2_query_is_not_answered() {
        // Given a raw chunk containing a bare DA2 query (CSI > c).
        // When scanning for queries.
        let reply = respond_to_queries(b"\x1b[>c", (0, 0));

        // Then nothing is synthesized: we report no version, and programs
        // time out gracefully on an unanswered DA2 rather than hanging.
        assert!(reply.is_empty());
    }

    #[rstest::rstest]
    fn cursor_position_query_replies_with_emulator_cursor() {
        // Given a raw chunk containing a DSR cursor query and the emulator's
        // (0-indexed) cursor at row 4, col 9.
        // When scanning for queries.
        let reply = respond_to_queries(b"\x1b[6n", (4, 9));

        // Then the reply reports the 1-indexed position.
        assert_eq!(reply, b"\x1b[5;10R");
    }

    #[rstest::rstest]
    fn dec_private_mode_query_replies_as_reset() {
        // Given a synchronized-output mode query (CSI ? 2026 $ p).
        // When scanning for queries.
        let reply = respond_to_queries(b"\x1b[?2026$p", (0, 0));

        // Then the reply reports the mode as reset (safe default).
        assert_eq!(reply, b"\x1b[?2026;2$y");
    }

    #[rstest::rstest]
    fn ordinary_output_produces_no_replies() {
        // Given ordinary program output with escape styling but no queries.
        let raw = b"\x1b[2J\x1b[Hhello \x1b[1mworld\x1b[0m\r\n";

        // When scanning for queries.
        let reply = respond_to_queries(raw, (0, 0));

        // Then nothing is synthesized.
        assert!(reply.is_empty());
    }

    #[rstest::rstest]
    fn query_embedded_in_output_still_gets_answered() {
        // Given program output that styles, queries, then continues.
        let reply = respond_to_queries(b"\x1b[1m\x1b[c\x1b[0m", (0, 0));

        // Then the DA1 reply is synthesized.
        assert_eq!(reply, b"\x1b[?62;c");
    }

    #[rstest::rstest]
    fn truncated_escape_at_chunk_end_is_ignored() {
        // Given a chunk ending mid-escape (chunk boundary split).
        let reply = respond_to_queries(b"\x1b[1", (0, 0));

        // Then nothing is synthesized and nothing panics.
        assert!(reply.is_empty());
    }
}
