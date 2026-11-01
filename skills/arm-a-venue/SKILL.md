---
name: arm-a-venue
description: What to do when a user asks an agent to arm, enable, activate, go live on, switch to demo on, connect, or "turn on" a venue (binance, bybit, okx, deribit, or any other venue) for real trading through Vike. No MCP tool can arm a venue — an account trades at its own tier only while its `account` row is active, a settings-database row written with `vike-cli secrets account`, and the credential store — the settings database `settings/db/vike.db`, the only one — is written by nothing but the narrow `vike-cli secrets set` verb — so this skill tells the agent to stop trying, hand the human the two gates in the right order, and then verify what it can from node_snapshot. Also use when a user asks "why is this venue still paper", "why did my order go to the paper book", "I set live and it is still paper", or asks the agent to put API keys somewhere.
metadata:
  tools: "node_snapshot"
  source: "trader/guides/arm-a-venue ai/trader/tools ai/trader/trading"
---

# Arm a venue

## The one thing to say first

You cannot do this. **No MCP tool can arm a venue.** A write tool arms nothing: it hands a
command to a tradehub that is already running, and what that tradehub may do to a venue was decided at
*its* startup, not by anything you send. An account trades at its own `tier` (`paper`, `demo`
or `live`) only while its ACTIVE `account` row states it, or it does NOT RUN: no credentials, no
active row, or a tier its keys cannot bind exactly leaves it not running, never at a lower tier and
never on paper in its place (decision 0156). The decision is `crates/vike-config/src/account_arming.rs`'s `account_tier`
(https://vike.io/docs/ai/trader/trading, https://vike.io/docs/ai/trader/tools).

So do not:

- try submit_order "to see whether it goes live" — it cannot change where the order goes;
- try to write the account row yourself — there is no file to edit, and `vike-cli secrets account`
  is a human's act on the box running the tradehub — or hand-write a credential file (nothing reads
  one), or ask for API keys in the chat;
- report a venue as armed because a write tool returned `outcome: "accepted"`.

Tell the human the two gates below, in this order, and that both are edits they make on the box
running the tradehub. Then the daemons must be started again, the gatewayhub first and the tradehub after it: the venue connections are the
gatewayhub's since wave 2, which reads the account rows once at its start and the credentials when it opens each
account's adapter (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `start_adapter`), and the tradehub builds
its engines from what that arming table answers (`crates/vike-tradehub-core/src/engine.rs`'s `make_engine`), so a daemon that
was already up before the edits still trades whatever it started with (https://vike.io/docs/trader/guides/arm-a-venue).

## Gate 1 — the account row, consulted first

`make_engine` reads the account's `account` rows **before** it reads any credential: an account
with no active row, or whose active rows say `paper`, does not run (a `paper` row runs the
simulator), having loaded no credential, fetched no instrument grid and opened no socket. An
inactive row is the usual reason a store full of keys still runs nothing (https://vike.io/docs/trader/guides/arm-a-venue).

The human runs these on the box running the tradehub (the rows live only in the settings database;
there is no `policy.toml` to hand-edit any more):

```sh
vike-cli secrets accounts                                             # every account row: id, tier, active
vike-cli secrets account add --venue binance --tier demo --no-label   # a new row, born ACTIVE
vike-cli secrets account activate --id 7                              # or switch an existing row on
vike-cli secrets account set-tier --id 7 --tier demo                  # or move it to another tier
```

Facts to carry with it (all from https://vike.io/docs/trader/guides/arm-a-venue):

1. The tiers are `paper < demo < live` (`crates/vike-model/src/accounts/account_tier.rs`'s `AccountTier`).
   `demo` is the venue's own demo/testnet/sandbox account — real wire, real rejections, no real
   funds. Recommend `demo` unless the human explicitly wants mainnet.
2. **The tier is the network, and it is honoured exactly or not at all.** For
   binance/bybit/okx/hyperliquid `live` means MAINNET directly and `demo` the demo network; there
   is no `{VENUE}_MAINNET` flag (it is DELETED, refused if still set anywhere). An account has ONE
   tier and ONE key set: a `live` account's keys are signed against mainnet, and a `live` account
   whose required keys are not all stored does NOT RUN (`LiveCredentialsAbsent`) — it never falls
   back to demo, aster included. To trade both networks the human keeps two accounts (two rows,
   two labels), each holding its own keys. Polymarket has no demo tier at all.
3. **Creating the account ARMS it; storing a key never creates one.** `vike-cli secrets account add`
   writes the row with its tier, born ACTIVE, and `vike-cli secrets set --account <ID> <FIELD>`
   only attaches a key to a row that already exists (an id no row carries is refused). So once a
   `live` row holds its complete key set, that account is on mainnet at the next start. Say so
   before the human stores live keys, and point at `vike-cli secrets account deactivate --id <N>`
   if they want the keys stored but the account off.
4. **Two active tiers of one account do NOT RUN** (`TierConflict`): a `demo` row and a `live` row
   with the same label, both active, are refused rather than picked between. The human deactivates
   one. Neither the tier nor `active` has an environment variable, so do not suggest one.

## Gate 2 — the credentials, in the one store

There is one store and no precedence: the settings database `<project>/settings/db/vike.db`
(`crates/vike-secrets/src/lib.rs` is the authority). A box with no database has NO credentials and
runs no venue at all (an account with no row or no keys does NOT RUN). `vike-cli secrets init` creates the empty store on a fresh box.

**There is no file for a human to edit.** Nothing reads a `KEY=VALUE` credential file; ask the
binary which store answered rather than assuming.

`<project>` is resolved at runtime by walking
up for a project marker; `VIKE_SETTINGS_DIR` names the `settings` directory outright — the level
holding the store, not `<project>` above it (https://vike.io/docs/trader/guides/arm-a-venue).

Tell the human to ask the binary rather than guess, with these read-only commands on the box:

```sh
vike-cli secrets path       # where it resolved for THIS invocation, and its exposure
vike-cli secrets list       # key NAMES only, never a value; its `source:` line names the store
```

⚠ Those two are read-only, but `vike-cli secrets` as a whole is NOT: `secrets set --account <ID>
<FIELD>` upserts ONE credential of an EXISTING account into an EXISTING store, taking the value
from stdin or a named environment variable and never from the command line. It is the only
venue-credential writer anywhere, it creates neither the store (`secrets init` does) nor an account
(`secrets account add` does), and it cannot be aimed at another path. The whole verb, its
refusals and what you must never do with a key is the `manage-the-credential-store` skill
(https://vike.io/docs/trader/guides/arm-a-venue).

Key facts for the human (from https://vike.io/docs/trader/guides/arm-a-venue):

1. **A credential belongs to its ACCOUNT**: it is one FIELD of that account, written with
   `vike-cli secrets set --account <ID> <FIELD>` (or `--venue <VENUE> [--label <LABEL>] <FIELD>`
   for the venue's one active account of that label). The fields are the venue's own words —
   `API_KEY` / `API_SECRET` / `API_PASSPHRASE` for the HMAC venues, bespoke sets for the FX venues
   (`crates/vike-venues/src/credential_schema.rs`'s `VENUE_KEY_SCHEMAS`). No key name carries a tier
   and nothing reads one out of a name: the tier is the account row's, and a second account per
   venue is a second row with its own LABEL.
2. Required is per-venue: `API_KEY` + `API_SECRET` for binance, bybit and deribit; **OKX
   additionally requires `API_PASSPHRASE`**. An account holding two of the three resolves `None`
   and does not run, exactly like an unconfigured one — `crates/vike-secrets/src/credentials.rs`'s
   `missing_required_passphrase` names the missing field at startup.
3. Absent credentials **are** the live gate: no store means no `demo` or `live` account runs. A store that
   exists and cannot be read is a different case and errors.
4. **A `{VENUE}_MAINNET` name in the store still refuses startup — decision 0095 retired the switch,
   it did not stop refusing it.** `crates/vike-config/src/arming.rs` refuses when one of the four
   names (`BINANCE_MAINNET`/`BYBIT_MAINNET`/`OKX_MAINNET`/`HYPERLIQUID_MAINNET`) carries the exact
   value `1`, anywhere in the store — not because it would arm anything (it arms nothing now, at any
   layer), but because an operator who wrote it believes it still does something, and the refusal
   names the real lever instead of silently doing nothing. It is handed
   the parsed credential map — the database's `credential` table — so a line in any file refuses
   nothing and arms nothing.

## Gate 3 — check before starting

Have the human run `vike-cli config check` (`crates/vike-cli/src/cmd/config/check.rs`): it
resolves the same directory through the same loader as `config show` and returns an exit code. An
absent store is `ok` (that is the live gate); a `VIKE_SETTINGS_DIR` naming a non-directory is a
failure; an unreadable store is a warning that becomes a failure only on a box armed for live
(https://vike.io/docs/trader/guides/arm-a-venue).

## Verify afterwards — what `node_snapshot` can and cannot tell you

After the human has made both edits and started the tradehub, call **`node_snapshot`**. It takes no
arguments (`inputSchema.properties` is empty in `tools_spec`) and reads the running tradehub's live
state: orders, positions, per-venue equity, recent events. It needs the server started with
`--node <host:port>` plus `VIKE_TRADEHUB_OBSERVE_KEY` (a row of the hub-key store); if the key is not
in the hub-key store, the tool error names the store and points at
`vike-cli secrets path` / `vike-cli backend status` (`crates/vike-cli/src/cmd/mcp/tool_schemas.rs`'s
`tools_spec`, `Server::ensure_observe`, `Server::missing_key`).

The result is the tradehub's `WireSnapshot` (`crates/vike-tradehub-client/src/wire.rs`) serialized
as-is, plus a `pre_fold` flag. The tool waits only for the tradehub's first frame on the connection —
normally one round trip, never more than about two seconds.

**If `pre_fold` is `true`, call again before you judge anything.** The frame carries nothing built
(`seq: 0`): `venues`, `balance`, `equity_total` and `accounts_epoch` are placeholders, not
readings, and `venues: []` does NOT mean a venue is not running. `identity` says which placeholder it
is, and `pre_fold_note` says the same in words: with `identity` present it is the frame the tradehub
publishes before its first fold — the tradehub is up and has built nothing yet; with `identity: null`
it is this client's own placeholder, because no frame from the tradehub arrived within the read's wait.
A tradehub that has nothing to fold yet (an idle paper tradehub with no feed) can stay `pre_fold` until its
first order, so if a stamped one persists, report "the tradehub is up and has built nothing yet" rather
than reading the empty lists (https://vike.io/docs/ai/trader/trading, `Server::tool_node_snapshot`).

Read these fields, from a frame with `pre_fold: false`:

1. `venues[]` — one ledger block per venue the tradehub carries, each with `venue`, `balance`,
   `equity`, `trading_state` and `positions`. A venue absent from this list is not running on
   this tradehub at all, whatever the files say. This holds only for a `pre_fold: false` frame.
2. `identity.live` — `true` iff the daemon is LIVE (`flags.tradehub_live`), `false` = paper.
   This is the **process-wide** gate, not the venue's tier.
3. `fault` — set once a handler panicked; the core is halted in safe-state.
4. `trading_state` and `recent_events` — the primary engine's state and the bounded journal tail.

**What the snapshot does not carry:** a per-venue `paper` / `demo` / `live` tier. Nothing in
`WireSnapshot` says which tier a venue started at, and the one per-venue `live` flag
(`WireMountRow::live`, filled from the venue's arming record) rides on a different response, not
on the snapshot. So report exactly what you can see — "the tradehub is up, it carries these venues,
`identity.live` is X" — and send the human to the tradehub's own startup output for the tier. Do not
infer "demo" or "live" from a balance or an equity number.

## When it is still paper

If the human says "I set it and it is still paper", `crates/vike-config/src/venue_arming.rs`'s
`ArmingBlock` names one observable cause per variant (https://vike.io/docs/trader/guides/arm-a-venue):

| Variant | Meaning | What the human checks |
| --- | --- | --- |
| `NoAccountRow` | no `account` row states a tier for this account | `vike-cli secrets accounts`; `account add` creates one |
| `AccountInactive` | the rows exist and none is active | `vike-cli secrets account activate --id <N>` |
| `PaperTier` | every active row says `paper` | `vike-cli secrets account set-tier --id <N> --tier demo` |
| `TierConflict` | two active rows of one account name two tiers | deactivate one of them |
| `NoCredentials` | the account does not hold every field its venue requires | `vike-cli secrets list` — the names filed under that account — against the venue's required fields |
| `FeatureAbsent` | this binary has no arm for the venue (`ibkr`, `polymarket`, `fxcm` features) | the build |
| `LiveCredentialsAbsent` | a `live` account whose required keys are not all stored does NOT RUN — the account has one key set, and a live account never trades demo (aster and ibkr included) | the names filed under that `live` account (`vike-cli secrets list`), not just which keys the store holds |
| `LiveTierNotWired` | a `live` account holds its keys and this venue's arm starts only its demo tier (alpaca, ig, deribit, ibkr, ctrader, fxcm, oanda, dukascopy), so it does NOT RUN and the daemon logs an `error!` | that arm cannot trade a live account: if those keys ARE the venue's demo account, `vike-cli secrets account set-tier --id <N> --tier demo`; otherwise a `demo` account holding the demo keys |
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
`Server::call_tool` and `crates/vike-cli/src/cmd/mcp/node_writes.rs`'s `Server::execute`). Even then, the account's tier and the risk
ceilings still apply: a write tool cannot loosen them (https://vike.io/docs/ai/trader/tools).
