//! The push sender: a settle becomes one request per subscription.
//!
//! Called from the settle handler for every non-subagent session that lands
//! on Idle — the same fact that turns its row gold — whether or not a page is
//! viewing it. The sender never asks who is watching: whether a banner is
//! *shown* is the service worker's call, because only it can see whether any
//! page of the origin is visible, and a push that shows nothing is what makes
//! Safari revoke the permission.
//!
//! Every subscription gets its own request, sent on its own task, so a slow
//! push service delays no other browser and never the settle handler. A
//! subscription the service says is gone is dropped; anything else is kept
//! and nothing is retried — the gold mark still records the settle.

use std::time::Duration;

use super::store::PushStore;
use super::webpush::{self, Payload, PushOutcome, Receiver};

/// The banner's title: the app's name, so every banner reads as coming from
/// the same place however the session is titled.
pub const BANNER_TITLE: &str = "jinn";

/// What the banner's body says, before the session it names. No message
/// content rides a push — a lock screen is not the place for model output.
pub const TURN_FINISHED: &str = "Turn finished:";

/// How long a session title may be, in grapheme clusters, before it is
/// cut. It only guards against a pathological title: the tray cuts the
/// line visually long before this.
const TITLE_MAX: usize = 120;

/// The untitled placeholder, the same one a session row shows.
const UNTITLED: &str = "Untitled Session";

/// How long one request waits on the push service before it is given up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the push service keeps a message for a browser it cannot reach.
const TTL_SECONDS: u32 = 60 * 60;

/// What the sender needs: the subscriptions and the key they were made under.
#[derive(Debug, Clone)]
pub struct PushSender {
    /// The store holding the key pair and every subscription.
    pub store: PushStore,
    /// The contact address for the VAPID token, from `[webui] push_contact`.
    pub contact: String,
}

impl PushSender {
    /// Opens the sender for one run: the store under the data dir, and the
    /// contact `[webui] push_contact` names.
    ///
    /// A data dir that cannot be written leaves push unsupported for this
    /// run rather than failing the activation — the web UI works without
    /// notifications. The store falls back to memory, so a browser that
    /// subscribes still can, and the subscriptions last until the process
    /// ends.
    #[must_use]
    pub fn open(
        services: &jinn_kernel::common::services::Services,
        config: &jinn_config::ConfigLayer,
    ) -> Self {
        let store = PushStore::open(&services.paths.app_data_dir().as_marked_path())
            .unwrap_or_else(|report| {
                tracing::warn!(%report, "web UI: push store unavailable; notifications are off");
                PushStore::in_memory()
            });
        let contact = config
            .get::<jinn_preferences_config::schemas::WebUi2Config>()
            .unwrap_or_else(|_| jinn_preferences_config::schemas::WebUi2Config::default())
            .push_contact;
        Self { store, contact }
    }

    /// The store of subscriptions, for the subscribe and unsubscribe
    /// handlers and the `push_info` frame.
    #[must_use]
    pub fn store(&self) -> &PushStore {
        &self.store
    }

    /// Sends a push for a settled session to every subscription.
    ///
    /// Returns how many requests were started. A store with no key pair or
    /// no subscriptions starts none — a page that never subscribed, or a
    /// data dir that could not be written.
    pub fn on_settle(&self, session_id: &str, title: Option<&str>) -> usize {
        let Some(keys) = self.store.key_pair() else {
            return 0;
        };
        let subscriptions = self.store.subscriptions();
        let payload = Payload {
            session_id: session_id.to_owned(),
            title: BANNER_TITLE.to_owned(),
            body: turn_finished_body(title),
        };
        let client = crate::attachment::http_client();
        let started = subscriptions.len();
        for subscription in subscriptions {
            let store = self.store.clone();
            let keys = keys.clone();
            let payload = payload.clone();
            let contact = self.contact.clone();
            let client = client.clone();
            let endpoint = subscription.endpoint.clone();
            tokio::spawn(async move {
                let outcome = send(
                    &client,
                    &endpoint,
                    &subscription.keys,
                    &payload,
                    keys.signing(),
                    &contact,
                )
                .await;
                if outcome == PushOutcome::Gone {
                    store.remove(&endpoint);
                }
            });
        }
        started
    }
}

/// Sends one push, mapping the service's answer to what becomes of the
/// subscription.
async fn send(
    client: &reqwest::Client,
    endpoint: &str,
    keys: &jinn_webui2_msg::PushKeysBody,
    payload: &Payload,
    signer: &p256::ecdsa::SigningKey,
    contact: &str,
) -> PushOutcome {
    let Some(audience) = webpush::audience_of(endpoint) else {
        return PushOutcome::Gone;
    };
    let receiver = Receiver {
        p256dh: keys.p256dh.clone(),
        auth: keys.auth.clone(),
    };
    // A fresh ephemeral key and salt per push: reusing either repeats a
    // (CEK, nonce) pair, which loses the GCM key.
    let ephemeral = super::keys::KeyPair::generate();
    let mut salt = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut salt);
    let Ok(record) = webpush::encrypt(payload, &receiver, ephemeral.signing(), &salt) else {
        return PushOutcome::Gone;
    };
    let authorization = webpush::sign(&audience, signer, contact, webpush::TOKEN_LIFETIME);
    let Ok(response) = client
        .post(endpoint)
        .timeout(REQUEST_TIMEOUT)
        .header("Content-Encoding", "aes128gcm")
        .header("TTL", TTL_SECONDS.to_string())
        .header("Authorization", authorization)
        .body(record.body)
        .send()
        .await
    else {
        return PushOutcome::RetryLater;
    };
    webpush::outcome_of(response.status().as_u16())
}

/// The body of a "turn finished" banner: what happened, then the session
/// it happened in. The session title is cut to the length a banner carries,
/// or is the untitled placeholder; the cut falls on a grapheme boundary so
/// no banner ever ends in half a character.
fn turn_finished_body(title: Option<&str>) -> String {
    use unicode_segmentation::UnicodeSegmentation as _;
    let title = title.unwrap_or(UNTITLED);
    let mut graphemes = title.graphemes(true);
    let mut kept: String = graphemes.by_ref().take(TITLE_MAX).collect();
    if graphemes.next().is_some() {
        kept.push('…');
    }
    format!("{TURN_FINISHED} {kept}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use jinn_testutil::paths::TestRoot;
    use unicode_segmentation::UnicodeSegmentation as _;

    use super::{BANNER_TITLE, PushSender, PushStore, TURN_FINISHED, turn_finished_body};
    use crate::push::store::Subscription;
    use jinn_webui2_msg::PushKeysBody;

    fn sender(store: super::PushStore) -> PushSender {
        PushSender {
            store,
            contact: "mailto:me@example.com".to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn an_empty_store_sends_nothing() {
        // Given a sender whose store holds no subscriptions.
        let sender = sender(super::PushStore::in_memory());

        // When a session settles.
        let sent = sender.on_settle("s1", Some("Title"));

        // Then no request is started.
        assert_eq!(sent, 0);
    }

    /// A store with a key pair and one subscription pointing at `endpoint`.
    fn store_subscribed_to(endpoint: &str) -> PushStore {
        let root = TestRoot::new();
        let paths = jinn_kernel::AppPaths::new_in(&root.path().as_marked_path());
        let store = PushStore::open(&paths.app_data_dir().as_marked_path()).expect("opens");
        store.add(Subscription {
            endpoint: endpoint.to_owned(),
            keys: PushKeysBody {
                p256dh: BROWSER_PUBLIC.to_owned(),
                auth: BROWSER_AUTH.to_owned(),
            },
        });
        store
    }

    /// A browser's receiving material, from the RFC 8291 vector: a real
    /// point on the curve, so encryption succeeds.
    const BROWSER_PUBLIC: &str = concat!(
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx",
        "aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"
    );
    const BROWSER_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

    /// Waits until the mock has seen its expected request. The sender
    /// posts from a spawned task, so the request lands after `on_settle`
    /// returns; polling `matched` — rather than a fixed sleep — keeps the
    /// test deterministic and fast.
    async fn await_hit(mock: &mockito::Mock) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !mock.matched() {
            assert!(
                std::time::Instant::now() < deadline,
                "the push never reached the mock"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Waits until the store reads as `wanted` says. The push is a spawned
    /// task, and the store write follows its response: a mock that has been
    /// hit proves the request arrived, not that the task has finished, so a
    /// test that reads the store must wait on the store itself.
    async fn await_store(store: &PushStore, wanted: impl Fn(&[Subscription]) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !wanted(&store.subscriptions()) {
            assert!(
                std::time::Instant::now() < deadline,
                "the store never reached the expected state"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_settle_pushes_to_the_subscription() {
        // Given a store subscribed to a mock push service that accepts.
        let mut service = mockito::Server::new_async().await;
        let endpoint = format!("{}{}", service.url(), "/push/abc");
        let store = store_subscribed_to(&endpoint);
        let mock = service
            .mock("POST", "/push/abc")
            .match_header("content-encoding", "aes128gcm")
            .with_status(201)
            .expect(1)
            .create_async()
            .await;

        // When a session settles.
        let sent = sender(store).on_settle("s1", Some("Title"));

        // Then one request was started, and it reached the service with the
        // aes128gcm content coding.
        assert_eq!(sent, 1);
        await_hit(&mock).await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_subscription_the_service_says_is_gone_is_dropped() {
        // Given a store subscribed to a mock push service that answers 410.
        let mut service = mockito::Server::new_async().await;
        let endpoint = format!("{}{}", service.url(), "/push/gone");
        let store = store_subscribed_to(&endpoint);
        let mock = service
            .mock("POST", "/push/gone")
            .with_status(410)
            .expect(1)
            .create_async()
            .await;

        // When a session settles and the service answers.
        sender(store.clone()).on_settle("s1", Some("Title"));
        // Two facts, in the order they happen: the request reached the
        // service (so the 410 is what the sender read), and only then did
        // the spawned task drop the row. Waiting on the hit alone raced the
        // removal; waiting on the store alone would pass for any removal.
        await_hit(&mock).await;
        await_store(&store, <[Subscription]>::is_empty).await;

        // Then the store no longer holds the subscription.
        assert_eq!(store.subscriptions(), Vec::<Subscription>::new());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_subscription_the_service_cannot_reach_is_kept() {
        // Given a store subscribed to a mock push service that answers 500.
        let mut service = mockito::Server::new_async().await;
        let endpoint = format!("{}{}", service.url(), "/push/later");
        let store = store_subscribed_to(&endpoint);
        let mock = service
            .mock("POST", "/push/later")
            .with_status(500)
            .expect(1)
            .create_async()
            .await;

        // When a session settles and the service answers.
        sender(store.clone()).on_settle("s1", Some("Title"));
        await_hit(&mock).await;

        // Then the store still holds the subscription.
        assert_eq!(store.subscriptions().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn every_subscription_gets_its_own_request() {
        // Given a store subscribed twice to a mock push service.
        let mut service = mockito::Server::new_async().await;
        let endpoint_a = format!("{}/push/a", service.url());
        let endpoint_b = format!("{}/push/b", service.url());
        let store = store_subscribed_to(&endpoint_a);
        store.add(Subscription {
            endpoint: endpoint_b.clone(),
            keys: PushKeysBody {
                p256dh: BROWSER_PUBLIC.to_owned(),
                auth: BROWSER_AUTH.to_owned(),
            },
        });
        let mock = service
            .mock("POST", "/push/a")
            .with_status(201)
            .expect(1)
            .create_async()
            .await;
        let mock_b = service
            .mock("POST", "/push/b")
            .with_status(201)
            .expect(1)
            .create_async()
            .await;

        // When a session settles.
        let sent = sender(store).on_settle("s1", Some("Title"));

        // Then each subscription got a request.
        assert_eq!(sent, 2);
        await_hit(&mock).await;
        await_hit(&mock_b).await;
    }

    #[rstest::rstest]
    #[test]
    fn the_banner_is_titled_with_the_apps_name() {
        // Given any session title.
        // When the banner's title is read.
        let title = BANNER_TITLE;

        // Then it is the app's name, never the session's.
        assert_eq!(title, "jinn");
    }

    #[rstest::rstest]
    #[test]
    fn the_body_names_what_happened_and_the_session() {
        // Given a session with a title.
        // When the body is built.
        let body = turn_finished_body(Some("Fix the bug"));

        // Then it says a turn finished, in that session.
        assert_eq!(body, "Turn finished: Fix the bug");
    }

    #[rstest::rstest]
    #[test]
    fn an_untitled_session_is_named_as_untitled() {
        // Given a session with no title.
        // When the body is built.
        let body = turn_finished_body(None);

        // Then it still names a session, so the line never ends bare.
        assert_eq!(body, "Turn finished: Untitled Session");
    }

    #[rstest::rstest]
    #[test]
    fn a_long_title_is_cut_on_a_grapheme_boundary() {
        // Given a title of 130 ASCII characters.
        let title = "a".repeat(130);

        // When the body is built.
        let body = turn_finished_body(Some(&title));

        // Then the title in it is 120 characters plus an ellipsis.
        let shown = body
            .strip_prefix(TURN_FINISHED)
            .expect("the body leads with what happened");
        assert_eq!(shown.trim_start().chars().count(), 121);
        assert!(shown.ends_with('…'));
    }

    #[rstest::rstest]
    #[test]
    fn a_short_title_is_kept_whole() {
        // Given a title well under the limit.
        // When the body is built.
        let body = turn_finished_body(Some("Fix the bug"));

        // Then the title is unchanged.
        assert!(body.ends_with("Fix the bug"));
    }

    #[rstest::rstest]
    #[test]
    fn a_title_cut_mid_grapheme_keeps_the_whole_cluster() {
        // Given a title whose 120th grapheme is the first of a flag.
        let title = format!("{}🇯🇵", "a".repeat(119));

        // When the body is built.
        let body = turn_finished_body(Some(&title));

        // Then the flag is not split: 119 letters and the whole flag.
        let shown = body
            .strip_prefix(TURN_FINISHED)
            .expect("the body leads with what happened");
        assert_eq!(shown.trim_start().graphemes(true).count(), 120);
        assert!(shown.ends_with("🇯🇵"));
    }
}
