//! Inter-session message rendering — the user renderer on its own background.
//!
//! The entry's text is self-describing: the sender bakes the
//! `[Message from session <id> (<title>)]: ` attribution into it at send
//! time. It renders through the same markdown pipeline as a user entry —
//! identical foreground treatment — on the `intersession_bg` block instead
//! of the user block, with no `@token` recoloring (tokens resolve at
//! user-enqueue time only). The kind survives as the styling key; no
//! header, no ids, no route chrome.

use ratatui::text::Line;

use jinn_core_types::AttachmentOutcome;

use super::RenderContext;
use super::user::text_block_lines;

pub fn to_lines(text: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    text_block_lines(
        text,
        &AttachmentOutcome::default(),
        ctx.theme.intersession_bg,
        ctx,
    )
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
    use crate::chat_log::EntryGraphics;
    use jinn_theme::default_theme;

    fn render_ctx() -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: EntryGraphics::default(),
            session_id: jinn_core_types::SessionId::new(),
        }
    }

    fn all_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.clone()))
            .collect()
    }

    #[rstest::rstest]
    fn the_attributed_text_renders_plain_with_no_header() {
        // Given a message whose text already carries the sender's attribution.
        let ctx = render_ctx();

        // When rendering.
        let lines = to_lines(
            "[Message from session abc-123]: handing off the stage",
            &ctx,
        );

        // Then the full text renders and no route chrome is added.
        let text = all_text(&lines);
        assert!(
            text.contains("[Message from session abc-123]: handing off the stage"),
            "the attributed text renders: {text:?}"
        );
        assert!(!text.contains('↔'), "no header arrow: {text:?}");
        assert!(!text.contains("→"), "no route arrow: {text:?}");
    }

    #[rstest::rstest]
    fn every_span_carries_the_intersession_background() {
        // Given a rendered inter-session message.
        let ctx = render_ctx();

        // When rendering.
        let lines = to_lines("[Message from session abc]: stage done", &ctx);

        // Then every span — content and full-width padding alike — carries
        // the inter-session block background.
        let theme = default_theme();
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| span.style.bg == Some(theme.intersession_bg)),
            "text should sit on intersession_bg"
        );
    }

    #[rstest::rstest]
    fn blank_padding_rows_carry_the_intersession_background() {
        // Given a rendered inter-session message.
        let ctx = render_ctx();

        // When rendering.
        let lines = to_lines("[Message from session abc]: stage done", &ctx);

        // Then the blank rows above and below the content carry the block
        // background, exactly as the user block's padding rows do.
        let theme = default_theme();
        assert!(lines.len() >= 3, "padding rows flank the content");
        for (which, line) in [
            ("first", lines.first().expect("first")),
            ("last", lines.last().expect("last")),
        ] {
            assert!(
                line.spans
                    .iter()
                    .all(|span| span.style.bg == Some(theme.intersession_bg)),
                "{which} padding row should carry intersession_bg"
            );
        }
    }

    #[rstest::rstest]
    fn foreground_treatment_matches_the_user_entry() {
        // Given the same body rendered as a user entry and as an inter-session
        // entry.
        let ctx = render_ctx();
        let body = "[Message from session abc]: stage **done**";
        let user_lines = super::super::user::to_lines(body, &AttachmentOutcome::default(), &ctx);

        // When rendering it as an inter-session entry.
        let intersession_lines = to_lines(body, &ctx);

        // Then the two differ only in block background: the same line count,
        // and the same foregrounds in the same order.
        assert_eq!(
            intersession_lines.len(),
            user_lines.len(),
            "same renderer, same shape"
        );
        for (u, s) in user_lines.iter().zip(intersession_lines.iter()) {
            for (us, ss) in u.spans.iter().zip(s.spans.iter()) {
                assert_eq!(us.style.fg, ss.style.fg, "same foreground treatment");
                assert_eq!(us.content, ss.content, "same content segmentation");
                assert_ne!(
                    us.style.bg, ss.style.bg,
                    "the block background is the only difference"
                );
            }
        }
    }

    #[rstest::rstest]
    fn embedded_newline_renders_as_a_soft_break_in_one_paragraph() {
        // Given a message with an embedded newline.
        let ctx = render_ctx();

        // When rendering.
        let lines = to_lines("[Message from session abc]: first\nsecond", &ctx);

        // Then both segments render (a soft break inside one paragraph, not a
        // per-segment line split), every span carries the block background,
        // and the padding rows flank the content.
        let text = all_text(&lines);
        assert!(text.contains("first"), "first segment renders: {text:?}");
        assert!(text.contains("second"), "second segment renders: {text:?}");
        let theme = default_theme();
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| span.style.bg == Some(theme.intersession_bg)),
            "every span sits on intersession_bg"
        );
    }

    #[rstest::rstest]
    fn no_span_carries_attachment_outcome_colors() {
        // Given a message whose text contains an `@`-token-shaped word that was
        // never resolved as an attachment.
        let ctx = render_ctx();

        // When rendering.
        let theme = render_ctx().theme;
        let lines = to_lines("[Message from session abc]: see @unresolved.png", &ctx);

        // Then no span carries the success or error_text color — the
        // inter-session pass applies no `@token` attachment coloring.
        let any_special = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| matches!(s.style.fg, Some(c) if c == theme.success || c == theme.error_text));
        assert!(
            !any_special,
            "inter-session render must not apply attachment coloring"
        );
    }
}
