//! The `[webui2]` section — the second web UI's socket binding.
//!
//! One key: `bind`, a host:port socket address (`127.0.0.1:8388`).
//! Sibling of [`super::WebUiConfig`]: the same fixed-port rationale (a
//! bookmarkable URL), read **once, at activation**, and frozen for the
//! process's lifetime. A live edit applies at the next restart.
//!
//! The listener that binds it lives in `jinn-webui2`. This module is
//! data plus the bind-string grammar — no listener, no I/O.

use error_stack::Report;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use wherror::Error;

/// The default bind: loopback on the fixed second-web-UI port — one
/// past the original web UI's, so both surfaces run side by side.
pub const DEFAULT_WEBUI2_BIND: &str = "127.0.0.1:8388";

/// The bind string failed to parse as a host:port socket address.
#[derive(Debug, Error)]
#[error(debug)]
pub struct WebUi2BindError;

fn default_bind() -> String {
    DEFAULT_WEBUI2_BIND.to_owned()
}

/// `[webui2]` preferences.
///
/// # Behavior: Event-Independent.
///
/// The bind is read once at activation and frozen; nothing re-reads
/// this section at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebUi2Config {
    /// Host:port the second web UI binds, e.g. `"127.0.0.1:8388"`.
    /// Port `0` binds an OS-assigned ephemeral port.
    #[serde(default = "default_bind")]
    pub bind: String,
}

impl Default for WebUi2Config {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

impl jinn_config::Configurable for WebUi2Config {
    const KEY: &'static str = "webui2";
}

impl WebUi2Config {
    /// Parses the configured bind string into a socket address.
    ///
    /// # Errors
    ///
    /// Returns [`WebUi2BindError`] when the value is not a host:port
    /// socket address (`127.0.0.1:8388`, `[::1]:9000`). Activation
    /// treats that as stop-and-report.
    pub fn parse_bind(&self) -> Result<SocketAddr, Report<WebUi2BindError>> {
        use error_stack::ResultExt as _;
        self.bind
            .parse::<SocketAddr>()
            .change_context(WebUi2BindError)
            .attach(format!("invalid [webui2] bind: {:?}", self.bind))
            .attach("expected a host:port socket address, e.g. 127.0.0.1:8388")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{DEFAULT_WEBUI2_BIND, WebUi2Config};
    use serde::Deserialize;

    fn from_table(raw: &str) -> WebUi2Config {
        #[derive(Deserialize)]
        struct Wrapper {
            webui2: WebUi2Config,
        }
        toml::from_str::<Wrapper>(raw)
            .expect("webui2 table parses")
            .webui2
    }

    #[rstest::rstest]
    #[test]
    fn absent_bind_deserializes_to_the_fixed_default_port() {
        // Given an empty `[webui2]` table.
        let config = from_table("[webui2]\n");

        // Then the bind is the fixed loopback default.
        assert_eq!(config.bind, DEFAULT_WEBUI2_BIND);
        assert_eq!(config.bind, "127.0.0.1:8388");
    }

    #[rstest::rstest]
    #[test]
    fn configured_bind_is_preserved_and_parses() {
        // Given a table with an explicit bind.
        let config = from_table("[webui2]\nbind = \"0.0.0.0:9001\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("valid bind");

        // Then the value survives round-trip through the schema.
        assert_eq!(addr.to_string(), "0.0.0.0:9001");
    }

    #[rstest::rstest]
    #[test]
    fn malformed_bind_is_rejected_with_a_named_error() {
        // Given a table whose bind is not a host:port pair.
        let config = from_table("[webui2]\nbind = \"localhost\"\n");

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
            webui2: WebUi2Config,
        }

        // Given the default config.
        let config = WebUi2Config::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&config).expect("serialize");
        let reparsed: WebUi2Config = toml::from_str::<Repr>(&format!("[webui2]\n{s}"))
            .expect("reparse")
            .webui2;

        // Then it equals the original.
        assert_eq!(config, reparsed);
    }
}
