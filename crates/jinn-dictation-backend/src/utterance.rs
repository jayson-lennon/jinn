//! The backend contract's data vocabulary: one closed utterance and one
//! fetch-progress report.

/// Text the backend's endpointing closed as one utterance.
///
/// Utterances are ordered: a backend that closes two in one [`feed`]
/// returns both, in the order they were spoken. Text arrives exactly as
/// the engine produced it — the caller appends it verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    /// The transcribed text, engine-formatted (Parakeet carries native
    /// punctuation and capitalization; nothing strips or re-cases it).
    pub text: String,
}

impl Utterance {
    /// One utterance carrying `text`.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// One snapshot of a model download's progress.
///
/// `total` is `None` while the content length is unknown (chunked
/// transfers, or the backend has not received headers yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchProgress {
    /// Bytes downloaded so far.
    pub bytes_done: u64,
    /// Total bytes, when known.
    pub bytes_total: Option<u64>,
}

impl FetchProgress {
    /// One snapshot at `bytes_done` of `bytes_total`.
    #[must_use]
    pub fn new(bytes_done: u64, bytes_total: Option<u64>) -> Self {
        Self {
            bytes_done,
            bytes_total,
        }
    }

    /// The progress as a rounded percentage (0–100).
    ///
    /// `None` while the total is unknown or zero — a caller that renders
    /// "N%" shows nothing rather than a fabricated number.
    #[must_use]
    pub fn percent(&self) -> Option<u8> {
        let total = self.bytes_total?;
        if total == 0 {
            return None;
        }
        Some(u8::try_from((self.bytes_done * 100) / total)
            .unwrap_or(u8::MAX)
            .min(100))
    }
}

#[cfg(test)]
mod tests {
    use super::FetchProgress;
    use super::Utterance;

    #[rstest::rstest]
    #[case(0, Some(1000), Some(0))]
    #[case(1, Some(1000), Some(0))]
    #[case(500, Some(1000), Some(50))]
    #[case(999, Some(1000), Some(99))]
    #[case(1000, Some(1000), Some(100))]
    #[case(1200, Some(1000), Some(100))]
    fn percent_rounds_the_ratio(#[case] done: u64, #[case] total: Option<u64>, #[case] expected: Option<u8>) {
        // Given progress at `done` of `total` bytes.
        let progress = FetchProgress::new(done, total);

        // When reading the percentage.
        let percent = progress.percent();

        // Then it is the rounded ratio, clamped at 100.
        assert_eq!(percent, expected);
    }

    #[rstest::rstest]
    #[test]
    fn percent_is_none_without_a_total() {
        // Given progress with an unknown total.
        let progress = FetchProgress::new(400, None);

        // When reading the percentage.
        // Then it is None — a caller renders nothing rather than a
        // fabricated number.
        assert_eq!(progress.percent(), None);
    }

    #[rstest::rstest]
    #[test]
    fn percent_is_none_for_a_zero_total() {
        // Given progress claiming a zero-byte total.
        let progress = FetchProgress::new(400, Some(0));

        // When reading the percentage.
        // Then it is None, not a division by zero.
        assert_eq!(progress.percent(), None);
    }

    #[rstest::rstest]
    #[test]
    fn utterance_carries_its_text() {
        // Given an utterance built from engine text.
        let utterance = Utterance::new("Hello, world.");

        // When reading it back.
        // Then the text is preserved verbatim.
        assert_eq!(utterance.text, "Hello, world.");
    }
}
