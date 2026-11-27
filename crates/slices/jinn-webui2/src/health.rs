//! The dashboard's `webui2` row: how the web UI is doing, not only
//! where it is.
//!
//! The row's note is republished whenever a fact changes — a page connects
//! or disconnects, a `hello` is answered, a write stalls — so a user
//! reading the dashboard can tell a healthy slice from a stuck one without
//! opening the page. The address stays in every note; the rest is the
//! slice's state at the instant of the last change.
//!
//! The row is the one place service health lives in the TUI, and the
//! launch note already publishes through the listener's bus. Every note
//! here goes out the same way, under the same `webui2` name, so the
//! dashboard sees one row with a changing note rather than two rows.

use std::{
    fmt::Write as _,
    sync::Arc,
    time::{Duration, Instant},
};

use jinn_slices::{NoteTone, ServiceStatusUpdate, bus::BusService};
use parking_lot::Mutex;

use crate::listener::WEBUI2_ROW_DESCRIPTION;

/// How long a `hello` may take before the row names it as a stall.
///
/// A bounded tail answers in milliseconds; five seconds is far past any
/// healthy reply and short enough to notice while the user is still
/// watching the overlay.
const HELLO_WARN_BUDGET: Duration = Duration::from_secs(5);

/// What the row knows about the slice right now.
#[derive(Debug, Default)]
struct Health {
    /// Pages whose sockets are open.
    pages: usize,
    /// How long the last `hello` took, once one has been answered.
    last_hello: Option<Duration>,
    /// The stall the row is warning about, named — cleared by the next
    /// healthy `hello`.
    stalled: Option<String>,
}

/// The `webui2` row's state and the bus its notes leave through.
#[derive(Debug, Clone)]
pub struct WebUi2Health {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// How a user reaches the page, from the address actually bound. Empty
    /// until the listener binds — the shared state is built before the
    /// bind, and the row's note is completed by [`WebUi2Health::bound`].
    bind_note: Mutex<String>,
    health: Mutex<Health>,
    bus: BusService,
}

impl WebUi2Health {
    /// A healthy row, not yet bound: no page connected, no address named.
    pub fn new(bus: BusService) -> Self {
        Self {
            inner: Arc::new(Inner {
                bind_note: Mutex::new(String::new()),
                health: Mutex::new(Health::default()),
                bus,
            }),
        }
    }

    /// Names the address the page is served on, once the listener has
    /// bound. Every later note carries it.
    pub fn bound(&self, bind_note: String) {
        *self.inner.bind_note.lock() = bind_note;
    }

    /// A page's socket opened. Follow with [`Self::publish`].
    pub fn connected(&self) {
        self.inner.health.lock().pages += 1;
    }

    /// A page's socket closed. Never below zero: a disconnect without a
    /// matching connect is a wiring bug, not a negative page count.
    /// Follow with [`Self::publish`].
    pub fn disconnected(&self) {
        let mut health = self.inner.health.lock();
        health.pages = health.pages.saturating_sub(1);
    }

    /// A `hello` was answered in `took`. A reply past the budget raises
    /// the warning; a healthy one clears whatever stall was being named.
    /// Follow with [`Self::publish`].
    pub fn hello_answered(&self, took: Duration) {
        let stalled = (took > HELLO_WARN_BUDGET).then(|| {
            format!(
                "a hello took {} ms — over the {} ms budget",
                took.as_millis(),
                HELLO_WARN_BUDGET.as_millis()
            )
        });
        let mut health = self.inner.health.lock();
        health.last_hello = Some(took);
        health.stalled = stalled;
    }

    /// The writer gave up on a page: a frame sat in a write past its
    /// budget, and the connection was closed. Follow with [`Self::publish`].
    pub fn write_stalled(&self) {
        self.inner.health.lock().stalled =
            Some("a write stalled past its budget — connection closed".to_owned());
    }

    /// The note the dashboard shows right now: the address, the pages,
    /// the last `hello`, and the stall if any.
    #[must_use]
    pub fn note(&self) -> String {
        let health = self.inner.health.lock();
        let mut note = self.inner.bind_note.lock().clone();
        if health.pages > 0 {
            let _ = write!(
                note,
                " · {} page{}",
                health.pages,
                if health.pages == 1 { "" } else { "s" }
            );
        }
        if let Some(last) = health.last_hello {
            let _ = write!(note, " · last hello {} ms", last.as_millis());
        }
        if let Some(stall) = &health.stalled {
            note.push_str(" · ");
            note.push_str(stall);
        }
        note
    }

    /// The tone for the note as it stands: a warning while a stall is
    /// being named, muted otherwise.
    #[must_use]
    pub fn tone(&self) -> Option<NoteTone> {
        self.inner
            .health
            .lock()
            .stalled
            .as_ref()
            .map(|_| NoteTone::Warning)
    }

    /// Republishes the row under the `webui2` name, the way the launch
    /// note did, so the dashboard shows the new state in place.
    ///
    /// Every mutator is a plain state change; this is the one step that
    /// reaches the bus, and the caller awaits it. Nothing here spawns a
    /// task, so a note is on the bus before the caller moves on.
    pub async fn publish(&self) {
        let update = ServiceStatusUpdate {
            name: crate::listener::WEBUI2_ACTOR_NAME.to_owned(),
            description: Some(WEBUI2_ROW_DESCRIPTION.to_owned()),
            status_message: Some(self.note()),
            note_tone: self.tone(),
        };
        self.inner.bus.publish(update).await;
    }
}

/// Times one `hello` dispatch, records it, and republishes the row — so
/// the caller cannot report a `hello` without its duration.
pub async fn timed_hello<F, Fut>(health: &WebUi2Health, dispatch: F) -> Fut::Output
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future,
{
    let started = Instant::now();
    let reply = dispatch().await;
    health.hello_answered(started.elapsed());
    health.publish().await;
    reply
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]

    use super::*;

    fn health() -> (WebUi2Health, jinn_slices::bus::BusAudit) {
        let (bus, audit) = BusService::new_recording();
        let health = WebUi2Health::new(bus);
        health.bound("serving on http://x/web2".to_owned());
        (health, audit)
    }

    #[rstest::rstest]
    #[test]
    fn a_fresh_row_names_the_address_and_no_pages() {
        // Given a row over a bound listener with no page connected.
        let (health, _audit) = health();

        // When reading its note.
        let note = health.note();

        // Then it is the address alone.
        assert_eq!(note, "serving on http://x/web2");
    }

    #[rstest::rstest]
    #[test]
    fn a_connected_page_is_counted_in_the_note() {
        // Given a row with no page connected.
        let (health, _audit) = health();

        // When a page connects.
        health.connected();

        // Then the note counts one page.
        assert!(health.note().ends_with(" · 1 page"));
    }

    #[rstest::rstest]
    #[test]
    fn a_disconnected_page_leaves_the_note() {
        // Given a row with one page connected.
        let (health, _audit) = health();
        health.connected();

        // When the page disconnects.
        health.disconnected();

        // Then the note names no page again.
        assert_eq!(health.note(), "serving on http://x/web2");
    }

    #[rstest::rstest]
    #[test]
    fn a_fast_hello_is_reported_without_a_warning() {
        // Given a row.
        let (health, _audit) = health();

        // When a hello is answered quickly.
        health.hello_answered(Duration::from_millis(90));

        // Then the note names the duration and carries no warning tone.
        assert!(health.note().ends_with(" · last hello 90 ms"));
        assert!(health.tone().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn a_slow_hello_raises_the_warning_and_names_the_stall() {
        // Given a row.
        let (health, _audit) = health();

        // When a hello takes six seconds.
        health.hello_answered(Duration::from_secs(6));

        // Then the note names the slow hello, with a warning tone.
        assert!(health.note().contains("a hello took 6000 ms"));
        assert!(matches!(health.tone(), Some(NoteTone::Warning)));
    }

    #[rstest::rstest]
    #[test]
    fn a_stalled_write_raises_the_warning_and_names_it() {
        // Given a row.
        let (health, _audit) = health();

        // When the writer gives up on a page.
        health.write_stalled();

        // Then the note names the stalled write, with a warning tone.
        assert!(health.note().contains("a write stalled"));
        assert!(matches!(health.tone(), Some(NoteTone::Warning)));
    }

    #[rstest::rstest]
    #[test]
    fn a_healthy_hello_after_a_stall_clears_the_warning() {
        // Given a row warning about a stalled write.
        let (health, _audit) = health();
        health.write_stalled();

        // When the next hello is answered quickly.
        health.hello_answered(Duration::from_millis(40));

        // Then the warning is gone.
        assert!(health.tone().is_none());
        assert!(!health.note().contains("stalled"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_connection_publishes_the_row_on_the_bus() {
        // Given a row over a recording bus.
        let (health, audit) = health();

        // When a page connects and the row is republished.
        health.connected();
        health.publish().await;

        // Then one status update for webui2 was published, naming the page.
        let updates = audit
            .of_type::<ServiceStatusUpdate>()
            .into_iter()
            .filter(|update| update.name == "webui2")
            .collect::<Vec<_>>();
        let [update]: [ServiceStatusUpdate; 1] =
            updates.try_into().expect("exactly one webui2 update");
        assert!(
            update
                .status_message
                .as_deref()
                .unwrap_or("")
                .contains("1 page")
        );
    }
}
