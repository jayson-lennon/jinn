# Remote: messaging a running instance from outside

Every running jinn instance serves an **unauthenticated HTTP control surface
on loopback** and registers itself in an instance registry. Scripts, hooks,
or plain `curl` can deliver a message into a live session of that instance.
The `jinn remote` subcommand is the friendly client for the same surface;
use it first. Reach for curl when you're already in a shell script or hook.

**Strictly delivery.** An external send lands in an existing live session,
and delivery is the surface's entire job; bot sessions are refused. The
listener binds loopback only, and the OS user boundary is the security
model, same as the canvas server. Text arrives verbatim; write your own
attribution into the message if you want it.

## Find instances

| Command                 | What it does                                                              |
| ----------------------- | ------------------------------------------------------------------------- |
| `jinn remote instances` | One row per running instance: PID, PORT, STARTED, CWD                     |
| `jinn remote send`      | Deliver a message (flags below)                                    |

Each running instance registers `{pid, port, cwd, started_at}` in a registry
file under the state dir. Omit the instance and **the newest one is the
target**; `--instance <pid>` picks a specific one. Every reader probes
each row's port and prunes the row once it stops answering, so a killed
instance drops out of every listing at once, ahead of the row rewrite on
disk.

## Send: queue vs steer

| Flag              | Meaning                                                                        |
| ----------------- | ------------------------------------------------------------------------------ |
| `--queue`         | Default. Waits until the session's current turn finishes, then runs as the user's next turn |
| `--steer`         | Lands as a steering fragment at the session's next prompt-assembly boundary (mid-turn guidance, leaves work running) |
| `--instance <pid>`| Target a specific instance by PID; the default target is the newest             |

A steer aimed at an **idle** session falls back to queue delivery; the CLI
prints `(idle target; the steer was delivered as a queue message)`, so a
script survives the race between its check and the session going idle.
`--queue` and `--steer` together are a usage error.

```bash
jinn remote send --session 01a1a05a-f0f3-77d6-8521-b48c3e420f40 "build finished"
jinn remote send --session 01a1a05a-f0f3-77d6-8521-b48c3e420f40 --steer "also run the tests"
jinn remote send --instance 4242 --session 01a1a05a-f0f3-77d6-8521-b48c3e420f40 "for that instance"
```

Errors are named sentences, exit 1: unknown `--session`, unknown
`--instance`, zero instances running.

## The HTTP surface (plain curl)

| Route       | Method | Purpose                                                        |
| ----------- | ------ | -------------------------------------------------------------- |
| `/health`   | GET    | `{"pid":…,"version":"…","port":…}` (liveness probe)            |
| `/sessions` | GET    | Live sessions: `id`, `title`, `phase` (`idle`/`streaming`/`working`/`cancelling`), `bot` |
| `/send`     | POST   | Deliver a message; JSON body                                   |

The port comes from `jinn remote instances` (or the registry); discovery
runs through jinn. All requests are plain `http://127.0.0.1:<port>/…`.

```bash
PORT=$(jinn remote instances | awk 'NR==2{print $2}')
SESS=$(curl -s http://127.0.0.1:$PORT/sessions | jq -r '.sessions[0].id')

# Queue a message (Content-Type is required; without it: 400).
curl -s -X POST http://127.0.0.1:$PORT/send \
  -H 'Content-Type: application/json' \
  -d "{\"session_id\":\"$SESS\",\"text\":\"hello via curl\"}"
# → {"status":"queued","session_id":"01a1a05a-…","fell_back":false}

# Steer instead:
curl -s -X POST http://127.0.0.1:$PORT/send -H 'Content-Type: application/json' \
  -d "{\"session_id\":\"$SESS\",\"text\":\"change course\",\"mode\":\"steer\"}"
# → {"status":"steered",…,"fell_back":false}   (busy target)
# → {"status":"queued",…,"fell_back":true}     (idle target; fell back)
```

The send body is `{"session_id": …, "text": …, "mode": "queue"|"steer"}`;
`mode` is optional and defaults to `queue`. Error responses are JSON
`{"error": "…"}`:

| Situation              | Status | Error body says                                  |
| ---------------------- | ------ | ------------------------------------------------ |
| Missing/incorrect `Content-Type` | 400 | `malformed request body: …`          |
| Session id of another shape | 400  | `session_id is not a valid session id`           |
| `text` missing         | 400    | `text must not be empty`                         |
| Target is a bot session| 400    | `session … is a bot run; external delivery does not target bot sessions` |
| Ended or unknown session id | 404  | `no live session with id …`                      |

## Configuration

The bind lives in `jinn.toml` (see `configuration.md` for the path and
the reload categories; a bind edit takes effect at once):

```toml
[remote]
# bind = "127.0.0.1:0"   # default: loopback, OS-assigned ephemeral port
```

Port `0` means the OS picks a free port at launch, so concurrent instances
each get their own; the resolved port is what `jinn remote instances`
reports. The launch refuses a non-loopback bind, keeping the surface
loopback-only. A bind edit rebinds the running surface at once; the
dashboard row names the new port.

## Gotchas

- **The CLI needs the instance alive, not just installed.** `jinn remote
  instances` reads the registry, then probes each port. A crashed instance
  leaves a stale row that the probe filters out of every listing.
  Conversely, a *running* instance from a build that predates `[remote]`
  exposes only its dashboard; the registry row exists solely for builds
  with the remote slice.
- **`jinn remote` touches only the delivery path.** It is a short-lived
  HTTP client. `--db-path` stays optional for it, debug builds included.
- **Ack ≠ turn start.** A `200` means the message was accepted onto the
  delivery path (queued or steered). Delivery happens when the agent
  picks the message up on its next turn or boundary.
- **`Cancelling` counts as busy** for steer routing. A steer during a
  cancel drain is accepted and lands at the next boundary; the turn
  finishes unwinding before any new turn starts.
