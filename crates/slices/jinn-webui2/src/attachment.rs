//! Image ingestion at the `/web2` send boundary.
//!
//! The composer's three image sources — pasted clipboard bytes, picked
//! files, and image URLs scanned out of the message text — all arrive at
//! the server as base64 bytes (or, for a URL, as bytes the server fetches).
//! Everything here happens when the message is sent: bytes are sniffed by
//! magic number, non-native formats transcode to PNG through
//! [`jinn_llm_support`'s converter][svc] (the `read_image` tool's
//! machinery, shared as a library crate), and each image is capped before
//! conversion. A refusal is one sentence naming the offender — the
//! original web UI's rules and sentences, copied because no slice may
//! depend on that slice's crate.
//!
//! [svc]: jinn_llm_support::image_convert::ImageConverterService

use std::time::Duration;

use base64::Engine as _;
use jinn_core_types::attachment::Attachment;
use jinn_webui2_msg::ImageUploadBody;

/// Per-image size cap, byte-for-byte the `read_image` tool's rule and
/// Anthropic's per-image request limit: an oversized image refuses at the
/// boundary with guidance instead of failing the turn later with no cap.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// The URL extension gate — an https link is fetched only when its path
/// ends in one of these. Anything else (a blog post, a page that happens
/// to embed images) is never fetched: the extension is the user's
/// assertion that the link names an image.
const IMAGE_URL_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// How long one image-URL fetch may take before the send is refused.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The URL extension gate: whether a link names an image worth fetching.
///
/// The scheme must be `https`, the path's last segment must end
/// (case-insensitively) in one of the image extensions. Query strings and
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
    // A dotfile (`…/.png`) has an empty stem and no extension: the name
    // must be non-empty before the dot for the dot to separate one.
    let Some((stem, extension)) = last_segment.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    IMAGE_URL_EXTENSIONS
        .iter()
        .any(|allowed| extension.eq_ignore_ascii_case(allowed))
}

/// Scans message text for image URLs, in order of appearance, deduplicated.
///
/// The scan is deliberately shallow: `https://`-prefixed tokens running to
/// the next whitespace, gated by [`is_image_url`]. A URL that fails the gate
/// is skipped — it stays a plain link in the message, exactly as on the
/// original web UI.
#[must_use]
pub fn scan_image_urls(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|token| token.trim_end_matches(['.', ',', ')', ';', ':', '!']))
        .filter(|trimmed| is_image_url(trimmed))
        .fold(Vec::new(), |mut urls, trimmed| {
            if !urls.iter().any(|url| url == trimmed) {
                urls.push(trimmed.to_owned());
            }
            urls
        })
}

/// Decodes one composer upload into bytes, naming the chip position on
/// failure.
///
/// Two wire shapes are accepted: bare base64 (what the page sends) and a
/// whole `data:<mime>;base64,<…>` URL (what a hand-rolled client may
/// send). The shape is decided by the leading `data:`; the declared mime is
/// never trusted — sniffing decides the format.
///
/// # Errors
///
/// The refusal sentence when the payload does not decode.
pub fn decode_upload(value: &str, index: usize) -> Result<Vec<u8>, String> {
    let encoded = match value.strip_prefix("data:") {
        Some(rest) => rest.split_once(',').map_or("", |(_, payload)| payload),
        None => value,
    };
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_e| format!("image {index} is not a valid base64 data URL"))
}

/// Ingests every image a send carries: the composer uploads in chip order,
/// then the scanned image URLs in text order.
///
/// Blocking work (decode, sniff, convert) and the network fetch run off
/// the dispatch's thread. The first refusal aborts the whole send and
/// names its offender.
///
/// # Errors
///
/// One sentence, for the first image that fails — a chip's position, or
/// the URL.
pub async fn ingest_send_images(
    text: &str,
    uploads: &[ImageUploadBody],
) -> Result<Vec<Attachment>, String> {
    let urls = scan_image_urls(text);
    if uploads.is_empty() && urls.is_empty() {
        return Ok(Vec::new());
    }

    let converter = jinn_llm_support::image_convert::ImageConverterService::system();
    let mut attachments = Vec::new();

    // Composer uploads, in chip order.
    for (index, image) in uploads.iter().enumerate() {
        let bytes = decode_upload(&image.data_base64, index)?;
        let converter = converter.clone();
        let ingested = tokio::task::spawn_blocking(move || ingest_bytes(bytes, index, &converter))
            .await
            .map_err(|_join| format!("image {index} could not be ingested"))??;
        attachments.push(ingested);
    }

    // Scanned image URLs, in text order.
    for url in urls {
        let bytes = fetch_image_url(&url).await?;
        let converter = converter.clone();
        let url_for_task = url.clone();
        let ingested =
            tokio::task::spawn_blocking(move || ingest_fetched(&url_for_task, bytes, &converter))
                .await
                .map_err(|_join| format!("image URL {url} could not be ingested"))??;
        attachments.push(ingested);
    }

    Ok(attachments)
}

/// Ingests one fetched URL's bytes into an attachment.
fn ingest_fetched(
    url: &str,
    bytes: Vec<u8>,
    converter: &jinn_llm_support::image_convert::ImageConverterService,
) -> Result<Attachment, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(oversize_sentence(
            &format!("the image at {url}"),
            bytes.len(),
        ));
    }
    ingest_bytes(bytes, usize::MAX, converter).map_err(|error| match error {
        // The upload-shaped sentences name a chip position; for a URL the
        // offender is the address.
        ref upload if upload.contains("is not a recognized image format") => {
            format!("the content at {url} is not a recognized image format")
        }
        ref upload if upload.contains("no converter is available") => {
            format!("the image at {url} needs conversion (e.g. HEIC) but no converter is available")
        }
        otherwise => otherwise,
    })
}

/// The cap, then the classify-or-convert decision over bytes.
fn ingest_bytes(
    bytes: Vec<u8>,
    index: usize,
    converter: &jinn_llm_support::image_convert::ImageConverterService,
) -> Result<Attachment, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(oversize_sentence(&format!("image {index}"), bytes.len()));
    }
    jinn_llm_support::image_convert::ingest_image_bytes_blocking(bytes, converter).map_err(
        |error| match error {
            jinn_llm_support::image_convert::ImageBytesError::NotAnImage => {
                format!("image {index} is not a recognized image format")
            }
            jinn_llm_support::image_convert::ImageBytesError::ConversionUnavailable => {
                format!("image {index} needs conversion (e.g. HEIC) but no converter is available")
            }
        },
    )
}

/// Fetches one gated image URL's bytes, with a timeout so a slow host
/// delays but never hangs the send. The cap rides here too: the body is
/// refused once it passes [`MAX_IMAGE_BYTES`].
///
/// # Errors
///
/// `FetchFailed` on transport, status, or timeout — one sentence naming the
/// URL.
async fn fetch_image_url(url: &str) -> Result<Vec<u8>, String> {
    let response = http_client()
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|_e| format!("image URL {url} could not be fetched"))?;
    if !response.status().is_success() {
        return Err(format!("image URL {url} could not be fetched"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_e| format!("image URL {url} could not be fetched"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(oversize_sentence(
            &format!("the image at {url}"),
            bytes.len(),
        ));
    }
    Ok(bytes.to_vec())
}

/// The oversize sentence: the offender, its size, the cap, and the way out.
fn oversize_sentence(offender: &str, bytes: usize) -> String {
    format!(
        "{offender} is {bytes} bytes; the limit is {MAX_IMAGE_BYTES} — resize it first \
         (e.g. an image tool) and re-attach"
    )
}

/// The shared HTTP client for URL-attachment fetches (the same pinned
/// reqwest shape the provider backends use; the rustls provider is
/// installed at process start).
///
/// The e2e tier's loopback TLS fixture serves a self-signed cert; when
/// `JINN_WEBUI_TEST_ROOT_CERT` names a PEM file the client trusts it *in
/// addition to* the system roots. The var is a test-only knob read once at
/// client construction — production never sets it, and the standard trust
/// store is always in place, so this cannot weaken a real fetch.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::LazyLock<reqwest::Client> =
        std::sync::LazyLock::new(|| match std::env::var("JINN_WEBUI_TEST_ROOT_CERT") {
            Ok(path) => match std::fs::read(&path)
                .ok()
                .and_then(|pem| reqwest::Certificate::from_pem(&pem).ok())
            {
                Some(cert) => reqwest::Client::builder()
                    .add_root_certificate(cert)
                    .build()
                    .unwrap_or_else(|_e| reqwest::Client::new()),
                None => reqwest::Client::new(),
            },
            Err(_) => reqwest::Client::new(),
        });
    &CLIENT
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
    #[case("https://example.com/photo.gif", true)]
    #[case("https://example.com/page", false)]
    #[case("https://example.com/page.html", false)]
    #[case("https://example.com/.png", false)]
    fn image_url_gate(#[case] url: &str, #[case] expected: bool) {
        // Given a URL.
        // When the gate reads it.
        let gate = is_image_url(url);

        // Then it answers whether the link names an image.
        assert_eq!(gate, expected);
    }

    #[rstest]
    #[test]
    fn scan_finds_every_image_url_once() {
        // Given text naming the same image twice and a page once.
        let text = "see https://x.test/a.png and https://x.test/a.png and https://x.test/post";

        // When scanning.
        let urls = scan_image_urls(text);

        // Then the image is listed once and the page is not listed.
        assert_eq!(urls, ["https://x.test/a.png"]);
    }

    #[rstest]
    #[test]
    fn scan_trims_trailing_punctuation() {
        // Given an image URL a sentence swallows.
        let text = "at https://x.test/a.png.";

        // When scanning.
        let urls = scan_image_urls(text);

        // Then the period is not part of the address.
        assert_eq!(urls, ["https://x.test/a.png"]);
    }

    #[rstest]
    #[test]
    fn decode_accepts_bare_base64() {
        // Given a bare base64 payload.
        let bytes = decode_upload("aGVsbG8=", 0).expect("decodes");

        // Then the bytes come back.
        assert_eq!(bytes, b"hello");
    }

    #[rstest]
    #[test]
    fn decode_accepts_a_data_url() {
        // Given a whole data URL.
        let bytes = decode_upload("data:image/png;base64,aGVsbG8=", 0).expect("decodes");

        // Then the bytes come back.
        assert_eq!(bytes, b"hello");
    }

    #[rstest]
    #[test]
    fn decode_refuses_an_undecodable_payload_naming_the_chip() {
        // Given a payload that is not base64.
        let refusal = decode_upload("***", 2).expect_err("refuses");

        // Then the sentence names the chip.
        assert_eq!(refusal, "image 2 is not a valid base64 data URL");
    }

    #[rstest]
    #[test]
    fn an_upload_over_the_cap_is_refused_before_conversion() {
        // Given bytes one over the cap and a converter.
        let converter = jinn_llm_support::image_convert::ImageConverterService::system();
        let bytes = vec![0u8; MAX_IMAGE_BYTES + 1];

        // When ingesting.
        let refusal = ingest_bytes(bytes, 0, &converter).expect_err("refuses");

        // Then the sentence names the size and the cap.
        assert!(refusal.contains("is 5242881 bytes; the limit is 5242880"));
    }

    #[rstest]
    #[test]
    fn bytes_that_are_not_an_image_are_refused() {
        // Given bytes that are not a recognizable image.
        let converter = jinn_llm_support::image_convert::ImageConverterService::system();

        // When ingesting.
        let refusal = ingest_bytes(b"not an image".to_vec(), 1, &converter).expect_err("refuses");

        // Then the sentence names the chip.
        assert_eq!(refusal, "image 1 is not a recognized image format");
    }

    #[rstest]
    #[test]
    fn a_real_png_ingests_as_an_image_attachment() {
        // Given the smallest image the sniffer accepts.
        let converter = jinn_llm_support::image_convert::ImageConverterService::system();
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
            .expect("base64");

        // When ingesting.
        let attachment = ingest_bytes(png, 0, &converter).expect("ingests");

        // Then it is a PNG image attachment.
        assert_eq!(attachment.media_type(), "image/png");
    }
}
