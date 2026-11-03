# Remote — messaging a running instance from outside

Every running jinn instance serves an **unauthenticated HTTP control
surface on loopback** and registers itself in an instance registry, so
scripts, hooks, or plain `curl` can deliver a message into a live session
of that instance. The `jinn remote` subcommand is the friendly client for
the same surface — use it first; reach for curl when you're already in a
shell script or hook.

**Strictly delivery.** An external send lands in an existing live
session — nothing can create sessions or edit history, and bot sessions
are refused. The listener is loopback-only with no token: the OS user
boundary is the security model, same as the canvas server. Text arrives
verbatim — no prefix is added; write your own attribution into the
message if you want it.

## Finding instances

| Command                 | What it does                                                              |
| ----------------------- | ------------------------------------------------------------------------- |
| `jinn remote instances` | One row per running instance: PID, PORT, STARTED, CWD                     |
| `jinn remote send`      | Deliver a message — see the flags below                                    |

Each running instance registers `{pid, port, cwd, started_at}` in a
registry file under the state dir. With no instance named, **the newest
instance is the target**; `--instance <pid>` picks a specific one. A
registry entry whose port no longer answers is pruned by every reader, so
a killed instance is never listed even before its stale row is rewritten.

## Sending: queue vs steer

| Flag              | Meaning                                                                        |
| ----------------- | ------------------------------------------------------------------------------ |
| `--queue`         | Default. Waits until the session's current turn finishes, then runs as the user's next turn |
| `--steer`         | Lands as a steering fragment at the session's next prompt-assembly boundary — mid-turn guidance, never cancels work |
| `--instance <pid>`| Target a specific instance instead of the newest                                |

A steer aimed at an **idle** session falls back to queue delivery — the
CLI prints `(idle target; the steer was delivered as a queue message)` —
so a script never fails on the race between its check and the session
going idle. `--queue` and `--steer` together are a usage error.

```bash
jinn remote send --session 01a1a05a-f0f3-77d6-8521-b48c3e420f40 "build finished"
jinn remote send --session 01a1a05a-f0f3-77d6-8521-b48c3e420f40 --steer "also run the tests"
jinn remote send --instance 4242 --session 01a1a05a-f0f3-77d6-8521-b48c3e420f40 "for that instance"
```

Errors are named sentences, exit 1: unknown `--session`, unknown
`--instance`, no instances running.

## The HTTP surface (plain curl)

| Route       | Method | Purpose                                                        |
| ----------- | ------ | -------------------------------------------------------------- |
| `/health`   | GET    | `{"pid":…,"version":"…","port":…}` — liveness probe            |
| `/sessions` | GET    | Live sessions: `id`, `title`, `phase` (`idle`/`streaming`/`working`/`cancelling`), `bot` |
| `/send`     | POST   | Deliver a message; JSON body                                   |

The port comes from `jinn remote instances` (or the registry) — curl has
no way to discover it alone. All requests are plain `http://127.0.0.1:<port>/…`,
no auth headers.

```bash
PORT=$(jinn remote instances | awk 'NR==2{print $2}')
SESS=$(curl -s http://127.0.0.1:$PORT/sessions | jq -r '.sessions[0].id')

# Queue a message (Content-Type is required — without it: 400).
curl -s -X POST http://127.0.0.1:$PORT/send \
  -H 'Content-Type: application/json' \
  -d "{\"session_id\":\"$SESS\",\"text\":\"hello via curl\"}"
# → {"status":"queued","session_id":"01a1a05a-…","fell_back":false}

# Steer instead:
curl -s -X POST http://127.0.0.1:$PORT/send -H 'Content-Type: application/json' \
  -d "{\"session_id\":\"$SESS\",\"text\":\"change course\",\"mode\":\"steer\"}"
# → {"status":"steered",…,"fell_back":false}   (busy target)
# → {"status":"queued",…,"fell_back":true}     (idle target — fell back)
```

The send body is `{"session_id": …, "text": …, "mode": "queue"|"steer"}`;
`mode` is optional and defaults to `queue`. Error responses are JSON
`{"error": "…"}`:

| Situation              | Status | Error body says                                  |
| ---------------------- | ------ | ------------------------------------------------ |
| Missing/incorrect `Content-Type` | 400 | `malformed request body: …`          |
| Session id not a UUID  | 400    | `session_id is not a valid session id`           |
| Empty `text`           | 400    | `text must not be empty`                         |
| Target is a bot session| 400    | `session … is a bot run; external delivery does not target bot sessions` |
| Session id not live    | 404    | `no live session with id …`                      |

## Configuration

The bind lives in `jinn.toml` (see `configuration.md` for the path and
the restart rule):

```toml
[remote]
# bind = "127.0.0.1:0"   # default: loopback, OS-assigned ephemeral port
```

Port `0` means the OS picks a free port at launch, so concurrent
instances never collide — the resolved port is what `jinn remote
instances` reports. A bind that is not a loopback address refuses the
launch (the surface has no authentication). Config changes take effect
on the next jinn start, like everything else in `jinn.toml`.

## Gotchas

- **The CLI needs the instance alive, not just installed.** `jinn remote
  instances` reads the registry, then probes each port; a crashed
  instance leaves a stale row that never lists. Conversely, a *running*
  instance started before `[remote]` existed in your build serves
  nothing — the dashboard lists its actors, but the registry row only
  exists for builds with the remote slice.
- **`jinn remote` never touches your session store or config** — it is a
  short-lived HTTP client. `--db-path` is not required for it even in
  debug builds.
- **Ack ≠ turn start.** A `200` means the message was accepted onto the
  delivery path (queued or steered); it does not wait for the agent to
  begin a turn.
- **`Cancelling` counts as busy** for steer routing — a steer during a
  cancel drain is accepted and lands at the next boundary; new *turn*
  work is not started mid-cancel.
