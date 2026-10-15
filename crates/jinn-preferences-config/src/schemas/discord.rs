//! The `[discord]` section — the Discord bot's configuration.
//!
//! All fields are optional and default to a disabled bot. When
//! `enabled = true`, the TUI process spawns a Discord bot (via the
//! `jinn-discord` crate) that drives the same running jinn instance.
//!
//! The gateway connection, slash-command registration, and thread
//! bridging stay in `jinn-discord`. This module is data only.

use serde::{Deserialize, Serialize};

/// Discord bot configuration.
///
/// Serialized as the `[discord]` table in `jinn.toml`. All fields are optional
/// and default to a disabled bot — the bot only starts when `enabled = true`
/// AND a token is available (config field or `DISCORD_BOT_TOKEN` env var).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiscordConfig {
    /// Master switch. `false` (default) = no bot, zero behavior change.
    #[serde(default)]
    pub enabled: bool,

    /// Bot token. If absent, the gateway reads the `DISCORD_BOT_TOKEN`
    /// environment variable at startup.
    #[serde(default)]
    pub bot_token: Option<String>,

    /// Discord guild id (numeric, as a string) to scope slash-command
    /// registration. Slash commands registered globally take up to an hour to
    /// propagate; per-guild registration is instant and is the recommended dev
    /// setup. If absent, commands are registered globally.
    #[serde(default)]
    pub guild_id: Option<String>,

    /// Id (numeric, as a string) of the `GUILD_FORUM` channel where the bot
    /// creates forum threads when a user lifts an in-flight session into
    /// Discord via the `gdc` ("to-thread") command. If absent, `gdc` is
    /// rejected with an in-chat error. The bot must have `Manage Threads` and
    /// view access to this channel.
    #[serde(default)]
    pub forum_channel: Option<String>,

    /// Discord user IDs (numeric, as strings) allowed to interact with the
    /// bot. Deny-by-default: an empty or missing list authorizes nobody —
    /// slash commands get an ephemeral refusal and plain messages are dropped
    /// silently. Entries that don't parse as numeric IDs are ignored.
    ///
    /// Like every `[discord]` field, changes apply on restart only.
    #[serde(default)]
    pub authorized_users: Vec<String>,
}

impl jinn_config::Configurable for DiscordConfig {
    const KEY: &'static str = "discord";
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]
    use super::DiscordConfig;
    use serde::Deserialize;

    #[rstest::rstest]
    #[test]
    fn empty_table_parses_to_disabled_default() {
        // Given an empty `[discord]` table (all fields defaulted).
        let raw = "";

        // When deserializing.
        let config: DiscordConfig = toml::from_str(raw).expect("empty table");

        // Then the bot is disabled by default.
        assert!(!config.enabled);
        assert_eq!(config.authorized_users, [] as [std::string::String; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn populated_discord_table_round_trips() {
        // Given a populated `[discord]` table.
        #[derive(Deserialize)]
        struct Wrapper {
            discord: DiscordConfig,
        }
        let toml_str = r#"
            [discord]
            enabled = true
            bot_token = "abc123"
            guild_id = "9999"
        "#;

        // When deserializing.
        let parsed: Wrapper = toml::from_str(toml_str).expect("parse");

        // Then all fields are preserved.
        assert!(parsed.discord.enabled);
        assert_eq!(parsed.discord.bot_token.as_deref(), Some("abc123"));
        assert_eq!(parsed.discord.guild_id.as_deref(), Some("9999"));
    }

    #[rstest::rstest]
    #[test]
    fn leftover_lifecycle_key_is_ignored() {
        // Given a `[discord]` table with a stale `lifecycle` key plus the
        // current fields.
        #[derive(Deserialize)]
        struct Wrapper {
            discord: DiscordConfig,
        }
        let toml_str = r#"
            [discord]
            enabled = true
            bot_token = "abc123"
            lifecycle = "x"
            guild_id = "9999"
        "#;

        // When deserializing.
        let parsed: Wrapper = toml::from_str(toml_str).expect("parse");

        // Then the stale `lifecycle` key is silently dropped and the
        // remaining fields are populated.
        assert!(parsed.discord.enabled);
        assert_eq!(parsed.discord.bot_token.as_deref(), Some("abc123"));
        assert_eq!(parsed.discord.guild_id.as_deref(), Some("9999"));
    }

    #[rstest::rstest]
    #[test]
    fn re_serializing_disabled_default_round_trips() {
        // Given a default config.
        let cfg = DiscordConfig::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&cfg).expect("serialize");
        let reparsed: DiscordConfig = toml::from_str(&s).expect("reparse");

        // Then it equals the original (still disabled).
        assert_eq!(cfg, reparsed);
        assert!(!reparsed.enabled);
    }

    #[rstest::rstest]
    #[test]
    fn missing_authorized_users_deserializes_to_empty_list() {
        // Given a `[discord]` table with no `authorized_users` key.
        #[derive(Deserialize)]
        struct Wrapper {
            discord: DiscordConfig,
        }
        let toml_str = r#"
            [discord]
            enabled = true
            bot_token = "abc123"
        "#;

        // When deserializing.
        let parsed: Wrapper = toml::from_str(toml_str).expect("parse");

        // Then the allow-list is empty (which denies everybody).
        assert_eq!(
            parsed.discord.authorized_users,
            [] as [std::string::String; 0]
        );
    }
}
