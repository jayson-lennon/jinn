# Discord Integration

jinn runs a Discord bot beside the TUI. Each Discord forum thread becomes a
jinn session: messages posted in the thread are forwarded to the session,
and the session's replies are posted back into the thread. The TUI and the
bot operate on the same sessions — you can start work in the terminal and
continue it from your phone.

## Setup

Add the section to `~/.config/jinn/jinn.toml` (see `configuration.md` for
the edit protocol):

```toml
[discord]
enabled = true
bot_token = "..."              # or the DISCORD_BOT_TOKEN env var
guild_id = "<guild id>"        # the guild (server) the bot operates in
forum_channel = "<channel id>" # forum channel where threads are created
authorized_users = ["<numeric user id>"]
```

Token resolution prefers the `DISCORD_BOT_TOKEN` environment variable and
falls back to `bot_token` — prefer the env var; the config file is a
readable file on disk.

Requirements:

- A Discord bot set up on your server
- A forum channel for the bot to create session threads in
- Projects pre-configured in jinn (`<leader>sp`) — `/new` presents them as
  the choices for a new thread's session

## Who can use the bot

`authorized_users` is a list of numeric Discord user IDs and is
**deny-by-default**: an empty list authorizes nobody, and any user not in
the list is refused with one generic ephemeral message — identical for
every command, so a denial reveals nothing about bot state. Plain messages
are gated the same way as slash commands.

Find a user's numeric ID in Discord: enable Developer Settings
(Appearance → Advanced), then right-click a user → Copy User ID.

## The commands

Run these in a thread the bot has bound to a session:

| Command     | Action                                                                  |
| ----------- | ----------------------------------------------------------------------- |
| `/new`      | Create a session for this thread — runs the lifecycle wizard (the `[[session_lifecycle]]` entries plus a blank option), then a project picker for the session's working directory |
| `/prompts`  | List the prompt templates (`#name`) available to the thread's session   |
| `/teardown` | Run the bound session's lifecycle teardown script                       |
| `/archive`  | Archive the thread's session                                            |

Plain messages in a bound thread are forwarded to the session as user
messages (split across replies when the text is long). Messages from
unauthorized users are refused.

## Continuing a TUI session in Discord

Press `gdc` in normal mode to lift the active session into a Discord forum
thread — same session, new surface; history stays in place. The reverse is
implicit: the TUI already shows everything the bot writes.

## Enabling and disabling live

`[discord]` edits apply when the bot next goes idle — set `enabled = true`
and the dormant bot connects; set it false and the gateway parks until
sessions go idle, then disconnects. The dashboard's `discord` row reports
the state. See `configuration.md` for the reload categories.
