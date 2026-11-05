//! Late-attached slice-registry facades: cell access and view/input read-modify helpers.

use super::*;

impl ChatSessionState {
    /// Attaches the slice registry handle carrying this session's
    /// chat-log-view entry. Called once at wiring; later calls are ignored.
    pub fn attach_slices(&self, slices: jinn_slices::Slices) {
        let _ = self.slices.set(slices);
    }

    /// The session's chat-log-view cell, if the handle is attached and the
    /// slice's `activate()` minted the cell.
    fn view_cell(
        &self,
    ) -> Option<jinn_slices::cell::TypedCell<jinn_chat_log_view_msg::ChatLogViews>> {
        let slices = self.slices.get()?;
        slices.reader::<jinn_chat_log_view_msg::ChatLogViews>(
            &jinn_chat_log_view_msg::chat_log_views_slot(),
        )
    }

    /// The session's chat-input cell, if the handle is attached and the
    /// slice's `activate()` minted the cell.
    fn input_cell(&self) -> Option<jinn_slices::cell::TypedCell<jinn_chat_input_msg::ChatInputs>> {
        let slices = self.slices.get()?;
        slices.reader::<jinn_chat_input_msg::ChatInputs>(&jinn_chat_input_msg::chat_inputs_slot())
    }

    /// Runs `f` against this session's input draft (buffer, cursor, wrap
    /// cache, submission mode, autocomplete), keyed by the session id.
    /// Writers get-or-insert their session's entry.
    ///
    /// A no-op when the cell is absent (handle unattached, or the catalog
    /// never ran). The draft the cell holds for this session is the only
    /// copy — there is no second one on the session struct to fall back to,
    /// so a missing cell means there is no draft to write.
    pub fn update_input<F>(&self, f: F)
    where
        F: FnOnce(&mut jinn_chat_input_msg::ChatInputBoxState),
    {
        if let Some(cell) = self.input_cell() {
            let id = self.session_id().clone();
            cell.update(|inputs| f(inputs.entry(id).or_default()));
        }
    }

    /// Reads this session's input draft through `f`, falling back to
    /// `default` when the cell is absent (handle unattached, or the catalog
    /// never ran) or when the session has no entry. Readers never grow the
    /// map: a session with no entry reads as its default draft.
    pub fn with_input<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&jinn_chat_input_msg::ChatInputBoxState) -> R,
        D: FnOnce() -> R,
    {
        let Some(cell) = self.input_cell() else {
            return default();
        };
        let inputs = cell.read();
        let id = self.session_id();
        match inputs.get(id) {
            Some(input) => f(input),
            None => default(),
        }
    }

    /// Runs `f` against this session's view state (scroll, selection,
    /// expand/ignore sets, pins position, render caches), keyed by the
    /// session id. Writers get-or-insert their session's entry; a no-op
    /// when the cell is absent (handle unattached or slice not activated).
    pub fn update_view<F>(&self, f: F)
    where
        F: FnOnce(&mut jinn_chat_log_view_msg::ChatLogViewUi),
    {
        match self.view_cell() {
            Some(cell) => {
                let id = self.session_id().clone();
                cell.update(|views| f(views.entry(id).or_default()));
            }
            None => {
                let mut view = self.view_fallback.write();
                f(&mut view);
            }
        }
    }

    /// Reads this session's view state through `f`, falling back to
    /// `default` when the cell is absent (handle unattached or slice not
    /// activated). Readers never grow the map: a session with no entry
    /// reads as its default view.
    pub fn with_view<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&jinn_chat_log_view_msg::ChatLogViewUi) -> R,
        D: FnOnce() -> R,
    {
        match self.view_cell() {
            Some(cell) => {
                let views = cell.read();
                let id = self.session_id();
                match views.get(id) {
                    Some(view) => f(view),
                    None => default(),
                }
            }
            None => {
                let view = self.view_fallback.read();
                f(&view)
            }
        }
    }

    /// Take-style mutation for view fields whose semantics consume the old
    /// value (the ignore-sweep). Returns what `f` removed.
    pub(super) fn update_view_taking<R, F>(&self, f: F) -> Option<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut jinn_chat_log_view_msg::ChatLogViewUi) -> Option<R>,
    {
        let taken = parking_lot::Mutex::new(None);
        self.update_view(|v| *taken.lock() = f(v));
        taken.into_inner()
    }
}
