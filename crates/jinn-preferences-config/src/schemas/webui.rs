//! The `[webui]` section — the web UI's socket binding.
//!
//! One key: `bind`, a host:port socket address (`127.0.0.1:8387`).
//! Unlike `[remote]`, the shipped default names a **fixed port**: the
//! web UI's SSE connections are long-lived pipes tied to the address
//! they opened on, so a per-start ephemeral port would make the URL
//! unbookmarkable and strand every open page on restart. A fixed
//! default keeps `http://127.0.0.1:8387/` stable across launches; port
//! `0` remains configurable for the ephemeral-port preference.
//!
//! The section is read **once, at activation** — the bind is frozen
//! for the process's lifetime. A live edit applies at the next
//! restart: unlike the remote control surface (stateless HTTP, safe to
//! hot-rebind), a webui rebind would silently strand every connected
//! page's SSE stream with no way for the page to discover the new
//! port.
//!
//! The listener that binds it lives in `jinn-webui`. This module is
//! data plus the bind-string grammar — no listener, no I/O.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use wherror::Error;

use error_stack::Report;

/// The default bind: loopback on the fixed webui port.
///
/// Deliberately *not* port 0: a fixed URL is bookmarkable from phones,
/// and the section comment documents the trade.
pub const DEFAULT_WEBUI_BIND: &str = "127.0.0.1:8387";

/// The bind string failed to parse as a host:port socket address.
#[derive(Debug, Error)]
#[error(debug)]
pub struct WebUiBindError;

fn default_bind() -> String {
    DEFAULT_WEBUI_BIND.to_owned()
}

/// `[webui]` preferences.
///
/// # Behavior: Event-Independent.
///
/// The bind is read once at activation and frozen; nothing re-reads
/// this section at runtime.
///
/// Serialized as `[webui]` in `jinn.toml`. The single field is the
/// socket the web UI binds; there is deliberately no separate port key
/// — the address carries the port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebUiConfig {
    /// Host:port the web UI binds, e.g. `"127.0.0.1:8387"`. Port `0`
    /// binds an OS-assigned ephemeral port; the resolved URL is noted
    /// on the webui dashboard row.
    #[serde(default = "default_bind")]
    pub bind: String,
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

impl jinn_config::Configurable for WebUiConfig {
    const KEY: &'static str = "webui";
}

impl WebUiConfig {
    /// Parses the configured bind string into a socket address.
    ///
    /// # Errors
    ///
    /// Returns [`WebUiBindError`] when the value is not a host:port
    /// socket address (`127.0.0.1:8387`, `[::1]:9000`). Activation
    /// treats that as stop-and-report.
    pub fn parse_bind(&self) -> Result<SocketAddr, Report<WebUiBindError>> {
        use error_stack::ResultExt as _;
        self.bind
            .parse::<SocketAddr>()
            .change_context(WebUiBindError)
            .attach(format!("invalid [webui] bind: {:?}", self.bind))
            .attach("expected a host:port socket address, e.g. 127.0.0.1:8387")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{DEFAULT_WEBUI_BIND, WebUiConfig};
    use serde::Deserialize;

    fn from_table(raw: &str) -> WebUiConfig {
        #[derive(Deserialize)]
        struct Wrapper {
            webui: WebUiConfig,
        }
        toml::from_str::<Wrapper>(raw)
            .expect("webui table parses")
            .webui
    }

    #[rstest::rstest]
    #[test]
    fn absent_bind_deserializes_to_the_fixed_default_port() {
        // Given an empty `[webui]` table.
        let config = from_table("[webui]\n");

        // Then the bind is the fixed loopback default.
        assert_eq!(config.bind, DEFAULT_WEBUI_BIND);
        assert_eq!(config.bind, "127.0.0.1:8387");
    }

    #[rstest::rstest]
    #[test]
    fn configured_bind_is_preserved_and_parses() {
        // Given a table with an explicit bind.
        let config = from_table("[webui]\nbind = \"0.0.0.0:9000\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("valid bind");

        // Then the value survives round-trip through the schema — and a
        // non-loopback host parses here: refusing exposed binds is
        // explicitly *not* webui policy (the operator's VPN is the
        // security boundary).
        assert_eq!(addr.to_string(), "0.0.0.0:9000");
    }

    #[rstest::rstest]
    #[test]
    fn port_zero_parses_as_os_assigned() {
        // Given a table whose bind names port 0.
        let config = from_table("[webui]\nbind = \"127.0.0.1:0\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("port 0 is a valid socket addr");

        // Then the parse keeps port 0; resolving it is serve-time behavior.
        assert_eq!(addr.port(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn malformed_bind_is_rejected_with_a_named_error() {
        // Given a table whose bind is not a host:port pair.
        let config = from_table("[webui]\nbind = \"localhost\"\n");

        // When parsing the bind.
        let result = config.parse_bind();

        // Then parsing fails (activation treats this as stop-and-report).
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn serializing_the_default_round_trips() {
        #[derive(Deserialize)]
        struct Repr {
            webui: WebUiConfig,
        }

        // Given the default config.
        let config = WebUiConfig::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&config).expect("serialize");
        let reparsed: WebUiConfig = toml::from_str::<Repr>(&format!("[webui]\n{s}"))
            .expect("reparse")
            .webui;

        // Then it equals the original.
        assert_eq!(config, reparsed);
    }
}
