---
name: connect-to-a-node
description: Get vike-mcp talking to a running vike-tradehub so node_snapshot and the order-write tools work. Use when the user asks to connect to a node, attach a laptop to a tradehub, run vike-cli backend mint-missing or vike-cli backend connect (formerly vike-cli backend setup, vike-cli node setup or vike-cli node connect), mint the hub keys, point vike at a tradehub, set --node, register a device key or ask about the retired VIKE_TRADEHUB_OBSERVE_KEY or VIKE_TRADEHUB_CONTROL_KEY, reach a remote tradehub over an ssh tunnel, or when a tool returns "no vike-tradehub to dial", "device key is not in its settings database", "cannot open observe connection", "cannot open control connection", a "NOT SENT" error, an observe connection reported DOWN with a STALE seq, or an UNKNOWN outcome after a tunnel drop. Covers the device key and its role on the server's access list, loopback vs tunnel, error shapes, and the fixed recovery order after a dropped connection.
metadata:
  tools: "list_templates node_snapshot"
  source: "ai/trader/setup ai/trader/setup/remote-node ai/trader/setup/claude-code ai/trader"
---

# Connect vike-mcp to a vike-tradehub

`vike-mcp` is a local stdio MCP server the client launches as a subprocess. Without `--node` the
offline authoring tools and the data and backtest tools still work; the tradehub is what `node_snapshot`
reads and what the order-write tools write to. This procedure gets that tradehub reachable and
proves it with `node_snapshot`, the one tool this skill calls.

## Do this with the two verbs — the steps below are what they perform

`vike-cli backend` is the onboarding surface, and every line of it says which BOX it runs on
(`crates/vike-cli/src/cmd/backend.rs`'s `USAGE`). The word "node" in the flag `--node`, in the skill names and in `node_snapshot` / `node_verdict` means the running `vike-tradehub` daemon.

```sh
# on the DAEMON's box — the server install runs this at every start; by hand it mints each missing key
vike-cli backend mint-missing

# on the CLIENT's box — register its own device key over ssh, raise the tunnel, record the dial addresses, VERIFY
vike-cli backend connect <host> --server-project <server-project-dir>
vike-cli backend status     # what this box reaches, its device key, which keys resolved, live
vike-cli backend disconnect # close the forward `connect` raised
```

Every device signs with its OWN key (decision 0288, 2026-11-16): `connect --server-project` makes this box's Ed25519
device key once and pipes its PUBLIC half over the ssh login into the server's `vike-cli access add`, so no shared key
and no server secret is copied to a client; the server's `vike-cli access role` decides what the device may do. A person
with no shell on the server pastes the owner's ONE invitation into `vike-cli backend connect --invitation` (stdin). On
the same box as the server nothing is needed: `backend mint-missing` made that box's own device key.

`mint-missing` MINTS the keys a server lacks from the CSPRNG (`crates/vike-cli/src/cmd/backend.rs`'s `mint_key`); it
accepts no key, prints no key, and has no argv, stdin or `--from-env` form for one. What it prints is
each key's `key_id` — an HMAC fingerprint under a domain separator proved disjoint from the auth
domain, so it is safe to read aloud, paste into an issue and keep
(`crates/vike-cli/src/cmd/backend.rs`'s `key_id`). `connect` prints the DEVICE key's id after its round trip
(`crates/vike-cli/src/cmd/backend/connect.rs`'s `verify`): **the server's `vike-cli access ls` shows that id beside
this device**, which is how you know you reached the right tradehub with the right key.

`connect` finishes with a real observe-scoped `vike_tradehub_client::strategy_status` round trip
rather than writing files
and reporting success, because files leave three links untested: the tunnel, the key, and the tradehub's
own arming (`crates/vike-cli/src/cmd/backend/connect.rs`'s `verify`). Without `--server-project` it registers
nothing — it raises the tunnel, records the dial address and verifies with the device key this box already holds.
That is the ordinary second run.

⚠ **A refusal that is the correct answer rather than an obstacle:**

- **No store.** `mint-missing` and `connect` write into an EXISTING store and create none
  (`crates/vike-cli/src/cmd/backend.rs`'s `open_store`). That store
  is `<project>/settings/db/vike.db`, the settings database, and nothing else: this box's device key
  is its `device_key` row, a server's list its `access_key` table and the daemons' hub keys its
  `hub_key` table. One `vike_secrets::backend_in` decides it per RUN for the reader and the writer
  alike, so they cannot disagree — and note the keys have their own tables, never the venue store's,
  so a service holding no venue key never reads one.
  `status` and `disconnect` never open the store, so on a storeless box they fail on the ABSENT
  device key instead — a different message, and the right one. Make the store first —
  `vike-cli secrets init` (it creates the EMPTY database; the `device_key`, `access_key` and
  `access_invite` tables are created by their writers when they first write) — and re-run. See the
  `manage-the-credential-store` skill before you run it.

`mint-missing` never replaces a key the store already holds
(`crates/vike-cli/src/cmd/backend/mint_missing.rs`'s `missing`), so running it again is a no-op.

⚠ **`backend connect` does NOT make `--node` optional for `vike-mcp`.** It records
`config.node_addr`, and that key reaches the `backend` arm alone today — `vike-mcp` and every `trade` verb
(the REPL, and the one-shot `status`/`halt`/`resume`) each
still parse `--node` as mandatory (`crates/vike-client/src/resolved.rs`'s `node_addr`, which lists them).
Step 2 below is still the step that points the MCP server at the tradehub.

⚠ **`backend disconnect` is not automated on Windows**, and it says so instead of reporting a success
that closed nothing: `ssh -f` forks, so there is no pid to keep, and matching a command line needs a
Win32 call this workspace forbids. While the forward answers it prints the exact PowerShell to run,
which also removes its record; once nothing answers it says so and exits 0
(`crates/vike-cli/src/cmd/backend/connect.rs`'s `close_by_hand`).

The rest of this page is what those verbs do for you — and the fallback for a tradehub you did not stand
up, where nobody ran `mint-missing` on your behalf and access is granted by whoever did.

## Step 1 — Establish what the node is listening on

- The daemon listens on nothing until `config.tradehub_addr` (a settings-database row — `vike-cli
  config set config.tradehub_addr <addr>`) names an address. `127.0.0.1:7879` is the conventional one
  (`crates/vike-config/src/config.rs`'s `DEFAULT_TRADEHUB_ADDRESS`), and it is what `backend
  mint-missing` writes when no row names one.
- Keep it loopback. A wildcard or non-loopback bind is classified public by `bind_decision` in
  `crates/vike-tradehub/src/server.rs`, and without the separate opt-in
  `flags.tradehub_allow_public_bind` row the observe server is
  not started while the daemon keeps trading headless. The hub protocol is plaintext and
  authenticates the connection, not each frame — a tunnel or VPN supplies confidentiality and
  in-flight integrity (https://vike.io/docs/ai/trader/setup/remote-node).
- The daemon admits a device by its OWN project's access list and nothing else (decision 0288: no
  shared `VIKE_TRADEHUB_*` key is read any more). An EMPTY list admits nobody, refused as "no device
  registered"; `backend mint-missing` registers the server box's own devices, and `vike-cli access`
  the others. Writes are gated once more by `flags.tradehub_control`.
- `backend mint-missing` writes the address only where no row names one, and never touches the control
  flag (ON by default since decision 0280; `vike-cli config set flags.tradehub_control false` turns
  it off). Flags and the daemons' keys are read ONCE, at daemon start: there is no hot-reload, so
  nothing of those changes until you restart it. The access list is read at every handshake, so a
  newly registered device is admitted with no restart.

## Step 2 — Local node: pass `--node`

```sh
vike-mcp [--addr 127.0.0.1:7878] [--node <host:port>]
```

`--node` has no default. Give the client entry `--node 127.0.0.1:7879`; for Claude Code the `--`
is what hands the flag to `vike-mcp` rather than to `claude mcp add`:

```sh
claude mcp add vike-trader -- vike-mcp --node 127.0.0.1:7879
```

Restart any client session that was already open — sessions read the server list at start
(https://vike.io/docs/ai/trader/setup/claude-code).

## Step 3 — Remote node: tunnel first, then the same `--node`

Nothing on the daemon changes. `vike-cli backend connect <host>` raises the forward for you — the
LONG-LIVED form two blocks down is the one it issues, not this first one. Both are here because the
minimal shape is what explains the syntax. Bring the daemon's loopback port to your machine:

```sh
ssh -L 7879:localhost:7879 <host>
```

`7879` on the left is the port opened on your machine; `localhost:7879` is evaluated on the remote
host, so it is the daemon's loopback. For a tunnel that outlives a terminal:

```sh
ssh -N -f -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
    -o ExitOnForwardFailure=yes -L 7879:localhost:7879 -L 7878:localhost:7878 <host>
```

**That block is what `vike-cli backend connect` issues** — option for option
(`crates/vike-client/src/attach.rs`'s `raise_tunnel`) — so it is also what to type when you
are working without the verb.

`ServerAlive*` makes a silently dead link fail within about 90 seconds; `ExitOnForwardFailure`
turns a taken local port into a non-zero exit instead of a tunnel that forwards nothing — and with
`-f` backgrounding ssh only AFTER the forward is established, that pair is what makes the tunnel's
success synchronously observable instead of something to poll for. `autossh` or a systemd user unit
with `Restart=always` re-establishes it. The second `-L` is the datahub's: `data` reads and
`trade health`'s gatewayhub row go through it, and `connect` records it as `config.datahub_addr`.
Then point the client at the local end exactly as in
Step 2: `vike-mcp --node 127.0.0.1:7879` (https://vike.io/docs/ai/trader/setup/remote-node).

A client already inside the trust boundary needs no forward at all: `backend connect --no-tunnel` dials
the host directly and raises none. Inside a container `127.0.0.1` is the container's own loopback, so
a containerised `vike-cli` needs an address it can actually reach
(https://vike.io/docs/ai/trader/setup).

## Step 4 — This box's device key, and its role on the server

Every dial signs with ONE key: this box's own Ed25519 device key (decision 0288), and the server's access
list decides what it reaches (`crates/vike-client/src/hubkeys.rs`'s module doc):

| Role on the server's list | Grants | Used by |
| --- | --- | --- |
| `observe` | read | `node_snapshot` and the tradehub's other reads |
| `control` | read and write, but NO setting | the tradehub's order writes |
| `admin` | everything `control` may, plus settings | the settings write too (decision 0288's 1A) |

- The key is a `device_key` row of this box's settings database (`<project>/settings/db/vike.db`) and
  is never an environment variable, a file or a command-line argument. `backend mint-missing` makes it
  on the server's own box; `backend connect` makes it on any other and registers its PUBLIC half with
  the server. A process that has a retired `VIKE_TRADEHUB_OBSERVE_KEY` / `VIKE_TRADEHUB_CONTROL_KEY`
  exported refuses to start: no shared hub key is read, copied or typed any more.
- The server must list this device: `vike-cli access ls` on the server shows its key id and role, and
  `vike-cli access role` changes the role. A box with no device key, or one the list does not hold, is
  refused at the handshake.
- Every local precondition — the settings directory and the store's existence — is settled BEFORE the tunnel is
  raised and BEFORE stdin is read, so a refusal never costs a guest the invitation it pasted
  (`crates/vike-cli/src/cmd/backend/connect.rs`'s `run_connect`).
- The scopes are separate at the tradehub too: an observe-scoped connection that sends a command is
  refused (https://vike.io/docs/ai/trader). A write role on the server's list says nothing about
  whether the tradehub admits orders — that is
  `flags.tradehub_control` on the daemon's box, which this side cannot read, and `backend status` says
  so rather than letting you conclude otherwise
  (`crates/vike-cli/src/cmd/backend/connect.rs`'s `status_lines`).
- A GUI-launched client (Claude Desktop, Cursor, VS Code) inherits none of your shell's exports,
  and no export carries a key anyway: the device key is a row of the store. Which store answers
  depends on where the client was launched unless `VIKE_SETTINGS_DIR` (the pointer to the
  database, the one variable that matters here) names a directory outright, in the client's own
  `env` block or your shell (https://vike.io/docs/ai/trader/setup/claude-code).
- `vike-cli secrets path` prints which store this project resolves to. `vike-cli backend status` shows
  this box's device key's `key_id` — the id to look for in the server's `vike-cli access ls`.
- The keyring is resolved once, when the server starts (`run` in `crates/vike-mcp/src/lib.rs`
  takes the already-resolved `HubKeyring`), so a new device key needs a restarted `vike-mcp`.

## Step 5 — Prove the path with `node_snapshot`

1. Ask for the offline `list_templates` first if the client wiring itself is in doubt — it needs no
   tradehub, no key and no network, so it isolates the launch from every address and key question.
2. Call **`node_snapshot`** — it takes no arguments (`inputSchema.properties` is empty). It reads
   the running tradehub's live state: orders, positions, per-venue equity, recent events. It opens the
   observe connection lazily and waits for the tradehub's first pushed frame — normally one round
   trip, never more than about 2 s (`tool_node_snapshot` in `crates/vike-mcp/src/node_reads.rs`).
3. Read `identity` in the result. The tradehub stamps it into every frame it pushes, so a non-null
   `identity` proves the whole path; the empty placeholder the connection holds before the tradehub
   pushes anything carries `identity: null`. (A daemon older than the identity block stamps none;
   against one, a non-zero `seq` is the proof.)
4. Then read `pre_fold`. `false` means the frame is the tradehub's state. `true` means the frame
   carries nothing built (`seq: 0`), so `venues`, `balance`, `equity_total` and `accounts_epoch`
   are placeholders and `venues: []` does NOT mean nothing is running — and `identity` says WHICH
   placeholder it is (`pre_fold_note` says the same in words):
   - **`identity` present** — the tradehub's own placeholder, the frame it publishes before its first
     fold. The path is proven; the tradehub's state is not read yet, so call again before reporting
     any of it. An idle paper tradehub with no feed can stay here until its first order.
   - **`identity: null`** — this client's OWN placeholder: the connection opened, but no frame
     from the tradehub arrived within the read's ~2 s wait. Nothing in it came from the tradehub, so it
     proves the handshake and nothing more. Call again. (A daemon older than the identity block
     looks exactly like this until its first fold.)

`vike-cli backend status` proves the same chain from a shell with no MCP server in the picture, which
is what separates "the tradehub is unreachable" from "the MCP server was launched wrong". A write tool's
preview is never evidence that `--node` arrived: a call without both `confirm: true` and a valid
`preview_token` returns a preview whatever the flag says, and the connection error surfaces only on a
fully confirmed call. A `node_snapshot` is the evidence (https://vike.io/docs/ai/trader/setup).

## Error shapes and what each one means

All of these arrive as tool results with `isError: true`, not as JSON-RPC errors
(https://vike.io/docs/ai/trader). The exact text is in `crates/vike-mcp/src/node_reads.rs`
(the read side) and `crates/vike-mcp/src/node_writes.rs` (the write side):

| Text | Where | Meaning and fix |
| --- | --- | --- |
| `error [dial.node.unset]: no vike-tradehub to dial: no --node <host:port> was given and this box's config.node_addr row is not set. ...` | `missing_node` | Neither `--node` nor this box's `config.node_addr` row reached `vike-mcp`; checked before any socket opens. For Claude Code, most often a missing `--` in the add command; `vike-cli backend connect` writes the row, which every node tool (and every `vike-cli trade` verb) then dials without the flag. |
| `error [dial.device_key.not_in_store]: this box's device key is not in its settings database (<path>: <present or absent>). ...` | `Server::missing_key` | This box holds no device key. On the server, `vike-cli backend mint-missing` makes one; any other computer runs `vike-cli backend connect`. `vike-cli secrets path` says which store this project resolves to. |
| `error [dial.device_key.no_settings_directory]: this box's device key could not be read: this program resolved no settings directory ...` | `Server::missing_key` | The server started outside the project folder, so it read no store at all and says nothing about the key. Launch it from inside the project (over ssh, `cd` there first) or set `VIKE_SETTINGS_DIR`. |
| `cannot open observe connection to <addr>: <error>` / `cannot open control connection to <addr>: <error>` | `ensure_observe` / `ensure_control` | The flag and key were present but the connect failed — usually a refused connection because nothing listens on the local port (tunnel down, daemon not bound). Nothing was sent. |
| `the observe connection to <addr> is DOWN (...)` | `Server::observe_down` | A `node_snapshot` found its held connection dead and could not get a live frame on one fresh one. It names the last frame's `seq` as STALE (or says no frame was received) and returns NO frame. Do not act on any earlier snapshot; the next call reconnects by itself. |
| `node <addr>: NOT SENT: ... NOTHING was sent for this call and nothing was retried ...` | `Server::execute` | The held control connection was already dead, so not one byte of the command went out, and the one reconnect this call made failed (or died too). Nothing executed: fix the tunnel, then call the tool again. |
| `node <addr>: OUTCOME UNKNOWN: the control connection dropped after this command was written and before the node answered, so it may have executed. ...` | `Server::offer_command` (`CommandOutcome::Disconnected`) | Mid-command drop. Treat as unknown; it was not resent. |
| `node <addr>: OUTCOME UNKNOWN: the node did not answer within <wait>, so this command may still execute. ...` | `Server::offer_command` | The tradehub did not answer within the ack wait (`config.node_ack_wait_ms`, 6 s unless a row says otherwise). It may still execute. |

A device the server's list does not hold (as opposed to a missing key) fails the handshake as an `AuthDenied`
(https://vike.io/docs/ai/trader/setup). ⚠ **A version skew fails the SAME way** — the protocol version is
folded into the signed message, so an old client against an upgraded daemon reports an auth denial
rather than a version error. Check the builds match, and that the server's `vike-cli access ls`
holds the key id the box presented (`backend connect`'s own refusal names it), before changing any key.

## After a tunnel drop — what each shape answers, and the recovery order

The observe and control connections are opened lazily and held for the life of the `vike-mcp`
process (`Server` in `crates/vike-mcp/src/lib.rs`) — but a dead one is let go of by the call
that finds it dead, and the next call dials a fresh one by itself. Restarting `vike-mcp` is NOT
part of recovering from a drop. The shapes, by when the tunnel went
(https://vike.io/docs/ai/trader/setup/remote-node):

1. **Before first use** — `ensure_observe`/`ensure_control` return the `cannot open ... connection`
   error above. Nothing was sent.
2. **Mid-command** — the write tool returns the `Disconnected` error: the command was written and
   its reply lost, so its outcome is UNKNOWN and it may have executed. It is never resent.
3. **While idle, between calls** — the sender checks the held control link before it writes, so a
   drop the socket REPORTED (the tunnel process exiting, the daemon restarting) is found BEFORE a
   byte goes out: the command was NEVER SENT, and the same call dials once and sends it on the
   fresh connection. If the tunnel is already back you get the tradehub's own answer (accepted or
   refused) as if nothing had happened; if it is not, you get the `NOT SENT: ...
   NOTHING was sent` error — nothing executed, so call the tool again once the tunnel is up
   (`Server::execute` in `crates/vike-mcp/src/node_writes.rs`). A link that died SILENTLY
   cannot be seen before the write: that write is answered UNKNOWN once its reply deadline passes
   — treat it as case 2.
4. **Reads after a drop** — `node_snapshot` never answers from a dead connection. A drop the socket
   reported is an ERROR naming the address DOWN and the last frame's `seq` STALE (or saying no frame
   was received), never that frame as if it were live; the same call lets the dead connection go
   and tries one fresh one, so once the tunnel is back the read simply answers
   (`Server::tool_node_snapshot`). A link that died SILENTLY (no FIN — a sleeping laptop under an
   `ssh -L` without `ServerAliveInterval`) is caught once the tradehub's idle heartbeat goes missing,
   about 45 s, and only against a tradehub that advertises one; until then the last frame is still
   answered as live, and the frame carries no timestamp to age it by.
5. **The daemon itself restarted** — the tradehub comes back with an EMPTY order book. A shipped daemon
   never puts a previous session's orders back into its book at boot: with its journal on it
   restores positions, armed stops and who owns each order (`vike_core::replay::restore_for_boot`),
   and its first reconcile pass adopts the orders the venue still holds; with it off, nothing. So
   the fresh frame is `pre_fold: true` until the tradehub's first fold and then shows only what
   happened since the restart until that pass. An order placed before the restart can still be
   working at the venue while the tradehub shows none.

Recovery, in this order and never shortcut:

1. Re-establish the tunnel — `vike-cli backend connect <host>` with no `--server-project` (this box's
   device key is already registered), or Step 3 by hand. Run `vike-cli backend disconnect` first if a half-dead forward
   still owns the local port; `ExitOnForwardFailure` refuses on that rather than forwarding nothing.
2. Call **`node_snapshot`** — the same `vike-mcp` process, no restart. A frame with `identity`
   set (or, from a daemon older than the identity block, a non-zero `seq`) proves the path is back.
   `pre_fold: true` beside a set `identity` means the tradehub has folded nothing since it started — a
   tradehub that held orders before the drop and answers that way has restarted (case 5).
3. Only then retry anything whose outcome was UNKNOWN — and only after the snapshot shows it did
   not already execute. ⚠ After case 5 the tradehub's book cannot show that: it starts empty, so an
   order's absence there proves nothing about the venue; have the operator check the venue first.
   A `NOTHING was sent` answer needs no such check — nothing executed.

The one case a restart of `vike-mcp` still answers: a tradehub too old to send the idle heartbeat,
behind a link that died silently while its local tunnel process stays up. The held connection then
never learns it is dead, and the same `seq` comes back on every read; closing the old tunnel
(`vike-cli backend disconnect`) ends it as well. A new device key also needs a restart (Step 4).

## The write-tool gate, as it applies here

Every write tool (submit_order, cancel_order, modify_order, flatten, close_all, mass_cancel,
set_trading_state — none of which this skill calls) executes only through two calls
(`Server::call_tool` in
`crates/vike-mcp/src/lib.rs`): first WITHOUT `confirm` — read `will_execute` (`false`),
`node_verdict` (the tradehub's own dry-run, or `checked_by: "none"` when the tradehub could not be asked)
and the `preview_token`; then again with BOTH `confirm: true` AND that `preview_token`. The token
fires once, expires after 60 s (`PREVIEW_WINDOW`), and is bound to the command it previewed;
`confirm: true` alone returns another preview — not an error, not an execution. An unknown outcome
means the command MAY HAVE EXECUTED: call `node_snapshot` before retrying anything.

Connection state matters to that gate: a preview whose `node_verdict` says `the node could not be
asked` is the tunnel or the key, not the order — fix the connection (this skill) before confirming
anything, because `guardrail` on that path is an unverified client-side estimate.
