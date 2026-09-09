---
name: connect-to-a-node
description: Get vike-cli mcp talking to a running vike-tradehub node so node_snapshot and the order-write tools work. Use when the user asks to connect to a node, attach a laptop to a tradehub, run vike-cli node setup or vike-cli node connect, mint or rotate the node keys, point vike at a tradehub, set --node, configure VIKE_TRADEHUB_OBSERVE_KEY or VIKE_TRADEHUB_CONTROL_KEY, reach a remote node over an ssh tunnel, or when a tool returns "no vike-tradehub node configured", "set neither in the process environment nor in the credential store", "cannot open observe connection", "cannot open control connection", a "control command not sent" error ending in Gone, or an UNKNOWN outcome after a tunnel drop. Covers key precedence (env over store), loopback vs tunnel, error shapes, and the fixed recovery order after a dropped connection.
metadata:
  tools: "list_templates node_snapshot"
  source: "ai/trader/setup ai/trader/setup/remote-node ai/trader/setup/claude-code ai/trader"
---

# Connect vike-cli mcp to a vike-tradehub node

`vike-cli mcp` is a local stdio MCP server the client launches as a subprocess. Without `--node` the
offline authoring tools and the datahub backtest tools still work; the node is what `node_snapshot`
reads and what the seven order-write tools write to. This procedure gets that node reachable and
proves it with `node_snapshot`, the one tool this skill calls.

## Do this with the two verbs — the steps below are what they perform

`vike-cli node` is the onboarding surface, and every line of it says which BOX it runs on
(`crates/vike-cli/src/cmd/node/mod.rs`'s `USAGE`). "Node" is the running `vike-tradehub` daemon.

```sh
# on the DAEMON's box — mint both keys, set the bind address, print each key's ID
vike-cli node setup [--addr 127.0.0.1:7879] [--control] [--rotate]

# on the CLIENT's box — raise the tunnel, take the keys, record the dial address, VERIFY
printf '%s\n%s\n' "$OBSERVE" "$CONTROL" | vike-cli node connect <host> --manual
vike-cli node status        # what this box reaches, which keys resolved and from where, live
vike-cli node disconnect    # close the forward `connect` raised
```

`setup` MINTS both keys from the CSPRNG (`crates/vike-cli/src/cmd/node/mod.rs`'s `mint_key`); it
accepts no key, prints no key, and has no argv, stdin or `--from-env` form for one. What it prints is
each key's `key_id` — an HMAC fingerprint under a domain separator proved disjoint from the auth
domain, so it is safe to read aloud, paste into an issue and keep
(`crates/vike-cli/src/cmd/node/mod.rs`'s `key_id`). `connect` prints the same ids after its round
trip — the observe one always, the control one only when a control key resolved
(`crates/vike-cli/src/cmd/node/connect.rs`'s `verify`), so an observe-only box compares ONE id
rather than a pair. **Comparing the two pairs is how you know you reached the right node with the right key** — a
mismatch is a different node, not a bad password.

`connect` finishes with a real observe-scoped `vike_tradehub_client::strategy_status` round trip
rather than writing files
and reporting success, because files leave three links untested: the tunnel, the key, and the node's
own arming (`crates/vike-cli/src/cmd/node/connect.rs`'s `verify`). Without `--manual` it writes no
key at all — it raises the tunnel, records the dial address and verifies with whatever this box
already resolves. That is the ordinary second run.

⚠ **Three refusals, each of them the correct answer rather than an obstacle:**

- **No store.** `setup` and `connect --manual` upsert into an EXISTING `<project>/settings/secrets.env`
  and create none (`crates/vike-cli/src/cmd/node/mod.rs`'s `open_store`, called from those two alone).
  `status` and `disconnect` never open the store, so on a storeless box they fail on the ABSENT
  observe key instead — a different message, and the right one. Make one first —
  `vike-cli secrets template > settings/secrets.env`, then `chmod 600` it — and re-run.
- **Keys already in the store.** `setup` refuses without `--rotate`, and rotation is not an
  idempotent re-run: every client still holding the old pair — a laptop's `vike-cli`, a
  `vike-app --observe`, a thin container — stops at the daemon's next restart, and its symptom is an
  auth denial that reads like a revoked credential
  (`crates/vike-cli/src/cmd/node/setup.rs`'s `rotation_refusal`).
- **A different local key.** `connect --manual` refuses to overwrite a key this box already holds
  under that name, printing BOTH key ids, because replacing it silently detaches whatever node that
  one reached (`crates/vike-cli/src/cmd/node/connect.rs`'s `replace_refusal`). Pass `--replace` when
  you mean it.

The settings are written BEFORE the keys are minted, deliberately
(`crates/vike-cli/src/cmd/node/setup.rs`'s `run_setup`): a settings write is reversible and a mint is
not, so a malformed `--addr` fails with the store untouched instead of leaving a box with fresh keys,
a stale address and no way back to the pair its clients already hold.

⚠ **`node connect` does NOT make `--node` optional for `vike-cli mcp`.** It records
`config.node_addr`, and that key reaches the `node` arm alone today — `mcp`, `trade` and
`strategy-status` each still parse `--node` as mandatory (`crates/vike-cli/src/lib.rs`'s
`node_addr`). Step 2 below is still the step that points the MCP server at the node.

⚠ **`node disconnect` is not automated on Windows**, and it says so instead of reporting a success
that closed nothing: `ssh -f` forks, so there is no pid to keep, and matching a command line needs a
Win32 call this workspace forbids. It prints the exact PowerShell to run
(`crates/vike-cli/src/cmd/node/connect.rs`'s `run_disconnect`).

The rest of this page is what those verbs do for you — and the fallback for a node you did not stand
up, where nobody ran `setup` on your behalf and the keys arrive from whoever did.

## Step 1 — Establish what the node is listening on

- The daemon listens on nothing until `tradehub_addr` in `<project>/settings/config.toml` (or
  `VIKE_TRADEHUB_ADDR`) names an address. `127.0.0.1:7879` is the conventional one
  (`crates/vike-tradehub/src/server.rs`'s `DEFAULT_ADDR`), and it is what `node setup --addr`
  defaults to (`crates/vike-cli/src/cmd/node/mod.rs`'s `DEFAULT_BIND_ADDR`).
- Keep it loopback. A wildcard or non-loopback bind is classified public by `bind_decision` in
  `crates/vike-tradehub/src/server.rs`, and without the separate opt-in
  `flags.tradehub_allow_public_bind` (or `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1`) the observe server is
  not started while the daemon keeps trading headless. The node protocol is plaintext and
  authenticates the connection, not each frame — a tunnel or VPN supplies confidentiality and
  in-flight integrity (https://vike.io/docs/ai/trader/setup/remote-node).
- The daemon also needs `VIKE_TRADEHUB_OBSERVE_KEY` in its own `<project>/settings/secrets.env`
  at bind time, or it starts no server; control is gated once more by `flags.tradehub_control`.
- `node setup` writes the address every time and touches the control flag ONLY under `--control` —
  never defaulted, never inferred from whether the daemon is live, because arming the write channel
  is a loosening and this surface keeps a loosening ceremonious. Both keys are read ONCE, at daemon
  start: there is no credential hot-reload, so nothing changes until you restart it.

## Step 2 — Local node: pass `--node`

```sh
vike-cli mcp [--addr 127.0.0.1:7878] [--node <host:port>]
```

`--node` has no default. Give the client entry `--node 127.0.0.1:7879`; for Claude Code the `--`
is what hands the flag to `vike-cli` rather than to `claude mcp add`:

```sh
claude mcp add vike-trader -- vike-cli mcp --node 127.0.0.1:7879
```

Restart any client session that was already open — sessions read the server list at start
(https://vike.io/docs/ai/trader/setup/claude-code).

## Step 3 — Remote node: tunnel first, then the same `--node`

Nothing on the daemon changes. `vike-cli node connect <host>` raises the forward for you — the
LONG-LIVED form two blocks down is the one it issues, not this first one. Both are here because the
minimal shape is what explains the syntax. Bring the daemon's loopback port to your machine:

```sh
ssh -L 7879:localhost:7879 <host>
```

`7879` on the left is the port opened on your machine; `localhost:7879` is evaluated on the remote
host, so it is the daemon's loopback. For a tunnel that outlives a terminal:

```sh
ssh -N -f -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
    -o ExitOnForwardFailure=yes -L 7879:localhost:7879 <host>
```

**That block is what `vike-cli node connect` issues** — option for option
(`crates/vike-cli/src/cmd/node/connect.rs`'s `raise_tunnel`) — so it is also what to type when you
are working without the verb.

`ServerAlive*` makes a silently dead link fail within about 90 seconds; `ExitOnForwardFailure`
turns a taken local port into a non-zero exit instead of a tunnel that forwards nothing — and with
`-f` backgrounding ssh only AFTER the forward is established, that pair is what makes the tunnel's
success synchronously observable instead of something to poll for. `autossh` or a systemd user unit
with `Restart=always` re-establishes it. If the datahub lives on the same host, add
`-L 7878:localhost:7878` to the same command. Then point the client at the local end exactly as in
Step 2: `vike-cli mcp --node 127.0.0.1:7879` (https://vike.io/docs/ai/trader/setup/remote-node).

A client already inside the trust boundary needs no forward at all: `node connect --no-tunnel` dials
the host directly and raises none. Inside a container `127.0.0.1` is the container's own loopback, so
a containerised `vike-cli` needs an address it can actually reach
(https://vike.io/docs/ai/trader/setup).

## Step 4 — Supply the two HMAC keys

| Key | Grants | Used by | Absent means |
| --- | --- | --- | --- |
| `VIKE_TRADEHUB_OBSERVE_KEY` | read | `node_snapshot` | 1 tool disabled |
| `VIKE_TRADEHUB_CONTROL_KEY` | write | the seven order-write tools | 7 tools disabled |

- Each key is resolved independently, per key: **the process environment first, the credential
  store (`<project>/settings/secrets.env`) second**. A value blank after trimming counts as absent
  in both places, so an empty export cannot shadow a stored key. Values are trimmed on both sides
  because the daemon trims too (`resolve` in `crates/vike-cli/src/cmd/nodekeys.rs`).
- The values must be the same ones the daemon loaded on its machine. On a remote setup the store
  the client resolves is *your* `<project>/settings/secrets.env`, not the daemon's —
  `node connect --manual` is what writes them into it
  (https://vike.io/docs/ai/trader/setup/remote-node).
- **`--manual` is the ONE path on which a human touches key material**, and it reads both keys from
  **stdin**, one per line, observe then control — never argv, which lands in shell history and in
  `ps` output for every user on the box. A blank second line means observe-only: that box watches
  the node and can place nothing (`crates/vike-cli/src/cmd/node/connect.rs`'s
  `read_keys_from_stdin`). Editing the store by hand does the same job and is the fallback when
  `vike-cli` is not on the client at all.
- Every local precondition — the settings directory, the store's existence and readability, the key
  names — is settled BEFORE the tunnel is raised and BEFORE stdin is read, so a refusal never
  consumes key material you pasted from another box's screen and may not have a second copy of
  (`crates/vike-cli/src/cmd/node/connect.rs`'s `run_connect`).
- The scopes are separate at the node too: an observe-scoped connection that sends a command is
  refused, and a control key cannot authenticate a read (https://vike.io/docs/ai/trader). A control
  key resolving on YOUR box says nothing about whether the node admits orders — that is
  `flags.tradehub_control` on the daemon's box, which this side cannot read, and `node status` says
  so rather than letting you conclude otherwise
  (`crates/vike-cli/src/cmd/node/connect.rs`'s `status_lines`).
- A GUI-launched client (Claude Desktop, Cursor, VS Code) inherits none of your shell's exports:
  use the store or the client's own `env` block. Claude Code is started from your terminal and does
  inherit exports; absent those, which store answers depends on where you launched it unless
  `VIKE_SETTINGS_DIR` names a directory outright (https://vike.io/docs/ai/trader/setup/claude-code).
- `vike-cli secrets path` prints which store this project resolves to; `vike-cli secrets list`
  shows whether a key is in it, by name — never values. `vike-cli node status` adds the half those
  two cannot: each key's `key_id`, and which of the two places it came from.
- The keyring is resolved once, when the server starts (`run` in `crates/vike-cli/src/cmd/mcp.rs`
  takes the already-resolved `NodeKeyring`), so a rotated key needs a restarted `vike-cli mcp`.

## Step 5 — Prove the path with `node_snapshot`

1. Ask for the offline `list_templates` first if the client wiring itself is in doubt — it needs no
   node, no key and no network, so it isolates the launch from every address and key question.
2. Call **`node_snapshot`** — it takes no arguments (`inputSchema.properties` is empty). It reads
   the running node's live state: orders, positions, per-venue equity, recent events. It opens the
   observe connection lazily and waits up to about 2 s for the node's first pushed frame
   (`tool_node_snapshot` in `crates/vike-cli/src/cmd/mcp.rs`).
3. Read `seq` in the result. A real frame has a non-zero `seq`; `seq: 0` is the empty placeholder
   the connection holds before the node pushes anything.

`vike-cli node status` proves the same chain from a shell with no MCP server in the picture, which
is what separates "the node is unreachable" from "the MCP server was launched wrong". A write tool's
preview is never evidence that `--node` arrived: a call without both `confirm: true` and a valid
`preview_token` returns a preview whatever the flag says, and the connection error surfaces only on a
fully confirmed call. A node snapshot is the evidence (https://vike.io/docs/ai/trader/setup).

## Error shapes and what each one means

All of these arrive as tool results with `isError: true`, not as JSON-RPC errors
(https://vike.io/docs/ai/trader). The exact text is in `crates/vike-cli/src/cmd/mcp.rs`:

| Text | Where | Meaning and fix |
| --- | --- | --- |
| `no vike-tradehub node configured — pass --node <host:port> to read node state` | `Server::ensure_observe` | `--node` never reached `vike-cli`; checked before any socket opens. For Claude Code, most often a missing `--` in the add command. The control twin ends `to enable order-write tools`. |
| `<KEY> is set neither in the process environment nor in the credential store — run vike-cli secrets path ... vike-cli secrets list ...` | `Server::missing_key` | That key is in neither place. Check the source you expected: `vike-cli secrets path` for the store, the shell for the export. |
| `cannot open observe connection to <addr>: <error>` / `cannot open control connection to <addr>: <error>` | `ensure_observe` / `ensure_control` | The flag and key were present but the connect failed — usually a refused connection because nothing listens on the local port (tunnel down, daemon not bound). Nothing was sent. |
| `control command not sent: Gone` | `Server::execute` | The control handle was opened earlier and its connection has since died; nothing reopens it in this process. Recovery below. |
| `the control connection dropped before the node answered this command — its outcome is UNKNOWN and it may have executed. Call node_snapshot to check BEFORE retrying.` | `Server::execute` (`CommandOutcome::Disconnected`) | Mid-command drop. Treat as unknown. |
| `outcome: "unknown"` in a non-error result (`sent: true`) | `Server::execute` | The node did not answer within 2 s (`ACK_WAIT`). It may still execute. |

A wrong key value (as opposed to a missing one) fails the handshake as an opaque `AuthDenied`
(https://vike.io/docs/ai/trader/setup). ⚠ **A version skew fails the SAME way** — the protocol version is
folded into the signed message, so an old client against an upgraded daemon reports an auth denial
rather than a version error. Check the builds match before rotating anything: a rotation that was
not needed has already been paid for once, and `node connect`'s own refusal now says so beside the
key id the box presented.

## After a tunnel drop — the fixed recovery order

The observe and control connections are opened lazily and kept for the life of the `vike-cli mcp`
process (`Server` in `crates/vike-cli/src/cmd/mcp.rs`). Four shapes, by when the tunnel went
(https://vike.io/docs/ai/trader/setup/remote-node):

1. **Before first use** — `ensure_observe`/`ensure_control` return the `cannot open ... connection`
   error above. Nothing was sent.
2. **Mid-command** — the write tool returns the `Disconnected` error: outcome unknown, it may have
   executed.
3. **While idle, between calls** — the commonest shape. Nothing notices a dead control socket until
   the next write uses it, so the first write after an idle drop gets the mid-command answer even
   though the tunnel had been gone for minutes. Treat it exactly as case 2.
4. **Afterwards, same process** — every later write is refused with `control command not sent:
   Gone`, and worse for reads: `tool_node_snapshot` does not consult the handle's connected state
   and simply returns the last frame the receive thread stored, so a `node_snapshot` after the drop
   returns a **stale** frame with no error.

Recovery, in this order and never shortcut:

1. Re-establish the tunnel — `vike-cli node connect <host>` with no `--manual` (the keys are already
   on this box), or Step 3 by hand. Run `vike-cli node disconnect` first if a half-dead forward
   still owns the local port; `ExitOnForwardFailure` refuses on that rather than forwarding nothing.
2. **Restart the `vike-cli mcp` process** — for a GUI client, restart the session that launched it.
   The dead handle is never reopened otherwise.
3. Call **`node_snapshot`** and confirm a fresh `seq`.
4. Only then retry anything whose outcome was unknown — and only after the snapshot shows it did
   not already execute.

## The write-tool gate, as it applies here

Every write tool (submit_order, cancel_order, modify, flatten, market_exit, mass_cancel,
set_trading_state — none of which this skill calls) executes only through two calls
(`Server::call_tool` in
`crates/vike-cli/src/cmd/mcp.rs`): first WITHOUT `confirm` — read `will_execute` (`false`),
`node_verdict` (the node's own dry-run, or `checked_by: "none"` when the node could not be asked)
and the `preview_token`; then again with BOTH `confirm: true` AND that `preview_token`. The token
fires once, expires after 60 s (`PREVIEW_WINDOW`), and is bound to the command it previewed;
`confirm: true` alone returns another preview — not an error, not an execution. An unknown outcome
means the command MAY HAVE EXECUTED: call `node_snapshot` before retrying anything.

Connection state matters to that gate: a preview whose `node_verdict` says `the node could not be
asked` is the tunnel or the key, not the order — fix the connection (this skill) before confirming
anything, because `guardrail` on that path is an unverified client-side estimate.
