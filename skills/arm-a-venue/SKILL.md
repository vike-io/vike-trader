---
name: arm-a-venue
description: What to do when a user asks an agent to arm, enable, activate, go live on, switch to demo on, connect, or "turn on" a venue (binance, bybit, okx, deribit, or any other venue) for real trading through Vike. No MCP tool can arm a venue — an account trades at its own tier only while its `account` row is active, a settings-database row written with `vike-cli secrets account`, and the credential store — the settings database `settings/db/vike.db`, the only one — is written by nothing but the narrow `vike-cli secrets set` verb — so this skill tells the agent to stop trying, hand the human the two gates in the right order, and then verify what it can from node_snapshot. Also use when a user asks "why is this venue still paper", "why did my order go to the paper book", "I set live and it is still paper", or asks the agent to put API keys somewhere.
metadata:
  tools: "node_snapshot"
  source: "trader/guides/arm-a-venue ai/trader/tools ai/trader/trading"
---

# Arm a venue

## The one thing to say first

"Arming" is this page's word for turning an account on, so that it trades. You cannot do this. **No
MCP tool can arm a venue.** A write tool arms nothing: it hands a
command to a tradehub that is already running, and what that tradehub may do to a venue is decided by
the account rows, not by anything you send. An account trades at its own `tier` (`paper`, `demo`
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
running the tradehub. No restart follows: the running tradehub follows the rows by itself within about a second, starts an account
that now runs and stops one that no longer does, and touches no other (decision 0169, `crates/vike-tradehub/src/account_follow.rs`).
A restart is for what says so: an account listed NOT RUNNING with `restart required to start`, a `live` account while
`flags.tradehub_live` is off, and a key changed under an account already running.
Stopping an account cancels its resting orders and leaves its position open at the venue, so it is closed first.

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

1. The tiers are `paper < demo < live` (`crates/vike-model/src/accounts/tier.rs`'s `AccountTier`).
   `demo` is the venue's own demo/testnet/sandbox account — real wire, real rejections, no real
   funds. Recommend `demo` unless the human explicitly wants mainnet.
2. **The tier is the network, and it is honoured exactly or not at all.** For
   binance/bybit/okx/hyperliquid `live` means MAINNET directly and `demo` the demo network; there
   is no `{VENUE}_MAINNET` flag (it is DELETED, and nothing reads it). An account has ONE
   tier and ONE key set: a `live` account's keys are signed against mainnet, and a `live` account
   whose required keys are not all stored does NOT RUN (`LiveCredentialsAbsent`) — it never falls
   back to demo, aster included. To trade both networks the human keeps two accounts (two rows,
   two labels), each holding its own keys. Polymarket has no demo tier at all.
3. **Creating the account ARMS it; storing a key never creates one.** `vike-cli secrets account add`
   writes the row with its tier, born ACTIVE, and `vike-cli secrets set --account <ID> <FIELD>`
   only attaches a key to a row that already exists (an id no row carries is refused). So once a
   `live` row holds its complete key set, that account is on mainnet within about a second. Say so
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
4. **A `{VENUE}_MAINNET` name does nothing — decision 0095 retired the switch.** A row stored under
   one of the four names (`BINANCE_MAINNET`/`BYBIT_MAINNET`/`OKX_MAINNET`/`HYPERLIQUID_MAINNET`) is
   an unknown credential name, read by nothing and stopping nothing, so do not offer one as a lever:
   the lever is the account's tier. The same name in a process's ENVIRONMENT refuses that process's
   startup (`crates/vike-config/src/removed/table.rs`'s `REMOVED_ENV`).

## Gate 3 — check before starting

Have the human run `vike-cli config check` (`crates/vike-cli/src/cmd/config/check.rs`): it
resolves the same directory through the same loader as `config show` and returns an exit code. An
absent store is `ok` (that is the live gate); a `VIKE_SETTINGS_DIR` naming a non-directory is a
failure; an unreadable store is a warning that becomes a failure only on a box armed for live
(https://vike.io/docs/trader/guides/arm-a-venue).

## Verify afterwards — what `node_snapshot` can and cannot tell you

After the human has made both edits, call **`node_snapshot`**. It takes no
arguments (`inputSchema.properties` is empty in `tools_spec`) and reads the running tradehub's live
state: orders, positions, per-venue equity, recent events. It needs the server started with
`--node <host:port>` plus `VIKE_TRADEHUB_OBSERVE_KEY` (a row of the hub-key store); if the key is not
in the hub-key store, the tool error names the store and points at
`vike-cli secrets path` / `vike-cli backend status` (`crates/vike-mcp/src/tool_schemas.rs`'s
`tools_spec`, `Server::ensure_observe`, `Server::missing_key`).

The result is the tradehub's `CoreSnapshotWire` (`crates/vike-tradehub-client/src/wire.rs`) serialized
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

1. `venues[]` — one block per account the tradehub runs, each with `venue`, `account` (absent for
   the unlabelled one), `tier` (`paper`, `demo` or `live`), `balance`, `equity`, `trading_state` and
   `positions`. `tier` is what an order sent to that account goes to; absent means the node did not
   say, so treat it as `live`. Do not infer a tier from a balance or an equity number.
2. `not_running[]` — every account the tradehub knows and does NOT run, each with `venue`, `account`,
   `tier` and `reason`, the word the table below explains. An account in neither list is one this
   tradehub does not know about. Both lists hold only for a `pre_fold: false` frame.
3. `identity.live` — `true` iff `flags.tradehub_live` is on. Off, a `demo` or `live` account does not
   run (`flags.tradehub_live off` in `not_running[]`), and a `paper` account still does. It never
   turns an account into paper.
4. `fault` — set once a handler panicked; the core is halted in safe-state.
5. `trading_state` and `recent_events` — the primary engine's state and the bounded journal tail.

## When an account does not run

An account is never "still paper". It runs at its own tier or it does not run, so an account that was
set to `demo` or `live` and places no order on that network has a `reason` in `not_running[]`. If the
human says "I set it and it is still paper", read that row. Each reason is one word of
`crates/vike-model/src/accounts/arming.rs`'s `ArmingBlock`, naming one cause
(https://vike.io/docs/trader/guides/arm-a-venue):

| `reason` | Meaning | What the human checks |
| --- | --- | --- |
| `no account row` | no `account` row states a tier for this account | `vike-cli secrets accounts`; `account add` creates one |
| `account inactive` | the rows exist and none is active | `vike-cli secrets account activate --id <N>` |
| `two active tiers` | two active rows of one account name two tiers | deactivate one of them |
| `no credentials` | the account does not hold every field its venue requires | `vike-cli secrets list` — the names filed under that account — against the venue's required fields |
| `no live credentials` | a `live` account whose required keys are not all stored — the account has one key set, and a live account never trades demo | the names filed under that `live` account (`vike-cli secrets list`), not just which keys the store holds |
| `live tier not wired` | a `live` account holds its keys and this account's arm starts only its demo tier, so nothing reads them and the daemon logs an `error!` | if those keys ARE the venue's demo account, `vike-cli secrets account set-tier --id <N> --tier demo`; otherwise a `demo` account holding the demo keys |
| `demo-only arm` | this account can reach only the venue's demo endpoint | a `demo` account |
| `live-only arm` | the venue runs no testnet (Polymarket) | there is no `demo` for it |
| `flags.tradehub_live off` | the switch for every `demo` and `live` account of this tradehub is off | the flag, then a restart |
| `flags.poly_exec off` | the switch for every `demo` and `live` Polymarket account is off | the flag |
| `feature absent` | the `vike-gatewayhub` that answered was built without this venue (`ibkr`, `polymarket`, `fxcm`) | that binary |
| `SDK not loaded` | FXCM's shim did not load on this box | the box |
| `gatewayhub unavailable` | the daemon that holds every venue connection did not answer | start `vike-gatewayhub`; the tradehub retries by itself |
| `price feeds unavailable (datahub)` | the tradehub takes every price from the datahub, and that path did not answer | the datahub and `vike-gatewayhub` behind it; the tradehub retries by itself |
| `preflight failed` | the venue refused the credentials at startup, or its clock is out of band | the keys, the clock |
| `adapter failed` | the venue's start failed after the check passed | the daemon's log says which cause |
| `account not in the store` | the settings database cannot say which account this is | `vike-cli secrets accounts` |
| `another account holds the session` | only one account of this venue may hold its one session per process (dukascopy) | the other account |

Two words are not refusals. `paper tier` is an account whose active row says `paper`: it runs the
simulator, and `vike-cli secrets account set-tier --id <N> --tier demo` moves it. `orders held` runs at
its tier and refuses every order, because that order path is not proven there; no row lifts it.

## If the conversation goes on to placing an order

Once the venue is armed, order placement is a separate skill and every write tool
(submit_order, cancel_order, modify, flatten, market_exit, mass_cancel and
set_trading_state — none of which this skill calls) is a two-call gate: call once without
`confirm` and read `will_execute`
(`false`), `node_verdict` and the `preview_token`; call again with both `confirm: true` and that
exact `preview_token`. The token fires once, expires after 60 seconds and is bound to the command
it previewed; `confirm: true` on its own returns another preview, not an error and not an
execution. An `outcome: "unknown"` or a dropped-connection error means the command may have
executed — call `node_snapshot` before retrying anything (`crates/vike-mcp/src/lib.rs`'s
`Server::call_tool` and `crates/vike-mcp/src/node_writes.rs`'s `Server::execute`). Even then, the account's tier and the risk
ceilings still apply: a write tool cannot loosen them (https://vike.io/docs/ai/trader/tools).
