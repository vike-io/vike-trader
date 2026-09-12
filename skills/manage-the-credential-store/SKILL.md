---
name: manage-the-credential-store
description: Inspect and write the ONE credential store Vike reads, `<project>/settings/secrets.env`, from the command line. Use when a user asks where their API keys live, which venue keys are configured, why a key is not being picked up, how to create the store, or asks you to add, set, rotate or change one venue credential. No MCP tool reads or writes a credential. From the CLI the store is written by `vike-cli secrets set`, which upserts ONE named key and takes the value from stdin or a named environment variable and never from the command line, and by `vike-cli backend setup` / `vike-cli backend connect` / `vike-cli datahub setup`, which write NODE keys — and those live in `<project>/settings/node.env`, a separate file, so `secrets list` does not list them and `backend status` is what reports them. All of them upsert in place, and none of them creates a store that does not exist. Covers `secrets path`, `secrets list`, `secrets template`, `secrets set`, the store resolution rules and the exposure warning.
metadata:
  tools: ""
  source: "trader/guides/arm-a-venue ai/trader/setup"
---

# Manage the credential store

There is ONE store and no precedence chain — `<project>/settings/secrets.env`
(`crates/vike-secrets/src/lib.rs` is the authority). Every venue credential the platform will ever
use comes out of that file, and "which file are my keys actually coming from" has a one-line
answer the binary will print for you.

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
| `vike-cli backtest` | run a backtest on a remote vike-datahub server and print the report — or, when the profile carries a [sweep] grid, a ranked parameter search |
| `vike-cli walkforward` | run an anchored walk-forward validation on a remote vike-datahub server |
| `vike-cli data` | the hist store: fetch real bars or seed the demo tape into it (write, local), list what it holds and report coverage (read, over --addr) |
| `vike-cli config` | settings provenance (`show`) and a validating pre-flight (`check`) for this box |
| `vike-cli mcp` | serve the create+backtest tools over stdio MCP (for Claude / an agent) |
| `vike-cli trade` | observe + control a running vike-tradehub node: `trade status\|halt\|resume` run one operation and exit, and a bare `trade` opens the interactive REPL (for a human) |
| `vike-cli report` | ask a vike-tradehub node for a tearsheet over its live journal (read-only; no node serves it yet — it refuses and names the command that does) |
| `vike-cli study` | ask the backend to run a compiled study over the hist store it holds (no backend serves it yet — it refuses and names the command that does) |
| `vike-cli secrets` | inspect the credential store `<project>`/settings/secrets.env, and set ONE key in it (list \| path \| template \| set) |
| `vike-cli backend` | stand a vike-tradehub node — the running daemon — up, and attach this box to one (setup on the daemon \| connect \| status \| disconnect on the client) |
| `vike-cli datahub` | MINT the node key pair a vike-datahub server authenticates with (setup), on that server's box |
| `vike-cli init` | create `<project>`/user_data — strategies, profiles, results — with examples |
| `vike-cli indicators` | print the indicators a Rhai strategy can call, with their parameters |

`vike-cli secrets` is the row that matters here. It has four subcommands, three of them read-only.

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
- `--file PATH` points `path`/`list`/`template` at some other store instead of the project's. It
  is REFUSED on `set`.

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
| an ABSENT store | creating one is the operator's editor's job — use `template` above |
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
