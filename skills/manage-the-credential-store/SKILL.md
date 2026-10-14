---
name: manage-the-credential-store
description: Inspect and write the ONE credential store Vike reads, the settings database `<project>/settings/db/vike.db`. Use when a user asks where their API keys live, which venue keys are configured, why a key is not being picked up, how to create the store, or asks you to add, set, rotate, change or delete one venue credential. No MCP tool reads or writes a credential. From the CLI `vike-cli secrets init` creates the empty store on a fresh box, `vike-cli secrets account add` creates an account with its tier, `vike-cli secrets set` upserts ONE credential — a field of an existing account (`--account ID FIELD`) or a deployment name — (value from stdin or a named env var, never from the command line), `vike-cli secrets delete` takes ONE key away; `vike-cli backend setup` / `connect`, `vike-cli datahub setup`, `vike-cli gatewayhub setup` and `vike-cli secrets copy-hub-keys` (from another project's database) write HUB keys, which `secrets list` does not show.
metadata:
  tools: ""
  source: "trader/guides/arm-a-venue ai/trader/setup"
---

# Manage the credential store

There is ONE store and no precedence chain: the settings database `<project>/settings/db/vike.db`
(`crates/vike-secrets/src/lib.rs` is the authority). Every venue credential the platform will ever
use is a row in it, and "where are my keys actually coming from" has a one-line answer the binary
will print for you.

⚠ **No credential FILE is a store.** A box with no settings database has NO credentials: no `demo`
or `live` account runs. Nothing reads a `KEY=VALUE` file, so a credential file on disk tells you nothing
about what is being read.

**Never guess. Ask, and quote what it answers:** `vike-cli secrets list` leads with a `source:` line
naming the store that answered, and `vike-cli secrets path` reports where the database is and
whether it exists. An edit made to a file changes NOTHING and reports no error — that is the one
failure this section exists to keep you from causing.

⚠ **A HUB key is not a venue credential.** The four hub keys — `VIKE_TRADEHUB_OBSERVE_KEY`,
`VIKE_TRADEHUB_CONTROL_KEY`, and the `VIKE_DATAHUB_` pair — live in the same database, in its own
`hub_key` table. This does NOT create a precedence chain: which table a NAME belongs to is decided
statically, every key has exactly one home, and no name is ever looked for in both. What it means
for you, concretely:

- `vike-cli secrets list` does not list a hub key, on purpose. **Do not report that absence as
  "you have no hub key"** — it is the wrong table. `vike-cli backend status` owns that question.
- `secrets set` never writes one: `vike-cli backend setup`, `vike-cli datahub setup` and `vike-cli gatewayhub setup` MINT them,
  `vike-cli backend connect --manual` reads the tradehub pair from stdin, and
  `vike-cli secrets copy-hub-keys --from-settings-dir DIR` COPIES them out of another project's
  settings database (read-only there) so a re-homed service keeps the keys its clients hold —
  minting would lock every client out. It refuses a key this store holds with a DIFFERENT value
  unless the operator says `--replace` (a rotation for every client); `--dry-run` writes nothing.
  It prints key names and counts, never a value. (Until 2026-10-08 there was no copy verb.)

## The one thing to say first

**No MCP tool on this server reads or writes a credential**, so nothing you can call from a tool
list will answer these questions or change anything. The whole surface is one CLI verb, run by the
operator (or by you, if you have a shell on the box):

| verb | what it does |
| --- | --- |
| `vike-cli backtest` | compute a strategy over history and judge what came back: `backtest run` runs one on a remote backtesthub — `vike-backend backtesthub --addr`, config.backtest_addr — or --local, and the PROFILE says what — a [paramscan] grid searches the parameter space, a [walkforward] table walks it forward, and declaring both ships the profile whole to the walk-forward runner. Then `backtest ls\|show\|path` over the run directory, `tag` to name a run, `diff` to see what moved between two, `gate` to turn that into a CI exit code, and `params` and `strategies` for what can be tuned and what can be run. Before a strategy exists: `templates` ships starters, `script-api` prints everything a Rhai script may call, and `script-check` compiles one and answers in the exit code |
| `vike-cli data` | the hist store: fetch real bars or seed the demo tape into it (write, local), list what it holds and report coverage (read, over --addr) |
| `vike-cli config` | this box's settings: provenance (`show`), a validating pre-flight (`check`), and a journalled one-key write (`set`) |
| `vike-cli mcp` | serve the create+backtest tools over stdio MCP (for Claude / an agent) |
| `vike-cli trade` | observe + control a running vike-tradehub: every verb but three lives under a REQUIRED group (`order`, `position`, `strategy` built; `account`, `watch` named in the roster and refused as designed-not-built) — `trade status\|halt\|resume` are the three NODE-WIDE, risk-REDUCING words that take no book and so take no group, and a bare `trade` opens the interactive REPL (for a human) |
| `vike-cli report` | ask a vike-tradehub for a tearsheet over its live journal, or re-render a FINISHED run from this machine's own run directory (read-only; a node built from this tree serves the live verb, an older one refuses and names the command that does) |
| `vike-cli research` | investigate a signal and FIT a model — the plane BEFORE a strategy exists: `research study` asks the backend to run a compiled study over the hist store it holds (served by `vike-backend backtesthub --addr` when it mounts the study runner; a peer that does not refuses by name and points at `vike-backend study`) |
| `vike-cli secrets` | inspect the credential store THIS box reads — the settings database `<project>`/settings/db/vike.db, the only store, which `secrets path` reports — set ONE key in it, and create it EMPTY (list \| path \| set \| init) |
| `vike-cli backend` | stand a vike-tradehub — the running daemon — up, and attach this box to one (setup on the daemon \| connect \| status \| disconnect on the client) |
| `vike-cli datahub` | MINT the hub key pair a vike-datahub server authenticates with (setup), or hand its CONTROL key to a pipe for `just studio` (control-key), on that server's box |
| `vike-cli gatewayhub` | MINT the hub key pair a vike-gatewayhub server authenticates with (setup), on that server's box |
| `vike-cli init` | create `<project>`/user_data — strategies, profiles, results — with examples |
| `vike-cli indicators` | print the indicators a Rhai strategy can call, with their parameters |
| `vike-cli surface` | write this binary's own command surface as JSON, for the documentation the docs site generates rather than hand-writes. Reads nothing and dials nothing: the table is compiled in, so the answer is a property of THIS build |

`vike-cli secrets` is the row that matters here. Two of its subcommands only READ (`path`, `list`);
`set` writes one key, `delete` takes one away, `init` is the only one that creates the store, and
`account add` the only one that creates an account (with its tier).

## Read the store — `path`, `list`

```sh
vike-cli secrets path       # where it resolved FOR THIS INVOCATION, whether it exists, its exposure
vike-cli secrets list       # key NAMES only, never a value, filed under the ACCOUNT row owning each
vike-cli secrets list --json  # the same disclosure as one JSON object — still never a value
```

Rules worth carrying:

- **`list` never prints a value, in either form.** If a user asks you to read a key back, the
  answer is that the tooling will not do it and neither will you — a secret echoed into a
  transcript has been copied somewhere the operator did not choose.
- `list` prints every ACCOUNT row (id, venue, tier, label, active) with the names filed under it,
  then the names no account owns (the deployment's own, and a venue-wide field such as cTrader's
  `CLIENT_ID`). A credential belongs to its account by the row it is filed against, never by its
  NAME: no name carries a tier and nothing parses one out of a name. A key that "is not being
  picked up" is very often filed under a different account than the one that runs, or under no
  account at all.
- **`path` also reports exposure.** On Unix a database whose mode grants anything to group or other
  is a warning, not a refusal — the credentials still load. Group-WRITABLE is the sharp end of
  that: it lets somebody substitute the keys an order is signed with. Tell the user to
  `chmod 600` it.
- **To look at a DIFFERENT project's store, name its settings directory**:
  `VIKE_SETTINGS_DIR=<project>/settings vike-cli secrets list`. There is no flag that aims a
  `secrets` verb at a file.

## Where `<project>` is, when the answer is surprising

`<project>` is resolved at RUNTIME by walking up from the working directory, so it depends on
where the command was run. `$VIKE_SETTINGS_DIR` names the `settings` directory outright and beats
the walk — note that it names the level HOLDING `db/`, not `<project>` above it.

Do not reason about the walk in the abstract. `vike-cli secrets path` answers it for the exact
invocation the user is complaining about, which is the only invocation that matters.

## Create the store — `secrets init`

`vike-cli secrets init` is the operator saying this box starts fresh: it creates the EMPTY store (no
credential, no account), after which the operator creates each account with its tier
(`vike-cli secrets account add`) and puts its keys in one at a time with
`vike-cli secrets set --account <ID> <FIELD>`. On a box that already has a store it changes nothing.

```sh
vike-cli secrets init --dry-run    # what it WOULD do; writes nothing
vike-cli secrets init              # create the empty store
```

**Always run `--dry-run` first and show the user its output.** Creating the database is
irreversible in practice: from that moment every process on the box answers from it, and there is
no verb that undoes it. On a box that already has a store, `init` changes nothing.

`secrets template` is GONE. Do not suggest it, and do not hand-write a credential file — nothing
reads one.

## Write ONE key — `secrets set`

This is the one writer of a VENUE credential, and it is narrow on purpose. It **upserts** one
credential — replacing that row if it is there, adding it if it is not — touches no other row, and
journals the write (the NAME, never the value).

**A credential belongs to its ACCOUNT, and the account comes first.** The tier is written in ONE
place, the account row, by `vike-cli secrets account add --venue <VENUE> --tier <paper|demo|live>
--no-label` (or `--label <LABEL>` for a second account of the venue); `secrets set` attaches a key
to a row that already exists and NEVER creates an account or chooses its tier. It takes one of
three addresses:

| address | writes |
| --- | --- |
| `--account <ID> <FIELD>` | FIELD of account row ID (`vike-cli secrets accounts` lists the ids) |
| `--venue <VENUE> [--label <LABEL>] <FIELD>` | FIELD of that venue's ONE active account of that label (none, or more than one, is refused naming the ids); a VENUE-wide field such as cTrader's `CLIENT_ID` / `CLIENT_SECRET` takes `--venue <VENUE>` alone |
| `<NAME>` | a DEPLOYMENT name only — never an account's credential (the table below) |

FIELD is a word of the venue's own credential set — `API_KEY`, `API_SECRET`, `API_PASSPHRASE` for
the HMAC venues; `LOGIN` / `PASSWORD`, `ACCOUNT_ID`, … for the others
(`crates/vike-venues/src/credential_schema.rs`'s `VENUE_KEY_SCHEMAS`). The row it lands in is named
`{venue}#{account id}.{FIELD}` (a venue-wide one `{venue}.{FIELD}`) — a LABEL for `list` to print,
which no reader parses.

⚠ `vike-cli backend setup`, `vike-cli backend connect`, `vike-cli datahub setup` and `vike-cli gatewayhub setup` write the HUB
keys through the same upsert into the `hub_key` table — `setup` MINTS them and takes no value
from anyone, `connect --manual` reads the tradehub pair from stdin — and
`vike-cli secrets copy-hub-keys` copies them database to database, so `secrets set` refuses those
names and names the command that owns each pair instead. Every writer in the tree is pinned by a
gate — one row per file with the reason it may write — so the roster lives there and no count of
them is written down here.

```sh
vike-cli secrets account add --venue binance --tier demo --no-label    # prints `added account 7 in …`
printf %s "$SECRET" | vike-cli secrets set --account 7 API_KEY
vike-cli secrets set --account 7 API_SECRET --from-env BINANCE_SECRET
```

What it refuses, and why each refusal is the point:

| refusal | why |
| --- | --- |
| a value on the command line | argv is visible to every process on the box and lands in shell history |
| an account id no row carries | a credential write never creates an account or chooses its tier; the refusal names `vike-cli secrets account add`, which does |
| a FIELD the venue does not carry | a typo would sit in the store looking configured and arm nothing; the refusal lists the venue's own fields |
| a NAME plain `set` does not write | plain `set NAME` is for the DEPLOYMENT's own names: the CLOSED list of names the workspace reads out of the store by name (the pager's `VIKE_ALERT_*` trio, the Telegram control bot, `VIKE_API_KEY` and the other data-API keys, the JForex tool paths, the builder fees, the Studio chat pane's `ANTHROPIC_API_KEY` and `CEREBRAS_API_KEY`), an attribution code (`{VENUE}_BROKER_CODE` / `{VENUE}_BUILDER_CODE`), the IBKR gateway logins, or a deployment name the store already holds (a rotation). An account's key under a name — the retired `{VENUE}_{TIER}_{FIELD}` spelling included — is refused and pointed at `--account`; a SETTING read off the process environment is refused and pointed at `vike-cli config set` |
| ANY hub key — every service's pair | not even the right table (they live in `hub_key`), and a 256-bit HMAC key is minted rather than typed. The refusal names the command that owns the pair you asked for — `vike-cli backend setup` for the tradehub, `vike-cli datahub setup` for the datahub, `vike-cli gatewayhub setup` for the gatewayhub |
| an ABSENT store | this command creates nothing; the refusal names `vike-cli secrets init`, the one creator |
| an EMPTY value | blanking a credential is not a way to remove one |

Anything that would rewrite the store AS A WHOLE — regenerating it from a map, "resetting" it,
dropping keys it does not recognise — is the forbidden thing, whatever it is called. If a user
asks for that, offer the upsert instead.

## Take ONE key away — `secrets delete`

`vike-cli secrets delete KEY --confirm KEY` removes ONE venue credential from what the platform
reads: from the next restart the key reads as ABSENT, so whatever it configured stops being
configured (absent credentials are paper). It takes **no value in any form**, and the typed
`--confirm` must equal the key exactly. Nothing pre-fills it, and you must not either: the operator
types it.

KEY is the NAME `vike-cli secrets list` prints for the row — for an account's credential that is
`{venue}#{account id}.{FIELD}`:

```sh
vike-cli secrets delete 'binance#7.API_KEY' --dry-run --confirm 'binance#7.API_KEY'   # shows store, key, account
vike-cli secrets delete 'binance#7.API_KEY' --confirm 'binance#7.API_KEY'
```

- **Always run `--dry-run` first and show the user its output.** It names the store, the key and
  the account the key belongs to, and changes nothing.
- The row is **RETIRED, not erased**. It stays in the database as the evidence of the delete,
  journalled as `credential_retire` (the NAME, never the value), and `vike-cli secrets set` adds
  the key again (`--account <ID> <FIELD>` for an account's). Say so if a user wants a leaked key GONE: the value still sits in the 0600
  database, and what makes a leaked key harmless is revoking it at the venue.
- It refuses, naming the key: a name the store does not hold, a HUB key (minted, never deleted by
  name; `backend setup` / `datahub setup` / `gatewayhub setup` replaces a pair), and the live spelling of a credential a
  second spelling still answers for (delete the other spelling first).
- To keep keys but stop an account trading, DEACTIVATE the account row instead (below); that is
  the reversible act.

## The IBKR gateway's login — `secrets ibc-start`, `secrets ibkr-cp-login`

The IB Gateway (and the Client Portal Gateway) are logged in with a username and password that
live in the SAME database, as the DEPLOYMENT names `IBKR_DEMO_USERNAME` / `IBKR_DEMO_PASSWORD` (the
paper account) — written by name with plain `secrets set NAME`, not an account's fields.
**There is no other copy**: no credential file, no password in `ibc/config.ini`, no `.cpcreds`. A
rotation is `vike-cli secrets set IBKR_DEMO_PASSWORD` (value on stdin) and the next launch uses it —
a stale copy is how IBKR locks an account.

Two verbs LAUNCH the login process with that pair, and they are the only verbs that move a
credential VALUE anywhere (they never print one). The operator's gateway scripts call them; you
normally do not:

- `vike-cli secrets ibc-start --root <install> --gateway-version V --java-path <jre>/bin` runs IBC's
  launcher with the pair as its `--user=` / `--pw=` words. **Say this plainly if asked: IBC cannot
  take a login privately except through a file, the owner chose NO file, so the pair is visible in
  `ps` to every user on that box for as long as the gateway runs.** The verb itself prints, logs and
  journals nothing, and no script holds the value.
- `vike-cli secrets ibkr-cp-login` runs the Client Portal login driver, and only that script, with
  the pair in the driver's ENVIRONMENT (private to that process).

A box rides ONE IBKR session — one product, one tier: the tier of its ACTIVE ibkr `account` rows
(every label) and that tier's `venue.ibkr.<tier>.backend` row (`socket`, the default, or `cpapi`).
`ibc-start` logs in only when that session is `(socket, <its tier>)`, `ibkr-cp-login` only when it
is `(cpapi, <its tier>)`; anything else — no active row, active rows at demo AND live, the other
product — is refused before any value is read, naming the rows. `--check` on either verb runs only
that check: it reads no secret, starts nothing, and exits 0 when the session is the verb's — the safe
way to answer "which IBKR gateway does this box run?". Both verbs take `--tier demo|live` and default
to `demo` (the paper account); `--tier live` reads `IBKR_LIVE_USERNAME` / `IBKR_LIVE_PASSWORD` (the
same way: the value on stdin to `secrets set`, never on argv) and its login is approved on the
owner's phone, attempted once — a person's act, never yours to start. Both verbs also refuse, naming
the repair, when there is no database, when it cannot be read, or when either name is absent or
blank. **If a user asks you to show the password, or to run either verb so the password is echoed,
the answer is no.**

## Do not

- Do not ask for a key in the chat, and do not accept one that is volunteered. If a user pastes a
  secret at you, say it should be rotated, and hand them the `--from-env` or stdin form above.
- Do not echo a key's VALUE back, ever, including "to confirm it was set".
- Do not write a credential anywhere but through a verb that UPSERTS — `vike-cli secrets set
  --account <ID> <FIELD>` for a venue credential, `vike-cli backend setup` / `vike-cli backend
  connect` for the tradehub pair,
  `vike-cli datahub setup` for the datahub pair, `vike-cli gatewayhub setup` for the gatewayhub pair. A credential file written by hand is read by
  nothing.
- Do not put a `{VENUE}_MAINNET` key in the store expecting it to arm anything — decision 0095
  deleted that switch, so it arms nothing at any layer any more. A set one is still REFUSED at
  startup, not ignored: an operator who wrote it believes it does something, and the refusal names
  the real lever instead of silently doing nothing.

## What storing a credential ARMS

Absent credentials ARE the live gate — no store means no `demo` or `live` account runs — and every
account credential belongs to an account row. An account trades at its own `tier` only while its
`account` row is ACTIVE; `secrets account add` creates the row born active at the tier the operator
wrote, and `secrets set` never creates one or chooses its tier: storing the complete key set of a
`live` account puts that account on its live network at the next start of a live daemon. Say so
before the human stores live keys. To keep keys stored but the account off, the row is switched
off, never the key deleted:
`vike-cli secrets accounts` names the id, `vike-cli secrets account deactivate --id <N>` switches
it off. Arming a venue is the `arm-a-venue` skill; this one is only about the credential store.

## Failure checklist

- `no store found` from `list` — there is no settings database where the walk landed (or no
  project above the working directory at all). Run `vike-cli secrets path` and read what it
  resolved; if there is no database, `vike-cli secrets init --dry-run` is the next step
  (an EMPTY store; accounts then go in with `secrets account add`, their keys with
  `secrets set --account`).
- A store that EXISTS and cannot be READ is an error, not "no credentials" — a permissions bug
  wearing the unconfigured answer looks exactly like a correct fresh install while every venue
  silently drops to paper.
- A venue still on paper with keys present — check that the keys are filed under the account that
  runs (`secrets list` groups them by account), then the required SET for that venue (OKX needs
  `API_PASSPHRASE` as well as `API_KEY` and `API_SECRET`; an account holding two of the three
  resolves to nothing), then the policy ceiling.
