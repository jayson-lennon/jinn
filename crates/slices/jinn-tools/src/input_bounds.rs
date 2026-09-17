//! Input-size guards for built-in tools.
//!
//! Output truncation lives in [`super::truncation`]; this module is its
//! input-side counterpart. It guards against degenerate tool calls — the
//! canonical example being a model sampler repetition loop that emits hundreds
//! or thousands of identical adjacent entries into an `edit` `lines` array or
//! a `write` `content` blob.
//!
//! The detector rejects a run of ≥ [`MAX_IDENTICAL_RUN`] identical consecutive
//! entries. There is no legitimate reason for an agent to repeat 50+ identical
//! adjacent lines — a script is the right tool for that kind of bulk work.

use wherror::Error;

/// Minimum length of an identical consecutive run that indicates a degenerate
/// (e.g. sampler-repetition-loop) tool call.
pub const MAX_IDENTICAL_RUN: usize = 50;

/// Input payload failed a size/degeneracy guard.
///
/// Currently only raised for long identical runs; additional bound kinds would
/// be added as variants if scope ever expands.
#[derive(Debug, Error)]
#[error(debug)]
pub struct InputBoundsError;

/// Rejects a slice of strings containing a run of ≥ [`MAX_IDENTICAL_RUN`]
/// identical consecutive entries.
///
/// Generic over `AsRef<str>` so callers can pass `&[&str]` (`write`) or
/// `&[String]` (`edit`) without intermediate allocation.
///
/// # Errors
///
/// Returns [`InputBoundsError`] if the slice contains a long identical run.
pub fn check_repetition<S: AsRef<str>>(items: &[S]) -> Result<(), InputBoundsError> {
    // Slices shorter than the threshold cannot trip it.
    if items.len() < MAX_IDENTICAL_RUN {
        return Ok(());
    }
    let mut run = 1usize;
    for [a, b] in items.array_windows::<2>() {
        if a.as_ref() == b.as_ref() {
            run += 1;
            if run >= MAX_IDENTICAL_RUN {
                return Err(InputBoundsError);
            }
        } else {
            run = 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    fn accepts_slice_shorter_than_threshold() {
        // Given 10 identical entries (well below the threshold).
        let items: Vec<&str> = vec!["x"; 10];

        // When checking repetition.
        let result = check_repetition(&items);

        // Then it is accepted.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn accepts_run_of_49() {
        // Given exactly 49 identical entries.
        let items: Vec<&str> = vec!["x"; 49];

        // When checking repetition.
        let result = check_repetition(&items);

        // Then it is accepted (threshold is 50, inclusive).
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn rejects_run_of_50() {
        // Given exactly 50 identical entries.
        let items: Vec<&str> = vec!["x"; 50];

        // When checking repetition.
        let result = check_repetition(&items);

        // Then it is rejected.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    fn rejects_long_run() {
        // Given a 1000-entry identical run (the real pathology scale).
        let items: Vec<&str> = vec![","; 1000];

        // When checking repetition.
        let result = check_repetition(&items);

        // Then it is rejected.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    fn accepts_varied_slice_above_threshold_length() {
        // Given 1000 distinct entries.
        let items: Vec<String> = (0..1000).map(|i| format!("line {i}")).collect();

        // When checking repetition.
        let result = check_repetition(&items);

        // Then it is accepted.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn run_resets_on_change() {
        // Given two runs of 49 separated by a different entry.
        let mut items: Vec<&str> = vec!["x"; 49];
        items.push("y");
        items.extend(vec!["x"; 49]);

        // When checking repetition.
        let result = check_repetition(&items);

        // Then it is accepted — neither run reaches 50.
        assert!(result.is_ok());
    }
}
