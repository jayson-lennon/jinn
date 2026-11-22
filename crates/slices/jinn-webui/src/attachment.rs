//! Image ingestion at the web send boundary.
//!
//! The web composer's three image sources — pasted clipboard bytes, picked
//! files, and image URLs scanned out of the message text — all arrive as
//! bytes (or data URLs decoding to bytes). Everything here happens at the
//! POST: bytes are sniffed by magic number, non-native formats transcode to
//! PNG through [`jinn_llm_support`'s converter][svc] (the `read_image` tool's
//! machinery, shared as a library crate), and each image is capped before
//! conversion and before any persistence. A refusal is a typed
//! [`IngestError`] the caller maps to a 400 naming the offender — the web
//! POST is where the user is looking, unlike the TUI's silent-degrade path.
//!
//! [svc]: jinn_llm_support::image_convert::ImageConverterService

use base64::Engine as _;
use wherror::Error;

/// Per-image size cap, byte-for-byte the `read_image` tool's rule and
/// Anthropic's per-image request limit: an oversized image refuses at the
/// boundary with guidance instead of failing the turn later with no cap.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// The URL extension gate — an https link is fetched only when its path
/// ends in one of these. Anything else (a blog post, a page that happens to
/// embed images) is never fetched: the extension is the user's assertion
/// that the link names an image.
const IMAGE_URL_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// Why a web image could not become an attachment.
///
/// Every variant maps to a 400 whose sentence names the offender — the
/// upload's chip index or the URL — matching the web action-refusal
/// precedent (`EmptyText`, `UnknownModel`).
#[derive(Debug, Error)]
#[error(debug)]
pub enum IngestError {
    /// The data URL on the wire was not a parseable `data:<mime>;base64,<…>`
    /// shape, or its base64 payload did not decode.
    BadDataUrl(usize),
    /// The decoded bytes exceeded [`MAX_IMAGE_BYTES`].
    Oversize {
        /// Which pending image (0-based chip position).
        index: usize,
        /// The image's actual size, for the refusal sentence.
        bytes: usize,
    },
    /// The bytes are not a recognizable image format.
    NotAnImage(usize),
    /// The image needs conversion but the converter chain failed.
    ConversionUnavailable(usize),
    /// A scanned image URL was not `https`.
    InsecureUrl(String),
    /// The scanned URL's path carries no image extension, so it is not
    /// fetched (surfaced only when the caller asks to refuse rather than
    /// skip — the send path skips).
    DisallowedUrl(String),
    /// The URL fetch failed (transport error, non-2xx status, or a body
    /// exceeding the cap).
    FetchFailed(String),
    /// The fetched bytes were not an image.
    UrlNotAnImage(String),
    /// The fetched image needed conversion and the chain failed.
    UrlConversionUnavailable(String),
}

impl IngestError {
    /// The refusal sentence the send response body carries.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::BadDataUrl(index) => {
                format!("image {index} is not a valid base64 data URL")
            }
            Self::Oversize { index, bytes } => format!(
                "image {index} is {bytes} bytes; the limit is {MAX_IMAGE_BYTES} — resize it \
                 first (e.g. an image tool) and re-attach"
            ),
            Self::NotAnImage(index) => {
                format!("image {index} is not a recognized image format")
            }
            Self::ConversionUnavailable(index) => {
                format!("image {index} needs conversion (e.g. HEIC) but no converter is available")
            }
            Self::InsecureUrl(url) => {
                format!("only https image URLs are fetched; {url} is not https")
            }
            Self::DisallowedUrl(url) => {
                format!("{url} does not end in an image extension (png, jpg, jpeg, gif, webp)")
            }
            Self::FetchFailed(url) => format!("image URL {url} could not be fetched"),
            Self::UrlNotAnImage(url) => {
                format!("the content at {url} is not a recognized image format")
            }
            Self::UrlConversionUnavailable(url) => format!(
                "the image at {url} needs conversion (e.g. HEIC) but no converter is available"
            ),
        }
    }

    /// The HTTP status this refusal maps to — every ingestion refusal is a
    /// client error: the bytes or the URL came from the browser.
    #[must_use]
    pub const fn status(&self) -> u16 {
        400
    }
}

/// One image the send path ingested successfully.
#[derive(Debug)]
pub struct Ingested {
    /// The attachment (native bytes as-is, or transcoded PNG).
    pub attachment: jinn_provider::Attachment,
    /// How the image arrived — the refusal sentence's subject when a
    /// sibling image in the same send failed.
    pub source: ImageSource,
}

/// Where an ingested image came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// A composer upload or paste, at its chip position.
    Upload(usize),
    /// A scanned URL, as it appeared in the text.
    Url(String),
}

/// The URL extension gate: whether a link names an image worth fetching.
///
/// The scheme must be `https`, the path's last segment must end (case-
/// insensitively) in one of the image extensions. Query strings and
/// fragments do not participate — the gate reads the path only.
#[must_use]
pub fn is_image_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    // The path ends at the first `?` or `#`; the extension gate reads the
    // path's last segment only (`…/photo.png?width=800` is an image; the
    // query is not part of the name).
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    let last_segment = path.rsplit('/').next().unwrap_or_default();
    let Some((_, extension)) = last_segment.rsplit_once('.') else {
        return false;
    };
    IMAGE_URL_EXTENSIONS
        .iter()
        .any(|allowed| extension.eq_ignore_ascii_case(allowed))
}

/// Scans message text for image URLs, in order of appearance, deduplicated.
///
/// The scan is deliberately shallow: `https://`-prefixed tokens running to
/// the next whitespace, gated by [`is_image_url`]. A URL that fails the gate
/// is skipped — it stays a plain link in the message, exactly the Slack /
/// Discord behavior the user chose.
#[must_use]
pub fn scan_image_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for token in text.split_whitespace() {
        // Trailing punctuation a sentence swallows (`... at https://x/y.png.`)
        // is not part of the URL when it trails the extension.
        let trimmed = token.trim_end_matches(['.', ',', ')', ';', ':', '!']);
        if is_image_url(trimmed) && !urls.contains(&trimmed.to_owned()) {
            urls.push(trimmed.to_owned());
        }
    }
    urls
}

/// Decodes a wire data URL into bytes, naming the chip position on failure.
///
/// # Errors
///
/// [`IngestError::BadDataUrl`] when the string is not a `data:<mime>;base64,<…>`
/// shape or its payload does not decode.
pub fn decode_data_url(data_url: &str, index: usize) -> Result<(String, Vec<u8>), IngestError> {
    let Some((metadata, encoded)) = data_url.split_once(',') else {
        return Err(IngestError::BadDataUrl(index));
    };
    let mime = metadata
        .strip_prefix("data:")
        .and_then(|rest| rest.strip_suffix(";base64"))
        .ok_or(IngestError::BadDataUrl(index))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_e| IngestError::BadDataUrl(index))?;
    Ok((mime.to_owned(), bytes))
}

/// Ingests one composer upload (data URL from the wire) into an attachment.
///
/// Blocking — the caller runs it inside `spawn_blocking` alongside its
/// siblings.
///
/// # Errors
///
/// Every failure mode is a typed [`IngestError`]: cap before conversion,
/// sniff before anything.
pub fn ingest_upload(
    data_url: &str,
    index: usize,
    converter: &jinn_llm_support::image_convert::ImageConverterService,
) -> Result<Ingested, IngestError> {
    let (_declared_mime, bytes) = decode_data_url(data_url, index)?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(IngestError::Oversize {
            index,
            bytes: bytes.len(),
        });
    }
    let attachment = ingest_bytes(bytes, index, converter)?;
    Ok(Ingested {
        attachment,
        source: ImageSource::Upload(index),
    })
}

/// Ingests one fetched URL's bytes into an attachment.
///
/// Blocking — called inside `spawn_blocking` after the fetch.
///
/// # Errors
///
/// Cap first, then sniff; [`IngestError::UrlNotAnImage`] when the body is
/// not an image, [`IngestError::UrlConversionUnavailable`] when a
/// conversion-needing image cannot be transcoded.
pub fn ingest_fetched(
    url: &str,
    bytes: Vec<u8>,
    converter: &jinn_llm_support::image_convert::ImageConverterService,
) -> Result<Ingested, IngestError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(IngestError::Oversize {
            index: usize::MAX,
            bytes: bytes.len(),
        });
    }
    let attachment = ingest_bytes(bytes, usize::MAX, converter).map_err(|error| match error {
        IngestError::NotAnImage(_) => IngestError::UrlNotAnImage(url.to_owned()),
        IngestError::ConversionUnavailable(_) => {
            IngestError::UrlConversionUnavailable(url.to_owned())
        }
        otherwise => otherwise,
    })?;
    Ok(Ingested {
        attachment,
        source: ImageSource::Url(url.to_owned()),
    })
}

/// The classify-or-convert decision over bytes, mapping converter outcomes
/// to ingest refusals.
fn ingest_bytes(
    bytes: Vec<u8>,
    index: usize,
    converter: &jinn_llm_support::image_convert::ImageConverterService,
) -> Result<jinn_provider::Attachment, IngestError> {
    jinn_llm_support::image_convert::ingest_image_bytes_blocking(bytes, converter).map_err(
        |error| match error {
            jinn_llm_support::image_convert::ImageBytesError::NotAnImage => {
                IngestError::NotAnImage(index)
            }
            jinn_llm_support::image_convert::ImageBytesError::ConversionUnavailable => {
                IngestError::ConversionUnavailable(index)
            }
        },
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("https://example.com/photo.png", true)]
    #[case("https://example.com/photo.PNG", true)]
    #[case("https://example.com/deep/dir/photo.jpeg", true)]
    #[case("https://example.com/photo.png?width=800", true)]
    #[case("https://example.com/photo.webp#fragment", true)]
    #[case("https://example.com/photo.jpg", true)]
    #[case("http://example.com/photo.png", false)]
    #[case("https://example.com/page.html", false)]
    #[case("https://example.com/noextension", false)]
    #[case("https://example.com/", false)]
    #[case("ftp://example.com/photo.png", false)]
    #[case("https://example.com/photo.png.evil", false)]
    fn url_gate_admits_only_image_extension_https_links(#[case] url: &str, #[case] expected: bool) {
        assert_eq!(is_image_url(url), expected, "gate verdict for {url}");
    }

    #[rstest]
    fn scan_collects_image_urls_in_order_and_skips_other_links() {
        // Given text mixing an image link, an ordinary link, and prose.
        let text = "look at https://x.com/a.png and read https://y.com/article.html then \
                    https://z.com/b.gif";

        // When scanning.
        let urls = scan_image_urls(text);

        // Then only the two image links, in text order.
        assert_eq!(urls, vec!["https://x.com/a.png", "https://z.com/b.gif"]);
    }

    #[rstest]
    fn scan_deduplicates_repeated_urls() {
        // Given text mentioning the same image twice.
        let text = "https://x.com/a.png and again https://x.com/a.png";

        // When scanning.
        let urls = scan_image_urls(text);

        // Then one fetch.
        assert_eq!(urls, vec!["https://x.com/a.png"]);
    }

    #[rstest]
    fn scan_strips_trailing_sentence_punctuation() {
        // Given a URL a sentence's period rides on.
        let text = "see https://x.com/a.png.";

        // When scanning.
        let urls = scan_image_urls(text);

        // Then the period is not part of the fetched URL.
        assert_eq!(urls, vec!["https://x.com/a.png"]);
    }

    #[rstest]
    fn scan_of_empty_text_is_empty() {
        // Given empty text.
        // When scanning.
        let urls = scan_image_urls("");
        // Then nothing is collected.
        assert_eq!(urls, Vec::<String>::new());
    }

    #[rstest]
    fn data_url_roundtrip_decodes_mime_and_bytes() {
        // Given a data URL the browser's FileReader shape produces.
        let data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nbody")
        );

        // When decoding.
        let (mime, bytes) = decode_data_url(&data_url, 0).expect("decodes");

        // Then the mime and bytes come back.
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"\x89PNG\r\n\x1a\nbody");
    }

    #[rstest]
    #[case("not-a-data-url")]
    #[case("data:image/png;base64")]
    #[case("data:image/png,not-base64!")]
    fn malformed_data_urls_name_their_index(#[case] data_url: &str) {
        let error = decode_data_url(data_url, 2).unwrap_err();
        assert!(matches!(error, IngestError::BadDataUrl(2)));
    }

    #[rstest]
    fn ingest_upload_refuses_oversize_before_sniffing() {
        // Given a data URL whose bytes exceed the cap but carry a valid
        // native PNG prefix — the cap must fire first.
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.resize(MAX_IMAGE_BYTES + 1, 0);
        let data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        let converter = jinn_llm_support::image_convert::ImageConverterService::unavailable();

        // When ingesting.
        let error = ingest_upload(&data_url, 1, &converter).unwrap_err();

        // Then the refusal is the cap, not a sniff verdict.
        assert!(matches!(error, IngestError::Oversize { index: 1, .. }));
    }

    #[rstest]
    fn ingest_upload_attaches_native_png() {
        // Given a data URL carrying real PNG bytes.
        let png = tiny_png();
        let data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        let converter = jinn_llm_support::image_convert::ImageConverterService::system();

        // When ingesting.
        let ingested = ingest_upload(&data_url, 0, &converter).expect("attaches");

        // Then the attachment is the original PNG, sourced as upload 0.
        assert_eq!(ingested.attachment.media_type(), "image/png");
        assert_eq!(ingested.attachment.data(), png.as_slice());
        assert_eq!(ingested.source, ImageSource::Upload(0));
    }

    #[rstest]
    fn ingest_upload_refuses_non_image_bytes() {
        // Given a data URL whose bytes are HTML.
        let data_url = format!(
            "data:text/html;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(b"<html></html>")
        );
        let converter = jinn_llm_support::image_convert::ImageConverterService::system();

        // When ingesting.
        let error = ingest_upload(&data_url, 3, &converter).unwrap_err();

        // Then the refusal names the chip and its reason.
        assert!(matches!(error, IngestError::NotAnImage(3)));
        assert!(error.sentence().contains('3'));
    }

    #[rstest]
    fn ingest_fetched_maps_sniff_verdicts_to_url_named_refusals() {
        // Given fetched bytes that are not an image.
        let converter = jinn_llm_support::image_convert::ImageConverterService::system();

        // When ingesting them against a URL.
        let error =
            ingest_fetched("https://x.com/a.png", b"<html/>".to_vec(), &converter).unwrap_err();

        // Then the refusal names the URL, not an index.
        assert!(matches!(
            error,
            IngestError::UrlNotAnImage(ref url) if url == "https://x.com/a.png"
        ));
        assert!(error.sentence().contains("https://x.com/a.png"));
    }

    #[rstest]
    fn ingest_fetched_cap_refuses_before_sniffing() {
        // Given fetched bytes over the cap with a valid PNG prefix.
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.resize(MAX_IMAGE_BYTES + 1, 0);
        let converter = jinn_llm_support::image_convert::ImageConverterService::unavailable();

        // When ingesting.
        let error = ingest_fetched("https://x.com/a.png", bytes, &converter).unwrap_err();

        // Then the cap fired (no conversion attempted).
        assert!(matches!(error, IngestError::Oversize { .. }));
    }

    #[rstest]
    fn every_refusal_maps_to_client_error_status() {
        // Given one instance of each refusal shape.
        let errors = [
            IngestError::BadDataUrl(0),
            IngestError::Oversize { index: 0, bytes: 6 },
            IngestError::NotAnImage(0),
            IngestError::ConversionUnavailable(0),
            IngestError::InsecureUrl("http://x/y.png".to_owned()),
            IngestError::DisallowedUrl("https://x/y.html".to_owned()),
            IngestError::FetchFailed("https://x/y.png".to_owned()),
            IngestError::UrlNotAnImage("https://x/y.png".to_owned()),
            IngestError::UrlConversionUnavailable("https://x/y.png".to_owned()),
        ];

        // When reading each status and sentence.
        // Then every status is 400 and every sentence is non-empty.
        for error in errors {
            assert_eq!(error.status(), 400);
            let sentence = error.sentence();
            assert!(!sentence.is_empty(), "sentence for {error:?} is empty");
        }
    }

    /// A 1x1 PNG generated in-process — no fixture file.
    fn tiny_png() -> Vec<u8> {
        use std::io::Cursor;
        let img = image::DynamicImage::new(1, 1, image::ColorType::Rgb8);
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .expect("1x1 png encodes");
        out.into_inner()
    }
}
