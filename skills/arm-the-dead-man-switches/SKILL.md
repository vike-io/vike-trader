---
name: arm-the-dead-man-switches
description: Configure what a running vike-tradehub node does when it loses sight of a venue while orders are resting — the CONNECTION dead-man (`link_deadman_grace_ms`, on by default) and the SILENCE dead-man (`deadman_timeout_ms`, off by default), both set only in `<project>/settings/policy.toml`. Use when a user asks for cancel-on-disconnect, a kill switch on a dropped feed, what happens to resting orders if the connection dies, why the daemon halted itself, why it opened halted after a weekend, how to turn a dead-man off, or what `deadman_action` does. No MCP tool can arm either one, and nothing in the environment can — that is deliberate.
metadata:
  tools: "node_snapshot"
  source: "trader/guides/stop-liquidate-restart ai/trader/tools/node_snapshot"
---

# Arm the dead-man switches

Two switches decide what a live node does when it stops being able to see a venue with orders
resting at it. They are independent — either, both or neither may be on — and they are **policy**,
which in this workspace means the file and nothing else can set them.

## The one thing to say first

**You cannot arm, disarm or change either switch from a tool call, and neither can an environment
variable.** Both live in `<project>/settings/policy.toml`, which has no environment layer at all —
an override is a compile error, not a review comment. That is the point of the class: a ceiling
somebody can raise from a shell export is not a ceiling, and a stale service-unit line that
disarmed a dead-man would leave a book resting behind a dead socket with no diff to review.

So this skill's output is a FILE EDIT for the operator plus what to check afterwards.

## The two switches, and why there are two

| key | observes | default |
| --- | --- | --- |
| `link_deadman_grace_ms` | the CONNECTION — the per-venue feed status the bridge itself discloses | **ON**, absent means armed at the default grace |
| `deadman_timeout_ms` | SILENCE — how long the core's whole ingest has been quiet | **OFF**, absent means off with one warning at mount |

They are not the same switch wearing different numbers, and the difference is the whole design.

**The silence switch cannot tell a closed market from a dead socket.** It counts ingest — venue
events, ticks, bars, quotes, trades, book updates — and deliberately nothing else, so a market
that simply closed looks exactly like an outage. It shipped armed-by-default for one morning and
that bought three halts nobody asked for, the deterministic one being the session close on every
FX and equities venue: every order resting over the close cancelled, `Halted` set, and the daemon
opening the NEXT session still halted because the trip's latch re-arms and the halt does not.

**The connection switch observes what the bridge reports about the link.** `Disconnected` on an
armed venue opens the grace window, a `Live` for the same key closes it, and a `Stale` NEVER
counts in either direction — "no fresh price exists" is exactly what a weekend looks like, and
reacting to it would be the silence switch again. That is why THIS one may have an armed default:
a per-venue table decides where it arms, and every session-bounded venue on the roster is off in
it with the reason.

## The numbers, and what they mean

| constant | value | what it bounds |
| --- | ---: | --- |
| `DEFAULT_LINK_DEADMAN_GRACE_MS` | 120000 | `link_deadman_grace_ms` when the key is ABSENT — the armed default, derived as about five ordinary reconnect cycles |
| `MIN_LINK_DEADMAN_GRACE_MS` | 30000 | the smallest ARMED grace the file accepts — below one ordinary reconnect, so a re-dial would trip it |
| `MAX_LINK_DEADMAN_GRACE_MS` | 3600000 | the largest — beyond this the switch would never fire |
| `LINK_DEADMAN_DISABLED_MS` | 0 | the ONLY spelling that turns the connection switch off |
| `RECOMMENDED_DEADMAN_TIMEOUT_MS` | 60000 | what the mount-time warning SUGGESTS for a 24/7 venue. A recommendation, never a default — nothing applies it silently |
| `MIN_DEADMAN_TIMEOUT_MS` | 1000 | the smallest ARMED silence timeout the file accepts |
| `MAX_DEADMAN_TIMEOUT_MS` | 86400000 | the largest. ⚠ An FX weekend close is longer than this, which is why a silence switch cannot be tuned to survive one |
| `DEADMAN_DISABLED_MS` | 0 | the spelling that turns the silence switch off WITHOUT the mount warning — the operator having decided |

Every one of those bounds is REJECTED BY NAME rather than clamped. A value that was silently
raised to the floor is a timeout the operator believes they set and do not have.

## What a trip DOES

Both switches share one action key, because "what does a trip do" is the same question for both
and a second key would be a second answer.

```toml
# <project>/settings/policy.toml
deadman_action = "cancel_all_and_halt"   # the default
# deadman_action = "cancel_all"
```

- `cancel_all_and_halt` — cancel the resting orders AND engage HALT: the in-process halted trading
  state on every engine, plus the cross-process HALT sentinel file, the same one a manual kill
  switch writes and every venue's submit boundary checks. **An operator un-halts.** The reasoning
  is that an outage which pulled the book is an incident, and a strategy re-quoting into a market
  it has not seen for a minute is the wrong first thing to happen after one.
- `cancel_all` — cancel and leave the trading state alone, so a strategy may re-quote once data
  resumes. The lighter action, for a maker whose whole job is to be quoting.

⚠ **The SCOPE of a connection trip is the venue whose link died, not the whole core** — a bybit
socket death leaves a binance book resting. HALT is the exception and is deliberately
process-wide, because that sentinel file is process-wide by construction and a half-halted daemon
is a state nobody asked for.

## Recommended shape

For a crypto-only, 24/7 mount that also wants a silence detector:

```toml
# <project>/settings/policy.toml
# link_deadman_grace_ms is ABSENT on purpose — absent is ON at the default grace
deadman_timeout_ms = 60000
deadman_action = "cancel_all_and_halt"
```

For a mount that includes any venue with SESSIONS (FX, equities), leave `deadman_timeout_ms` out
or write `0`. Writing `0` is how an operator says the decision was made, and it is the only
spelling that silences the mount-time warning — "off by omission" and "off on purpose" are
different states and the daemon cannot tell them apart otherwise.

To turn the connection switch off, which should be rare and deliberate:

```toml
link_deadman_grace_ms = 0
```

## After the edit — what to check

The switches are constructed at MOUNT, so a change needs a restart of the node, and neither is
built for a paper mount at all.

1. Read the node with `node_snapshot` and record `trading_state` and the resting `orders` before
   anything, so you have a before-picture.
2. Restart is the operator's action. On the way back up the daemon logs what it armed, including
   the warning when `deadman_timeout_ms` is absent — that warning is the confirmation the key was
   read, and its absence after writing `0` is also confirmation.
3. Read `node_snapshot` again and check `trading_state` is `active` rather than `halted`. A node
   that comes back HALTED is very often a previous trip's sentinel file still on disk; clearing it
   is an operator action, and setting the state back to active does NOT clear it.

## Do not

- Do not suggest an environment variable for either key. There is none, on purpose, and inventing
  one sends the operator to look for something that cannot exist.
- Do not arm the SILENCE switch on a mount that includes a session-bounded venue. Read the table
  above rather than re-deriving it from the safety argument — the argument is right and the
  mechanism is the wrong one for it.
- Do not describe a dead-man as a replacement for the manual kill switch. It is the automatic twin
  of one, and an operator still has to be able to stop trading by hand.
- Do not tell a user a trip "closed their positions". Both actions CANCEL RESTING ORDERS; neither
  flattens a position. Flattening is a deliberate write through the preview gate.
