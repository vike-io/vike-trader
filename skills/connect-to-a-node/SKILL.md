---
name: connect-to-a-node
description: Get vike-cli mcp talking to a running vike-tradehub node so node_snapshot and the order-write tools work. Use when the user asks to connect to a node, point vike at a tradehub, set --node, configure VIKE_TRADEHUB_OBSERVE_KEY or VIKE_TRADEHUB_CONTROL_KEY, reach a remote node over an ssh tunnel, or when a tool returns "no vike-tradehub node configured", "set neither in the process environment nor in the credential store", "cannot open observe connection", "cannot open control connection", a "control command not sent" error ending in Gone, or an UNKNOWN outcome after a tunnel drop. Covers key precedence (env over store), loopback vs tunnel, error shapes, and the fixed recovery order after a dropped connection.
metadata:
  tools: "list_templates node_snapshot"
  source: "ai/trader/setup ai/trader/setup/remote-node ai/trader/setup/claude-code ai/trader"
---

# Connect vike-cli mcp to a vike-tradehub node

`vike-cli mcp` is a local stdio MCP server the client launches as a subprocess. Without `--node` the
offline authoring tools and the datahub backtest tools still work; the node is what `node_snapshot`
reads and what the seven order-write tools write to. This procedure gets that node reachable and
proves it with `node_snapshot`, the one tool this skill calls.

## Step 1 — Establish what the node is listening on

- The daemon listens on nothing until `tradehub_addr` in `<project>/settings/config.toml` (or
  `VIKE_TRADEHUB_ADDR`) names an address. `127.0.0.1:7879` is the conventional one
  (`crates/vike-tradehub/src/server.rs`'s `DEFAULT_ADDR`).
- Keep it loopback. A wildcard or non-loopback bind is classified public by `bind_decision` in
  `crates/vike-tradehub/src/server.rs`, and without the separate opt-in
  `flags.tradehub_allow_public_bind` (or `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1`) the observe server is
  not started while the daemon keeps trading headless. The node protocol is plaintext and
  authenticates the connection, not each frame — a tunnel or VPN supplies confidentiality and
  in-flight integrity (https://vike.io/docs/ai/trader/setup/remote-node).
- The daemon also needs `VIKE_TRADEHUB_OBSERVE_KEY` in its own `<project>/settings/secrets.env`
  at bind time, or it starts no server; control is gated once more by `flags.tradehub_control`.

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

Nothing on the daemon changes. Bring its loopback port to your machine:

```sh
ssh -L 7879:localhost:7879 <host>
```

`7879` on the left is the port opened on your machine; `localhost:7879` is evaluated on the remote
host, so it is the daemon's loopback. For a tunnel that outlives a terminal:

```sh
ssh -N -f -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
    -o ExitOnForwardFailure=yes -L 7879:localhost:7879 <host>
```

`ServerAlive*` makes a silently dead link fail within about 90 seconds; `ExitOnForwardFailure`
turns a taken local port into a non-zero exit instead of a tunnel that forwards nothing. `autossh`
or a systemd user unit with `Restart=always` re-establishes it. If the datahub lives on the same
host, add `-L 7878:localhost:7878` to the same command. Then point the client at the local end
exactly as in Step 2: `vike-cli mcp --node 127.0.0.1:7879` (https://vike.io/docs/ai/trader/setup/remote-node).

Inside a container `127.0.0.1` is the container's own loopback, so a containerised `vike-cli` needs
an address it can actually reach (https://vike.io/docs/ai/trader/setup).

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
  the client resolves is *your* `<project>/settings/secrets.env`, not the daemon's — put both values
  in it or export them (https://vike.io/docs/ai/trader/setup/remote-node).
- The scopes are separate at the node too: an observe-scoped connection that sends a command is
  refused, and a control key cannot authenticate a read (https://vike.io/docs/ai/trader).
- A GUI-launched client (Claude Desktop, Cursor, VS Code) inherits none of your shell's exports:
  use the store or the client's own `env` block. Claude Code is started from your terminal and does
  inherit exports; absent those, which store answers depends on where you launched it unless
  `VIKE_SETTINGS_DIR` names a directory outright (https://vike.io/docs/ai/trader/setup/claude-code).
- `vike-cli secrets path` prints which store this project resolves to; `vike-cli secrets list`
  shows whether a key is in it, by name — never values.
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

A write tool's preview is never evidence that `--node` arrived: a call without both `confirm: true`
and a valid `preview_token` returns a preview whatever the flag says, and the connection error
surfaces only on a fully confirmed call. A node snapshot is the evidence (https://vike.io/docs/ai/trader/setup).

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
(https://vike.io/docs/ai/trader/setup).

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

1. Re-establish the tunnel (Step 3).
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
