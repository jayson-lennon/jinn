//! String truncation for session titles.

/// Truncates a string to fit within `max_len` graphemes, appending `…` if truncated.
pub(crate) fn truncate_str(s: &str, max_len: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if max_len == 0 {
        return String::new();
    }
    let graphemes: Vec<&str> = s.graphemes(true).collect();
    if graphemes.len() <= max_len {
        return s.to_owned();
    }
    let truncated = graphemes.get(..max_len.saturating_sub(1)).unwrap_or(&[]);
    let mut result: String = truncated.concat();
    result.push('…');
    result
}
