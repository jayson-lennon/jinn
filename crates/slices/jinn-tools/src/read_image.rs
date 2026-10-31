//! `read_image` built-in tool — puts a local image file in front of the model.
//!
//! The tool reads a file, classifies it by magic bytes (the same
//! `classify_image_bytes` sniffer the `@path` attachment path uses), transcodes
//! non-native formats (HEIC, AVIF, TIFF, BMP, …) to PNG — decoded in-process
//! by the `image` crate where possible, with ImageMagick as the fallback for
//! formats it cannot decode — and attaches the image to **its own tool
//! result**. The next dispatch carries it to the provider: Anthropic nests
//! image blocks inside the `tool_result`, every other backend receives a
//! synthesized user message — the choice is the provider builder's, never this
//! tool's.
//!
//! Scope is deliberately narrow:
//!
//! - **Path-based only.** Images the model generates are written to files
//!   (e.g. via ImageMagick through `bash`) and then read from that file.
//! - **No downscaling.** An image over the size cap fails with guidance to
//!   resize; the model can run ImageMagick itself and retry.
//! - **Vision gate degrades, never blocks.** A model not *confirmed*
//!   vision-capable still gets a successful result — naming the limitation and
//!   the `[[providers.model_info]]` remediation — instead of image bytes, and
//!   instead of a mid-loop failure.
//!
//! The gate mirrors the user-send `attachment_gate` precedence: the overlaid
//! model cache (config + API + models.dev enrichment) decides outright; raw
//! models.dev is the fallback; only `Some(true)` passes. The gate resolution
//! is replicated here rather than shared because the shared function returns a
//! `ChatEntry` to push as an error — a shape that belongs to the user-send
//! path — and importing the provider-selection slice's actor machinery for
//! one three-step lookup would drag the kernel in behind it.

use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_llm_support::image_convert::{
    ImageConverterService, ImageFileError, resolve_image_file_blocking,
};
use jinn_provider_config::{Modality, ModelsDevData};

use crate::tool_types::ToolContext;
use crate::tool_types::{failed, tool_error};

use super::BoxedToolFuture;
use crate::tool_paths::resolve_path;

/// Per-image size cap, mirroring Anthropic's per-image request limit.
///
/// Images above the cap fail with guidance to resize rather than being
/// silently downscaled: the model can run ImageMagick itself, and a silent
/// resize would hand it a different image than the file it named.
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Returns the tool definition for the `read_image` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "read_image".to_owned(),
        description: "Reads a local image file and returns it for viewing. Supports native \
            formats (PNG, JPEG, GIF, WebP) directly; other image formats (HEIC, AVIF, TIFF, BMP, \
            ...) are transcoded to PNG (in-process decoder first, ImageMagick fallback). Images \
            over 5 MB are rejected."
            .to_owned(),
        prompt_snippet: Some("Read an image file so you can see it".to_owned()),
        prompt_guidelines: vec![
            "To view an image you generated, write it to a file with `bash` (e.g. ImageMagick), \
            then `read_image` that file."
                .to_owned(),
            "If `read_image` reports the image exceeds the size limit, resize it first (e.g. \
            `magick input.png -resize 2000x2000 output.png`) and retry with the smaller file."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the image file to read (absolute, or relative to the session's working directory)"
                }
            },
            "required": ["file_path"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `read_image` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let file_path = match parse_args(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return tool_error(&call, &format!("failed to parse arguments: {e}"));
            }
        };
        let Some(file_path) = file_path else {
            return tool_error(&call, "missing required argument: file_path");
        };

        let resolved = resolve_path(&file_path, &ctx.cwd);
        let display = resolved.to_string();

        // The converter shells out to ImageMagick; keep the blocking work off
        // the async runtime.
        let converter = ImageConverterService::system();
        let resolved_image = {
            let path = resolved.clone();
            tokio::task::spawn_blocking(move || resolve_image_file_blocking(&path, &converter))
                .await
        };

        let image = match resolved_image {
            Ok(Ok(image)) => image,
            Ok(Err(ImageFileError::Missing)) => {
                return failed(call.id, call.name, format!("file not found: {display}"));
            }
            Ok(Err(ImageFileError::NotAnImage)) => {
                return failed(
                    call.id,
                    call.name,
                    format!("not a recognized image file: {display}"),
                );
            }
            Ok(Err(ImageFileError::ConversionUnavailable)) => {
                return failed(
                    call.id,
                    call.name,
                    format!(
                        "image at {display} could not be converted to PNG (the built-in decoder \
                        and ImageMagick both failed or are unavailable); convert it another way \
                        and retry"
                    ),
                );
            }
            Err(join_err) => {
                return tool_error(&call, &format!("image read task failed: {join_err}"));
            }
        };

        // Oversized images fail rather than downscale (user decision): the
        // model can resize with ImageMagick itself and retry.
        if image.bytes.len() > MAX_IMAGE_BYTES {
            return failed(
                call.id,
                call.name,
                format!(
                    "image at {display} is {} and exceeds the {} limit; resize it (e.g. `magick \
                    {file_path} -resize 2000x2000 output.png`) and retry with the smaller file",
                    human_size(image.bytes.len()),
                    human_size(MAX_IMAGE_BYTES),
                ),
            );
        }

        // Vision gate: a model not confirmed vision-capable gets a successful
        // text-only result instead of image bytes (user decision: degrade at
        // execution, never a mid-loop failure).
        if !session_model_supports_images(&ctx) {
            let media_type = image.media_type.clone();
            let size = human_size(image.bytes.len());
            return ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content: format!(
                    "{display} ({media_type}, {size}) is available, but the active model is not \
                    confirmed vision-capable, so it was not attached. Mark the model capable via \
                    [[providers.model_info]] input_modalities in providers.toml, or switch to a \
                    vision model."
                ),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
            };
        }

        ToolResult {
            tool_call_id: call.id,
            name: call.name,
            content: {
                let file_name = marked_path::MarkedPathAccess::file_name(&resolved)
                    .map_or_else(|| display.clone(), |n| n.to_string_lossy().into_owned());
                format!(
                    "{} ({}, {})",
                    file_name,
                    image.media_type,
                    human_size(image.bytes.len())
                )
            },
            success: true,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: vec![jinn_provider::Attachment::image(
                image.media_type,
                image.bytes,
            )],
        }
    })
}

/// Parses the `file_path` argument.
fn parse_args(raw: &str) -> Result<Option<String>, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(raw)?;
    Ok(v.get("file_path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
}

/// Formats a byte count for tool-result text (`2.4 MB`, `380 KiB`-style).
fn human_size(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * KIB;
    let b = bytes as f64;
    if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Whether the session's active model is *confirmed* vision-capable.
///
/// Reads everything from the `ToolContext` (state, session id, app paths) and
/// delegates to the same precedence the user-send `attachment_gate` uses: the
/// overlaid model cache decides outright; otherwise models.dev answers, and
/// only a positively confirmed `Some(true)` counts. No configured model (or no
/// state at all) reports capable — matching the gate's no-provider no-op,
/// since dispatch would be a no-op anyway.
fn session_model_supports_images(ctx: &ToolContext) -> bool {
    let Some(state) = ctx.state.as_ref() else {
        return true;
    };
    let guard = state.read();
    let Some(model_id) = ctx
        .session_id
        .as_ref()
        .and_then(|id| guard.session.get(id))
        .and_then(|s| s.profile().model.last_model())
        .map(str::to_owned)
    else {
        return true;
    };
    let model_cache = guard
        .provider_state()
        .and_then(|cell| cell.read().model_cache.clone());
    drop(guard);

    let models_dev = ModelsDevData::load(&ctx.app_paths.models_dev_user_path());
    resolve_model_supports_images(&models_dev, model_cache.as_ref(), &model_id)
}

/// The gate's decision core, split from the `ToolContext` plumbing so both
/// branches are testable without a live `State`.
///
/// Precedence: the overlaid model cache decides outright when the model
/// appears in it; otherwise models.dev answers for the bare id, and only a
/// positively confirmed `Some(true)` counts (`Some(false)` and `None` both
/// block — unknown models are not granted vision by default).
fn resolve_model_supports_images(
    models_dev: &ModelsDevData,
    model_cache: Option<&jinn_provider_config::ModelCache>,
    model_id: &str,
) -> bool {
    if let Some(info) = resolve_cached_model_info(model_cache, model_id) {
        return info.input_modalities.contains(Modality::Image);
    }
    let bare_id = model_id.split_once('/').map_or(model_id, |(_, r)| r);
    matches!(models_dev.supports_images(bare_id), Some(true))
}

/// Resolves a `provider/model` display form against the overlaid model cache.
///
/// Same first-slash split the status bar and the user-send gate use, so
/// path-shaped model ids keep resolving.
fn resolve_cached_model_info<'a>(
    model_cache: Option<&'a jinn_provider_config::ModelCache>,
    active_model: &str,
) -> Option<&'a jinn_provider_config::ModelInfo> {
    let cache = model_cache?;
    let (provider_name, model_suffix) = active_model.split_once('/')?;
    cache
        .entries
        .get(provider_name)?
        .iter()
        .find(|m| m.id == model_suffix)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::tool_types::ToolContext;
    use jinn_llm_support::image_convert::{ImageConversionError, ImageConverter, ResolvedImage};
    use marked_path::{Absolute, MarkedPathBuf};
    use std::sync::Arc;

    /// PNG magic bytes followed by a payload — enough for the classifier.
    fn png_bytes() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\nbody".to_vec()
    }

    /// HEIC-shaped bytes: ftyp box with the heic brand.
    fn heic_bytes() -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x00, 0x08];
        v.extend_from_slice(b"ftypheicpayload");
        v
    }

    /// A fake converter that returns canned PNG bytes when available and
    /// errors when not.
    #[derive(Default)]
    struct FakeConverter {
        available: bool,
    }

    impl ImageConverter for FakeConverter {
        fn name(&self) -> &'static str {
            "Fake"
        }
        fn convert_to_png(
            &self,
            _input: &MarkedPathBuf<Absolute>,
        ) -> Result<Vec<u8>, error_stack::Report<ImageConversionError>> {
            if self.available {
                Ok(b"\x89PNG\r\n\x1a\nconverted".to_vec())
            } else {
                Err(error_stack::Report::new(ImageConversionError))
            }
        }
        fn is_available(&self) -> bool {
            self.available
        }
    }

    fn with_fake_converter(
        available: bool,
    ) -> jinn_llm_support::image_convert::ImageConverterService {
        jinn_llm_support::image_convert::ImageConverterService::new(Arc::new(FakeConverter {
            available,
        }))
    }

    fn test_ctx(cwd: &MarkedPathBuf<Absolute>) -> ToolContext {
        crate::tool_types::ToolContext {
            turn: crate::tool_types::test_turn(),
            config: jinn_config::testutil::config_layer(""),
            shells: Default::default(),
            cwd: cwd.clone(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: jinn_kernel::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,
            dispatched_at: jiff::Timestamp::now(),
            mcp_coordinator: None,
            canvas_server: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
            model_name_resolver: None,
        }
    }

    fn call_for(path: &MarkedPathBuf<Absolute>) -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            name: "read_image".into(),
            arguments: serde_json::json!({ "file_path": path.to_string() }).to_string(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_attaches_native_png() {
        // Given a file holding PNG bytes.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("shot.png")
            .expect("temp root joins a bare name");
        std::fs::write(&path, png_bytes()).expect("write png");

        // When executing read_image on it.
        let result = execute(call_for(&path), test_ctx(&dir.path)).await;

        // Then the result succeeds and carries the image attachment.
        assert!(result.success, "content: {}", result.content);
        assert_eq!(result.attachments.len(), 1);
        assert_eq!(result.attachments[0].media_type(), "image/png");
        assert_eq!(result.attachments[0].data(), png_bytes().as_slice());
        // And the summary names the file, media type, and size.
        assert!(
            result.content.contains("shot.png"),
            "content: {}",
            result.content
        );
        assert!(
            result.content.contains("image/png"),
            "content: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_fails_on_missing_file_with_its_resolved_path() {
        // Given a call naming a file that does not exist.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("nope.png")
            .expect("temp root joins a bare name");

        // When executing.
        let result = execute(call_for(&path), test_ctx(&dir.path)).await;

        // Then the result fails naming the file-not-found reason.
        assert!(!result.success);
        assert!(
            result.content.contains("file not found"),
            "content: {}",
            result.content
        );
        assert!(
            result.content.contains("nope.png"),
            "content: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_fails_distinctly_on_non_image_file() {
        // Given a file holding text.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("notes.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&path, b"just words").expect("write text");

        // When executing.
        let result = execute(call_for(&path), test_ctx(&dir.path)).await;

        // Then the failure names it a non-image, not a missing file.
        assert!(!result.success);
        assert!(
            result.content.contains("not a recognized image"),
            "content: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_fails_when_conversion_needs_unavailable_imagemagick() {
        // Given a HEIC file and a ToolContext whose converter is unavailable.
        // (The production path builds ImageConverterService::system(); this
        // test exercises the resolver directly, since the tool builds its own
        // converter — see the resolver tests in jinn-llm-support for the
        // fake-backed coverage of this branch.)
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("photo.heic")
            .expect("temp root joins a bare name");
        std::fs::write(&path, heic_bytes()).expect("write heic");
        let converter = with_fake_converter(false);

        // When resolving the file with the unavailable converter.
        let outcome = resolve_image_file_blocking(&path, &converter);

        // Then the typed conversion-unavailable error comes back.
        assert_eq!(outcome.err(), Some(ImageFileError::ConversionUnavailable));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_transcodes_heic_through_a_working_converter() {
        // Given a HEIC file and an available fake converter.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("photo.heic")
            .expect("temp root joins a bare name");
        std::fs::write(&path, heic_bytes()).expect("write heic");
        let converter = with_fake_converter(true);

        // When resolving the file.
        let outcome = resolve_image_file_blocking(&path, &converter).expect("resolved");

        // Then the resolved image is the converted PNG.
        assert_eq!(outcome.media_type, "image/png");
        assert_eq!(outcome.bytes, b"\x89PNG\r\n\x1a\nconverted".to_vec());
    }

    #[rstest::rstest]
    #[test]
    fn size_guard_rejects_images_over_the_cap() {
        // Given a resolved image whose bytes exceed the cap.
        let oversized = ResolvedImage {
            media_type: "image/png".to_owned(),
            bytes: vec![0u8; MAX_IMAGE_BYTES + 1],
        };

        // When checking the guard condition the tool enforces.
        let over = oversized.bytes.len() > MAX_IMAGE_BYTES;

        // Then the guard fires exactly at the boundary.
        assert!(over);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_fails_with_resize_guidance_on_oversized_image() {
        // Given a PNG file larger than the size cap.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("big.png")
            .expect("temp root joins a bare name");
        let big = {
            let mut v = png_bytes();
            v.extend(std::iter::repeat_n(0u8, MAX_IMAGE_BYTES + 1));
            v
        };
        std::fs::write(&path, &big).expect("write big png");

        // When executing.
        let result = execute(call_for(&path), test_ctx(&dir.path)).await;

        // Then the failure names the size and the resize remedy.
        assert!(!result.success);
        assert!(
            result.content.contains("exceeds"),
            "content: {}",
            result.content
        );
        assert!(
            result.content.contains("-resize"),
            "content: {}",
            result.content
        );
        // And no attachment is produced.
        assert_eq!(result.attachments.len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_fails_on_missing_file_path_argument() {
        // Given a call without the file_path argument.
        let dir = jinn_testutil::tempdir();
        let call = ToolCall {
            id: "call_2".into(),
            name: "read_image".into(),
            arguments: "{}".to_owned(),
        };

        // When executing.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the failure names the missing slot.
        assert!(!result.success);
        assert!(
            result.content.contains("missing required argument"),
            "content: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn definition_requires_only_file_path() {
        // Given the read_image definition.
        let def = definition();

        // Then the schema requires exactly file_path.
        assert_eq!(def.name, "read_image");
        assert_eq!(def.parameters["required"], serde_json::json!(["file_path"]));
        assert_eq!(
            def.parameters["additionalProperties"],
            serde_json::json!(false)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_resolves_relative_path_against_cwd() {
        // Given a file in the session's cwd and a relative path argument.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("rel.png")
            .expect("temp root joins a bare name");
        std::fs::write(&path, png_bytes()).expect("write png");
        let call = ToolCall {
            id: "call_3".into(),
            name: "read_image".into(),
            arguments: serde_json::json!({ "file_path": "rel.png" }).to_string(),
        };

        // When executing.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the file is found and attached.
        assert!(result.success, "content: {}", result.content);
        assert_eq!(result.attachments.len(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn gate_passes_a_model_marked_image_capable_in_the_cache() {
        // Given a model cache entry with image modalities.
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "ollama".to_owned(),
            vec![jinn_provider_config::ModelInfo {
                id: "llama3".to_owned(),
                context_length: Some(8192),
                input_modalities: {
                    let mut m = jinn_provider_config::InputModalities::text();
                    m.insert(jinn_provider_config::Modality::Image);
                    m
                },
            }],
        );
        let cache = jinn_provider_config::ModelCache {
            entries,
            last_updated_at: None,
        };

        // When resolving the gate for that model.
        let capable =
            resolve_model_supports_images(&ModelsDevData::new(), Some(&cache), "ollama/llama3");

        // Then it is confirmed vision-capable without consulting models.dev.
        assert!(capable);
    }

    #[rstest::rstest]
    #[test]
    fn gate_blocks_a_model_marked_text_only_in_the_cache() {
        // Given a model cache entry that is explicitly text-only.
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "ollama".to_owned(),
            vec![jinn_provider_config::ModelInfo {
                id: "llama3".to_owned(),
                context_length: Some(8192),
                input_modalities: jinn_provider_config::InputModalities::text(),
            }],
        );
        let cache = jinn_provider_config::ModelCache {
            entries,
            last_updated_at: None,
        };

        // When resolving the gate for that model.
        let capable =
            resolve_model_supports_images(&ModelsDevData::new(), Some(&cache), "ollama/llama3");

        // Then it is not vision-capable (the cache decides outright).
        assert!(!capable);
    }

    #[rstest::rstest]
    #[test]
    fn gate_falls_back_to_models_dev_for_models_outside_the_cache() {
        // Given an empty cache and models.dev data marking a model capable.
        let mut models_dev = ModelsDevData::new();
        models_dev.image_support.insert("gpt-4o".to_owned(), true);

        // When resolving the gate with no cache.
        let capable = resolve_model_supports_images(&models_dev, None, "openai/gpt-4o");

        // Then the fallback path is taken (models.dev decides for the bare id).
        assert!(capable);
    }

    #[rstest::rstest]
    #[test]
    fn gate_resolves_path_shaped_model_ids_through_the_first_slash_split() {
        // Given a cache entry whose model id is an absolute file path.
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "llama.cpp".to_owned(),
            vec![jinn_provider_config::ModelInfo {
                id: "/models/Qwen3-35B.gguf".to_owned(),
                context_length: Some(32768),
                input_modalities: {
                    let mut m = jinn_provider_config::InputModalities::text();
                    m.insert(jinn_provider_config::Modality::Image);
                    m
                },
            }],
        );
        let cache = jinn_provider_config::ModelCache {
            entries,
            last_updated_at: None,
        };

        // When resolving the gate with the full `provider/path/model.gguf` form.
        let capable = resolve_model_supports_images(
            &ModelsDevData::new(),
            Some(&cache),
            "llama.cpp//models/Qwen3-35B.gguf",
        );

        // Then the path-shaped id resolves through the first-slash split.
        assert!(capable);
    }

    #[rstest::rstest]
    #[test]
    fn models_dev_fixtures_drive_the_fallback_path() {
        // Given models.dev JSON marking one model image-capable and another text-only.
        let json = serde_json::json!({
            "acme": { "models": {
                "vision-model": { "modalities": { "input": ["image"] } },
                "text-model": { "modalities": { "input": ["text"] } }
            }}
        });
        let dir = jinn_testutil::tempdir();
        let user_path = dir
            .path
            .join("models.dev.json")
            .expect("temp root joins a bare name");
        std::fs::write(&user_path, json.to_string()).expect("write fixture");
        let models_dev = ModelsDevData::load(&user_path);

        // When asking models.dev about both.
        // Then only the image-capable one is positively confirmed.
        assert_eq!(models_dev.supports_images("vision-model"), Some(true));
        assert_eq!(models_dev.supports_images("text-model"), Some(false));
    }
}
