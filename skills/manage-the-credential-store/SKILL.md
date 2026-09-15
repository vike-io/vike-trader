---
name: manage-the-credential-store
description: Inspect and write the ONE credential store Vike reads, `<project>/settings/secrets.env` — or, once this box has MIGRATED, the settings database `<project>/settings/db/vike.db`, which then answers wholly and leaves that file unread. Use when a user asks where their API keys live, which venue keys are configured, why a key is not being picked up, how to create the store, or asks you to add, set, rotate or change one venue credential. No MCP tool reads or writes a credential. From the CLI `vike-cli secrets set` upserts ONE named key, value from stdin or a named env var and never from the command line; `vike-cli backend setup` / `connect` and `vike-cli datahub setup` write NODE keys, which live in `node.env` beside the store and which `secrets list` does not show. `vike-cli secrets migrate` is the one act that CREATES the database. Covers `secrets path`, `list`, `template`, `set`, `migrate`, store resolution and the exposure warning.
metadata:
  tools: ""
  source: "trader/guides/arm-a-venue ai/trader/setup"
---

# Manage the credential store

There is ONE store and no precedence chain — `<project>/settings/secrets.env`
(`crates/vike-secrets/src/lib.rs` is the authority). Every venue credential the platform will ever
use comes out of that file, and "which file are my keys actually coming from" has a one-line
answer the binary will print for you.

⚠ **…unless this box has MIGRATED, and then it is a DATABASE instead — never both.** A project whose
`<project>/settings/db/vike.db` exists
reads every credential out of that database and stops reading `secrets.env` **wholly**. The choice
is made ONCE per run, on whether **the database** exists — never per key — so a half-filled database
can never answer half from each, and there is still no precedence chain. The old file STAYS on disk,
because nothing in this workspace deletes a credential file, which is exactly why you must not
assume from its presence that it is being read.

**Never guess which one a box is on. Ask, and quote what it answers:** `vike-cli secrets list`
prints a `source:` line naming the store that actually answered, and `vike-cli secrets path` prints
both locations. An edit made to the file on a migrated box changes NOTHING and reports no error —
that is the one failure this section exists to keep you from causing.

⚠ **A NODE key is not a venue credential and does not live in that file.** Since **2026-09-08** the
four node keys — `VIKE_TRADEHUB_OBSERVE_KEY`, `VIKE_TRADEHUB_CONTROL_KEY`, and the `VIKE_DATAHUB_`
pair — live in `<project>/settings/node.env`, beside the store and never inside it. This does NOT
create a precedence chain: which file a NAME belongs to is decided statically, every key still has
exactly one home, and no name is ever looked for in both. What it means for you, concretely:

- `vike-cli secrets list` does not list a node key, on purpose. **Do not report that absence as
  "you have no node key"** — it is the wrong file. `vike-cli backend status` owns that question.
- `vike-cli secrets path` prints BOTH paths, so it is still the one command that answers "where
  are my keys".
- A box upgraded from before that date still has its pair in `secrets.env` and still works: the
  binaries fall back to it and print a migration notice naming both files. That notice is a notice
  period, not decoration — the fallback goes away.

If a user wants to move a pair, the ORDER is the whole safety of it, and you should walk it rather
than editing two files at once:

1. Copy both lines into `node.env` (`chmod 600`, owned by the account the daemon runs as — a
   root-owned `0600` file is one the daemon cannot read, and it silently falls back instead).
2. Restart, and confirm the migration notice is GONE. That absence is the proof the new file is
   being read. ⚠ It only proves anything on a binary NEW enough to print the notice at all: on an
   older one there was never a notice, so "no notice" means nothing. Check the version first.
3. Only then delete the two lines from `secrets.env`, with a dated backup beside it.

Rollback is deleting `node.env` and restarting — available up to step 3 and not after, which is
exactly why step 3 is separate. ⚠ **Move the pair or move neither.** Whichever file answers first
answers WHOLLY; a split pair is a key the node never sees, and it surfaces there as an opaque
`bad mac` rather than as anything naming a file.

## The one thing to say first

**No MCP tool on this server reads or writes a credential**, so nothing you can call from a tool
list will answer these questions or change anything. The whole surface is one CLI verb, run by the
operator (or by you, if you have a shell on the box):

| verb | what it does |
| --- | --- |
| `vike-cli backtest` | compute a strategy over history and judge what came back: `backtest run` runs one on a remote vike-datahub server (or --local), and the PROFILE says what — a [paramscan] grid searches the parameter space, a [walkforward] table walks it forward, and declaring both ships the profile whole to the walk-forward runner. Then `backtest ls\|show\|path` over the run directory, `tag` to name a run, `diff` to see what moved between two, `gate` to turn that into a CI exit code, and `params` and `strategies` for what can be tuned and what can be run |
| `vike-cli data` | the hist store: fetch real bars or seed the demo tape into it (write, local), list what it holds and report coverage (read, over --addr) |
| `vike-cli config` | this box's settings: provenance (`show`), a validating pre-flight (`check`), and a journalled one-key write (`set`) |
| `vike-cli mcp` | serve the create+backtest tools over stdio MCP (for Claude / an agent) |
| `vike-cli trade` | observe + control a running vike-tradehub node: `trade status\|halt\|resume` run one operation and exit, and a bare `trade` opens the interactive REPL (for a human) |
| `vike-cli report` | ask a vike-tradehub node for a tearsheet over its live journal (read-only; no node serves it yet — it refuses and names the command that does) |
| `vike-cli research` | investigate a signal and FIT a model — the plane BEFORE a strategy exists: `research study` asks the backend to run a compiled study over the hist store it holds (served by `vike-backend backtest --addr` when it mounts the study runner; a peer that does not refuses by name and points at `vike-backend study`) |
| `vike-cli secrets` | inspect the credential store THIS box reads — `<project>`/settings/secrets.env, or the settings database `<project>`/settings/db/vike.db once migrated, which `secrets path` reports — set ONE key in it, and perform that migration (list \| path \| template \| set \| migrate) |
| `vike-cli backend` | stand a vike-tradehub node — the running daemon — up, and attach this box to one (setup on the daemon \| connect \| status \| disconnect on the client) |
| `vike-cli datahub` | MINT the node key pair a vike-datahub server authenticates with (setup), on that server's box |
| `vike-cli init` | create `<project>`/user_data — strategies, profiles, results — with examples |
| `vike-cli indicators` | print the indicators a Rhai strategy can call, with their parameters |
| `vike-cli surface` | write this binary's own command surface as JSON, for the documentation the docs site generates rather than hand-writes. Reads nothing and dials nothing: the table is compiled in, so the answer is a property of THIS build |

`vike-cli secrets` is the row that matters here. Three of its subcommands only READ (`path`, `list`,
`template`); `set` writes one key, and `migrate` is the only one that creates anything.

## Read the store — `path`, `list`, `template`

```sh
vike-cli secrets path       # where it resolved FOR THIS INVOCATION, whether it exists, its exposure
vike-cli secrets list       # key NAMES only, never a value, plus the ACCOUNTS those names resolve to
vike-cli secrets list --json  # the same disclosure as one JSON object — still never a value
vike-cli secrets template   # an EMPTY key grid, every name this workspace can look up, to stdout
```

Rules worth carrying:

- **`list` never prints a value, in either form.** If a user asks you to read a key back, the
  answer is that the tooling will not do it and neither will you — a secret echoed into a
  transcript has been copied somewhere the operator did not choose.
- `list` distinguishes `KEY__LABEL` (a double underscore — a second ACCOUNT on that venue) from
  `KEY_LABEL`, which names nothing and is read by nothing. A key that "is not being picked up" is
  very often this.
- **`path` also reports exposure.** On Unix a store whose mode grants anything to group or other
  is a warning, not a refusal — the credentials still load. Group-WRITABLE is the sharp end of
  that: it lets somebody substitute the keys an order is signed with. Tell the user to
  `chmod 600` it.
- `--file PATH` points `path`/`list` at some other credential FILE instead of the project's store.
  It is REFUSED on `set`, on `migrate` and on `template` — and refused outright when PATH holds a
  settings DATABASE, because `--file` reads `KEY=VALUE` text and a database read that way is
  SILENT rather than loud: its pages are mostly NUL bytes, so the parse succeeds, finds nothing and
  prints `0 secret(s)` about the one artifact holding every key. **Reach a database by naming its
  PROJECT** — `VIKE_SETTINGS_DIR=<project>/settings vike-cli secrets list`.
- ⚠ **`--file` at a file the database has SHADOWED still works and now says so.** Both `path` and
  `list` check for a `db/vike.db` beside the path you named and report it: the names print, with
  the finding that the file they came from is no longer read. Do not quote such a listing as this
  box's configuration.

## Where `<project>` is, when the answer is surprising

`<project>` is resolved at RUNTIME by walking up from the working directory, so it depends on
where the command was run. `$VIKE_SETTINGS_DIR` names the `settings` directory outright and beats
the walk — note that it names the level HOLDING `secrets.env`, not `<project>` above it.

Do not reason about the walk in the abstract. `vike-cli secrets path` answers it for the exact
invocation the user is complaining about, which is the only invocation that matters.

## Create the store

There is no `--out` flag on `template`, deliberately: the redirection is the operator's, because
it TRUNCATES an existing file and no tool should be able to do that to the only copy of somebody's
live venue keys.

```sh
vike-cli secrets template > settings/secrets.env
chmod 600 settings/secrets.env
```

`template --venue okx` emits one venue's rows instead of the whole grid.

⚠ **On a MIGRATED box that redirect writes a file nothing reads, and the shell reports success.**
`template` says so on stderr when it sees a database, but a redirect swallows nothing and exit 0
still looks like it worked — so check `vike-cli secrets list`'s `source:` line before you suggest
this. On such a box there is nothing to create: the store already exists and
`vike-cli secrets set KEY` puts a key in the one that answers.

## Move the store into the database — `secrets migrate`

`vike-cli secrets migrate` is the ONE command that creates the settings database and carries the
existing keys into it. It reads `secrets.env` and `node.env`, files each name into the table its
namespace owns, and **opens neither file for writing** — nothing is deleted, moved or truncated.

```sh
vike-cli secrets migrate --dry-run    # what it WOULD do; writes nothing
vike-cli secrets migrate              # do it
```

**Always run `--dry-run` first and show the user its output.** The first successful run is the
irreversible one: from the moment the database exists, every process on that box reads it instead
of the file, and there is no un-migrate verb. A run with nothing to migrate creates NO database and
exits successfully — that is a correct outcome, not a failure to report as one.

⚠ **A run can succeed and still not carry everything.** A key whose file value disagrees with a row
already stored is refused individually and named in the report, while every other key lands. If the
output names refused keys, say so explicitly — the operator has two values for one name and only
they know which is current.

## Write ONE key — `secrets set`

This is the one writer of a VENUE credential, and it is narrow on purpose. It
**upserts** a named key — replacing that key's line if it is there, appending it if it is not — and
leaves every other line, comment, blank line and their ORDER byte-identical, landing the result
atomically.

⚠ **This page used to say "the ONE writer in the whole workspace", and that stopped being true on
2026-09-07.** `vike-cli backend setup` and `vike-cli backend connect` now write the two vike-tradehub NODE
keys through the same upsert — `setup` MINTS them and takes no value from anyone, `connect --manual`
reads the pair from stdin — so `secrets set` refuses those two names and names that command instead
of telling you to open an editor. `vike-cli datahub setup` joined them on 2026-09-08 for the
`VIKE_DATAHUB_` pair, and the refusal picks the command by SERVICE rather than naming one
unconditionally: a refusal that sends somebody to the wrong command costs more than one that names
none. ⚠ Those writers all target `node.env`, not this file — and they do so even on a box whose pair
is still in `secrets.env`, because a writer that chose the old file when it found one would mean no
box ever finished migrating. Every writer in the tree is pinned by a gate — one row per file with
the reason it may write — so the roster lives there and no count of them is written down here.

```sh
printf %s "$SECRET" | vike-cli secrets set BINANCE_LIVE_API_KEY
vike-cli secrets set BINANCE_LIVE_API_KEY --from-env BINANCE_KEY
```

What it refuses, and why each refusal is the point:

| refusal | why |
| --- | --- |
| a value on the command line | argv is visible to every process on the box and lands in shell history |
| a key name the workspace cannot read | a typo would sit in the file looking configured and arm nothing |
| ANY node key — both pairs | not even the right FILE (they live in `node.env`), and a 256-bit HMAC key is minted rather than typed. The refusal names the command that owns the pair you asked for — `vike-cli backend setup` for the tradehub, `vike-cli datahub setup` for the datahub |
| an ABSENT store | creating one is the operator's editor's job — use `template` above. ⚠ The one exception is `secrets migrate`, which creates the DATABASE: nobody makes a SQLite file in an editor, so that act had to move into the tooling |
| `--file PATH` | a writer that can be aimed anywhere is a writer that can overwrite anything |

Anything that would rewrite the file AS A WHOLE — regenerating it from a map, "resetting" it,
dropping keys it does not recognise — is the forbidden thing, whatever it is called. If a user
asks for that, offer the upsert instead.

## Do not

- Do not ask for a key in the chat, and do not accept one that is volunteered. If a user pastes a
  secret at you, say it should be rotated, and hand them the `--from-env` or stdin form above.
- Do not echo a key's VALUE back, ever, including "to confirm it was set".
- Do not write the file with a shell redirection, an editor script, or any tool other than a verb
  that UPSERTS — `vike-cli secrets set` for a venue credential, `vike-cli backend setup` /
  `vike-cli backend connect` for the tradehub pair, `vike-cli datahub setup` for the datahub pair.
  Everything else rewrites the whole file; a half-written credential store is a live account with
  the wrong keys in it.
- Do not hand-move a node key out of `secrets.env` with an editor as a side errand. If a user wants
  the migration, walk the three ordered steps above: the old lines come out only AFTER a
  restart has proven the daemon reads the new file, because that order is what keeps a rollback
  available.
- Do not put a `{VENUE}_MAINNET` line in this file expecting it to arm anything. It is refused at
  startup — refused, not ignored — so that nobody reads demo fills as real ones.

## What credentials do NOT do on their own

Absent credentials ARE the live gate — no store means every venue stays paper — but present
credentials are not the whole gate. A per-venue ceiling in `<project>/settings/policy.toml` is
consulted FIRST and can only ever refuse, so a box with no `[venues]` table mounts all paper
whatever this store holds. Arming a venue is the `arm-a-venue` skill; this one is only about the
file.

## Failure checklist

- `no store found` from `list` — the walk found no project above the working directory, or there
  is no `settings/` there. Run `vike-cli secrets path` and read what it resolved.
- A store that EXISTS and cannot be READ is an error, not "no credentials" — a permissions bug
  wearing the unconfigured answer looks exactly like a correct fresh install while every venue
  silently drops to paper.
- A venue still on paper with keys present — check the required SET for that venue (OKX needs a
  passphrase as well as key and secret; a store with two of the three resolves to nothing), then
  check the policy ceiling.
