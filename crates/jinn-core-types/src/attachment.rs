//! Multimodal content attachments for LLM messages.
//!
//! [`Attachment`] is the protocol-level representation of non-text content
//! (images, and future media types) carried alongside the text of an
//! [`crate::llm_message::LlmMessage::User`] message. It is provider-agnostic: raw bytes are
//! stored, and per-provider base64 encoding happens in the request builders.

use serde::{Deserialize, Serialize};

/// A non-text content attachment on a user message.
///
/// Designed as an extensible enum: only [`Attachment::Image`] is populated
/// today, but future variants (e.g. `Audio`) can be added without changing the
/// `LlmMessage::User` / `ChatEntryKind::User` shapes that carry `Vec<Attachment>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attachment {
    /// An image attachment.
    Image {
        /// The image MIME type (e.g. `"image/png"`, `"image/jpeg"`).
        ///
        /// Guides the provider request block shape and the serialized data URL.
        media_type: String,
        /// Raw decoded image bytes.
        ///
        /// Stored as-is (not base64); base64 encoding is applied only when
        /// building provider HTTP request bodies.
        data: Vec<u8>,
    },
}

impl Attachment {
    /// Constructs an image attachment from raw bytes and a media type.
    #[must_use]
    pub fn image(media_type: String, data: Vec<u8>) -> Self {
        Self::Image { media_type, data }
    }

    /// Returns the MIME media type of this attachment.
    #[must_use]
    pub fn media_type(&self) -> &str {
        match self {
            Self::Image { media_type, .. } => media_type,
        }
    }

    /// Returns the raw (unencoded) bytes of this attachment.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        match self {
            Self::Image { data, .. } => data,
        }
    }

    /// Returns `true` if this is an image attachment.
    #[must_use]
    pub fn is_image(&self) -> bool {
        matches!(self, Self::Image { .. })
    }

    /// Renders this attachment as a base64 data URL (e.g.
    /// `data:image/png;base64,iVBOR...`).
    ///
    /// Used by provider request builders that embed image bytes inline.
    #[must_use]
    pub fn data_url(&self) -> String {
        use base64::Engine as _;
        match self {
            Self::Image { media_type, data } => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                format!("data:{media_type};base64,{encoded}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    fn image_attachment_roundtrips() {
        // Given an image attachment.
        let attachment = Attachment::image("image/png".to_owned(), vec![1, 2, 3]);

        // When serializing and deserializing.
        let json = serde_json::to_string(&attachment).expect("serialize");
        let back: Attachment = serde_json::from_str(&json).expect("deserialize");

        // Then it roundtrips.
        assert_eq!(back, attachment);
    }

    #[rstest::rstest]
    fn image_attachment_tagged_with_kind() {
        // Given an image attachment.
        let attachment = Attachment::image("image/png".to_owned(), vec![]);

        // When serializing.
        let json = serde_json::to_string(&attachment).expect("serialize");

        // Then the JSON is tagged with "kind": "image".
        assert!(json.contains(r#""kind":"image""#));
        assert!(json.contains(r#""media_type":"image/png""#));
    }

    #[rstest::rstest]
    fn accessors_return_image_fields() {
        // Given an image attachment.
        let attachment = Attachment::image("image/jpeg".to_owned(), vec![10, 20, 30]);

        // Then accessors return the expected fields.
        assert_eq!(attachment.media_type(), "image/jpeg");
        assert_eq!(attachment.data(), &[10, 20, 30]);
        assert!(attachment.is_image());
    }

    #[rstest::rstest]
    fn data_url_emits_base64_data_url() {
        // Given an image attachment.
        let attachment = Attachment::image("image/png".to_owned(), vec![1, 2, 3]);

        // When rendering the data URL.
        let url = attachment.data_url();

        // Then it is a base64 data URL with the media type.
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(url.ends_with("AQID"));
    }
}
