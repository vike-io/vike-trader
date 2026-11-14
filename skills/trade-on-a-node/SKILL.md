---
name: trade-on-a-node
description: Place, change or cancel orders on a running vike-tradehub through the vike-mcp write tools (submit_order, submit_order_list, submit_combo, cancel_order, modify, flatten, mass_cancel, market_exit, set_trading_state) using the mandatory two-call preview gate — preview first, then confirm with the preview_token — reading node_snapshot before and after, and settling an unknown outcome before any retry. Use when asked to buy, sell, place a market, limit, stop or take-profit order, cancel or amend a resting order, flatten or close a position, cancel all orders, halt or resume trading, hit the kill switch, or get out of everything on a live or paper vike-tradehub.
metadata:
  tools: "node_snapshot health_node report submit_order submit_order_list submit_combo cancel_order modify flatten mass_cancel set_trading_state market_exit"
  source: "ai/trader/trading ai/trader/tools/submit_order ai/trader/tools/cancel_order ai/trader/tools/modify ai/trader/tools/flatten ai/trader/tools/mass_cancel ai/trader/tools/set_trading_state ai/trader/tools/market_exit ai/trader/tools/node_snapshot trader/guides/connect-an-agent"
---

# Trade on a node

These tools talk to a running `vike-tradehub`: the reads `node_snapshot`, `health_node` and `report`,
and the order writes below. Every write is preview-gated — a first call sends nothing, and only a second call carrying BOTH
`confirm: true` AND the `preview_token` the first call returned reaches the tradehub
(`crates/vike-mcp/src/lib.rs`'s `Server::call_tool`). Anything less is another preview, not an
error. The tradehub's own limits and the core risk gate are what enforce; the client-side preview is
advisory (`https://vike.io/docs/trader/guides/connect-an-agent`).

## Preconditions

- The server was started as `vike-mcp --node <host:port>`. Without `--node`, `node_snapshot`
  returns a clean error and every write tool still previews; what errors is a call that passes the
  gate and finds no tradehub to send to (`https://vike.io/docs/trader/guides/connect-an-agent`).
- `VIKE_TRADEHUB_OBSERVE_KEY` for the read, `VIKE_TRADEHUB_CONTROL_KEY` for the writes — rows of the
  hub key store, and nowhere else (never an environment variable). A missing key is a tool error
  naming the store (`https://vike.io/docs/ai/trader/tools/node_snapshot`).
- Nothing here can arm a venue: a venue with no credentials does not run, and the per-venue
  policy ceiling can only ever refuse (`https://vike.io/docs/ai/trader/trading`).

## The gate — applies to every write tool

1. Call the tool with its arguments and NO `confirm`. The response has `will_execute: false`, the
   resolved `wire_command`, an advisory `guardrail`, `node_verdict`, `verified_by_node`,
   `venue_check`, `tier` for a command that names one account, the echoed `reason`, a `note`, and
   the `preview_token` (`crates/vike-mcp/src/node_writes.rs`'s `preview_of`).
2. Read `node_verdict.checked_by`. `"node"` means the tradehub answered and `accepted` is its own
   judgement; `"none"` means it could not be asked and `reason` names the fault; a `null`
   `node_verdict` means no `--node` or no control key. `verified_by_node` is `true` exactly when
   `checked_by` is `"node"` — a fault is never a verdict. Never read an absent verdict as approval
   (`https://vike.io/docs/ai/trader/trading`).
3. If the preview is what you intend, call the SAME tool with the SAME arguments plus BOTH
   `confirm: true` (the JSON boolean — the string `"true"`, `1` and `"yes"` fall through to a
   preview) AND `preview_token: "<token from step 1>"`.
4. Read `will_execute` / `outcome` in the reply. `confirm: true` without a token, or a token without
   `confirm: true`, returns a FRESH preview with a new token and sends nothing — it is not an error
   (`Server::call_tool`; `https://vike.io/docs/ai/trader/tools/submit_order`). `outcome: "accepted"`
   is only the node's RECEIPT for the command: its core acts afterwards, and a denial or a rejection
   shows up only in `node_snapshot`'s `recent_events`, so read them before calling the order placed
   (the reply's `note` says the same).

`venue_check` is on both the preview and the confirmed reply, and says which of three answers the
venue gate gave: `mounted` (the daemon reports running this venue), `none` (the command names no
venue — `market_exit` and `mass_cancel` with `venue` omitted mean EVERY engine), or `unverified`
(the set of venues the daemon runs could not be read, so nothing was compared). Do not read `unverified` as a
pass — it means the check did not run (`Server::vet_commanded_venue`).

`tier` is the tier of the account the command reaches, in the node's own word (`demo` and `live` can
share a venue id). `tier: null` means the node could not be asked or does not say, `tier_unread` says
which, and you treat that account as live. A command that reaches no one account has no `tier` key.

`guardrail` is advisory and nothing refuses on it. `notional` is what the node's own value of the
contract makes it, so an inverse contract's is its quantity. `notional: null` with
`within_limits: null` means NOT CHECKED, and `unpriced` says why (a market order on a linear contract,
a symbol the account's engine does not trade yet, no frame to read): `node_verdict` is what counts,
and a null is never a pass.

Token rules, each enforced in `Server::call_tool` and `PendingPreviews`:

- Fires once: `PendingPreviews::take` removes it before execution; a repeat gets the error
  `unknown or already used`. Read that error as possibly-already-sent — settle it with
  `node_snapshot`, do not re-confirm (`https://vike.io/docs/ai/trader/tools/submit_order`).
- Expires after 60 s (`PREVIEW_WINDOW`) — an expired token is consumed as it is rejected. Take a
  fresh preview.
- Bound to the command it previewed: `same_intent` compares the whole wire command, ignoring only
  the minted `order_client_id`. Confirming a different command is refused and consumes the token.
  Changing any argument between preview and confirm means a new preview.
- Dies with the `vike-mcp` process (`https://vike.io/docs/ai/trader/trading`).

What executes is the STORED previewed command, so the `order_client_id` shown in the preview is
the one sent; you do not pass it back (`Server::call_tool`).

## Procedure

1. **Read the board first** — `node_snapshot` (no arguments). Check `trading_state`, `orders`,
   `positions`, `venues[]`, and `fault`: a non-null `fault` means the core is in safe state with
   every engine halted (`https://vike.io/docs/ai/trader/tools/node_snapshot`). Do not open risk into a faulted
   tradehub. **Take every write's `venue` from `venues[].venue` in this snapshot**, exactly as spelled
   — never from the operator's wording, the symbol, or a venue you know exists. A venue the tradehub
   does not run is REFUSED at the preview and no token is minted, so the command cannot be
   confirmed at all; the error names the venues the tradehub does have
   (`crates/vike-mcp/src/venue_gate.rs`'s `Server::vet_commanded_venue`).
2. **Name the account.** A command that opens or closes on a venue reaches exactly ONE account, and
   the sender says which (decision 0254). Take `account` from `venues[].account` in the same
   snapshot, exactly as spelled. Omit it only where the node has one account of that venue, running
   or not (`not_running[]` counts): anywhere else the call is REFUSED with nothing sent, and the
   refusal lists the accounts to choose from. `DEFAULT` names the unlabelled account on purpose. A
   node with twenty accounts of a venue never reads a bare venue as all twenty. This covers
   `submit_order`, `flatten`, and `market_exit` and `mass_cancel` when they name a `venue`.
3. **Pick the verb** from the table below and build its arguments. Pass a `reason` — it is recorded
   in the tradehub's audit trail (control characters stripped, capped at 512 chars) and never reaches
   the order (`tools_spec`'s `reason` argument).
4. **Preview** (gate step 1), read `wire_command`, `guardrail` and `node_verdict`.
5. **Confirm** (gate step 3) within 60 s, with both fields.
6. **Classify the outcome** using the table under Outcomes.
7. **Watch the result** with `node_snapshot` — acceptance means the tradehub took the command, not
   that an order filled or left the book.

Two reads answer in one call what the board leaves you to work out. `health_node` judges the
tradehub: read `verdict` and the `checks[]` rows, and read it as the answer even when the tradehub is
dead, which is `verdict: FAIL` rather than a tool error (`crates/vike-mcp/src/node_reads.rs`'s
`Server::tool_health_node`). `report` is the live tearsheet from the tradehub's own journal; `cash` and
`periods_per_year` rescale it, and a tradehub that cannot render one says why in a tool error, never
with an empty sheet (`Server::tool_report`).

## Verb reference (argument names from `tools_spec`)

| Tool | Required | Optional | Does |
| --- | --- | --- | --- |
| `submit_order` | `venue`, `symbol`, `side` (1 buy / -1 sell), `qty` | `order_type` (market \| limit \| stop \| stop_limit \| take_profit \| market_to_limit \| trailing \| trailing_limit \| pegged \| trailing_take_profit (default market); stop, stop_limit and take_profit fire at trigger_price; trailing and trailing_limit trail the market by `trail` (or `trail_percent`) and take no price, their trigger_price an optional activation; trailing_take_profit is trailing's if-touched twin, a sell's trigger sits `trail` (or `trail_percent`) above the lowest price since it rests and fires when the price rises to it, a buy's the mirror, with no price and no trigger_price; pegged takes `peg` and no price or trigger_price), `price`, `trigger_price`, `reduce_only`, `trail` (optional, trailing, trailing_limit and trailing_take_profit only (it or trail_percent required there): the trail, the distance in price (never a percent) the stop keeps from the best price since it activated. A modify changes a trailing order's size only), `trail_percent` (optional, the three trailing kinds only, in place of trail: the trail as a percent of the best price since it activated, above 0 and below 100; a venue that trails by price only refuses it by name), `limit_offset` (optional, trailing_limit (and required there) and trailing_take_profit only: the limit offset in price, zero or more; its limit is the stop less it for a sell, plus it for a buy (a trailing_take_profit with none fires as a market order)), `peg` (optional, pegged only (and required there): the anchor the venue keeps the order at, best (its own side of the touch) \| mid \| market (the far side) \| primary (its own side of the national best bid and offer); a venue that holds no pegged order refuses it by name), `peg_offset` (optional, with peg only: the distance in price from the anchor, positive aggressive (a buy pays more), negative passive; default 0), `adjust_at` (optional, a stop or stop_limit only: once the price reaches this level, on the protected side of the stop (a sell stop's above it, a buy stop's below), the order becomes what adjust_to names, once; a venue that cannot adjust a stop refuses it by name), `adjust_to` (with adjust_at (and required there): what the order becomes, stop \| stop_limit \| trailing), `adjust_stop` (with adjust_at (and required there): the stop price of what it becomes, on the near side of adjust_at (a sell's below it, a buy's above)), `adjust_limit` (with adjust_to stop_limit only (and required there): its limit price), `adjust_trail` (with adjust_to trailing only (it or adjust_trail_percent required there): its trail in price), `adjust_trail_percent` (with adjust_to trailing only, in place of adjust_trail: its trail as a percent, above 0 and below 100), `account` (WHICH ACCOUNT of `venue` — node_snapshot's venues[].account, exactly. Omit it only where the node has ONE account of the venue, running or not: anywhere else an unnamed command is REFUSED, with nothing sent, and the refusal lists the books (a lane twin is that one account's second lane). DEFAULT names the unlabelled one deliberately. The preview_token is bound to it), `order_client_id` (optional: this order's id. Omitted, the server mints one and the preview shows it. A resend under an id the node already holds is refused; a resend under a minted id is a new order), `display_qty` (optional: the quantity the venue SHOWS of a limit order (an iceberg's visible slice, or 0 for a hidden order), below qty; a venue that cannot show a slice or hide an order refuses it by name), `all_or_none` (optional, a limit order only: fill all of it in one execution or none of it; it rests until it can (all-or-none at once is time_in_force fok); a venue that cannot refuses the order by name), `sweep_to_fill` (optional, a limit order only: take every venue's liquidity at once up to the limit; not with post_only; a venue that cannot refuses the order by name), `discretionary_amount` (optional, a limit order only: the market sees the limit price, and the order executes up to the price plus this for a buy, less this for a sell; above zero; a venue that cannot refuses the order by name), `release_at` (optional: hold the order on the node and send it at this moment, epoch milliseconds UTC, in the future. node_snapshot lists it in held_exits with its release until then, and cancel_order cancels it there), `release_when_price` (optional, with release_at: hold it also until the price is there, above:`<price>` or below:`<price>`; above:<SYMBOL>:`<price>` reads another symbol the account's engine trades), `release_when_price_on` (optional, with a release_when_price that names its symbol: the venue whose price it reads, a venue id (binance, bybit, ...) and a price source only, never traded; an account of that venue on the node must trade the symbol. node_snapshot's held_exits row names it as release_venue, and says in waiting why an order whose moment came is still held), `time_in_force` (optional: the order's lifetime, gtc (the default) \| day \| ioc \| fok \| gtd (with gtd_expiry) \| post_only on a limit order, or an auction on a market or limit order, at_open \| at_close (at_open on a market_to_limit too: the opening auction); a venue that cannot do it refuses the order by name), `gtd_expiry` (optional, with time_in_force gtd only: when the order ends, epoch milliseconds UTC, in the future) | Submit one order — the one verb that opens risk |
| `submit_order_list` | `venue`, `symbol`, `side` (1 buy / -1 sell: the entry's side (an oco's two legs both take it)), `qty` (the size of every leg but a ladder's targets (an oco's two legs, an oto's entry and exit, a ladder's entry and its stop)), `shape` (oco \| oto \| ladder. oco: a limit at limit_price and a stop at stop_price (a stop-limit at stop_limit_price), the first to fill cancels the other. oto: an entry (a limit at entry_price, else market) and ONE reduce-only exit on the other side held until it fills, a stop at stop_price or a limit at limit_price. ladder: an entry, one stop at stop_loss for the whole size and the partial take_profit targets) | `account` (as submit_order's account: every leg goes to this one account), `entry_price` (oto and ladder: the entry's limit price; omitted, a market entry), `limit_price` (oco: the limit leg's price; oto: the exit's limit price), `stop_price` (oco: the stop leg's trigger; oto: the exit's stop trigger), `stop_limit_price` (oco only: make the stop leg a stop-limit at this limit), `reduce_only` (oco only: both legs may only reduce an open position (an exit pair)), `stop_loss` (ladder only: the stop-loss trigger, for the whole size), `take_profit` (ladder only: the partial take-profits as price:qty,price:qty (one or more, summing to no more than qty)), `order_client_ids` (optional: every leg's id, as id,id,... one per leg in leg order (an oco's limit then its stop; an oto's entry then its exit; a ladder's entry, its stop, then its take-profits). Omitted, the server mints one per leg and the preview shows them. A resend under ids the node already holds is refused whole; a resend under minted ids is a new list) | Submit one order list (an oco pair, an oto entry with its exit, or a ladder) on one account and symbol, every leg with its own id; a venue that cannot hold a leg refuses the whole list |
| `submit_combo` | `venue`, `side` (1 buys the combo / -1 sells it (every leg flips with it)), `qty` (combo units: each leg trades \|ratio\| x qty of its own instrument), `legs` (two legs or more, in the venue's creation order, each an object {symbol, ratio, limit}: symbol the leg's instrument, ratio a non-zero integer, SIGNED (side x ratio above zero buys the leg), limit that leg's own limit price. Name a limit on every leg (a combo priced by its legs) or on none; any other key in a leg is refused) | `account` (as submit_order's account: the combo is ONE order on this one account), `net_limit` (the net limit per combo unit, SIGNED (the sum of ratio x price: a debit above zero, a credit below); omitted with no leg limit, a combo market order; never beside leg limits), `time_in_force` (optional: the combo's lifetime, submit_order's words: gtc (the default) \| day \| ioc \| fok on a priced combo, or an auction, at_open \| at_close; gtd (a combo carries no expiry) and post_only (no book models a maker-only combo) are refused; a book or a venue that cannot do it refuses the combo by name), `order_client_id` (optional: the combo's ONE order client id. Omitted, the server mints one and the preview shows it. A resend under an id the node already holds is refused whole; a resend under a minted id is a new combo) | Submit one combo: two legs or more traded as one order on one account, priced by its signed `net_limit`, by a `limit` on every leg, or at market, under one id; only deribit places one (by its net), only a paper book fills one priced by its legs |
| `cancel_order` | `order_client_id` | none | Cancel one resting order by id |
| `modify` | `order_client_id` | `new_qty`, `new_price`, `new_trigger_price` (the new trigger price of a stop, stop-limit or take-profit order (on a stop it wins over new_price); the node refuses it on an order with no trigger, and refuses it and new_price on a trailing order, whose modify changes its size only) | Change a resting order's qty, price and/or trigger price — at least one of `new_qty` / `new_price` / `new_trigger_price` |
| `flatten` | `venue`, `symbol` | `account` (WHICH ACCOUNT of `venue` — as submit_order's account) | Close that net position with a reduce-only market order sized on the tradehub |
| `mass_cancel` | none | `venue` (omit for every venue), `symbol` (omit for every symbol), `account` (WHICH ACCOUNT of `venue` — as submit_order's account; only with `venue`) | Cancel every live order in scope |
| `set_trading_state` | `state` (active \| reducing \| halted) | none | The account kill switch, applied to every engine |
| `market_exit` | none | `venue` (omit for every engine), `account` (WHICH ACCOUNT of `venue` — as submit_order's account; only with `venue`) | PANIC BUTTON: cancel every live order, then flatten every one-way position |

Every write tool above also takes the arguments below, and no row repeats them:
they are spliced into each schema from ONE definition, so they mean the same thing
on every one of them.

- `reason` — optional rationale — WHY this command is being issued. Recorded in the node's audit trail (control characters stripped, capped at 512 chars); never reaches the order, the core, or the venue.
- `confirm` — must be true AND accompanied by a valid `preview_token` to execute; either one missing returns a preview instead
- `preview_token` — the `preview_token` returned by this tool's preview call. Fires ONCE, expires after 60s, and is BOUND to the exact command it previewed — confirming a different command with it is refused.

A missing required field is a tool error before any command is built
(`https://vike.io/docs/ai/trader/tools/submit_order`).

Per-verb rules that change what you send:

- `submit_order`: a market order on a linear contract carries no price, so the client-side `guardrail`
  notional is `null` — only `node_verdict` can size it; a stop or take-profit is sized at its `trigger_price`.
  The tradehub refuses an empty `order_client_id`; the server mints one before the preview, so omit
  it (`https://vike.io/docs/ai/trader/tools/submit_order`). A `release_at` order is HELD on the tradehub
  until then: it is in `node_snapshot`'s `held_exits` with its `release`, not in `orders`, and
  `cancel_order` cancels it by its id.
- `modify`: name `new_price` whenever you name `new_qty` — on a tradehub with a notional ceiling, a
  qty change with no price is refused (`https://vike.io/docs/ai/trader/tools/modify`). A re-priced amend needs a
  re-preview.
- `flatten`: no quantity argument; it closes `|position|` resolved at apply time and is a no-op
  when flat. Partial close is a `reduce_only` `submit_order` (`https://vike.io/docs/ai/trader/tools/flatten`).
- `mass_cancel`: `symbol` without `venue` cancels NOTHING — the intent is ignored and only surfaced
  to recent events; the call still succeeds. Always name `venue` with `symbol`
  (`https://vike.io/docs/ai/trader/tools/mass_cancel`).
- `cancel_order`: accepted means the tradehub took the command; an id that never existed is
  acknowledged the same way. Confirm removal with `node_snapshot`. An order placed before a node
  restart is not in the node's order registry: the cancel is still acknowledged, nothing is sent to
  the venue, and the node rings an `OrderCancelRejected ... cancel it at the venue` line in
  `recent_events` — that line is NOT proof of removal, the order still rests and must be cancelled at
  the venue (`https://vike.io/docs/ai/trader/tools/cancel_order`).
- `set_trading_state`: `reducing` denies non-reducing orders; `halted` denies new orders but still
  admits a position-covered reduce. `active` does NOT clear the separate HALT file sentinel, and
  re-arming a latched drawdown against the same high-water mark can re-latch it
  (`https://vike.io/docs/ai/trader/tools/set_trading_state`).

## Outcomes of a confirmed call

`Server::execute` waits `config.node_ack_wait_ms` (6 s unless a row says otherwise) for the tradehub's answer to THIS command
(`crates/vike-mcp/src/node_writes.rs`):

| Reply | Meaning | Do |
| --- | --- | --- |
| `sent: true`, `outcome: "accepted"`, `order_client_id` | tradehub took the command (empty id = account-wide verb) | `node_snapshot` to watch it |
| error `node rejected the command: <reason>` | the tradehub's own refusal — usually rate or notional | read the reason; fix, re-preview |
| `sent: true`, `outcome: "unknown"` | no answer within the ack wait — MAY STILL EXECUTE | `node_snapshot` BEFORE any retry |
| error saying the control connection dropped and the outcome is UNKNOWN | MAY HAVE EXECUTED | `node_snapshot` BEFORE any retry |

The two ambiguous cases arrive in different shapes — one a successful result, one an `isError`.
Inspecting only `outcome` misses the second (`https://vike.io/docs/ai/trader/trading`).

## What an UNKNOWN outcome obliges

1. Do not retry, re-confirm, or re-preview the same intent yet.
2. Call `node_snapshot` and look for the effect: the `order_client_id` in `orders`, the position in
   `positions` / `venues[].positions`, the state in `trading_state`, the tail in `recent_events`.
3. Only if the effect is absent, take a fresh preview and confirm again. Retrying blind is how one
   intent becomes two orders; a repeated `submit_order` id leaves one registry row but two orders on
   a paper run (`https://vike.io/docs/ai/trader/tools/node_snapshot`, `https://vike.io/docs/ai/trader/trading`).
4. Remember the snapshot is lossy — the latest state, never every intermediate one; `recent_events`
   is a tail, not a ledger (`https://vike.io/docs/ai/trader/tools/node_snapshot`).

## `market_exit` is the panic button

Use it when the ask is "get out of everything" or an incident (feed outage, faulted tradehub, runaway
strategy). Its `tools_spec` description names it the PANIC BUTTON: it cancels every live order and
then flattens every position, on the whole node when `venue` is omitted, or on one account of one
`venue`.

1. Preview `market_exit` with the scope (`venue` and `account`, or neither) and a `reason`. It is gated like every
   other write — `confirm: true` alone hands back another preview and does not fire the exit
   (`https://vike.io/docs/ai/trader/tools/market_exit`).
2. Confirm with both `confirm: true` and the `preview_token`, inside the 60 s window.
3. Read `will_execute` / `outcome`. Accepted carries an empty `order_client_id`.
4. Call `node_snapshot`. The exit is best-effort, not atomic: a resting order can still fill after
   a flatten leg, so RE-ISSUE the exit (fresh preview, fresh confirm) if the board is not flat.
5. Hedge-mode `LONG`/`SHORT` legs are skipped — on an account with only hedge buckets the
   mass-cancel runs but the flatten half mints nothing; report that rather than assuming flat.

You do not need `set_trading_state active` to escape a halt: the exit works from a halted core,
because a position-covered reduce is admitted under `Halted`. Reach for `market_exit`, not for
un-halting (`https://vike.io/docs/ai/trader/tools/set_trading_state`). One residual: a position off the lot
grid rounds its own leg up past itself and that leg is denied `halted`
(`https://vike.io/docs/ai/trader/tools/market_exit`).

## Do not

- Treat a `confirm: true` reply as evidence you traded without reading `will_execute`.
- Reuse a token, confirm a changed command with an old token, or hold a token past 60 s.
- Retry after `outcome: "unknown"`, a dropped-connection error, or `unknown or already used`
  without a `node_snapshot` first.
- Send a `modify` with `new_qty` and no `new_price`, or a `mass_cancel` with `symbol` and no
  `venue`.
- Guess an `account`, or drop one to get past a refusal: the refusal is what lists the accounts.
- Infer a `venue` from the operator's request instead of reading it off `node_snapshot`, or treat
  `venue_check: "unverified"` as a venue that was checked.
- Assume the preview's `guardrail` is a pass when `node_verdict.checked_by` is `"none"`.
