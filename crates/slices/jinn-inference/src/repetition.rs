//! Exact-repetition detection for the inference actor's stream buffers.
//!
//! A pure module: no bus, no actor, no state. [`scan`] inspects the tail
//! of one accumulated stream buffer (assistant prose, thinking text, or
//! tool-call arguments) for a contiguous byte-exact repetition — the
//! signature every captured loop shares (planning-mantra prose, 3-char
//! argument garbage, empty-KV argument noise, thinking chants).
//!
//! Only a repetition that *ends* the buffer can fire: counting is
//! tail-anchored — for each candidate period, whole aligned copies ending
//! at the buffer's last byte are counted. Within a periodic run the
//! alignment is rotation-invariant, so a true continuation extends the
//! run immediately; only a tail that breaks the period delays detection
//! until the next unit boundary realigns it.
//!
//! Residual false-positive family, accepted and bounded (the watchdog
//! surrenders after its budget): a contiguous `MIN_RUN_BYTES`-byte run
//! of one repeated multi-byte fragment — spaced-ellipsis runs, banner
//! lines repeated verbatim past 400 bytes. The single-character guard
//! below excludes the common shapes (`===`, `---`, whitespace, ASCII
//! dots); a ≥400-byte *verbatim* repeat of real content is rare and
//! costs at most the retry budget.
//!
//! The thresholds are code constants, deliberately not configuration:
//! detector tuning is not a user-facing concern (the watchdog's budget
//! knob, `[watchdog.reasoning_loop]`, is the only user surface). The
//! signal reports counts for logging; it never carries text, because a
//! computed byte offset is not a `str` boundary and must never be sliced
//! as one.

/// The shortest repeating unit considered a loop. Units shorter than
/// three bytes are excluded: sub-3-byte periods collapse into the
/// whitespace-run and banner false-positive family, and every captured
/// loop repeats a unit of at least 3 chars (`dot`, ` : "" , "`).
pub const MIN_PERIOD: usize = 3;

/// The longest repeating unit considered. Longer loops either exceed the
/// window's evidence capacity or belong to legitimate structure.
pub const MAX_PERIOD: usize = 512;

/// The minimum number of contiguous repeats that constitutes a loop.
pub const MIN_REPEATS: usize = 5;

/// The minimum total size of the repeated run in bytes. Gates cheaply:
/// shorter buffers cannot carry the evidence, so `scan` returns before
/// touching any window work.
pub const MIN_RUN_BYTES: usize = 400;

/// How many tail bytes of the buffer are examined per scan.
///
/// Invariant: `MIN_REPEATS * MAX_PERIOD` (2560) must stay below this, so
/// the period loop's capacity break is sound. Widening `MAX_PERIOD`
/// requires re-checking the inequality.
pub const WINDOW_BYTES: usize = 8 * 1024;

/// A byte-exact repetition found in one buffer's tail.
///
/// Carries counts only — never text (a computed offset is not a UTF-8
/// boundary; see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepetitionSignal {
    /// The repeating unit's length in bytes (the smallest period that
    /// matched — an even-period loop aliases upward).
    pub period: usize,
    /// How many contiguous repeats of the unit end the buffer.
    pub repeats: usize,
}

/// Scans the tail of one stream buffer for contiguous exact repetition.
///
/// A repetition counts when a byte-exact unit of [`MIN_PERIOD`]..=[`MAX_PERIOD`]
/// bytes repeats [`MIN_REPEATS`] or more times contiguously at the end of
/// the examined window, totalling at least [`MIN_RUN_BYTES`] bytes, and the
/// unit is not single-character (fewer than 2 distinct bytes — the banner,
/// rule, and whitespace-run family that legitimately repeats forever).
///
/// Returns `None` when the buffer is below the evidence threshold or no
/// qualifying repetition ends it.
#[must_use]
pub fn scan(buf: &str) -> Option<RepetitionSignal> {
    let bytes = buf.as_bytes();
    if bytes.len() < MIN_RUN_BYTES {
        return None;
    }
    let (_, window) = bytes.split_at(bytes.len().saturating_sub(WINDOW_BYTES));
    scan_window(window)
}

/// Finds the fundamental repeated unit ending `window`, if any.
fn scan_window(window: &[u8]) -> Option<RepetitionSignal> {
    for period in MIN_PERIOD..=MAX_PERIOD {
        // Complete repeats need `repeats * period` bytes; once the window
        // cannot hold the minimum evidence, no larger period can either.
        if window.len() < MIN_REPEATS.saturating_mul(period) {
            break;
        }
        // In bounds by the break above: `window.len() >= MIN_REPEATS *
        // period >= period`.
        let Some(unit) = window.get(window.len() - period..) else {
            continue;
        };
        if distinct_byte_count(unit) < 2 {
            // Single-character repetition: `===…`, `---…`, whitespace
            // runs — legitimate structure, never a loop.
            continue;
        }
        let repeats = count_trailing_repeats(window, unit);
        if repeats >= MIN_REPEATS && repeats * period >= MIN_RUN_BYTES {
            return Some(RepetitionSignal { period, repeats });
        }
    }
    None
}

/// Counts how many whole copies of `unit` end `window`, counting the
/// unit itself as the first repeat. The anchor is the buffer's last
/// byte, so the match is rotation-invariant within a periodic run: any
/// offset that ends at the tail counts, not only offsets divisible by
/// the period. A tail that breaks the period caps the count.
fn count_trailing_repeats(window: &[u8], unit: &[u8]) -> usize {
    let period = unit.len();
    let mut count = 1;
    while let Some(candidate) = window
        .len()
        .checked_sub((count + 1) * period)
        .and_then(|start| window.get(start..window.len() - count * period))
        && candidate == unit
    {
        count += 1;
    }
    count
}

/// Counts distinct bytes in a slice (a slice of at most a few hundred
/// bytes; a fixed 256-flag sweep is the cheap way to stay branch-free).
fn distinct_byte_count(bytes: &[u8]) -> usize {
    let mut seen = [false; 256];
    let mut count = 0;
    for &b in bytes {
        if let Some(flag) = seen.get_mut(usize::from(b))
            && !*flag
        {
            *flag = true;
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use std::fmt::Write as _;

    use super::*;

    /// The assistant-response-loop sample's signature: a ~194-char
    /// planning mantra repeated 18+ times in prose.
    fn mantra_unit() -> String {
        "Let me carefully re-examine the situation. \
         I should reflect on what has been done so far. \
         The next step is to proceed methodically and without haste. \
         I need to remain focused on the task at hand. "
            .to_owned()
    }

    /// Varied prose with no repeated sentence — the negative control for
    /// "real writing never trips".
    fn varied_prose(bytes_target: usize) -> String {
        let sentences = [
            "The cache layer invalidates entries on write, not on read. ",
            "Config lives in two files, and only one of them is user-facing. ",
            "Sessions persist across restarts; in-flight turns do not. ",
            "The renderer draws on a fixed tick and skips frames when busy. ",
            "Tool output is truncated twice: by lines first, then by bytes. ",
            "A session id names the storage shard; a turn token names the attempt. ",
        ];
        let mut buf = String::new();
        let mut i = 0;
        while buf.len() < bytes_target {
            if let Some(sentence) = sentences.get(i % sentences.len()) {
                buf.push_str(sentence);
            }
            let _ = write!(buf, "(note {i}) ");
            i += 1;
        }
        buf
    }

    /// A non-periodic printable-ASCII byte string of `len` bytes: the
    /// byte at `i` is `0x30 + (7i mod 64)`, and since 7 is coprime with
    /// 64 the sequence has no repetition period below 64 bytes.
    fn aperiodic_bytes(len: usize) -> String {
        (0..len)
            .map(|i| {
                let b = 0x30 + u8::try_from((i * 7) % 64).expect("fits u8");
                b as char
            })
            .collect()
    }

    #[rstest::rstest]
    #[test]
    fn detects_planning_mantra_repetition() {
        // Given a buffer of one planning mantra repeated beyond the
        // thresholds (the assistant-response-loop signature).
        let unit = mantra_unit();
        let buf = unit.repeat(8);

        // When scanning.
        let signal = scan(&buf);

        // Then a signal fires with the mantra's period.
        let signal = signal.expect("mantra repetition detected");
        assert_eq!(signal.period, unit.len());
        assert_eq!(signal.repeats, 8);
    }

    #[rstest::rstest]
    #[test]
    fn detects_short_period_argument_garbage() {
        // Given a buffer of 3-char units repeated many times (the
        // call-loop signature: `dotdotdot…` decaying into garbage).
        let buf = "dot".repeat(400);

        // When scanning.
        let signal = scan(&buf);

        // Then a signal fires with the fundamental period.
        let signal = signal.expect("3-char period detected");
        assert_eq!(signal.period, 3);
        assert_eq!(signal.repeats, 400);
    }

    #[rstest::rstest]
    #[test]
    fn detects_empty_kv_argument_noise() {
        // Given a buffer of empty key/value argument noise (the
        // tool-call-doom-loop signature: `"" : "" , "`).
        let unit = r#""a" : "" , "b" : "" , ""#;
        let buf = unit.repeat(40);

        // When scanning.
        let signal = scan(&buf);

        // Then a signal fires naming the noise's period.
        let signal = signal.expect("empty-KV noise detected");
        assert_eq!(signal.period, unit.len());
    }

    #[rstest::rstest]
    #[test]
    fn detects_thinking_chant() {
        // Given a thinking buffer chanting the same short sentences
        // (the tool-call-doom-loop-2 signature).
        let unit = "Ok.  Done.  Yes.  Final.  ";
        let buf = unit.repeat(60);

        // When scanning.
        let signal = scan(&buf);

        // Then a signal fires with the chant's period.
        let signal = signal.expect("thinking chant detected");
        assert_eq!(signal.period, unit.len());
    }

    #[rstest::rstest]
    #[test]
    fn ignores_varied_prose() {
        // Given ordinary prose with no repeated sentence.
        let buf = varied_prose(2000);

        // When scanning.
        let signal = scan(&buf);

        // Then nothing fires.
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ignores_code_with_varying_lines() {
        // Given code whose lines differ (counters, indices), the normal
        // shape of generated code.
        let mut buf = String::new();
        for i in 0..40 {
            let _ = writeln!(buf, "    let item_{i} = process(batch[{i}], ctx[{i}]);");
        }

        // When scanning.
        let signal = scan(&buf);

        // Then nothing fires.
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ignores_ascii_ellipsis_runs() {
        // Given prose trailing into a long ASCII dot run.
        let mut buf = varied_prose(600);
        buf.push_str(&".".repeat(600));

        // When scanning.
        let signal = scan(&buf);

        // Then nothing fires: a single repeated character is not a loop.
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ignores_whitespace_runs() {
        // Given prose ending in a long whitespace run.
        let mut buf = varied_prose(600);
        buf.push_str("\n\n\n\n    \n\n");

        // When scanning.
        let signal = scan(&buf);

        // Then nothing fires.
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ignores_banner_rules() {
        // Given prose ending in a `===` banner rule.
        let mut buf = varied_prose(600);
        buf.push_str(&"=".repeat(600));

        // When scanning.
        let signal = scan(&buf);

        // Then nothing fires.
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ignores_fewer_repeats_than_minimum() {
        // Given a periodic tail of four copies (400 bytes total, so the
        // length gate passes but the repeat count cannot).
        let unit = aperiodic_bytes(100);
        let mut buf = varied_prose(400);
        buf.push_str(&unit.repeat(4));

        // When scanning.
        let signal = scan(&buf);

        // Then nothing fires.
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ignores_runs_below_minimum_bytes() {
        // Given five copies of a 79-byte unit (395 bytes of repetition,
        // one byte short of the run threshold, after a varied prefix).
        let unit = aperiodic_bytes(79);
        let mut buf = varied_prose(400);
        buf.push_str(&unit.repeat(5));

        // When scanning.
        let signal = scan(&buf);

        // Then nothing fires.
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ignores_buffers_shorter_than_the_minimum_run() {
        // Given a short buffer, repetition or not.
        let buf = "repeated repeated repeated ".repeat(10);

        // When scanning.
        let signal = scan(&buf);

        // Then the length gate bails before any window work.
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn fires_at_exact_threshold_repeats_and_bytes() {
        // Given exactly MIN_REPEATS copies of an 80-byte non-periodic
        // unit, totalling exactly MIN_RUN_BYTES.
        let unit = aperiodic_bytes(MIN_RUN_BYTES / MIN_REPEATS);
        let buf = unit.repeat(MIN_REPEATS);

        // When scanning.
        let signal = scan(&buf);

        // Then it fires exactly at the boundary. (A 64-byte internal
        // period exists in `aperiodic_bytes` sequences but fails the run
        // threshold at six copies, so the fundamental 80 wins.)
        let signal = signal.expect("exact threshold detected");
        assert_eq!(signal.period, MIN_RUN_BYTES / MIN_REPEATS);
        assert_eq!(signal.repeats, MIN_REPEATS);
    }

    #[rstest::rstest]
    #[test]
    fn trailing_nonperiodic_tail_caps_the_count() {
        // Given a loop interrupted by a non-periodic tail (the model
        // started writing something new — not a continuation).
        let unit = aperiodic_bytes(MIN_RUN_BYTES / MIN_REPEATS);
        let mut buf = unit.repeat(MIN_REPEATS);
        buf.push_str("xyz");

        // When scanning.
        let signal = scan(&buf);

        // Then nothing fires yet: the broken tail sits at the anchor,
        // so no period matches. (If the "xyz" grows back into the
        // loop's period, the run realigns and detection fires.)
        assert!(signal.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn loop_fires_once_the_run_resumes_and_realigns() {
        // Given the same loop with the run resumed after the break.
        let unit = aperiodic_bytes(MIN_RUN_BYTES / MIN_REPEATS);
        let mut buf = unit.repeat(MIN_REPEATS);
        buf.push_str("xyz");
        buf.push_str(&unit.repeat(MIN_REPEATS));

        // When scanning.
        let signal = scan(&buf);

        // Then it fires on the realigned run ending the buffer.
        let signal = signal.expect("resumed loop detected");
        assert_eq!(signal.period, unit.len());
        assert_eq!(signal.repeats, MIN_REPEATS);
    }

    #[rstest::rstest]
    #[test]
    fn detects_within_tail_window_only() {
        // Given a long non-repetitive prefix and a loop only at the end.
        let prefix = varied_prose(9 * WINDOW_BYTES / 10);
        let unit = mantra_unit();
        let mut buf = prefix;
        buf.push_str(&unit.repeat(MIN_REPEATS));

        // When scanning.
        let signal = scan(&buf);

        // Then the tail loop is found despite the long prefix.
        let signal = signal.expect("tail loop detected");
        assert_eq!(signal.period, unit.len());
        assert_eq!(signal.repeats, MIN_REPEATS);
    }

    #[rstest::rstest]
    #[test]
    fn utf8_multibyte_units_are_handled() {
        // Given a loop whose unit contains multibyte characters, with
        // enough copies to clear the run gate (the unit is 57 bytes).
        let unit = "重新検討しましょう。もう一度考えます。";
        let buf = unit.repeat(8);

        // When scanning.
        let signal = scan(&buf);

        // Then the byte-level scan still finds the repetition.
        let signal = signal.expect("multibyte repetition detected");
        assert_eq!(signal.period, unit.len());
    }
}
