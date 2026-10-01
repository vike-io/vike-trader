---
name: triage-a-stuck-order
description: Diagnose an order on a running vike-tradehub node that is neither filled nor gone — stuck, hanging, pending, un-acked, still Submitted, never accepted, not moving, missing from the venue, or an OrphanLocalOrder / MissingTerminal / UnknownOrder reconciliation alert. Walks the core's confirm-grace ladder (submit_ack_timeout, confirm grace), what a venue confirm can return, whether the order is pre-ack or a reconciliation divergence, and why resubmitting a replacement doubles the position. Reads node_snapshot, cancels with the two-call cancel_order gate. Use when an agent or operator asks "why is my order stuck", "order never filled", "cancel and resubmit", "should I place it again", or "the venue does not show my order".
metadata:
  tools: "node_snapshot cancel_order"
  source: "trader/guides/triage-a-stuck-order ai/trader/tools/node_snapshot ai/trader/tools/cancel_order"
---

# Triage a stuck order

"Stuck" is two different failures wearing one word, and they are triaged in opposite directions:

- **Pre-ack** — the order is `Initialized` or `Submitted` with no terminal from the venue. It may
  or may not exist at the venue at all. The core's own watchdog ladder owns this case.
- **Venue-silent** — your book says the order is live and the venue's reconcile report never
  mentions it (or the venue says it is terminal and your book does not). Reconciliation owns this
  case, and it will *tell* you but not *clear* it.

The one rule that spans both: **never place a replacement order until you know the first one is
gone.** A confirm that comes back inconclusive is mapped to an *optimistic* `OrderAccepted`
precisely because the venue may have opened the position; a second order then rests beside it, and
one intent becomes two orders (`https://vike.io/docs/trader/guides/triage-a-stuck-order`,
`https://vike.io/docs/ai/trader/tools/node_snapshot`).

Prerequisites: `vike-cli mcp --node <host:port>`; `node_snapshot` needs
`VIKE_TRADEHUB_OBSERVE_KEY`, `cancel_order` needs `VIKE_TRADEHUB_CONTROL_KEY`. Either key is read
from the process environment, else from the credential store the daemon itself reads.

## Procedure

### 1. Read the book — `node_snapshot`

Call `node_snapshot` with no arguments (`"arguments": {}`; the tool's `inputSchema` has no
properties and no `reason`). It returns the node's `WireSnapshot`
(`crates/vike-tradehub-client/src/wire.rs`'s `WireSnapshot`). Read, in this order:

- `fault` — if non-null, stop triaging orders: the core entered safe state, which halts every
  engine and sweeps its working orders. That is the most important field in the payload.
- `seq` — `0` means no real frame has arrived yet; the tool polls for about two seconds for a real
  one, so a `0` after that means the observe connection has nothing yet. Call again.
- `orders` — the order registry in insertion order, spanning **every** engine; `status` is the
  rendered string. Find your `client_order_id` and note its status.
- `recent_events` — the bounded delivered-event tail, already rendered. Look for a terminal for
  that id. It is a **tail, not a ledger**: the observer is lossy by contract (drop-oldest mailbox),
  so an absent event is not evidence that it never happened.
- `positions` / `venues[*].positions` — whether a fill has already changed exposure.

### 2. Classify by status

| What the snapshot shows | Case | Go to |
| --- | --- | --- |
| `Initialized` or `Submitted`, no terminal | pre-ack | step 3 |
| past `Submitted` (accepted, partially filled) and not moving | reconciliation | step 4 |
| the id is not in `orders` at all | it is gone or was never taken by the node | step 6 |

### 3. Pre-ack: let the confirm-grace ladder run

`crates/vike-core/src/runtime/watchdog.rs`'s `sweep_stuck_orders` is the backstop for an adapter
that accepted `submit` and then emitted no terminal. It is a **ladder, not a timeout**, designed so
it never races a slow-but-real venue ack into a phantom reject (a synthesized reject would strand a
real position, and the later genuine `OrderAccepted`/`OrderFilled` is then an illegal transition out
of `Rejected` and is dropped):

- **Stage 1**, at `created + submit_ack_timeout`: the order is flagged un-acked and the core issues
  one `confirm`, recording when. Nothing is terminalized.
- **Stage 2**, at `created + submit_ack_timeout + grace`: the hard reject — unless the in-flight
  confirm guard defers it. An order that had a stage-1 confirm and is still pre-ack has its
  deadline extended to `created + submit_ack_timeout + 2·grace`. Only past that, still pre-ack, is
  the backstop `OrderRejected` synthesized.
- An order reaching the reject deadline with no prior confirm is confirmed now and deferred; there
  is never a backstop reject without at least one active confirm and its grace window.

Two things to check before waiting on it:

- **The ladder is off unless armed.** `CoreConfig::submit_ack_timeout` is `None` by default. A run
  profile's `[guards]` table arms it: `submit_ack_timeout_ms` is stage 1,
  `submit_ack_confirm_grace_ms` is stage 2's window. With it unarmed, a pre-ack order waits
  forever and step 5 is your only exit.
- **Reconcile-seeded orders carry no creation timestamp and are never swept.**

**What the confirm can return.** `vike_exec::ExecutionClient`'s `confirm` re-queries one order's
status by client-order-id, fire-and-forget; its default is a no-op, so what a confirm buys depends
on the venue adapter overriding it. Where implemented, the mapping is
`crates/vike-bridge-core/src/rest.rs`'s `resolve_ambiguous_submit`, with three outcomes:

1. the venue **has** the order (live or filled) → a managed `OrderAccepted` carrying the venue order
   id, the fill following on the user-data lane;
2. the venue confirms it **never landed** → the true terminal `OrderRejected`;
3. the re-query was **inconclusive** (transport error, double timeout) → an *optimistic*
   `OrderAccepted` with no venue id — never a false terminal.

Outcome 3 is why you do not resubmit: the venue may hold the order. Re-run `node_snapshot` after
`submit_ack_timeout + 2·grace` has elapsed since the order's creation; by then it is accepted,
filled, or backstop-rejected. If it is still pre-ack after that, go to step 5.

### 4. Past ack: read it as a reconciliation divergence

The watchdog only sweeps `Initialized` and `Submitted`; anything past that is reconciliation's job,
and the divergence kind names your situation (`crates/vike-exec/src/recon/types.rs`'s
`DivergenceKind`):

- **`MissingTerminal`** — the venue's order is terminal, yours is not. Auto-applies under `hybrid`
  but **resolves to no events** — it neither folds nor alerts.
- **`OrphanLocalOrder`** — a live local order the venue's report never mentions. **Folds nothing
  under any policy**, deliberately: a synthesized cancel would terminalize an order that may still
  be resting at the venue.
- **`UnknownOrder`** — the venue reports an order with no local match. Quarantines with nothing to
  adopt unless `generate_missing_orders` is on, and even then only a *terminal* unknown order whose
  fills fell outside the lookback is adopted.

So under a policy that HOLDS the kind (`quarantine` for all three, `hybrid` for `OrphanLocalOrder`
and `UnknownOrder`) reconciliation surfaces the stuck order and will not clear it; `synthesize`
surfaces none of the three. A held divergence is NOT visible through `node_snapshot`: the wire
snapshot it returns (`crates/vike-tradehub-client/src/wire.rs`'s `WireSnapshot`) carries orders,
positions, held exits, recent events and a fault string, and no reconciliation block at all — the
held set lives on the daemon's in-process `CoreSnapshot` and surfaces only in the daemon's own log
and alert output, which the operator reads on the node. Confirming a held divergence is not one of
this skill's tools; cancelling the order is, and if the venue has genuinely lost it the cancel is
what proves so.

### 5. Cancel it — `cancel_order`, two calls

`cancel_order` withdraws exactly one resting order named by `client_order_id` (required). Optional:
`reason` (recorded in the node's audit trail, never reaches the order), `confirm`, `preview_token`.
The gate is `crates/vike-cli/src/cmd/mcp.rs`'s `Server::call_tool`:

1. **Preview.** Call with `client_order_id` (and a `reason`) and **no** `confirm`. Nothing is sent.
   Read the response: `will_execute: false`, the resolved `wire_command`, `guardrail` (always
   `within_limits: true` with a note that a cancel has no size to check — not a pass, just honest),
   `node_verdict` with `verified_by_node`, the echoed `reason`, and the **`preview_token`**. Read
   `node_verdict.checked_by`: `"node"` means the node ran its own dry-run and `accepted` is its
   verdict; `"none"` means it could not be asked (`reason` names the fault) even though the verdict
   object is present; `null` means no `--node` or no control key. `verified_by_node` is `true` only
   for `"node"`. On any other path the node was not asked: do not read the guardrail as approval.
2. **Execute.** Call again with the same `client_order_id`, **both** `confirm: true` **and** that
   exact `preview_token`. The token fires once, expires after 60 s, and is bound to the command it
   previewed. `confirm: true` on its own returns **another preview** — not an error, not an
   execution — so read `will_execute` rather than assuming the second call landed. An unknown or
   already-used token, an expired token, or a token issued for a different command is refused with
   an error and sends nothing: take a fresh preview.

Outcomes, from `Server::execute` (it awaits this command's own ticket for 2 s):

- `outcome: "accepted"`, `sent: true`, `client_order_id` echoed — the node **took the command**. It
  does not mean the order left the book: a cancel is fire-and-forget and an id that never existed is
  acknowledged exactly like one that did.
- an error result carrying the node's reason — refused.
- `outcome: "unknown"` (no answer within 2 s) or an error saying the control connection dropped —
  the outcome is **UNKNOWN and the command may have executed**. Call `node_snapshot` **before**
  retrying anything.

A cancel is never refused for being too large — the node's notional cap applies only to
order-increasing verbs. An id outside `^[A-Za-z0-9]{1,32}$` is sent anyway, deliberately, so an
order minted elsewhere with an odd coid stays cancellable.

### 6. Verify with `node_snapshot`, then decide about a replacement

Call `node_snapshot` again. The order has left the book only when its `status` in `orders` is
terminal (or a terminal for it appears in `recent_events`), and `positions` tells you whether it
filled on the way out. Only once it is terminal — and you have re-read `positions` — is a
replacement order safe; before that, a replacement rests beside an order the venue may still hold
and doubles the exposure. If the cancel was accepted and the order still shows live on the next
snapshot, the venue has not acknowledged the cancel yet: wait and re-read, do not resubmit.

## Do not help by hand

Cancelling the order in the venue's own web UI while the mount runs does not settle it quietly; it
moves the disagreement to the next reconcile pass — `MissingTerminal` while the venue report still
carries the now-terminal order, `OrphanLocalOrder` once it has aged out. Neither is classified
`External`, so `external-quarantine` treats both exactly as `hybrid` does, and neither folds an
event under any policy. If you must intervene at the venue, halt first:
`https://vike.io/docs/trader/guides/stop-liquidate-restart`.

## Errors you will meet

- `no vike-tradehub node configured — pass --node` — no network work happened; start the server
  with `--node <host:port>`.
- a missing observe/control key — the error names both places the key is looked for (process
  environment and credential store) and points at `vike-cli secrets path` / `vike-cli secrets list`.
- `client_order_id` omitted on `cancel_order` — a tool error before any command is built.

Concepts: `https://vike.io/docs/trader/concepts/order-lifecycle` (the states an order can legally reach),
`https://vike.io/docs/trader/concepts/reconciliation` and `https://vike.io/docs/trader/concepts/reconciliation/policies`.
Tools: `https://vike.io/docs/ai/trader/tools/node_snapshot`, `https://vike.io/docs/ai/trader/tools/cancel_order`.
