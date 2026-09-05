---
name: arm-a-venue
description: What to do when a user asks an agent to arm, enable, activate, go live on, switch to demo on, connect, or "turn on" a venue (binance, bybit, okx, deribit, or any other venue) for real trading through Vike. No MCP tool can arm a venue — the per-venue ceiling in policy.toml and the credential store secrets.env are both human-only edits — so this skill tells the agent to stop trying, hand the human the two gates in the right order, and then verify what it can from node_snapshot. Also use when a user asks "why is this venue still paper", "why did my order go to the paper book", "I set live and it is still paper", or asks the agent to put API keys somewhere.
metadata:
  tools: "node_snapshot"
  source: "trader/guides/arm-a-venue ai/trader/tools ai/trader/trading"
---

# Arm a venue

## The one thing to say first

You cannot do this. **No MCP tool can arm a venue.** A write tool mounts nothing: it hands a
command to a node that is already running, and what that node may do to a venue was decided at
*its* mount, not by anything you send. A venue with no credentials mounts as paper, and the
per-venue ceiling `policy.venues.<venue>` defaults to `paper` and can only ever refuse — it lowers
what credentials would otherwise allow and can never raise it. The fold is
`crates/vike-config/src/venue_mode.rs`'s `VenueMode::cap`, a minimum and never a maximum
(https://vike.io/docs/ai/trader/trading, https://vike.io/docs/ai/trader/tools).

So do not:

- try submit_order "to see whether it goes live" — it cannot change where the order goes;
- write to `policy.toml` or `secrets.env` yourself, or ask for API keys in the chat;
- report a venue as armed because a write tool returned `outcome: "accepted"`.

Tell the human the two gates below, in this order, and that both are edits they make on the box
running the node. Then the node must be started again: the ceiling and the credentials are read by
`crates/vike-mount/src/lib.rs`'s `make_engine` at mount, so a node that was already up before the
edits still trades whatever it mounted with (https://vike.io/docs/trader/guides/arm-a-venue).

## Gate 1 — the ceiling, consulted first

`make_engine` consults the deployment's per-venue arming ceiling **before** it reads any
credential: a `paper` venue returns the paper client having loaded no credential, fetched no
instrument grid and opened no socket. This is the usual reason a store full of keys still mounts
paper (https://vike.io/docs/trader/guides/arm-a-venue).

The human writes one line per venue in `<project>/settings/policy.toml`:

```toml
[venues]
binance = "demo"
```

Facts to carry with it (all from https://vike.io/docs/trader/guides/arm-a-venue):

1. The tiers are `paper < demo < live` (`crates/vike-config/src/venue_mode.rs`'s `VenueMode`).
   `demo` is the venue's own demo/testnet/sandbox account — real wire, real rejections, no real
   funds. Recommend `demo` unless the human explicitly wants mainnet.
2. **A ceiling only ever refuses.** `venues.bybit = "live"` does not put bybit live; it declines
   to stop bybit going live if its credentials, its `{VENUE}_MAINNET` flag and its mount arm all
   already say so. Arming still takes the credentials (gate 2).
3. The default with no `policy.toml` is a filled map, one `paper` row per roster venue. `Policy`
   has no environment layer at all (`crates/vike-config/src/policy.rs`) — there is no env variable
   that raises a ceiling, so do not suggest one.
4. If the box had credentials and never wrote a `[venues]` table, the first mount prints a
   paste-ready block naming every venue that has credentials and is now paper
   (`crates/vike-mount/src/paper_fallback.rs`'s `venue_arming_migration_message`). Writing the
   table at all silences it, including all `paper`.

## Gate 2 — the credentials, in the one store

There is one file and no precedence: `<project>/settings/secrets.env`
(`crates/vike-secrets/src/lib.rs` is the authority). `<project>` is resolved at runtime by walking
up for a project marker; `VIKE_SETTINGS_DIR` names the `settings` directory outright — the level
holding `secrets.env`, not `<project>` above it (https://vike.io/docs/trader/guides/arm-a-venue).

Tell the human to ask the binary rather than guess, with these read-only commands on the box:

```sh
vike-cli secrets path       # where it resolved for THIS invocation, and its exposure
vike-cli secrets template   # the key grid, empty values, to stdout
vike-cli secrets list       # key NAMES only, never a value
```

`vike-cli secrets` is read-only by construction and `template` has no `--out` flag on purpose;
creating the file is the human's editor's job — they redirect the template into it themselves
(https://vike.io/docs/trader/guides/arm-a-venue).

Key facts for the human (from https://vike.io/docs/trader/guides/arm-a-venue):

1. Names are `{VENUE}_{SIM|DEMO|LIVE}_API_KEY` / `_API_SECRET` / `_API_PASSPHRASE`; FX venues
   have bespoke shapes, and a second account per venue appends `__{LABEL}` after the whole key.
2. Required is per-venue: key + secret for binance, bybit and deribit; **OKX additionally
   requires `_API_PASSPHRASE`**. A store with two of the three resolves `None` and the venue
   stays paper exactly like an unconfigured one — `crates/vike-bridge-core/src/credentials.rs`'s
   `missing_required_passphrase` names the missing variable at the mount.
3. Absent credentials **are** the live gate: no store means every venue stays paper. A store that
   exists and cannot be read is a different case and errors.
4. **Never arm mainnet from the credential file.** `crates/vike-config/src/arming.rs` refuses
   startup when a `{VENUE}_MAINNET` line in `secrets.env` carries the exact value `1` — refused,
   not ignored, so an operator who wrote it does not read demo fills as real ones.

## Gate 3 — check before starting

Have the human run `vike-cli config check` (`crates/vike-cli/src/cmd/config_check.rs`): it
resolves the same directory through the same loader as `config show` and returns an exit code. An
absent store is `ok` (that is the live gate); a `VIKE_SETTINGS_DIR` naming a non-directory is a
failure; an unreadable store is a warning that becomes a failure only on a box armed for live
(https://vike.io/docs/trader/guides/arm-a-venue).

## Verify afterwards — what `node_snapshot` can and cannot tell you

After the human has made both edits and started the node, call **`node_snapshot`**. It takes no
arguments (`inputSchema.properties` is empty in `tools_spec`) and reads the running node's live
state: orders, positions, per-venue equity, recent events. It needs the server started with
`--node <host:port>` plus `VIKE_TRADEHUB_OBSERVE_KEY`; if the key is set neither in the process
environment nor in the credential store, the tool error names both places and points at
`vike-cli secrets path` / `vike-cli secrets list` (`crates/vike-cli/src/cmd/mcp.rs`'s
`tools_spec`, `Server::ensure_observe`, `Server::missing_key`).

The result is the node's `WireSnapshot` (`crates/vike-tradehub-client/src/wire.rs`) serialized
as-is. The tool waits up to about two seconds for a real frame; a `seq` of `0` is the empty
placeholder, not an answer — call again (https://vike.io/docs/ai/trader/trading, `Server::tool_node_snapshot`).

Read these fields:

1. `venues[]` — one ledger block per venue the node carries, each with `venue`, `balance`,
   `equity`, `trading_state` and `positions`. A venue absent from this list is not mounted on
   this node at all, whatever the files say.
2. `identity.live` — `true` iff the daemon is LIVE (`flags.tradehub_live`), `false` = paper.
   This is the **process-wide** gate, not the venue's tier.
3. `fault` — set once a handler panicked; the core is halted in safe-state.
4. `trading_state` and `recent_events` — the primary engine's state and the bounded journal tail.

**What the snapshot does not carry:** a per-venue `paper` / `demo` / `live` tier. Nothing in
`WireSnapshot` says which tier a venue mounted at, and the one per-venue `live` flag
(`WireMountRow::live`, filled from the mount's arming record) rides on a different response, not
on the snapshot. So report exactly what you can see — "the node is up, it carries these venues,
`identity.live` is X" — and send the human to the node's own startup output for the tier. Do not
infer "demo" or "live" from a balance or an equity number.

## When it is still paper

If the human says "I set it and it is still paper", `crates/vike-config/src/venue_arming.rs`'s
`ArmingBlock` names one observable cause per variant (https://vike.io/docs/trader/guides/arm-a-venue):

| Variant | Meaning | What the human checks |
| --- | --- | --- |
| `Disarmed` | the ceiling | the `[venues]` row in `policy.toml` |
| `NoCredentials` | the store | `vike-cli secrets list` for the required names |
| `FeatureAbsent` | this binary has no arm for the venue (`ibkr`, `polymarket`, `fxcm` features) | the build |
| `MainnetSwitchUnset` | `{VENUE}_MAINNET` is a conjunct of a `live` ceiling, never a replacement | the flag — and not in `secrets.env` |
| `LiveOnlyArm` | Polymarket runs no testnet | there is no `demo` for it |
| `SdkAbsent` | FXCM without its linked SDK | the binary |

## If the conversation goes on to placing an order

Once the venue is armed, order placement is a separate skill and every write tool
(submit_order, cancel_order, modify, flatten, market_exit, mass_cancel and
set_trading_state — none of which this skill calls) is a two-call gate: call once without
`confirm` and read `will_execute`
(`false`), `node_verdict` and the `preview_token`; call again with both `confirm: true` and that
exact `preview_token`. The token fires once, expires after 60 seconds and is bound to the command
it previewed; `confirm: true` on its own returns another preview, not an error and not an
execution. An `outcome: "unknown"` or a dropped-connection error means the command may have
executed — call `node_snapshot` before retrying anything (`crates/vike-cli/src/cmd/mcp.rs`'s
`Server::call_tool` and `Server::execute`). Even then, a ceiling still applies: a write tool
cannot loosen it (https://vike.io/docs/ai/trader/tools).
