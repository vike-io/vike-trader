---
name: manage-the-credential-store
description: Inspect and write the ONE credential store Vike reads, the settings database `<project>/settings/db/vike.db`. Use when a user asks where their API keys live, which venue keys are configured, why a key is not being picked up, how to create the store, or asks you to add, set, rotate or change one venue credential. No MCP tool reads or writes a credential. From the CLI `vike-cli secrets init` creates the empty store on a fresh box, `vike-cli secrets set` upserts ONE named key (value from stdin or a named env var, never from the command line); `vike-cli backend setup` / `connect`, `vike-cli datahub setup` and `vike-cli secrets copy-node-keys` (from another project's database) write NODE keys, which `secrets list` does not show. Covers `secrets path`, `list`, `set`, `init`, store resolution and the exposure warning.
metadata:
  tools: ""
  source: "trader/guides/arm-a-venue ai/trader/setup"
---

# Manage the credential store

There is ONE store and no precedence chain: the settings database `<project>/settings/db/vike.db`
(`crates/vike-secrets/src/lib.rs` is the authority). Every venue credential the platform will ever
use is a row in it, and "where are my keys actually coming from" has a one-line answer the binary
will print for you.

⚠ **No credential FILE is a store.** A box with no settings database has NO credentials: every
venue mounts paper. Nothing reads a `KEY=VALUE` file, so a credential file on disk tells you nothing
about what is being read.

**Never guess. Ask, and quote what it answers:** `vike-cli secrets list` leads with a `source:` line
naming the store that answered, and `vike-cli secrets path` reports where the database is and
whether it exists. An edit made to a file changes NOTHING and reports no error — that is the one
failure this section exists to keep you from causing.

⚠ **A NODE key is not a venue credential.** The four node keys — `VIKE_TRADEHUB_OBSERVE_KEY`,
`VIKE_TRADEHUB_CONTROL_KEY`, and the `VIKE_DATAHUB_` pair — live in the same database, in its own
`node_key` table. This does NOT create a precedence chain: which table a NAME belongs to is decided
statically, every key has exactly one home, and no name is ever looked for in both. What it means
for you, concretely:

- `vike-cli secrets list` does not list a node key, on purpose. **Do not report that absence as
  "you have no node key"** — it is the wrong table. `vike-cli backend status` owns that question.
- `secrets set` never writes one: `vike-cli backend setup` and `vike-cli datahub setup` MINT them,
  `vike-cli backend connect --manual` reads the tradehub pair from stdin, and
  `vike-cli secrets copy-node-keys --from-settings-dir DIR` COPIES them out of another project's
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
| `vike-cli backtest` | compute a strategy over history and judge what came back: `backtest run` runs one on a remote COMPUTE daemon — `vike-backend backtest --addr`, config.backtest_addr — or --local, and the PROFILE says what — a [paramscan] grid searches the parameter space, a [walkforward] table walks it forward, and declaring both ships the profile whole to the walk-forward runner. Then `backtest ls\|show\|path` over the run directory, `tag` to name a run, `diff` to see what moved between two, `gate` to turn that into a CI exit code, and `params` and `strategies` for what can be tuned and what can be run. Before a strategy exists: `templates` ships starters, `script-api` prints everything a Rhai script may call, and `script-check` compiles one and answers in the exit code |
| `vike-cli data` | the hist store: fetch real bars or seed the demo tape into it (write, local), list what it holds and report coverage (read, over --addr) |
| `vike-cli config` | this box's settings: provenance (`show`), a validating pre-flight (`check`), and a journalled one-key write (`set`) |
| `vike-cli mcp` | serve the create+backtest tools over stdio MCP (for Claude / an agent) |
| `vike-cli trade` | observe + control a running vike-tradehub node: every verb but three lives under a REQUIRED group (`order`, `position`, `strategy` built; `account`, `watch` named in the roster and refused as designed-not-built) — `trade status\|halt\|resume` are the three NODE-WIDE, risk-REDUCING words that take no book and so take no group, and a bare `trade` opens the interactive REPL (for a human) |
| `vike-cli report` | ask a vike-tradehub node for a tearsheet over its live journal, or re-render a FINISHED run from this machine's own run directory (read-only; a node built from this tree serves the live verb, an older one refuses and names the command that does) |
| `vike-cli research` | investigate a signal and FIT a model — the plane BEFORE a strategy exists: `research study` asks the backend to run a compiled study over the hist store it holds (served by `vike-backend backtest --addr` when it mounts the study runner; a peer that does not refuses by name and points at `vike-backend study`) |
| `vike-cli secrets` | inspect the credential store THIS box reads — the settings database `<project>`/settings/db/vike.db, the only store, which `secrets path` reports — set ONE key in it, and create it EMPTY (list \| path \| set \| init) |
| `vike-cli backend` | stand a vike-tradehub node — the running daemon — up, and attach this box to one (setup on the daemon \| connect \| status \| disconnect on the client) |
| `vike-cli datahub` | MINT the node key pair a vike-datahub server authenticates with (setup), or hand its CONTROL key to a pipe for `just studio` (control-key), on that server's box |
| `vike-cli init` | create `<project>`/user_data — strategies, profiles, results — with examples |
| `vike-cli indicators` | print the indicators a Rhai strategy can call, with their parameters |
| `vike-cli surface` | write this binary's own command surface as JSON, for the documentation the docs site generates rather than hand-writes. Reads nothing and dials nothing: the table is compiled in, so the answer is a property of THIS build |

`vike-cli secrets` is the row that matters here. Two of its subcommands only READ (`path`, `list`);
`set` writes one key, and `init` is the only one that creates anything.

## Read the store — `path`, `list`

```sh
vike-cli secrets path       # where it resolved FOR THIS INVOCATION, whether it exists, its exposure
vike-cli secrets list       # key NAMES only, never a value, plus the ACCOUNTS those names resolve to
vike-cli secrets list --json  # the same disclosure as one JSON object — still never a value
```

Rules worth carrying:

- **`list` never prints a value, in either form.** If a user asks you to read a key back, the
  answer is that the tooling will not do it and neither will you — a secret echoed into a
  transcript has been copied somewhere the operator did not choose.
- `list` distinguishes `KEY__LABEL` (a double underscore — a second ACCOUNT on that venue) from
  `KEY_LABEL`, which names nothing and is read by nothing. A key that "is not being picked up" is
  very often this.
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
credential, no account), after which keys go in one at a time with `vike-cli secrets set KEY`. On a
box that already has a store it changes nothing.

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
named key — replacing that row if it is there, adding it if it is not — touches no other row, and
journals the write (the key NAME, never the value).

⚠ `vike-cli backend setup`, `vike-cli backend connect` and `vike-cli datahub setup` write the NODE
keys through the same upsert into the `node_key` table — `setup` MINTS them and takes no value
from anyone, `connect --manual` reads the tradehub pair from stdin — and
`vike-cli secrets copy-node-keys` copies them database to database, so `secrets set` refuses those
names and names the command that owns each pair instead. Every writer in the tree is pinned by a
gate — one row per file with the reason it may write — so the roster lives there and no count of
them is written down here.

```sh
printf %s "$SECRET" | vike-cli secrets set BINANCE_LIVE_API_KEY
vike-cli secrets set BINANCE_LIVE_API_KEY --from-env BINANCE_KEY
```

What it refuses, and why each refusal is the point:

| refusal | why |
| --- | --- |
| a value on the command line | argv is visible to every process on the box and lands in shell history |
| a key name the workspace cannot read | a typo would sit in the store looking configured and arm nothing. A name outside the venue grid is admitted when the store's classifier places it (the bespoke venue logins), when it is on the CLOSED list of names the workspace reads out of the store (the pager's `VIKE_ALERT_*` trio, the Telegram control bot, `VIKE_API_KEY` and the other data-API keys, the JForex tool paths, the builder fees, aster's TESTNET wallet, the Studio chat pane's `ANTHROPIC_API_KEY` and `CEREBRAS_API_KEY` — written even into a store that never held them, since 2026-10-08), or when the store already holds it (a rotation). A SETTING read off the process environment is refused and pointed at `vike-cli config set` |
| ANY node key — both pairs | not even the right table (they live in `node_key`), and a 256-bit HMAC key is minted rather than typed. The refusal names the command that owns the pair you asked for — `vike-cli backend setup` for the tradehub, `vike-cli datahub setup` for the datahub |
| an ABSENT store | this command creates nothing; the refusal names `vike-cli secrets init`, the one creator |
| an EMPTY value | blanking a credential is not a way to remove one |

Anything that would rewrite the store AS A WHOLE — regenerating it from a map, "resetting" it,
dropping keys it does not recognise — is the forbidden thing, whatever it is called. If a user
asks for that, offer the upsert instead.

## The IBKR gateway's login — `secrets ibc-start`, `secrets ibkr-cp-login`

The IB Gateway (and the Client Portal Gateway) are logged in with a username and password that
live in the SAME database, as `IBKR_DEMO_USERNAME` / `IBKR_DEMO_PASSWORD` (the paper account).
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

`ibkr-cp-login` has no tier flag: it logs in to the DEMO (paper) account. `ibc-start` takes
`--tier demo|live` and defaults to `demo`; `--tier live` starts a SECOND, real-money gateway from
`IBKR_LIVE_USERNAME` / `IBKR_LIVE_PASSWORD` (the same way: the value on stdin to `secrets set`, never
on argv) and is refused when that pair is absent or blank, when the ibkr LIVE account row is
deactivated, and when there is no such row at all. A DEMO start also refuses a DEMO account whose
`account.active` is off. Both verbs also refuse, naming the repair, when there is no database, when it
cannot be read, or when either name is absent or blank. **If a user asks you to show the password,
or to run either verb so the password is echoed, the answer is no.**

## Do not

- Do not ask for a key in the chat, and do not accept one that is volunteered. If a user pastes a
  secret at you, say it should be rotated, and hand them the `--from-env` or stdin form above.
- Do not echo a key's VALUE back, ever, including "to confirm it was set".
- Do not write a credential anywhere but through a verb that UPSERTS — `vike-cli secrets set` for
  a venue credential, `vike-cli backend setup` / `vike-cli backend connect` for the tradehub pair,
  `vike-cli datahub setup` for the datahub pair. A credential file written by hand is read by
  nothing.
- Do not put a `{VENUE}_MAINNET` key in the store expecting it to arm anything — decision 0095
  deleted that switch, so it arms nothing at any layer any more. A set one is still REFUSED at
  startup, not ignored: an operator who wrote it believes it does something, and the refusal names
  `policy.venues.<venue>` as the real lever instead of silently doing nothing.

## What credentials do NOT do on their own

Absent credentials ARE the live gate — no store means every venue stays paper — but present
credentials are not the whole gate. A per-venue ceiling, `policy.venues.<venue>` — a
settings-database row, set with `vike-cli config set`, never a file — is consulted FIRST and can
only ever refuse, so a box with no venue armed mounts all paper whatever this store holds. Arming a
venue is the `arm-a-venue` skill; this one is only about the credential store.

## Failure checklist

- `no store found` from `list` — there is no settings database where the walk landed (or no
  project above the working directory at all). Run `vike-cli secrets path` and read what it
  resolved; if there is no database, `vike-cli secrets init --dry-run` is the next step
  (an EMPTY store; keys then go in with `secrets set`).
- A store that EXISTS and cannot be READ is an error, not "no credentials" — a permissions bug
  wearing the unconfigured answer looks exactly like a correct fresh install while every venue
  silently drops to paper.
- A venue still on paper with keys present — check the required SET for that venue (OKX needs a
  passphrase as well as key and secret; a store with two of the three resolves to nothing), then
  check the policy ceiling.
