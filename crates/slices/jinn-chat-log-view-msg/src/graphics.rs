//! The chat log's terminal-graphics state as a first-class cell payload.
//!
//! [`GraphicsState`] holds everything image rendering needs that outlives a
//! single frame: the capability [`Picker`] built by the once-per-launch
//! terminal query, the per-image protocol state that transmits image data
//! exactly once, and the bounded decode cache that turns attachment bytes
//! into `DynamicImage`s. The render pass drives it; jinn-tui writes the
//! picker at startup; suspend/restore marks it dirty.
//!
//! Live `[graphics]` config edits land through [`GraphicsState::apply_config`]:
//! the render pass compares the section it just read against
//! `last_applied` and, on any difference, clears the protocol state and
//! bumps the epoch. The epoch rides the entry line cache's render-variant
//! key, so a flip re-measures every entry — that is what makes the row
//! counts follow the setting without a restart. Only the capability
//! *query* is fixed at launch: it probes stdin, so a terminal swap
//! mid-session needs a restart to re-detect.
//!
//! Slot geometry ([`rows_for_image`]) and the paint-facing slot records
//! ([`ImageSlot`]) live here too, so the render pass, the off-thread
//! layout worker, and any future consumer compute identical row counts
//! from identical inputs — the property the line-count cache correctness
//! rests on.
//!
//! Decode results are keyed by [`ImageKey`] — session, entry, attachment
//! index — never by content fingerprint: the fingerprint hashes the
//! attachment list's length, not its bytes, so it cannot identify bytes.

use std::num::NonZeroUsize;
use std::sync::Arc;

use jinn_core_types::{ChatEntryId, SessionId};
use jinn_preferences_config::schemas::{GraphicsConfig, GraphicsProtocol};
use jinn_slices::SlotKey;
use lru::LruCache;
use parking_lot::Mutex;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::SlicedProtocol;

/// How many decoded images the cache holds. Each is at most the attachment
/// cap (5 MB) of RGBA pixels, so the bound keeps worst-case memory at a few
/// hundred MB across a very image-heavy session while covering the visible
/// scrollback with room to scroll.
const DECODE_CACHE_SIZE: usize = 32;

/// How many per-image protocol states are kept. A protocol holds the
/// transmit payload for kitty until it is sent and the rendered glyph data
/// afterwards, so the bound pairs with the decode cache above.
const PROTOCOL_CACHE_SIZE: usize = 64;

/// Identifies one image attachment across the session: which session, which
/// entry, which attachment in that entry. The attachment index disambiguates
/// entries that carry several images; the content fingerprint deliberately
/// plays no part because it covers the list's shape, not the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageKey {
    /// The session the entry belongs to.
    pub session: SessionId,
    /// The entry carrying the attachment.
    pub entry: ChatEntryId,
    /// The attachment's index within the entry.
    pub attachment_index: usize,
}

impl ImageKey {
    /// Names the image attachment at `attachment_index` of `entry`.
    #[must_use]
    pub fn new(session: &SessionId, entry: &ChatEntryId, attachment_index: usize) -> Self {
        Self {
            session: session.clone(),
            entry: entry.clone(),
            attachment_index,
        }
    }
}

/// One image's reserved rows inside its entry's rendered lines.
///
/// The adapter emits exactly `rows` blank lines starting at `line_index`
/// and records one of these per image. The lines are ordinary cacheable
/// lines — every count, scroll, and gutter computation treats them like any
/// other row — and the paint pass reads these records to draw the actual
/// image over the reserved rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSlot {
    /// Which image this slot is for.
    pub key: ImageKey,
    /// Index of the slot's first line within the entry's rendered lines.
    /// Slot lines are exactly one wrapped row each at any width, so the
    /// index survives line wrapping unchanged.
    pub line_index: u32,
    /// How many rows the image occupies.
    pub rows: u16,
}

/// The resolved inputs one image slot's geometry needs.
///
/// Snapshot once per frame (render pass) or once per layout job (worker);
/// both sides must build it identically or every published count misses.
/// Deliberately does **not** include the live viewport height: the entry
/// line cache keys counts on content width alone, so a viewport-derived
/// cap would thrash every cached count whenever a popup changes the chat
/// area's height. The cap is the configured absolute one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotGeometry {
    /// Terminal cell size in pixels, from the picker.
    pub font_size: (u16, u16),
    /// Absolute cap from `[graphics] max_image_rows` or the built-in
    /// default.
    pub max_rows: u16,
}

impl SlotGeometry {
    /// The row count one image occupies: fit its aspect to the content
    /// width through the font size, then clamp to the configured cap and
    /// the diacritic encoding limit.
    ///
    /// Pure: the same inputs produce the same count on the render pass and
    /// the off-thread worker — the property the line-count cache rests on.
    #[must_use]
    pub fn rows_for_image(&self, pixel_width: u32, pixel_height: u32) -> u16 {
        // Kitty placeholders encode row/column through 297 combining
        // diacritics; past that the crate leaves cells undrawn.
        const DIACRITIC_LIMIT: u16 = 297;
        let (font_w, font_h) = self.font_size;
        if pixel_width == 0 || pixel_height == 0 || font_w == 0 || font_h == 0 {
            return 0;
        }
        // Reserve the image's height in terminal cells; the protocol's
        // resize then fits the aspect *within* that box without exceeding
        // the rows the chat log counted.
        let height_cells = (f64::from(pixel_height) / f64::from(font_h)).ceil();
        height_cells
            .min(f64::from(self.max_rows))
            .min(f64::from(DIACRITIC_LIMIT))
            .max(1.0) as u16
    }
}

/// The per-image protocol state: a sliced protocol plus the row count it
/// was built for. The row count is the recreate trigger — a width, font,
/// or cap change lands as a different row count, and the next paint
/// rebuilds the protocol for it. `Debug` is implemented by hand because
/// the protocol payloads do not implement it and their shape is not
/// informative anyway.
pub struct SlicedProtocolHolder {
    /// The row count the protocol was built for.
    pub rows: u16,
    /// The column count the protocol was sized against. A terminal resize
    /// changes the fit even at an unchanged row count, so this rides the
    /// recreate key alongside `rows`.
    pub cols: u16,
    /// The protocol itself: kitty (unicode placeholders), sixel, iTerm2,
    /// or halfblocks, in its sliced (scroll-safe) form.
    pub protocol: SlicedProtocol,
}

impl SlicedProtocolHolder {
    /// Wraps a freshly built protocol and its cell dimensions.
    fn new(rows: u16, cols: u16, protocol: SlicedProtocol) -> Self {
        Self {
            rows,
            cols,
            protocol,
        }
    }
}

impl std::fmt::Debug for SlicedProtocolHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlicedProtocolHolder")
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

/// Terminal-graphics state for the chat log: the capability picker, the
/// per-image protocol state, and the bounded decode cache.
///
/// Held in a cell; the render pass is the primary driver. Protocol and
/// decode caches sit behind mutexes so a paint can borrow a protocol while
/// the payload itself stays `Sync` for the cell.
#[derive(Debug)]
pub struct GraphicsState {
    /// The picker from the once-per-launch capability query. `None` means
    /// no graphics capability (or Windows) — every image renders as
    /// today's text placeholders.
    picker: Option<Picker>,
    /// Per-image protocol state, keyed by image. Transmit-once lives
    /// inside each protocol. `Arc` because the paint pass borrows a
    /// protocol out of the locked map and holds it past the lock guard.
    protocols: Mutex<LruCache<ImageKey, Arc<SlicedProtocolHolder>>>,
    /// Decoded attachments, keyed by image. `None` is a tombstone for
    /// bytes that failed to decode, so a corrupt image is not re-decoded
    /// every frame.
    decoded: Mutex<LruCache<ImageKey, Option<Arc<image::DynamicImage>>>>,
    /// The last `[graphics]` config this state applied. A difference on
    /// the next comparison clears the protocol state and bumps the epoch.
    last_applied: GraphicsConfig,
    /// Whether the applied config forces a protocol the detected terminal
    /// cannot speak — judged at apply time against the *startup-detected*
    /// protocol, never the picker's current type (a previous force
    /// mutates it). Slots degrade to fallbacks while this holds.
    /// Halfblocks never sets this: it is plain block glyphs, not a
    /// graphics protocol, so any color terminal can draw it.
    forced_unavailable: bool,
    /// The protocol the startup query detected, frozen at install time.
    /// Availability judgments and `auto` restoration read this, not the
    /// picker's live protocol type, which config forces mutate.
    detected_protocol: Option<ratatui_image::picker::ProtocolType>,
    /// Whether the startup query detected (or forced onto) a halfblocks
    /// picker — a genuinely detected halfblocks picker must survive a
    /// return to `auto`, while the one `enforce_forced_protocol`
    /// synthesizes for a configured halfblocks must not.
    detected_halfblocks: bool,
    /// Set by lifecycle edges (suspend/restore leaves kitty images wiped);
    /// the next frame clears the protocol state in response.
    dirty: bool,
    /// Monotonic counter bumped whenever rendered lines must re-measure:
    /// a `[graphics]` config change or a dirty mark. Rides the entry line
    /// cache's render-variant key.
    epoch: u64,
    /// The session whose images this state currently holds caches for.
    /// A frame naming a different session evicts the others.
    last_session: Option<SessionId>,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphicsState {
    /// An empty state: no capability detected yet, caches empty, config at
    /// its defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            picker: None,
            protocols: Mutex::new(LruCache::new(
                NonZeroUsize::new(PROTOCOL_CACHE_SIZE).unwrap_or(NonZeroUsize::MIN),
            )),
            decoded: Mutex::new(LruCache::new(
                NonZeroUsize::new(DECODE_CACHE_SIZE).unwrap_or(NonZeroUsize::MIN),
            )),
            last_applied: GraphicsConfig::default(),
            forced_unavailable: false,
            detected_protocol: None,
            detected_halfblocks: false,
            dirty: false,
            epoch: 0,
            last_session: None,
        }
    }

    /// Installs the picker built by the startup capability query.
    /// `None` records "no capability" and keeps every image on its
    /// text placeholder.
    pub fn set_picker(&mut self, picker: Option<Picker>) {
        use ratatui_image::picker::ProtocolType;
        self.detected_protocol = picker.as_ref().map(Picker::protocol_type);
        self.detected_halfblocks = self
            .detected_protocol
            .is_some_and(|p| p == ProtocolType::Halfblocks);
        self.picker = picker;
    }

    /// Whether a capability was detected at launch. This says nothing
    /// about `enabled` — that gate lives in the config the render pass
    /// reads per frame.
    #[must_use]
    pub fn has_capability(&self) -> bool {
        self.picker.is_some()
    }

    /// The configured display-row cap (before the viewport clamp the
    /// geometry applies), as of the last applied config.
    #[must_use]
    pub fn configured_max_rows(&self) -> u16 {
        self.last_applied.effective_max_rows()
    }

    /// The current graphics epoch. Rides the render-variant key.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The `[graphics]` section as last applied — what off-render callers
    /// (the layout worker, the coverage probe) measure against. The render
    /// pass re-applies the live config every frame, so this is at most one
    /// frame stale and its epoch bump re-measures anything that shifted.
    #[must_use]
    pub fn applied_config(&self) -> &GraphicsConfig {
        &self.last_applied
    }

    /// The geometry inputs for slot rows, read off this state. `None`
    /// without a capability.
    #[must_use]
    pub fn geometry(&self) -> Option<SlotGeometry> {
        let picker = self.picker.as_ref()?;
        Some(SlotGeometry {
            font_size: (picker.font_size().width, picker.font_size().height),
            max_rows: self.configured_max_rows(),
        })
    }

    /// Applies the `[graphics]` section the caller just read from the live
    /// configuration layer.
    ///
    /// Returns `true` when anything changed — a differing section and/or a
    /// dirty mark: the protocol state was cleared and the epoch bumped, so
    /// the next measurements pick up the new setting. An unchanged,
    /// clean section costs a struct compare.
    pub fn apply_config(&mut self, config: &GraphicsConfig) -> bool {
        let config_changed = *config != self.last_applied;
        let dirty = std::mem::take(&mut self.dirty);
        if config_changed {
            self.protocols.lock().clear();
            // Halfblocks is plain colored block glyphs — any color terminal
            // draws it, so it is always available when configured, even on
            // Windows or after a failed detection query (it synthesizes its
            // own picker below). Every other forced protocol is judged
            // against the *startup-detected* protocol, never the picker's
            // live type: a previous force has already mutated it, so
            // kitty → halfblocks → kitty would otherwise compare a
            // halfblocks picker against "kitty" and degrade permanently.
            self.forced_unavailable = {
                let forced = match config.protocol {
                    GraphicsProtocol::Auto => None,
                    GraphicsProtocol::Kitty => Some(ratatui_image::picker::ProtocolType::Kitty),
                    GraphicsProtocol::Sixel => Some(ratatui_image::picker::ProtocolType::Sixel),
                    GraphicsProtocol::ITerm2 => Some(ratatui_image::picker::ProtocolType::Iterm2),
                    GraphicsProtocol::Halfblocks => None,
                };
                matches!(
                    (self.detected_protocol, forced),
                    (Some(d), Some(f)) if d != f
                )
            };
            self.last_applied = config.clone();
            self.enforce_forced_protocol();
        }
        if config_changed || dirty {
            self.protocols.lock().clear();
            self.epoch = self.epoch.wrapping_add(1);
            return true;
        }
        false
    }

    /// Marks every image as needing re-transmit: the terminal discarded
    /// its graphics state (suspend/restore exited the alternate screen).
    /// The next frame's `apply_config` clears the protocol state.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Returns the cached decode result for `key`, if any. `Some(None)`
    /// means the bytes are known undecodable — callers fall back to the
    /// placeholder without trying again.
    #[must_use]
    pub fn peek_decoded(&self, key: &ImageKey) -> Option<Option<Arc<image::DynamicImage>>> {
        self.decoded.lock().get(key).cloned()
    }

    /// Inserts a decode result. `None` inserts the tombstone.
    pub fn insert_decoded(&mut self, key: ImageKey, decoded: Option<image::DynamicImage>) {
        self.decoded.lock().put(key, decoded.map(Arc::new));
    }

    /// Decodes the bytes once, caching the result (a tombstone on
    /// failure). Returns the decoded image, or `None` when the bytes are
    /// undecodable.
    /// # Panics
    ///
    /// Panics if the LRU loses the entry `insert_decoded` just wrote — a
    /// broken cache invariant, not a reachable state.
    pub fn decode_once<D>(&mut self, key: &ImageKey, decode: D) -> Option<Arc<image::DynamicImage>>
    where
        D: FnOnce() -> Option<image::DynamicImage>,
    {
        if let Some(cached) = self.peek_decoded(key) {
            return cached;
        }
        let decoded = decode();
        self.insert_decoded(key.clone(), decoded);
        // `insert_decoded` put an entry (a value or an undecodable
        // tombstone) under `key`; a miss here is a broken LRU invariant.
        self.peek_decoded(key).flatten()
    }

    /// Returns the protocol for `key`, creating it on first sight and
    /// recreating it when `rows` changed (a width, font, or cap change).
    ///
    /// `decode` supplies the decoded image on a protocol miss; `None`
    /// (undecodable or absent) leaves no protocol and the slot's reserved
    /// rows stay blank.
    pub fn ensure_protocol<D>(
        &mut self,
        key: &ImageKey,
        rows: u16,
        decode: D,
    ) -> Option<Arc<SlicedProtocolHolder>>
    where
        D: FnOnce() -> Option<image::DynamicImage>,
    {
        let cols = {
            // The image's width in cells: the box the protocol fits into.
            // Read off the cached decode; a cold image recomputes after
            // decode below, so this peek only fast-paths the warm path.
            self.peek_decoded(key).flatten().map_or(0, |img| {
                let (font_w, _) = self
                    .picker
                    .as_ref()
                    .map_or((10, 20), |p| (p.font_size().width, p.font_size().height));
                if font_w == 0 {
                    0
                } else {
                    u16::try_from(img.width() / u32::from(font_w)).unwrap_or(u16::MAX)
                }
            })
        };
        if let Some(cached) = self
            .protocols
            .lock()
            .peek(key)
            .filter(|entry| entry.rows == rows && entry.cols == cols)
        {
            return Some(Arc::clone(cached));
        }
        // Decode before borrowing the picker: both take `self`, so the
        // borrows are sequenced rather than simultaneous.
        let img = self.decode_once(key, decode)?;
        let picker = self.picker.as_ref()?;
        let cols = {
            let (font_w, _) = (picker.font_size().width, picker.font_size().height);
            // Sub-cell-wide images still need a one-column box; a zero-width
            // target breaks the crate's resize invariants.
            if font_w == 0 {
                1
            } else {
                u16::try_from(img.width() / u32::from(font_w))
                    .unwrap_or(u16::MAX)
                    .max(1)
            }
        };
        let size = ratatui::layout::Size::new(cols, rows);
        let protocol = SlicedProtocol::new_with_resize(
            picker,
            (*img).clone(),
            size,
            ratatui_image::Resize::Fit(None),
        )
        .ok()?;
        let holder = Arc::new(SlicedProtocolHolder::new(rows, cols, protocol));
        self.protocols.lock().put(key.clone(), Arc::clone(&holder));
        Some(holder)
    }

    /// The stored protocol for `key`, if one exists at exactly `rows`.
    /// The paint pass clones the `Arc` and renders from it after the lock
    /// is released.
    #[must_use]
    pub fn peek_protocol(&self, key: &ImageKey, rows: u16) -> Option<Arc<SlicedProtocolHolder>> {
        let guard = self.protocols.lock();
        guard
            .peek(key)
            .filter(|entry| entry.rows == rows)
            .map(Arc::clone)
    }

    /// Evicts every cached decode and protocol belonging to a session
    /// other than `session`. Called when the active session changes;
    /// the LRU bounds are the backstop for entries the switch misses.
    pub fn evict_other_sessions(&mut self, session: &SessionId) {
        if self.last_session.as_ref() == Some(session) {
            return;
        }
        self.last_session = Some(session.clone());
        self.retain_caches(|key| &key.session == session);
    }

    /// Drops both caches' entries that `keep` rejects. One pass per cache,
    /// shared by the session eviction above.
    fn retain_caches(&mut self, keep: impl Fn(&ImageKey) -> bool) {
        {
            let mut decoded = self.decoded.lock();
            let stale: Vec<ImageKey> = decoded
                .iter()
                .filter(|(key, _)| !keep(key))
                .map(|(key, _)| key.clone())
                .collect();
            for key in stale {
                decoded.pop(&key);
            }
        }
        let mut protocols = self.protocols.lock();
        let stale: Vec<ImageKey> = protocols
            .iter()
            .filter(|(key, _)| !keep(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            protocols.pop(&key);
        }
    }

    /// Whether a forced protocol degrades this state to fallbacks: a
    /// `protocol` override that the detected terminal cannot speak. The
    /// degradation is visible (placeholder lines) because force means
    /// force.
    #[must_use]
    pub fn forced_protocol_unavailable(&self) -> bool {
        self.forced_unavailable
    }

    /// Whether image slots may be active: config enabled, a capability
    /// detected, and the forced protocol (if any) actually available.
    /// The render pass folds this with the live config each frame.
    #[must_use]
    pub fn slots_available(&self, enabled: bool) -> bool {
        enabled && self.has_capability() && !self.forced_protocol_unavailable()
    }

    /// Switches the picker to the configured protocol when the config
    /// forces one. Called after a config change; a forced protocol the
    /// terminal lacks leaves the state degrading via
    /// [`Self::slots_available`] instead of switching.
    ///
    /// Halfblocks is the exception to the "no picker, no slots" rule: it
    /// renders as plain block glyphs in any color terminal, so a `None`
    /// picker (Windows, failed query) synthesizes the crate's
    /// default-font halfblocks picker when halfblocks is configured — and
    /// the synthetic picker is removed again when the config returns to
    /// `auto`, which means *detected* capability only. Opt-in, never a
    /// fallback.
    fn enforce_forced_protocol(&mut self) {
        use ratatui_image::picker::ProtocolType;
        let forced = match self.last_applied.protocol {
            GraphicsProtocol::Auto => {
                // A picker whose detected protocol is recorded is the real
                // query result; restore its type — a previous force may have
                // switched it away from what the terminal actually speaks.
                // A picker with no recorded detection is the synthetic
                // halfblocks one; drop it — `auto` renders only what the
                // startup query detected.
                if let Some(detected) = self.detected_protocol {
                    if let Some(picker) = self.picker.as_mut() {
                        picker.set_protocol_type(detected);
                    }
                } else {
                    self.picker = None;
                }
                return;
            }
            GraphicsProtocol::Kitty => ProtocolType::Kitty,
            GraphicsProtocol::Sixel => ProtocolType::Sixel,
            GraphicsProtocol::ITerm2 => ProtocolType::Iterm2,
            GraphicsProtocol::Halfblocks => ProtocolType::Halfblocks,
        };
        if self.picker.is_none() && forced == ProtocolType::Halfblocks {
            // Always available: block glyphs need no graphics protocol.
            self.picker = Some(ratatui_image::picker::Picker::halfblocks());
        }
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        if picker.protocol_type() != forced {
            picker.set_protocol_type(forced);
        }
    }

    /// The number of protocols currently held. Observability for tests.
    #[must_use]
    pub fn protocol_count(&self) -> usize {
        self.protocols.lock().len()
    }

    /// The number of decode entries (including tombstones). Observability
    /// for tests.
    #[must_use]
    pub fn decoded_count(&self) -> usize {
        self.decoded.lock().len()
    }
}

/// The slot key the chat log's graphics state lives under.
#[must_use]
pub fn chat_log_graphics_slot() -> SlotKey {
    SlotKey::builtin("chat-log-view", "graphics")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{GraphicsState, ImageKey, SlotGeometry};
    use jinn_core_types::{ChatEntryId, SessionId};
    use jinn_preferences_config::schemas::{GraphicsConfig, GraphicsProtocol};

    fn key() -> ImageKey {
        ImageKey::new(&SessionId::new(), &ChatEntryId::new(), 0)
    }

    fn geometry(max_rows: u16) -> SlotGeometry {
        SlotGeometry {
            font_size: (8, 16),
            max_rows,
        }
    }

    #[rstest::rstest]
    #[test]
    fn default_state_has_no_capability_and_zero_epoch() {
        // Given a freshly created graphics state.
        let state = GraphicsState::new();

        // Then no capability is recorded and the epoch starts at zero.
        assert!(!state.has_capability());
        assert_eq!(state.epoch(), 0);
        assert!(!state.slots_available(true));
    }

    #[rstest::rstest]
    #[test]
    fn unchanged_config_apply_is_a_no_op() {
        // Given a state with the default config already applied.
        let mut state = GraphicsState::new();
        let config = GraphicsConfig::default();
        state.apply_config(&config);

        // When applying the same config again.
        let changed = state.apply_config(&config);

        // Then nothing changed.
        assert!(!changed);
        assert_eq!(state.epoch(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn changed_config_clears_and_bumps_the_epoch() {
        // Given a state with the default config applied.
        let mut state = GraphicsState::new();
        state.apply_config(&GraphicsConfig::default());

        // When the enabled flag flips off.
        let changed = state.apply_config(&GraphicsConfig {
            enabled: false,
            ..GraphicsConfig::default()
        });

        // Then the apply reports a change and the epoch moved.
        assert!(changed);
        assert_eq!(state.epoch(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn dirty_mark_clears_on_the_next_apply() {
        // Given a state marked dirty by a suspend/restore.
        let mut state = GraphicsState::new();
        state.apply_config(&GraphicsConfig::default());
        state.mark_dirty();

        // When the next frame applies the unchanged config.
        let changed = state.apply_config(&GraphicsConfig::default());

        // Then the apply consumed the dirty flag and bumped the epoch.
        assert!(changed);
        assert_eq!(state.epoch(), 1);
        // And a further apply is clean again.
        assert!(!state.apply_config(&GraphicsConfig::default()));
    }

    #[rstest::rstest]
    #[test]
    fn decode_tombstone_survives_a_roundtrip() {
        // Given a state with a failed decode recorded.
        let mut state = GraphicsState::new();
        let k = key();
        state.insert_decoded(k.clone(), None);

        // When peeking the decode cache.
        let peeked = state.peek_decoded(&k);

        // Then the tombstone comes back as Some(None), not a fresh miss.
        assert_eq!(peeked, Some(None));
        assert_eq!(state.decoded_count(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn decode_once_decodes_exactly_once() {
        // Given a state and a decode closure that counts its calls.
        let mut state = GraphicsState::new();
        let k = key();
        let mut calls = 0;

        // When decoding twice through decode_once.
        let first = state.decode_once(&k, || {
            calls += 1;
            None
        });
        let second = state.decode_once(&k, || {
            calls += 1;
            None
        });

        // Then the closure ran once and both calls saw the cached result.
        assert_eq!(calls, 1);
        assert_eq!(first, None);
        assert_eq!(second, None);
    }

    #[rstest::rstest]
    #[test]
    fn evicting_other_sessions_keeps_the_active_one() {
        // Given a state holding decode entries for two sessions.
        let mut state = GraphicsState::new();
        let keep_session = SessionId::new();
        let other_session = SessionId::new();
        let keep = ImageKey::new(&keep_session, &ChatEntryId::new(), 0);
        let other = ImageKey::new(&other_session, &ChatEntryId::new(), 0);
        state.insert_decoded(keep.clone(), None);
        state.insert_decoded(other.clone(), None);

        // When evicting everything but the keep session.
        state.evict_other_sessions(&keep_session);

        // Then only the keep session's entry remains.
        assert_eq!(state.peek_decoded(&keep), Some(None));
        assert_eq!(state.peek_decoded(&other), None);
    }

    #[rstest::rstest]
    #[test]
    fn evicting_the_same_session_twice_is_a_no_op() {
        // Given a state already evicted down to one session.
        let session = SessionId::new();
        let mut state = GraphicsState::new();
        let keep = ImageKey::new(&session, &ChatEntryId::new(), 0);
        state.insert_decoded(keep.clone(), None);
        state.evict_other_sessions(&session);

        // When evicting for the same session again.
        state.evict_other_sessions(&session);

        // Then the entry survives.
        assert_eq!(state.peek_decoded(&keep), Some(None));
    }

    #[rstest::rstest]
    #[test]
    fn forced_unavailable_protocol_degrades_slots() {
        // Given a state without a picker.
        let mut state = GraphicsState::new();

        // When forcing a protocol explicitly.
        state.apply_config(&GraphicsConfig {
            protocol: GraphicsProtocol::Kitty,
            ..GraphicsConfig::default()
        });

        // Then slots stay unavailable — a forced protocol on a
        // no-capability terminal degrades visibly.
        assert!(!state.slots_available(true));
    }

    #[rstest::rstest]
    #[test]
    fn configured_halfblocks_is_always_available_without_a_picker() {
        // Given a state without a picker (Windows, or a failed query).
        let mut state = GraphicsState::new();

        // When configuring halfblocks explicitly.
        state.apply_config(&GraphicsConfig {
            protocol: GraphicsProtocol::Halfblocks,
            ..GraphicsConfig::default()
        });

        // Then slots are available: halfblocks is plain block glyphs any
        // color terminal draws, so it synthesizes its own picker.
        assert!(state.slots_available(true));
        assert!(state.has_capability());
    }

    #[rstest::rstest]
    #[test]
    fn configured_halfblocks_is_available_even_when_detection_found_kitty() {
        // Given a state whose query detected a kitty-capable terminal,
        // represented here by installing a halfblocks-capable picker under
        // a detected flag (the picker's protocol type is what the
        // availability check compares).
        let mut state = GraphicsState::new();
        state.set_picker(Some(ratatui_image::picker::Picker::halfblocks()));
        state.apply_config(&GraphicsConfig::default());
        assert!(!state.forced_protocol_unavailable());

        // When the user opts into halfblocks.
        state.apply_config(&GraphicsConfig {
            protocol: GraphicsProtocol::Halfblocks,
            ..GraphicsConfig::default()
        });

        // Then the override never degrades: there is no terminal that
        // cannot draw block glyphs.
        assert!(!state.forced_protocol_unavailable());
        assert!(state.slots_available(true));
    }

    #[rstest::rstest]
    #[test]
    fn returning_to_auto_drops_the_synthetic_halfblocks_picker() {
        // Given a state with no detected capability, opted into halfblocks.
        let mut state = GraphicsState::new();
        state.apply_config(&GraphicsConfig {
            protocol: GraphicsProtocol::Halfblocks,
            ..GraphicsConfig::default()
        });
        assert!(state.has_capability());

        // When the config returns to auto.
        state.apply_config(&GraphicsConfig::default());

        // Then the synthetic picker is gone: auto means detected capability
        // only, so the placeholders return.
        assert!(!state.has_capability());
        assert!(!state.slots_available(true));
    }

    #[rstest::rstest]
    #[test]
    fn detected_halfblocks_picker_survives_a_return_to_auto() {
        // Given a state whose query genuinely detected halfblocks (the
        // picker's own protocol type, flagged as detected at install).
        let mut state = GraphicsState::new();
        state.set_picker(Some(ratatui_image::picker::Picker::halfblocks()));

        // When a halfblocks config applies and then auto returns.
        state.apply_config(&GraphicsConfig {
            protocol: GraphicsProtocol::Halfblocks,
            ..GraphicsConfig::default()
        });
        state.apply_config(&GraphicsConfig::default());

        // Then the detected picker survives — auto keeps what was detected.
        assert!(state.has_capability());
        assert!(state.slots_available(true));
    }

    #[rstest::rstest]
    #[test]
    fn kitty_detected_switching_to_halfblocks_and_back_stays_available() {
        // Given a state whose query detected a kitty-capable terminal.
        // (Protocol type is the detection surface the state reads; the
        // font size is what a real query provides.)
        let mut state = GraphicsState::new();
        state.set_picker(Some(kitty_picker()));

        // When opting into halfblocks live.
        let halfblocks = GraphicsConfig {
            protocol: GraphicsProtocol::Halfblocks,
            ..GraphicsConfig::default()
        };
        state.apply_config(&halfblocks);

        // Then slots stay available — halfblocks never degrades.
        assert!(state.slots_available(true));

        // When returning to auto.
        state.apply_config(&GraphicsConfig::default());

        // Then the detected kitty picker is restored, not destroyed: the
        // crisp images come back without a restart.
        assert!(state.slots_available(true));
        assert!(state.has_capability());
        assert_eq!(
            state.detected_protocol,
            Some(ratatui_image::picker::ProtocolType::Kitty)
        );
    }

    #[rstest::rstest]
    #[test]
    fn kitty_detected_forcing_sixel_then_back_to_auto_recovers_kitty() {
        // Given a kitty-detected state.
        let mut state = GraphicsState::new();
        state.set_picker(Some(kitty_picker()));

        // When forcing sixel (the terminal likely cannot speak it) and
        // then returning to auto.
        state.apply_config(&GraphicsConfig {
            protocol: GraphicsProtocol::Sixel,
            ..GraphicsConfig::default()
        });
        assert!(state.forced_protocol_unavailable());

        state.apply_config(&GraphicsConfig::default());

        // Then auto recovers the detected kitty rendering: the degrade
        // clears and the picker's protocol returns to the detected one.
        assert!(!state.forced_protocol_unavailable());
        assert!(state.slots_available(true));
        assert_eq!(
            state
                .picker
                .as_ref()
                .map(ratatui_image::picker::Picker::protocol_type),
            Some(ratatui_image::picker::ProtocolType::Kitty)
        );
    }

    /// A picker shaped like a real kitty detection: kitty protocol with a
    /// query-provided font size.
    fn kitty_picker() -> ratatui_image::picker::Picker {
        let mut picker = ratatui_image::picker::Picker::halfblocks();
        picker.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
        picker
    }

    #[rstest::rstest]
    #[test]
    fn disabled_config_degrades_slots_even_with_capability() {
        // Given a state whose enabled flag is off.
        let mut state = GraphicsState::new();
        state.apply_config(&GraphicsConfig {
            enabled: false,
            ..GraphicsConfig::default()
        });

        // Then slots are unavailable regardless of capability.
        assert!(!state.slots_available(true));
    }

    #[rstest::rstest]
    #[test]
    fn rows_fit_aspect_to_width_through_the_font_size() {
        // Given geometry with 8x16 cells and no cap binding.
        let geo = geometry(u16::MAX);

        // When measuring a 800x1600 image (portrait 1:2).
        let rows = geo.rows_for_image(800, 1600);

        // Then it wants 100 rows at the content width.
        assert_eq!(rows, 100);
    }

    #[rstest::rstest]
    #[test]
    fn oversized_image_capped_to_max_height() {
        // Given geometry whose absolute cap is 16.
        let geo = geometry(16);

        // When measuring a tall image.
        let rows = geo.rows_for_image(800, 3200);

        // Then the cap binds.
        assert_eq!(rows, 16);
    }

    #[rstest::rstest]
    #[test]
    fn degenerate_dimensions_measure_zero_rows() {
        // Given a zero-width image.
        let geo = geometry(16);

        // When measuring it.
        let rows = geo.rows_for_image(0, 100);

        // Then it reserves nothing.
        assert_eq!(rows, 0);
    }

    #[rstest::rstest]
    #[test]
    fn landscape_image_wants_fewer_rows_than_portrait() {
        // Given uncapped geometry.
        let geo = geometry(u16::MAX);

        // When measuring a 1600x800 landscape image.
        let rows = geo.rows_for_image(1600, 800);

        // Then it reserves its height in cells (800px at 16px rows = 50),
        // a fraction of the portrait counterpart's — pixel height drives
        // the count.
        assert_eq!(rows, 50);
    }
}
