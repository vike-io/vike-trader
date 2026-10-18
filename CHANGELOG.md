# Changelog

All notable changes to vike_trader_rust are recorded here.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/); versioning is
[SemVer](https://semver.org/) — pre-1.0, a minor bump may include breaking changes.

Per-release notes are also auto-generated on GitHub (the PRs merged since the previous tag)
by `.github/workflows/release.yml`; this file is the curated human summary. Cut a release with
`just release-prep <X.Y.Z>` in the last candidate PR, then `just release <X.Y.Z>` (`docs/ops/release.md`).

## [Unreleased]

## [0.1.76] - 2026-10-18

### Added

- **Hand-run live DEMO smokes for per-account leverage on bybit, okx and binance (test code only).** Two `#[ignore]`d smokes per venue (decision 0182's owed demo drill): a read of what the leverage reader asks, and a write that is READ BACK and restored; they resolve the default DEMO account only and self-skip without its keys. Nothing here runs in CI and no exchange was asked.

### Fixed

- **The posted-leverage hand-over record is keyed by (venue, account), not the account label alone (offline-proved; no exchange was asked).** One `BridgeTrade` lane serves every venue, so every venue's default account shared the key `""`: a tradehub-only restart answered okx "the start handed the bridge 10x, not the 5x now asked" from binance's record, and a default-account start of a venue that posts nothing wiped them all. Bybit's position/list reader also refuses an isolated `tradeMode` row (an absent field stays cross).
- **Two test-only flakes (test code only).** `any_symbol_twin` waits (bounded) for the published roster before it reads the node, where a loaded box read it before the coalesced snapshot; and `release_verify_selftest.sh` matches its needle in the shell, because under `pipefail` `printf | grep -q` read a FOUND needle as missing (4 in 4000 runs).
- **`release_verify.sh` can no longer miss a CI-skip marker on HEAD, nor skip its refusal of entries left in `[Unreleased]`.** The message went through `printf | grep -q`, so under `pipefail` an early match let the writer die of SIGPIPE, the pipeline read 141 and a marked HEAD passed (MEASURED on the latency box: 4 misses in 10000 runs for a 99-byte message, 50 of 50 for a 300 KB one, 0 with the here-string now used). The `[Unreleased]` refusal (the v0.1.63 guard) read `awk | grep -q` the same way and now reads a captured value (364 misses in 2000 runs at 10 KB, 100 of 100 at 300 KB, 0 after); the selftest has a case for each.

## [0.1.75] - 2026-10-18

### Added

- **OANDA takes a second instrument the account's own list carries, when it is quoted in the account's currency (decision 0174's reopener for oanda, step 1 of `docs/ops/any-symbol-remaining-venues-plan.md`; offline-proved, no order placed on a second instrument and the OANDA network was not touched).** `SYMBOL_RULES` (`crates/vike-tradehub/src/server/symbol_admission.rs`) gives oanda `Admits(Spelling::OandaInstrument)` instead of `PerOrder`: the symbol is the engine's own spelling (`GBPUSD`, `GBP_USD` read as the same; a CFD name keeps its underscore, `SPX500_USD`), and an engine that trades one spelling is not admitted the other. The facts are the account's OWN `/instruments` list and `/summary` currency (`OandaFacts` over `AccountFacts::listed`, one per account in a `FactsByAccount`, under `AccountFacts`' bounds: one listing pair per ten minutes, single flight, a failure remembered, a non-`demo` account refused). **The currency fence: an instrument is admitted only when its quote currency is the account's** (nothing in the engine converts a quote currency; `USDJPY` on a USD account stays refused by name), and the facts admit only a five-decimal price grid and a unit step of 1, which is all the adapter writes (`fmt_price`, `signed_units`), so JPY pairs, metals and indices stay refused by name. `LiveFeeds::Oanda` is a `QuoteFeed` that hands the door a price watcher (a box that mounts oanda; one `/pricing/stream` per admitted instrument), `OandaReconClient` reads every admitted instrument and `RECON_READS_ADMITTED` lists oanda. ⚠ An oanda engine now FAILS CLOSED like the other `Admits` venues: an order on an instrument it was never told about is denied `symbol-not-admitted` (before, a limit order on one reached the venue on the mounted symbol's grid), and the old oanda door fences (a paper account refused, an opening order with no limit price refused) are replaced by the `Admits` rules. No measured crate was edited. `docs/ops/trading-any-symbol.md` and the plan's "What oanda built" carry the words and the doubts.

### Fixed

- **OANDA review findings on the any-symbol step above (offline-proved; no order was placed and the OANDA network was not touched).** (1) **An OANDA account on a box that mounts no OANDA market keeps its foreign pairs**: the `Admits` row made the engine fail closed for every instrument it was not told about, yet the only price handle was the mount's `LiveFeeds::Oanda`, so every foreign order was refused `this node runs no price feed for oanda` where `PerOrder` used to let a priced limit order through. `crates/vike-tradehub/src/feeds.rs`'s `price_only_venues` / `wire_price_only_feeds` now include oanda (`wire_oanda_price_feed`: the mounted arm's dial and price tap, nothing subscribed until the door asks; fail closed when the datahub advertises no oanda strategy lane or the dial is refused; a mounted oanda is left to its mount). Its link dead-man reading is `Discloses` and its `SessionBounded` row keeps it out of the armed set, as before. alpaca, the other credentialed `Admits` venue without one, still has no price-only feed (its limitation row stands). (2) **A pair stream that cannot connect no longer holds its place for ever**: `crates/vike-tradehub/src/remote_backend/facts.rs`'s `gap_holds_pump` (already on main for ig) does not treat an open venue gap as a reason to keep an idle pump of a `SessionBounded` venue, because the core keeps no link dead-man window for it; the OANDA tests pin that oanda's row takes that path: the reaper closes the pump `IDLE_TTL` after the door let go of it, the quote lane is unsubscribed (the redials stop), the price tap forgets the gap and the place under `MAX_WATCHED` is free. An armed venue keeps the rule. The mounted stream thread is untouched. (3) **The OANDA facts cache is keyed by the account behind the credentials**: the `FactsByAccount` of `BridgeTrade` keeps a non-reversible 64-bit fingerprint of the account id (`vike_oanda::OandaCatalog::account_fingerprint`; no token, no id) beside each label's `OandaFacts`, so another account's credentials pasted under the same label no longer answer from the old account's instrument list and currency for up to ten minutes. (4) **`OandaReconClient` reports the fills of a symbol admitted after the fill floor moved**: the floor is account-wide and advanced over fills of instruments the client did not read yet; a newly admitted instrument is now read once from the floor the client started with (one more `sinceid` read), and the floor advances only after every read of the fetch succeeded. No measured crate was edited.

## [0.1.74] - 2026-10-18

### Added
- **`vike-cli trade doctor` reads the node's uptime, the age of its oldest resting order and the HALT sentinel from the node's frame, instead of telling the operator to ask the box.** Three additive, optional frame fields: `WireSnapshot::uptime_ms` (milliseconds since the node PROCESS started, a clock `tradehub_cli.rs` fixes before it reads its arguments, not the publisher's, which is spawned after the accounts are up), `WireSnapshot::halt_sentinel` (whether the HALT file exists, re-read for every frame with the same `vike_exec::halt::sentinel_engaged` every adapter's submit seam asks) and `WireOrderView::age_ms` (how long this process has known a non-terminal order, from a first-seen map the publisher prunes; never the venue's timestamp, so after a restart it is at most the uptime). The publisher stamps them in `frame_snapshot` after the pure `project`, in `vike-tradehub`, so no measured crate (`vike-core`, `vike-exec`, `vike-model`, `vike-venues`) is touched and no latency A/B is owed; `project` and its golden JSON are unchanged, and a node that says nothing writes no key, so an older reader and an older node both work. No feature string and no `NODE_PROTO_VERSION` change (decision 0112, verdict 3: a pure addition to a reply): each field is an `Option` so that silence is never a value, which matters most for the sentinel (a `bool` defaulting to false would read an older node's silence as a clear kill switch). The daemon hands its publisher the process's one resolved sentinel path (`PublisherHandle::with_halt_sentinel_path`, from `start_observe_server`); a publisher never handed one says nothing. The doctor's rows become checks: `uptime` is WARN under two minutes (`restarted recently`), `order-age` is WARN for a resting order older than a day, and `halt-sentinel` is FAIL when the file exists (exit 2); the uptime and sentinel rows are also judged on a node that has not folded yet. Against a node that predates the fields each row reads `unknown (node too old)` (and `unknown (not on the wire)` stays for what no node publishes), never zero uptime, a new order or "no sentinel". Not built: per-venue price age (`price-feeds` stays UNKNOWN: the age lives in the core's price board, a measured crate) and the leverage a venue applied (no per-account note exists on a non-measured wire type; the directory's `WireDirectoryAccount` frame is pinned). `docs/ops/trade-doctor.md`.
- **Dukascopy's any-symbol PRICE lane (built offline; nothing has run against a JForex login).** An opening order on a new pair was refused `no price feed for dukascopy` (the sidecar protocol had no tick). Now the JForex sidecar jar takes a `subscribe` / `unsubscribe` stdin command and emits a `tick` envelope (symbol, bid, ask, the tick's own time) for the instruments asked for and no other (lazy: nothing at login; bounded; two-sided only; the SDK subscribe never waits, so a price request cannot delay an order), announced by a `ticks` feature word on `ready` (an old jar is refused in words and an old Rust side never sends the command). Because the sidecar is ONE child per daemon, `vike_dukascopy::quotes::QuoteBus` is the meeting point of the exec client's reader (it attaches the sidecar and hands over each tick) and the gatewayhub's new credentialed stream `cs_venue=dukascopy` (`DukascopyQuotes`: logs in to nothing, quotes only, a real-time label, no link state disclosed). `quotes::accept` drops a tick that is one-sided, crossed, untimed or not within seconds of the box's clock. The tradehub wires a PRICE-ONLY dukascopy feed (`wire_dukascopy_price_feed`, `LiveFeeds::Dukascopy`, never a mount) when the datahub advertises the lane; the link dead-man reads it through the two hub links (`Inert` row, `StratLinkOnly`), so a sidecar that dies alone ages its prices out without being detected. ⚠ **The jar is a release asset: this needs the release that builds it, the box's `bin/jforex/jforex-bridge.jar` replaced (deploy helper or `scripts/fetch_release_tools.sh jforex`) and the dukascopy adapter restarted; until then the lane refuses in words and the mounted pair is unaffected.** No measured crate touched. `docs/ops/any-symbol-remaining-venues-plan.md`, "The price lane" and its STOP-AND-REPORT list.
- **An account can state its own margin leverage on bybit, okx and binance DEMO accounts, and the exchange is written once and READ BACK before the engine applies it (decision 0182, proposed; built offline, no exchange has been asked).** `vike-cli secrets account set-leverage --id N --leverage X` now accepts a whole number on a `demo` account of those three venues (the venue list in vike-secrets is now a table of venue, open tiers and whole-numbers-only; a `live` row, a fraction, aster and deribit are refused in words, hyperliquid is exactly as shipped). At the account's start the tradehub asks the venue for `floor(min(stated, policy.max_leverage, [risk] max_leverage))` (one new optional field on `AdapterStart`, `account_leverage`, additive on the wire); the gatewayhub re-reads the row, probes the account READ-ONLY (plain cross margin and one-way positions, the figure within the symbol's maximum, no open position moved to another leverage), hands the bridge that figure as its profile's number so its ONE existing start-time write carries it instead of the profile's number or the default 2x, then polls the venue (about 5 s, bounded) until it holds exactly that number. The engine applies the figure only on that read-back; any refusal leaves the account exactly as an account with no stated figure (1x, and the start posts what it always posted). Default none changes nothing and makes no exchange call. Not closed: an OKX order can still go isolated while the write is `cross`; admitted symbols keep the venue's own per-symbol leverage; the fixtures are the venues' documented shapes, not captures, so a live demo drill (accounts 4, 15, 3) is owed before any live tier opens.

### Fixed
- **The gatewayhub's `ibkr_reconnect` tests no longer flake (test code only).** The fake IBKR gateway keeps its port bound for its whole life and closing it is a flag, so no neighbouring test can take the freed port number (1 re-bind in 780 found its port taken, for 2.9 s); the fake now also ACCEPTS what its probe connects, because a kept listener nobody accepts from holds 129 connections and then drops every later SYN (MEASURED: the three real-client rigs of `real_bridge` answered 129 probes and every connect after that timed out, so the gateway that came back was never found). And a redial after the reconnect gave up is told apart from one that reached a FRESH session the orphan reaper opened (the epochs differ) and runs the scenario again (19 of 145 runs failed under load before).

## [0.1.73] - 2026-10-18

### Added
- **An operator can release ONE venue's link-dead-man halt early: `vike-cli trade resume-venue <venue> [--force]` and Telegram `/resumevenue <venue> [force]` (decision 0180).** `resume` is node-wide, so it also cleared every OTHER venue's halt and un-latched the drawdown latch; there was no way to bring back only the dead venue before its link recovered, or after a restart that left its halt in place. The new wire verb `WireCommand::ResumeVenue { venue, force }` (feature word `venue-resume`, refused client-side by an older node, audited as `resume_venue` with the venue and the override, preview / `--yes` / `--reason` as for the other control verbs) becomes `Command::ResumeVenue`: each engine of that exchange goes back to the state the latch recorded before the halt, nothing else changes. The core REFUSES (nothing changed, a WARN `OPERATOR RESUME OF THE VENUE REFUSED` and a recent-events note) while a link of the venue still reads down unless `--force`, on a faulted core, with no armed switch, for a venue it runs no engine of, and when the latch holds nothing for the venue and the other venues are `Halted` too (a node-wide halt, `resume`'s). A forced resume starts the venue's windows over, so a link that stays down trips again after a fresh grace. After a restart the overlay is gone (it was never journaled): the venue's `Halted` engines then take the strictest state the other venues hold. The command is not journaled. The CLI prints `accepted` when the command reached the core's queue; the verdict is the WARN above, `journalctl -u vike-tradehub | grep 'OPERATOR RESUME'`. `docs/ops/kill-switches.md`.
- **Dukascopy admits a currency PAIR by name (the first safe step of any-symbol for dukascopy; decisions 0173/0174, nothing has run against a sidecar).** `vike_dukascopy::any_symbol::instrument_facts` answers from the committed JForex SDK table alone (static, offline, no login): the tick of the pair, and the sidecar's own smallest order as both the minimum and the step. It admits a `FOREX` row quoted in the ledger's money (the constant `USD` in the gatewayhub, since the sidecar's `ready` line carries no currency): `AUDUSD`, `EURUSD`, `GBPUSD`, `NZDUSD`. It REFUSES by name: the two metals (`XAUUSD`, `XAGUSD`: the unit of a metal's amount is unmeasured, and the sidecar's 1000-unit floor would be 1000 troy ounces of gold, ~2.4M USD per minimum order, with nothing in the facts guarding a notional; they stay refused until one demo order proves the unit), a name the table does not carry, the 103 CFD rows whose pip is the SDK's blanket placeholder, the three CFDs with a real pip (the sidecar stamps their fills with the enum name, not the name the order is placed under), the two coin rows typed `FOREX` (`BTCUSD`, `ETHUSD`), and every other quote currency. The gatewayhub answers `SymbolFacts` for a `demo` dukascopy account (`BridgeTrade::dukascopy_symbol_facts`); the door row is `dukascopy: Admits(Spelling::PairName)` (the slash is dropped, `EUR/USD` is `EURUSD`; the engine's own pair under another spelling is placed on the engine's spelling on every tier; a pair is admitted only when it is quoted in the money of the engine's own pair), which makes the dukascopy engine fail closed for a symbol it was never told (`symbol-not-admitted`). ⚠ There is NO dukascopy price feed (the sidecar protocol has no tick message), so an opening order on a new pair is refused `no price feed for ...` exactly as for ibkr; no reconcile change (the venue hands the node no reconcile client). The mounted symbol's behaviour, `grid: None` and `point_divisor` are unchanged. Not touched: `vike-venues`, `vike-exec`, `vike-core`, `vike-model` (no latency A/B owed). `docs/ops/trading-any-symbol.md`, `docs/ops/any-symbol-remaining-venues-plan.md`, "What dukascopy built".
- **cTrader and IG foreign symbols are priced on a box that mounts neither (a price-only feed each; offline-proved, no ctrader session was opened and no IG login was made).** The two venues were `Admits` at the control door but priced by their MOUNT's feed only, so an account of an unmounted venue answered `this node runs no price feed for ctrader` / `... for ig` to every foreign symbol. `crates/vike-tradehub/src/feeds/credentialed_price_only.rs` builds the mounted arm's strategy-lane dial with nothing subscribed (`wire_ctrader_price_feed`, `wire_ig_price_feed`), and both join `price_only_venues` and `wire_price_only_feeds`, so the link dead-man and the core's price-only sweep read them like Hyperliquid, deribit and the CEX venues. It is lazy: the gatewayhub opens the venue's credentialed stream (ONE per venue, as the venue's default demo account) only at the first quote subscription the door sends. A session that cannot be had is that subscription's refusal in the gatewayhub's words, and a feed that cannot be built leaves no price (fail closed). ig keeps the mounted arm's cap of four quote lanes. ctrader's price-only disclosure is the two hub links, which ARM it like a mount (an outage past the grace cancels and halts it while it holds a live order or an open position); ig's `SessionBounded` row never arms. An idle pump of a `SessionBounded` venue is now closed under an open venue gap (`remote_backend/facts.rs`'s `gap_holds_pump`), so an epic whose session never connects no longer holds one of the four lanes for ever. No measured crate was edited. `docs/ops/any-symbol-remaining-venues-plan.md`, "What the price-only feeds built", carries the limits, the doubts and what stays unmeasured.
- **IBKR's any-symbol PRICE lane (built offline; nothing has run against a gateway).** An opening order on a new stock was refused `no price feed for ibkr` (the node had no ibkr feed). Now the gatewayhub serves ibkr as a CREDENTIALED stream (`cs_venue=ibkr`, the `ibkr` build, the datahub's `CREDENTIALED_VENUES`; `vike_ibkr::quote_handle::LazyQuotes`): built with nothing dialled, quotes only, ONE data client for every symbol dialled by the first quote subscription through the gate's data lane (its six connects an hour and backoff apply; a failed dial is remembered 30 s; the engine's cap on symbols). The tradehub wires a PRICE-ONLY ibkr feed (`wire_ibkr_price_feed`, `LiveFeeds::Ibkr`, never a mount) when the datahub advertises the lane, nothing subscribed until the door asks. Only a two-sided quote of a stream the venue labelled REAL-TIME prices an order (`PriceTap::real_time_only`): a delayed or frozen stream is refused by name (`no price feed for ...: ... quote stream is delayed ...`), an unlabelled one is never a price. The lookup of a stock and the price stream share ONE data connection per gateway session (`vike_ibkr`'s `data_session`: IB refuses a second client under one id), and so does the grid pre-fetch; the link dead-man reads ibkr through the two hub links (`Inert` row, `StratLinkOnly`). ⚠ Not proven: the paper account's real-time entitlement, the real tick fields, the budget; a data connection that dies is re-dialled (next entry), which has not run against a gateway either. `crates/vike-gatewayhub/src/data/credentialed.rs`'s `CredentialedLane::new` takes the daemon's venue settings. No measured crate touched. `docs/ops/any-symbol-remaining-venues-plan.md`, "What ibkr built" and its STOP-AND-REPORT list.
- **IBKR's price stream re-dials a dead data connection (built offline; nothing has run against a gateway).** The data connection that dies (IB's daily logoff at 23:45 UTC, a lost link, a closed socket) used to end its pumps for good: the held symbols' prices went stale until a tradehub restart. Now `vike_ibkr::quote_handle::LazyQuotes` is SUPERVISED (a thread `ibkr-px-redial`, started with the first dial): a stream whose connection is gone (`IbkrFeeds::is_dead`: a reset or closed socket, a reconnect `ibapi` gave up, a notice of 1100/1300/2110; a request IB refused is not one) is disclosed at once as a `GapStart` on every held symbol's `quotes` lane (once per outage), re-dialled, every held symbol subscribed again, and told `Live` after the new connection stood 5 s. The schedule (`vike_ibkr`'s `redial` module): 20 s for `ibapi`'s own reconnect, then one dial per backoff step (the gate's 60 s, 2 min, 5 min after each failure), a flapping gateway is ONE outage with ONE `GapStart`; before every dial the supervisor asks whether the gateway is ready (its port answers and `GatewayGate::data_ready` says yes), so a closed port costs no attempt and no backoff step; an outage that begins in IB's daily logoff window waits until the gateway can be back (80 s past 23:45:00 UTC), and a connection `ibapi` or a lookup already brought back is JOINED with no dial. The data lane's six dials an hour are now shared by two lanes (`DataLane`): the dials that ask for a connection (the first dial, a lookup, the backfill) and this re-dial, four each at most, so a flood of lookups on a dead gateway cannot starve the re-dial nor a flapping stream a lookup; neither lane delays an order-lane dial. The caller's subscription ids are stable across a re-subscribe. ⚠ The disclosure is a link dead-man window: it opens ibkr's grace window while a price-only ibkr holds a position or a live order, about 100 s at the daily logoff against the default 120 s grace, and a data outage past the grace cancels ibkr's resting orders and halts ibkr (decision 0180). ⚠ Not proven: what a real logoff does to the data client, the logoff's time and length (observed once), a server link lost with the socket up (1100 reaches the client's global notice stream, not bound here, so nothing is re-subscribed). No measured crate touched. `docs/ops/any-symbol-remaining-venues-plan.md`, STOP-AND-REPORT 10.
- **`vike-cli trade doctor --node <observe addr>` judges a deployed node in one screen, from what it already publishes (read-only, the observe key).** The by-hand checks after a deploy become one table with one word per row (PASS, WARN, FAIL) and a last line `DOCTOR: OK`, `DOCTOR: WARN <n>` or `DOCTOR: FAIL <n>`; `--json` gives one document for a script, and the exit code is the verdict (`vike-cli trade doctor --help` states the three numbers; a missing observe key is refused before anything is dialled and is not a verdict). Rows: the door (ready, STARTING with where the start has got to, pre-fold, key refused, unreachable), the build, a handler fault, one `engine` row per account (running at a tier and whether the active row agrees, NOT RUNNING with the node's reason, or an active row with no engine and no reason), a Reducing or Halted engine, one route key published twice or a stopped account that still publishes its block, a running account whose database key changed, held reconcile rows (count, kinds, what a confirm does, the list and confirm commands), resting orders, non-flat positions (WARN on an account that is NOT RUNNING) and a position with no price source. It adds no wire field and no event: it reads `trade status`'s snapshot, the node's advertised capabilities and the directory of active rows, each under the observe scope. A fact the node does not publish (uptime, the age of the oldest resting order, the HALT sentinel, whether a price feed is live, an account's leverage, a Hyperliquid approval, the journal's ERROR lines) is an UNKNOWN row that says `unknown (not on the wire)` and names the command that answers it; UNKNOWN rows never change the verdict. CLI only, no MCP tool. No measured crate (`vike-core`, `vike-exec`, `vike-model`, `vike-venues`) is touched, so no latency A/B is owed. `docs/ops/trade-doctor.md`.

### Changed
- **The verdict of an operator reconcile confirm now reaches the log file at the shipped `warn` level.** `reconcile confirm applied` is `info` and the units run the file at `warn`, so the hub's audit line (`command accepted`, written when the command reaches the core's queue) was the only durable trace of a confirm. The core's verdict lines ride the new target `vike_core::reconcile::confirm` (a child of `vike_core::reconcile`, so a filter naming the parent still matches), which `vike-tradehub` pins onto the file at `info` beside the audit target (`audit::CONFIRM_VERDICT_PIN`); the parent stays unpinned because it carries the per-pass `reconcile pass folded` line. A confirm of an unpriced non-flat venue row, which books nothing, used to leave only a ring note and now logs its refusal.
- **`vike-cli secrets accounts` shows each account's `leverage`** (decision 0181) in a new column beside `max exposure`: the figure with an `x` suffix, or `none` when nothing is stated (the engine then margins at 1x). A listing-only change; the wire account listings (the Directory and the account list) are unchanged.

### Fixed
- **A foreign fill (a manual trade, no client order id of ours) from a gateway ring is routed to the account whose ring delivered it, not by its symbol.** On a box running two accounts of one exchange (live and testnet hyperliquid), a manual fill in the LIVE ring on a symbol only the TESTNET engine claims (ETH) folded into the testnet book, and the reverse; the ring's route key was dropped after the cursor advanced. `CoreThread::route_event_via` now asks the ring between the client order id and the symbol, for `Event::Fill` only, ignoring a key that names no engine of the fill's exchange. The ring's own engine folds the fill when it trades that symbol and drops it when it does not (`ExecutionEngine::accepts_symbol`, reconcile re-converges), so the other account's book is never touched. A fill of an order we placed still routes by its coid, a plain `Event` (no ring) routes as before, and a single-account process never reaches the rung. A shared symbol that was unattributed (nothing folded) now books to the ring's account when the ring is known.
- **A node-wide `resume` no longer reopens a venue whose link is still dead.** It emptied the link dead-man's overlay while the venue's window stayed `tripped`, so the venue could not trip again until its own `Live` and a new drop, and one `resume` meant for venue B also reopened A. A venue with a link still down now STAYS `Halted` (the core logs `RESUME LEFT <venue> HALTED: its link is still down (use resume-venue <venue> --force)` and notes it in the ring; its prior becomes the operator's state, so it returns to `Active` when its links are up); `resume-venue --force` is the override. A venue whose links are up resumes as before. `docs/ops/kill-switches.md`, decision 0180.
- **A link trip for a venue the core runs no engine for now records the venue halted.** An account of it hot-started while the window was still down skipped the halt check and started `Active` behind the dead link; it now joins the halt and is released with the venue's re-arm.
- **A stopped PRIMARY engine is no longer swept by the stuck-order ladder.** v0.1.70 skipped a retired price-only extra engine (its client is detaching, so a confirm to it, or a synthesized `OrderRejected` into a dead engine, is wrongly terminal for an order that is live at the venue) but `stuck_ladder_covers` answered `true` for index 0 before looking at the tombstone, and an account stop can name the primary. It now skips any retired engine; a running engine is covered exactly as before.
- **A link outage already in progress when a price-only venue first holds something is now watched by the link dead-man.** A price-only venue is armed only while it holds a live order or an open position, and a `Disconnected` disclosed before that was dropped; nothing re-discloses a link that is already down, so the first order placed into the outage was unwatched for as long as it lasted. The link state now survives idleness as a fact, never a clock: a down link is remembered in a ledger that cannot trip, and the sweep that finds the venue holding something opens a fresh window from it (the grace runs from the moment there is something to protect). A `Live` from the same source, or the account's stop, ends the entry. A mounted venue is never idle and behaves as before.
- **`resume-venue B` no longer lifts a node-wide halt while venue A is link-halted, and a node-wide `resume` that keeps A halted now survives a restart (decision 0180).** With no overlay for B the verb read the OTHER venues' state and skipped every engine the link dead-man halts, so after an operator `halt` it read `Active` and opened B mid-halt (a drawdown `Reducing` held only in A's recorded prior was lost the same way); it reads those engines' recorded prior now. The journaled `SetTradingState` replays as set-all in a restore, which would have reopened the kept venue after a crash: a `resume` that keeps a venue halted now writes a journal `Snap` after the fold (no format change), so the restore takes the engines as they are.
- **After a restart, a link-halted primary venue is no longer read as a node-wide halt.** The overlay is not journaled, so engine 0 came back `Halted` with nothing saying why: the drawdown latch returned early for the whole node and an account hot-started on another venue inherited `Halted`. A `Halted` primary beside a non-`Halted` engine of another venue is now that venue's halt (a one-venue node, or every engine `Halted`, still reads as the node's halt). A node-wide `resume` also stops logging `RESUME LEFT <venue> HALTED` for a price-only venue whose link is only remembered down while it holds nothing: the link sweep releases such a venue anyway.
- **`vike-cli trade status` and the stdio summary line no longer read `Active` while one venue is halted (review of v0.1.71, finding 2).** `trading_state` is the PRIMARY engine's, so the venue-scoped link dead-man halt (decision 0180) on a non-primary venue was invisible there, and a dead primary venue read `Halted` for the whole node. `trade status` (and the REPL's `status`/`snapshot`) now add `halted venues: …` / `reducing venues: …` from each engine's own published block, `trade status --json` adds `halted_venues` / `reducing_venues` arrays, and the summary line adds the two comma-joined strings of the same names. The top line is unchanged; nothing is added when every engine is Active.
- **A burst of distinct symbols can no longer spend the venue's per-IP request budget on symbol-facts lookups (review of v0.1.71, finding 5).** Each venue now has a token bucket (burst 10, 20 lookups per 10 s) in front of its per-symbol upstream request; a refused token answers the transient "busy, try again" with no network call and is not remembered as the symbol's failure. `MAX_SLOTS` is a hard cap now: a full table drops the oldest finished slot (never one mid-lookup) or answers busy. A boot re-admission of more than the burst on one venue is paced by it, and what it refuses the retry thread admits.
- **The boot re-admission reports its progress and ends on a budget (review of v0.1.71, finding 6).** Each restored symbol is announced on the `{"kind":"starting"}` stream as `readmitting <venue> <k>/<n>` (then `pricing …`), and the pass stops after 60 s: the symbols it did not reach are said at WARN and left to the retry thread instead of holding `ready` for the sum of N slow lookups.
- **A flapping tradehub link no longer re-reads a Hyperliquid account's leverage cap from the venue on every reconnect (review of v0.1.71, finding 7).** A confirmed cap is kept per `(account, tier, coin, stated leverage)` until the account's adapter is started again, so an `AdapterStart` that finds the adapter running spends nothing (it was up to ~100 weight of the per-IP budget per ask); the reads that remain ride the per-IP window of the newest adapter start instead of minting a fresh one. A refusal is never kept.
- **The boot re-admission is paced to the symbol-facts lookup bucket instead of failing past its burst (independent review of the v0.1.71 fixes, low).** More than ten restored symbols on a per-symbol venue (bybit, binance spot) were answered "busy" after the burst, logged at ERROR and left HELD (their restored resting orders unadopted) until the retry thread's first round 5 s later. The boot pass now waits 500 ms (one token) and asks again, at most six times per symbol and inside its 60 s budget, saying each wait at WARN ("paced, retrying"); a symbol still busy after that is a WARN and a miss for the retry thread, as before. The bucket (burst 10, 20 lookups per 10 s per venue, per `SymbolFacts` instance: per account for alpaca) and the door's asks are unchanged. ⚠ ig's and ibkr's per-minute budgets answer the same "busy" word, so a restore of more than 12 ig epics also waits up to 3 s per symbol before it hands the rest to the retry thread.
- **A STOPPED account no longer shows as a halted or reducing venue for ever (independent review of the v0.1.71 fixes, low).** A stopped account that is not started again keeps its last engine block (its position stays valued), and if that block was `Halted` or `Reducing` it was named in `vike-cli trade status`'s `halted venues:` / `reducing venues:` lines and `--json` arrays, the REPL's `status`, and the stdio summary line's `halted_venues` / `reducing_venues`. They now skip every account on the node's not-running list (the frame's `not_running`; in the daemon's own process, `PublisherHandle::not_running`, because the core publishes `CoreSnapshot::not_running` empty). Nothing in the engine block marks a stopped engine, so no measured crate was touched.
- **Review fixes of the ctrader and IG price-only feeds (no measured crate edited; offline, scripted doubles).** (1) `price_source_tiers` (`crates/vike-tradehub/src/venue_arming/deadman.rs`) now names `ctrader` and `ig` as `Demo`: the gatewayhub opens each venue's ONE credentialed stream as its default `demo` account whatever other accounts exist, so a `live`-tier account of either venue is kept out of the margin call and the drawdown latch (its protection is the stuck-order ladder and the dead-man) instead of being swept off the demo feed's price or symbol offset; a `demo` account is swept as before. (2) `QuoteFeed` (`crates/vike-tradehub/src/feeds/live.rs`) no longer holds the dial across a subscribe round trip: the dial is lent out, so the idle reaper's close and the teardown's shutdown never wait for a first ctrader handshake (~12 s) or an IG login (a close is applied, and a shutdown finished, when the round trip gives the dial back; other subscribers still queue for the one dial), and a capped feed (ig) remembers a failed subscribe for 30 s and refuses every ask in that window with its words, so a retried order spends no further IG login. (3) IG's four-lane cap: the lane of an ADMITTED epic is never reaped (it prices an engine's position, and the node cannot see that the engine holds nothing), so it stays until its account's engine stops or the node restarts; the fifth epic's refusal now says exactly that instead of promising the idle reaper would free a lane. `docs/ops/price-only-venue-safety-plan.md`, `docs/ops/any-symbol-remaining-venues-plan.md`.
- **IG reconcile no longer drops an admitted epic's fills past the first page of the activity feed (offline-proved against the scripted loopback IG double; not measured on the IG network).** `IgReconClient::fetch_fill_reports` read ONE page of 50 rows of the account-wide `GET /history/activity` and never looked at `metadata.paging`, so on a busy account some admitted epic's fills fell off the page and the report silently omitted them (a missed fill, never a false one; a restore or `hybrid` pass could not see it). With a second epic admitted the read now follows `metadata.paging.next` (IG's own link, only if it is a `/history/activity?` path) for at most 10 requests, keeps a row that two pages both carry once (a repeated row, not a repeated `dealId`: a close may reuse its open's), and FAILS the pass when the history is still truncated after the tenth page, when a next page adds no row or when `next` is not an activity link, instead of reporting a short list. With nothing admitted the read is unchanged (one request, the first page), although IG's docs make that page equally short for the mounted epic; whether to page it too is the owner's call. ⚠ Unmeasured live: the order of the rows, the exact shape of `next`, how the last page ends, and what a page costs against the historical-data allowance. A busy account's pass can now FAIL (visible, retried at the next interval) where it used to pass short. `crates/bridges/ig/CLAUDE.md`, `docs/ops/any-symbol-remaining-venues-plan.md` ("What ig built", STOP-AND-REPORT item 9). No measured crate was edited.
- **A bar seed over the strategy lane's frame ceiling is delivered as its newest bars, not dropped (the datahub's `a frame could not be sent and was dropped ... 149869 bytes exceeds the 131072-byte ceiling`, at every tradehub start since v0.1.60).** The frame was bybit's REST warm-up, 999 one-minute bars, a JSON object each: the tradehub dials the datahub's strategy lane for the mount's bar subscription, the lane's 128 KiB ceiling was sized for a 1000-level book, and the writer owed the seed as a hole, so the maker's core started with NO bar history (the margin-call and drawdown sweeps are per closed bar and were not affected; indicators and `Broker::bars` were cold). The tube's writer (`crates/vike-market-stream/src/writer.rs`'s `Batch::put_seed`) now sends the longest run of the seed's NEWEST bars whose frame fits the ceiling (most of bybit's 999) and logs a `WARN` with the count of older bars left out; a seed that fits is the same bytes as before, and a seed of which not even one bar fits is still a disclosed hole. The ceiling is NOT raised (a read bound on the tradehub's side). Delivering all 999 needs a paged seed on the wire and is undecided. Tests: `writer_tests.rs`'s `a_thousand_bar_kline_seed_over_the_lane_ceiling_reaches_the_client_as_its_newest_bars` and three siblings. No measured crate is touched.
- **Four credential-exposure findings of the v0.1.66..v0.1.72 audit (all LOW; no order, fill or venue read changes).** (1) The Hyperliquid agent-approval watcher and the IBKR link supervisor kept a clone of the whole account snapshot, credential book included, in a closure that lives as long as the process, so a key retired by a rotation stayed in memory; they now keep the account ROWS only (`AccountDirectory::without_credentials`). (2) A Hyperliquid `PRIVATE_KEY` with a bad character put that character and its position in the tradehub log (the leverage-cap refusal and the start's `invalid private key` warning printed `hex::decode`'s error); `Signer::from_private_key` now says `private key is not valid hex` and names nothing of the key. (3) cTrader's account-listing reader (it holds the account's access and refresh tokens) was kept in the facts slot after each ask; it is now passed per ask and dropped when the ask returns. (4) `IbkrFactsSource`'s derived `Debug` printed the IB account number; it now names the gateway only. No measured crate is touched.

## [0.1.72] - 2026-10-18

### Added

- **Dukascopy: the JForex SDK's price-grid constants are a committed, drift-gated table (`crates/bridges/dukascopy/data/jforex_instruments.tsv`, read by `vike_dukascopy::jforex_grid`).** 180 predefined `com.dukascopy.api.Instrument` constants (72 FOREX incl. BTC/ETH, 2 metals, 106 CFDs) with pip value, pip scale and tick scale (tick = 10^-tick_scale: 0.00001 EUR/USD, 0.001 USD/JPY and XAU/USD). `InstrumentTableTest` (run by `jforex-bridge.yml`, which now also triggers on `crates/bridges/dukascopy/data/**`) re-derives the file from the SDK jar and fails on a difference; the jar is unchanged. 103 CFD rows are the SDK's blanket placeholder (pip 0.01) and are marked `cfd-placeholder`; the `default_*` amount columns are the SDK's own run-time-overridden defaults, not venue facts. Nothing consumes the table yet: no behaviour changes. Findings, pinned by a test: the SDK puts XAU/USD, XAG/USD, EUR/HUF (3 decimals) and EUR/CZK (4) on a different price scale than the HTTP lane's `point_divisor` assumes, and GBP/NOK and GBP/SEK are not predefined. `docs/ops/any-symbol-remaining-venues-plan.md`, "Grid data for fxcm and dukascopy".
- **IBKR admits ONE US stock by name (the first safe step of any-symbol for ibkr; decisions 0173/0174, nothing has run against a gateway).** `vike_ibkr::any_symbol` resolves `TICKER.SMART.CCY` (`AAPL.SMART.USD`; the door reads a bare `AAPL` as the USD stock) through a scripted-testable seam: a symbol search, the contract details of the one stock found, and its market rules, on one data connection (`any_symbol_socket`, through `GatewayGate::dial_data`, so it never delays an order-lane connect; at most six data connects an hour per gateway). The grid tick is the COARSEST band of the market rules when every band divides it. It REFUSES by name: any other form (forex, future, option, combo, explicit exchange), a ticker with no stock or only stocks of other currencies, a ticker naming several stocks in its currency, a stock of a currency other than the engine's money, contract details that disagree or carry a multiplier, and a grid that cannot be read. The gatewayhub answers `SymbolFacts` for a `demo` ibkr account on the socket backend with per-name bounds (`ibkr_facts`: kept ten minutes, a refusal 30 s, single flight, six lookups a minute, 64 names); the door row is `ibkr: Admits(Spelling::StockName)` with an alias refusal (the same stock under another spelling) and a currency fence, which makes the ibkr engine fail closed for a symbol it was never told (`symbol-not-admitted`); the SOCKET reconcile client reads every admitted stock (`admit_symbol`/`admitted_symbols`: one account-wide read parsed per conId) and `RECON_READS_ADMITTED` lists ibkr. The mounted symbol's behaviour is unchanged. ⚠ No ibkr price feed exists on the node (the tradehub has no ibkr arm, the gatewayhub's data lane no ibkr stream), so an opening order on a new stock is refused `no price feed for ...`; the price handle `vike_ibkr::quote_handle` (one data client for every symbol, a real-time two-sided quote or no price) is built and tested but wired to nothing. Not touched: `vike-venues`, `vike-exec`, `vike-core`, `vike-model` (no latency A/B owed). `docs/ops/trading-any-symbol.md`, `docs/ops/any-symbol-remaining-venues-plan.md`, "What ibkr built".
- **IG takes a second EPIC when it is a USD-quoted FX market on a USD demo account (decision 0174's reopener for ig, step 5 of `docs/ops/any-symbol-remaining-venues-plan.md`; offline-proved against a scripted loopback IG double, no order was placed and the IG network was not touched).** `SYMBOL_RULES` (`crates/vike-tradehub/src/server/symbol_admission.rs`) gives ig `Admits(Spelling::Epic)` instead of `PerOrderUnsafe`: the symbol is an upper-case dot-separated epic (`CS.D.EURUSD.MINI.IP`; a market name like `EURUSD` is refused by name, and an epic in another case than one the engine trades is refused as the same instrument). The facts are the account's OWN login (`crates/bridges/ig/src/market_facts.rs`'s `IgFactsSource`: `GET /accounts` and `GET /markets/{epic}`, one kept session per account, at most 12 epic lookups a minute, `crates/vike-gatewayhub/src/trade/adapters/ig_facts.rs`'s `IgFacts`) and **only a CURRENCIES epic is admitted**: what IG's `size` means for an index, a share, a commodity, a coin, a bond or an option is UNMEASURED, so each is refused by its class, and so is a dated contract or spread bet, a non-USD account and a non-USD quote (`currencyCode` is USD on every order; nothing converts a quote currency). An FX epic is admitted only when the payload agrees with itself (`contractSize x pip` equals `valueOfOnePip`), with the pip as the tick, `minDealSize` as minimum and step and the contract size as the linear contract value the mounted epic's start never carried. `LiveFeeds::Ig` is a `QuoteFeed` that hands the door a price watcher on a box that MOUNTS ig, at most four extra quote lanes (each is an IG login and a socket; `IG_MAX_QUOTE_LANES`), `IgReconClient` reads every admitted epic and `RECON_READS_ADMITTED` lists ig. ⚠ An ig engine now FAILS CLOSED like the other `Admits` venues: an order on an epic it was never told is denied `symbol-not-admitted` (before, the door refused a foreign epic outright). No measured crate was edited. `docs/ops/trading-any-symbol.md` and the plan's "What ig built" carry the words, the refusals and the STOP-AND-REPORT list.
- **cTrader takes any forex pair quoted in the account's deposit currency (decision 0174's reopener for ctrader; offline-proved, no demo session opened and no order placed on a second symbol yet).** `SYMBOL_RULES` (`crates/vike-tradehub/src/server/symbol_admission.rs`) gives ctrader `Admits(Spelling::BrokerName)` instead of `PerOrderUnsafe`: the symbol is the broker's own name, case kept (`GBPUSD`; the broker's list is case-sensitive, so `gbpusd` is another, unlisted name). The facts are the account's OWN symbol list read with its demo grant (`BridgeTrade::ctrader_symbol_facts`, `CtraderFacts` over `AccountFacts::listed`: one listing session per ten minutes per account, single-flight, a failure remembered; only a `demo` account is looked up for), together with the account's deposit currency, which the handshake already fetched in `ProtoOATrader.deposit_asset_id` and dropped. `crates/bridges/ctrader/src/catalog.rs`'s `any_symbol_refusal` refuses by name everything the engine cannot value: every CFD (what one unit of volume means for it is unmeasured), any quote currency other than the deposit currency (nothing converts a currency, so USDJPY on a USD account would be badly mis-valued), and a symbol with no published minimum volume. `LiveFeeds::Ctrader` is a `QuoteFeed` that hands the door a price watcher (a box that mounts ctrader), behind a `MountScopedSink` that keeps the quotes of any symbol but the mount's out of the mount's bar synthesizer. `CtraderReconClient` reads every admitted symbol on its same three account-wide requests (and fails the pass whole when an admitted name has left the broker's list or the deal list says `has_more`), and `RECON_READS_ADMITTED` lists ctrader. ⚠ A ctrader engine now FAILS CLOSED like the other `Admits` venues: an order on a symbol it was never told about is denied `symbol-not-admitted` (before, a strategy's order on an undeclared symbol was judged on the mounted symbol's grid). Still refused, with the exact blocker for each: `docs/ops/any-symbol-remaining-venues-plan.md`, "What ctrader built". No measured crate was edited.
- **A Hyperliquid API-wallet approval that is ending or gone is now told to the operator, instead of surfacing as per-order rejections.** An agent wallet is approved by its master for at most 180 days; when the approval lapses the exchange rejects every order with `User or API Wallet 0x... does not exist` and nothing else tells you (the testnet approval lapsed 2026-10-10 and was noticed 2026-10-18; for an account that states its master address the boot probe read `missing` as "unanswered" and logged nothing). At a Hyperliquid account's engine start and then every hour the gatewayhub asks the exchange's public `/info` (`extraAgents` of the master and `userRole` of the signer; no key travels, only addresses) and classifies the approval: OK, expires within 14 days, within 3 days, expired or missing. Anything but OK is a `TRADE-AGENT:` `WARN` naming the account, network, master and signer address and the cure (approve the address on `app.hyperliquid.xyz/API`, testnet `app.hyperliquid-testnet.xyz/API`), plus one alert a day per account (and at once when it gets worse) through the store's alert targets (Telegram, the generic webhook). A venue that does not answer is an `info` line, never an alarm. It is a warning only: it runs on its own thread after the start, changes no trading decision and halts nothing. New: `vike_hyperliquid::agent_expiry`, the gatewayhub's `trade/adapters/agent_watch.rs`, and the optional `vike-alerting` edge of `vike-gatewayhub` (behind `trade`). Not in the engine block or `trade status` (no wire field carries it). No measured crate is touched. `docs/ops/trading-any-symbol.md`, "A Hyperliquid API wallet's approval expires, and the gatewayhub tells you".

### Fixed

- **A tradehub boot held by one slow account no longer looks dead to the deploy helper (the v0.1.71 deploy rolled back after 46 s of silence).** The `{"kind":"starting","detail":...}` lines were printed only when an account's state CHANGED, and the helper moves its health deadline only when their count grows, so one account's login that hung for 70 s was read as a dead daemon. While the start runs, the tradehub now repeats the line every 10 s with `; waiting on <account> <N>s` appended (the phase when no account is starting; `StartProgress::heartbeat`, `crates/vike-tradehub/src/server/starting.rs`), until the real server takes the listener over. The line keeps its `"kind":"starting"` shape and there is still exactly one `"kind":"ready"`; the door's own `starting:` answer is unchanged, and the helper is untouched (its stretch window, up to `STARTING_CAP`, now works as designed).
- **A Dukascopy JForex sidecar that never answers its login is killed and started again.** A sidecar that sent neither `ready` nor `fatal` held the account's start for up to 300 s (2026-10-17: the login hung after the SDK's `Authorize task in queue` line; a fresh sidecar 70 s later was ready in 5 s, and 29 other logins since 10-16 were ready 2-3 s after that line). `DukascopyExecutionClient::spawn_with_attempts` (`crates/bridges/dukascopy/src/exec.rs`) now waits 25 s per sidecar, kills AND reaps the silent one before the next starts (never two JForex sessions on one login), and tries up to 3 times (~75 s at most) before the existing `the JForex sidecar would not start` paper answer. A `fatal`, a failed spawn and a child that dies silently are not retried. ⚠ A FRESH platform cache's first login (documented as over 60 s) is now cut at 25 s per try and may never complete; a warm cache (the production box) is unaffected. No Java change: the jar is not rebuilt.
- **After an account was turned off and on in the running tradehub, the control door judged its orders against the STOPPED engine.** The core's engine list only grows, so a restarted account has its old block before the new one under one route key, and the door took the first (`PublisherHandle::engine_blocks` returned the raw list; only the wire frame kept the newest). A symbol the old engine had traded (SOL on `hyperliquid#LIVE`) was planned as a plain placement with no admission, and the new engine, which did not know it, answered `symbol-not-admitted`. `engine_blocks` now carries the newest block of each route key, like the frame, so the symbol, bracket and notional gates all read the running engine. No measured crate is touched.
- **An order the node can never price no longer spends a venue connection on its facts first (ibkr).** The control door looked the new symbol's facts up BEFORE it asked for a price feed, so for a venue this node holds no price watcher for (ibkr today) six distinct tickers in a minute spent the gateway's six data connects an hour (shared with the live data feed's reconnects and the historical fetcher) on orders that were then refused `no price feed`. `SymbolFacts::price_source` (`crates/vike-tradehub/src/server/symbol_admission.rs`) is asked first for an opening order on a new symbol, on a Preview too, and refuses with the same words (`no price feed for ...: this node runs no price feed for <venue>`) before any lookup. Every other check, order and wording is unchanged; a reduce-only order is not touched.
- **cTrader: a deal list that says `has_more` no longer fails every fill pass once a symbol is admitted.** `CtraderReconClient::fetch_fill_reports` pages a truncated list (the next window starts at the last page's newest row, or ends at its oldest when the reply lists newest first, a shared-boundary deal dropped by id) for at most 10 requests, and fails the pass only if it is still truncated. With nothing admitted it reads the first chunk in one request, as before.
- **cTrader, ig and ibkr facts remember a failed fetch for 5 minutes (`SESSION_FAILED_TTL`), not 30 s.** Each of their fetches costs a whole authenticated session or connect (cTrader: application auth plus account auth), so an outage dialled about twenty sessions in ten minutes. The public and per-symbol sources (alpaca included) keep 30 s.
- **IG: the account's currency is read once per facts source, not on every epic.** An epic now costs one market read (`GET /markets/{epic}`) after the first; the 12 lookups a minute bound is unchanged.
- **IBKR: a stock whose contract details carry no size rules at all is refused (`size rules unavailable`) instead of admitted with an unknown minimum.** A usable `sizeIncrement` alone still admits (minimum 0, the step is the floor). The coarsest-band tick rule is unchanged; its sub-dollar rounding is recorded as a doubt in `crates/bridges/vike-ibkr/src/any_symbol.rs`.

## [0.1.71] - 2026-10-18

### Added
- **An account can state its own margin leverage, and its engine applies it only when the exchange confirms it (Hyperliquid; decision 0181, proposed).** `vike-cli secrets account set-leverage --id N --leverage X|none` writes the new optional `account.leverage` column (`NULL` = nothing stated; a store gains the column on its next write, and a read-only reader selects `NULL` without it). At the account's engine start (a boot, or `account.active` off and on, no tradehub restart) the gatewayhub reads the row and asks the exchange what the account may use on the started coin (public `meta` `maxLeverage` and the account's own cross leverage from `activeAssetData`; new `AdapterReply::Live::leverage_cap`, additive on the wire), and the shared tail sets that ONE engine's `im_requirement` to `1 / leverage` only if the answer covers the figure, else the engine stays at 1x with the reason at ERROR. `policy.max_leverage`, a profile's `[risk] max_leverage` and every exposure ceiling (`max_account_exposure`, the account's `max_exposure`) still cap it; another account of the venue is untouched; the adapter sends no `updateLeverage`; default none changes nothing and makes no exchange call. Only hyperliquid accepts a figure (`vike_secrets::LEVERAGE_VENUES`); other venues post a leverage from the run profile and are not changed. No measured crate (`vike-exec`, `vike-core`, `vike-model`, `vike-venues`) is touched, so no latency A/B is owed. `docs/ops/trading-any-symbol.md`, "Trading with leverage on one Hyperliquid account".

### Changed
- **⚠ A link loss on ONE venue no longer halts the other venues: the link dead-man's halt is the dead venue's alone (owner ruling 2026-10-18, decision 0180).** A trip of the CONNECTION-state dead-man (`policy.link_deadman_grace_ms`, on by default) used to set EVERY engine `Halted` and write the process-wide HALT sentinel, so a broken IBKR link stopped bybit. Under the default `deadman_action` it now cancels the open orders of every engine of THAT exchange and halts those engines only (their opening orders are refused, position-covered reduces still pass), writes NO sentinel file, and leaves every other venue trading. The venue is released by itself when none of its links is down, each engine back to the state it held before the halt (a `Reducing` drawdown latch is not lost); an operator's `resume` / `SetTradingState` replaces it, and nothing re-arms over that or over a faulted core. A node-wide `SetTradingState`, the HALT file, the SILENCE dead-man (opt-in), the panic safe state and the drawdown latch are unchanged. A restart keeps a persisted `Halted` (an operator resumes) and a link that is down at boot trips again after the grace. `LinkDeadManConfig::halt_file` is removed (the link switch has no sentinel to write). Measured crate `vike-core` changed (cold path only: a sweep and a control arm); the the latency box latency A/B is owed. `docs/ops/kill-switches.md`, `docs/ops/price-only-venue-safety-plan.md`.
- **The gatewayhub reads ONE symbol's facts where the venue can filter, instead of downloading a whole instrument list.** A binance or aster SPOT symbol is `exchangeInfo?symbol=` (a few hundred bytes; the unfiltered spot list is about 17.6 MB, and the lookup held that venue's one slot for the whole transfer, so every other binance ask waited behind it), every bybit symbol is `instruments-info?symbol=` (linear, then inverse only when linear carries no row), and every alpaca symbol is `GET /v1/assets/{symbol}` with the account's own sandbox credentials (the symbol percent-encoded, so `BTC/USD` is `BTC%2FUSD`; before, the whole active asset list of several MB for any one symbol, through one slot shared by every account). Each symbol is kept ten minutes and single-flighted under its own slot, so a slow read holds up no other symbol or venue; alpaca's state is now per account. The binance and aster USDS-M perpetual lane stays one list (fapi ignores `symbol=`) and is now the perpetual list alone, not spot plus perpetual. An alpaca `404` or non-tradable asset is the named refusal `alpaca: unknown asset <SYMBOL>`; an alpaca account with no complete credentials is refused naming the fields (`CLIENT_ID`, `CLIENT_SECRET`, `ACCOUNT_ID`) instead of "the list came back empty". `crates/vike-gatewayhub/CLAUDE.md`, "The trade venue half (feature `trade`)".

### Fixed
- **A symbol-facts read that half-failed no longer reads as "no such instrument" for ten minutes.** The facts took the picker's catalog, which keeps the rows of whichever endpoint answered when the other failed (binance and aster spot plus futures, bybit's three categories) and returned them as a success cached for `LISTING_TTL`; the lane whose endpoint was down was then answered `no instrument ... in the venue's instrument list`. A failed or half read is now an error (`the venue's instrument list could not be fetched`, transient to the node), remembered for `FAILED_TTL` (30 s), and "no such symbol" is said only after a read that succeeded. The picker's tolerant list is unchanged.

## [0.1.70] - 2026-10-18

### Fixed
- **A `PositionDrift` on a flat, unpriced venue row is held under EVERY reconcile policy; `hybrid` and `synthesize` no longer close it at price `0`.** `resolve` (`crates/vike-exec/src/recon/resolve.rs`'s `unpriced_position_hold`) holds a drift whose venue row has `qty == 0` and no positive average price like `quarantine` does, with the same event-free row and local drop as above, instead of folding legs priced at the row's `0.0` (a fake realized loss of the whole local position; the Alpaca flat row for an absent symbol has this shape). A drift of that shape netted against a missed fill of the same pass is held too, with no event and no drop (its legs are also priced at `0`, and a drop would end at the wrong size): the pass after the missed fill folds re-raises what is left as the plain drop; this also removes the `0`-priced legs from the held-then-confirmed netted row under `quarantine`. A priced row and a non-flat row fold under `hybrid`/`synthesize` exactly as before, and `mode_applies` (the per-kind answer the startup `auto_applied_kinds` line and the docs roster read) is unchanged. Measured crates `vike-exec` (the change) and `vike-core` (its fold test, doc comment): cold path only (a reconcile pass), the the latency box latency A/B is owed. `docs/ops/reconcile-on-restart.md`, "Resolving what the engine holds".
- **The stuck-order ladder now covers a price-only account's engine, not only the node's first engine.** `sweep_stuck_orders` (`crates/vike-core/src/runtime/watchdog/orders.rs`) walked the primary engine's registry alone, so an order of a price-only account (binance, okx, aster, hyperliquid, deribit on a box that does not mount it) that never got a venue answer stayed `Initialized`/`Submitted` for ever unless `config.inflight_confirm_ms` was set, counting against the account's exposure. It now walks the primary and every extra engine whose venue is in `CoreConfig::price_only`, with the same rungs and thresholds (active confirm at `submit_ack_timeout`, the in-flight-confirm guard, the local `OrderRejected` at `submit_ack_timeout + 2 x grace`, the held-event ask-again, the trade-link deferral and resume); the confirm goes to the order's own client and the reject into the order's own engine. A core without `price_only` and the primary engine behave exactly as before, and a further account of a MOUNTED venue (an extra engine outside the price-only set) is still not swept. Measured crate `vike-core` (cold path: the boundary timer, never the fold): the the latency box latency A/B is owed. `docs/ops/price-only-venue-safety-plan.md`, "What stays open".
- **The IBKR cpapi reconcile no longer reads a finished order as a resting one, and no longer understates a cancelled partial fill.** `parse_order_reports` (`crates/bridges/vike-ibkr/src/recon/cpapi.rs`'s `row_status`) reported a row with a missing or unknown status word as resting (`Accepted`, or `PartiallyFilled`), so a fully filled row with no word read as a live order and `recover_orders` could re-register an order that no longer exists, and an unknown word reversed the owner's ruling that an unknown IBKR status is `Rejected`. An unknown word is `Rejected` again (as on the exec path); a row with no word is read off its quantities (filled == total > 0 is `Filled`, nothing remaining is `Canceled`, never `Accepted`). The total is now the largest of `filled + remaining`, the `sizeAndFills` total and `totalSize`, so a partly filled then cancelled row (remaining 0) keeps the order's own quantity instead of the filled part.
- **An IBKR data connect can no longer delay the order lane's reconnect (IBKR socket bridge).** `dial_data_client` held the gateway's turn for the settle wait, the 2 s gap and up to 15 s of open, and on a gateway that takes the socket and never answers repeated data dials each held it, with the exec reconnect (a plain mutex, not FIFO) queued behind whole slots. The data lane now goes through `GatewayGate::dial_data`: it never queues for the turn (refused, nothing dialled, while any dial holds it or an order-lane dial waits), gives way at once to an order-lane dial that arrives, keeps its abandoned open threads apart from the order lane's settle count, spends an hourly budget of its own and backs off 60 s, 2 and 5 min after a data dial the deadline won. Nothing in the tree retries the data door yet (`connect_data_client` has no production caller).
- **The alpaca symbol facts are kept per account.** `AccountFacts` (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`) keyed its listing slot by venue only: an account with no credentials was remembered as `came back empty` for 30 s and a different account asking inside that window was handed the first one's refusal, and the one shared lister was overwritten on every ask, so a fetch could run with the latest asker's credentials. The slot, the remembered failure, the single-flight and the lister are now per account (label and tier); the ten-minute listing keep, the live-tier refusal and the no-credential-in-logs rule are unchanged.
- **A reduce-only order is covered by the venue's reported position only ONCE, and only where the venue can refuse it.** On a flat local book `cover_position_size` let a `reduce_only` order borrow the venue's last-reported net position (reconcile evidence) for the gate's exposure, margin, collar and floor bypasses. It now subtracts the remaining quantity of every other live reduce-only order of that symbol and side first, so two reduce-only SELL 5 orders are not both covered by one +5, and it borrows nothing unless `vike_venues::caps_for(venue).supports_reduce_only` (binance, bybit, okx, aster, deribit, hyperliquid put the flag on the wire; alpaca and ibkr never do, so the order would execute against a flat book and leave the account short, and an unknown venue fails closed). No capability row changed. Measured crate `vike-exec` changed (per-order cold path); the the latency box latency A/B is owed. `docs/ops/reconcile-on-restart.md`.
- **The price-only margin call and drawdown latch never act on a stale or foreign number.** `CoreConfig::price_cfg` is the permissive default on the live core, so a dead pump with the position still marked fed a frozen quote of any age to the sweeps. They now price a price-only engine with every rung capped at the control door's price age (`MAX_PRICE_AGE`) and skip an engine whose open position is older than that (the latch skips its evaluation that sweep, its peak untouched), said once per episode in the recent events instead of every 30 s. A price-only engine on a real network other than its feed's (deribit: testnet exec over the public MAINNET host; Hyperliquid beside a live account) is kept out of the margin call and the latch's curve: the basis was phantom PnL that could set `Reducing` on every engine, mounted venues included. Which host is dialled is unchanged. The CEX venues' mainnet-over-demo feeds are not touched. Mounted venues are byte-identical. Measured crate `vike-core` changed (cold path); the the latency box latency A/B is owed. `docs/ops/price-only-venue-safety-plan.md`.
- **A past symbol admission no longer keeps a price-only venue armed for the link dead-man.** A venue counted as holding something while any symbol had ever been admitted (admissions are never released), so a datahub or gatewayhub outage longer than the grace halted the whole node with no position and no order on it. It now holds only with a live order or a non-flat position, and a `Disconnected` of a venue that holds nothing opens no window at all (the sweep-time discard alone let a window opened while idle trip up to one sweep interval early once an order rested). An outage already in progress when a venue first holds something is still not seen: a window needs a disclosed `Disconnected`. Mounted venues unchanged.
- **A held reconcile divergence that a later pass no longer raises is no longer listed or confirmable.** `hold_recon_alerts` only inserted and refreshed rows, so after pass N omitted `AAPL` (an `OrphanLocalPosition` held) and pass N+1 reported it correctly, `trade recon ls` still listed the phantom and `trade recon confirm` closed the local position while the venue held it (`crates/vike-core/src/runtime/recon_pass.rs`). A delivered pass now removes the held rows of the kinds it can judge and did not raise (position kinds when the pass carried position rows, `BalanceDrift` when it reported a figure under `reconcile_balance`, order kinds always), and the confirm re-validates the row against its account's latest pass before folding. An empty position report, a pass with no balance figure and a failed pass clear nothing; a `MissingFill` row is never cleared by absence (the fill ages out of the lookback while it stays true, and its confirm is idempotent). A stale 0.5 external-position row is now replaced, not kept beside, the 0.9 row a changed size raises.
- **A retried `trade recon confirm <id>` after a restart can no longer confirm a different divergence.** The id was a counter that restarted at 1 with the daemon, so `confirm 3` landed on whatever divergence the new run held as #3. It is now a 48-bit digest of the divergence's identity (venue, account, kind, instrument): the same divergence has the same id after a restart, and an id the node does not hold is refused (a note and a WARN, nothing folded). Wire-compatible (`WireReconAlert::id` is still a `u64`, `ReconConfirm { id }` is unchanged), so no feature word moves; ids are longer numbers (up to 15 digits).
- **The `recon_confirm` audit line now names what was confirmed.** It logged only the verb with an empty coid. It now carries `recon_id`, `recon_account`, `recon_kind`, `recon_detail` (read from the published snapshot at acceptance) and `recon_held` (false when the snapshot no longer held the id), and the core logs its own verdict, `reconcile confirm applied` / `reconcile confirm REFUSED`, with the same id (`vike_core::reconcile`).
- **A `PositionDrift` or `PositionOnlyExternal` whose venue row carries an unusable average price is held under EVERY reconcile policy, flat or not.** The hold above keyed on `qty == 0`, but `synth_position_legs` prices every leg at the row's `avg_px` verbatim: local long `5` at `100` against venue long `3` at `0` sold `2` at `0` (a fake loss of `200`), a sign flip closed and opened at `0`, an external position was adopted at cost basis `0`, and a venue quantity of `1e-12` slipped past `== 0`. `unpriced_position_hold` (`crates/vike-exec/src/recon/resolve.rs`) now holds any such divergence (an `avg_px` that is not a finite positive number, whenever the resolution would price a leg) under `quarantine`, `hybrid`, `synthesize` and `external-quarantine`, proposing no event; a venue quantity within `1e-9` counts as flat. Its confirm: a flat row drops the local remainder at the local average price as before, a row netted against a missed fill acknowledges, and any other row REFUSES (new `ReconAlert::confirm_refused`; the node books nothing, notes the refusal and the next pass raises the row again), so read the position at the venue and fix it there. A row with a real average price folds exactly as before. ⚠ Behaviour changes to know: a held flat-unpriced netted row lingers until a human confirms it or the account changes, and under unattended `synthesize` a phantom position that used to be closed at price `0` now stays held until a human confirms it. Measured crate `vike-exec` (cold path, a reconcile pass) and `vike-core` (the refusal): the the latency box latency A/B is owed. `docs/ops/reconcile-on-restart.md`, "Resolving what the engine holds".
- **The stuck-order ladder skips a STOPPED price-only account's engine.** `stuck_ladder_covers` (`crates/vike-core/src/runtime/watchdog/orders.rs`) did not exclude retired engines as `engines_of_venue` does, so a pre-ack order of a stopped account got a `confirm_order` to a detaching client and a synthesized `OrderRejected` into a dead engine, wrongly terminal if the order is live at the venue. A retired extra engine is now left out of the ladder while the running price-only engines are swept as before. Measured crate `vike-core` (cold path): the the latency box latency A/B is owed.

## [0.1.69] - 2026-10-18

### Fixed
- **`trade recon confirm` on a held `PositionDrift` whose venue row is flat and unpriced no longer books the whole position as a realized loss.** The drift's legs closed the local remainder at the venue row's average price, `0` when the venue reports the symbol flat (the CI box: bybit#DEMO ETHUSDT, local `8.34` at about 2414, venue `0`, held as recon row 8; the orphan sweep never raises it because the venue returns no rows). Such a held row now proposes no fill and carries the `OrphanLocalPosition` drop: the confirm books ONE closing fill at the LOCAL position's own average price, no fee, so PnL, cash and fees do not move, and refuses with a note when the local qty changed since the pass. A venue row with a real average price and a non-flat row resolve as before; `trade recon ls` words the `CONFIRM DOES` column and the detail for the new case. Measured crates `vike-exec` and `vike-core` changed (cold path only); the the latency box latency A/B is owed. `docs/ops/reconcile-on-restart.md`, "Resolving what the engine holds".

## [0.1.68] - 2026-10-18

### Added

- **A price-only venue (binance, okx, aster, hyperliquid, deribit on a box that does not mount it) gets a clock-driven margin call and drawdown latch, and the link dead-man hears it.** No closed bar ever reaches such an engine, and both sweeps ran only from a closed bar, so they never ran there. `vike_core::CoreConfig::price_only` (`PriceOnlyConfig`: the venue set and an interval) arms one more boundary timer, every 30 s, that runs the margin-call sweep for those venues' engines and the account-wide drawdown latch; the per-mount budget latch has nothing to latch (the venue has no mount). The link dead-man's venue list now includes these venues (`crates/vike-tradehub/src/feeds.rs`'s `price_only_venues` is the one set the feed wiring, the dead-man and the core read), and the core acts on one only while it holds an admitted symbol or a live order: a datahub, gatewayhub or pump outage past the grace then cancels that exchange's resting orders and, under the default `deadman_action`, halts the node (the HALT is node-wide, as for a mounted venue); with nothing admitted and nothing resting the window is discarded and nothing trips. Mounted venues are unchanged. ⚠ The stuck-order ladder that rejects an order the venue never acknowledged still covers the node's first engine only. The design and what stays open: `docs/ops/price-only-venue-safety-plan.md`; the measured `vike-core` edit is cold-path only and owes the the latency box latency A/B.

- **A key changed in the database under a RUNNING account now shows in the Trade window and in Telegram `/status`, not only in `vike-cli trade`.** The desktop reads the route keys the observe frame already carries (`WireSnapshot::key_changed`) through a cell of the observe bridge (`BridgeHandle::key_changed`, cleared on a link drop) into `ToolCtx::key_changed`, and the window of that account adds one calm Info line to its status strip, `key changed in the database - the old key is still in use until a restart`, below anything the window refuses and below its own last order; the account keeps trading. Telegram `/status` appends `<route key>: <the same sentence>` per account (`PublisherHandle::key_changed`); no changed key adds no text. No `vike-exec`, `vike-core`, `vike-model` or `vike-venues` edit: `CoreSnapshot` and `NotRunningAccount` are unchanged. `docs/ops/accounts-follow-the-active-flag.md`, "A key changed under a RUNNING account".

- **An operator can now resolve a reconcile divergence the node HOLDS: `vike-cli trade recon ls` and `trade recon confirm <id>`.** Under the default `quarantine` the engine held divergences nothing could resolve (`Command::ConfirmRecon` had no production constructor), so a phantom position stayed in the book, counted against `max_account_exposure` and denied every new order, and a cash break stayed until a restart. `ls` (OBSERVE key, `--node` the observe address, `--json`) lists each held row with its id, account, kind, what a confirm does and the detail; `confirm <id>` (CONTROL key, `--node` the control address, preview, `confirm? [y/N]`, `--yes`, `--reason`) sends the new `WireCommand::ReconConfirm` (feature `recon-verbs`, audited as `recon_confirm`), which folds what the row proposed into ITS account's book. The node publishes the rows in `WireSnapshot::recon`. **An `OrphanLocalPosition` confirm DROPS the local position**: one closing fill at the position's own average price, no fee, so realized PnL and cash do not move (a `0.0` lands in the closed-trade list), refused with a note when the local qty is no longer the one the pass saw. The other kinds fold what they already proposed (the venue's position, the missed fill, the venue's cash figure). There is no `--adopt`/`--drop` choice, and a deposit is still not adopted on its own: that direction is the proposal in decision 0179 (status proposed, nothing built). `docs/ops/reconcile-on-restart.md`, "Resolving what the engine holds".
- **A reduce-only order may close a position the venue holds and the book has not folded.** Each reconcile pass records the venue's net position per symbol on the engine; while the book is FLAT in that symbol, a `reduce_only` order that opposes it and does not exceed it is judged a covered reduce, so `max_account_exposure` (and a `Halted` state) no longer treat it as new exposure. Direction, coverage and the flag all still have to hold. The evidence is at most one pass old, and the venue rejects a reduce-only order on a flat position. ⚠ Measured crates `vike-core` and `vike-exec` changed (a cold per-order path only); the the latency box latency A/B is owed.
- **Alpaca takes any equity or crypto pair its asset list carries (decision 0174's reopener for alpaca; offline-proved, no order placed on a second symbol yet).** `SYMBOL_RULES` (`crates/vike-tradehub/src/server/symbol_admission.rs`) gives alpaca `Admits(Spelling::AssetSymbol)` instead of `PerOrderUnsafe`: the symbol is the venue's own (`AAPL`, `BRK.B`, or a crypto pair with its slash, `BTC/USD`; nothing is translated, and an engine that trades `BTCUSD` is not admitted `BTC/USD`). The facts are the account's OWN asset list (alpaca's list needs a Bearer: `AccountFacts`, one listing per ten minutes, a `live` account refused), a price grid that is not whole cents is refused by name (the adapter writes prices to the cent), `LiveFeeds::Alpaca` is a `QuoteFeed` that hands the door a price watcher (a box that mounts alpaca), `AlpacaReconClient` reads every admitted symbol and `RECON_READS_ADMITTED` lists alpaca. ⚠ An alpaca engine now FAILS CLOSED like the other `Admits` venues: an order on a symbol it was never told about is denied `symbol-not-admitted` (before, it was signed on the mounted symbol's grid). `docs/ops/any-symbol-remaining-venues-plan.md` is the plan for ctrader, ig, fxcm, ibkr, dukascopy (and oanda/polymarket): what blocks each and the order of work.

### Changed
- **The JRE and the FXCM runtime bundle are no longer built and attached by every release; they are built on request by `release-image-assets.yml` (release tooling only; no binary changes; decision 0178, proposed).** `jre-linux-x64.tar.gz` (47 MB) and `fxcm-runtime-linux-x64.tar.gz` (10 MB) have one consumer, the container image (`release-image.yml`, disabled), and were downloaded 0-3 times per release; a deploy installs neither. `gh workflow run release-image-assets.yml --ref main -f tag=vX.Y.Z` builds them for a published release (the JRE through the pinned `deploy/jre/provision-jre.sh`, the bundle around the release's own `libfcshim.so` verified against `SHA256SUMS`) and attaches them with their own manifest `SHA256SUMS.image`; `SHA256SUMS` is never rewritten, and a release published before this change is refused. **`libfcshim.so` stays in every release** (a deployed FXCM box hand-installs it from there). `scripts/fetch_release_tools.sh` reads `SHA256SUMS.image` for an asset the main manifest does not list, and `jre` is no longer a DEFAULT tool (a default fetch of the latest release would die on a tag with no JRE; `deploy/jre/provision-jre.sh` is the release-free route). `release-image.yml` stays disabled and now refuses a tag with no image assets, naming the command. The procedure is `docs/ops/release.md`, "The image assets".

### Fixed
- **The IBKR data lane's own connects are bounded and take the gateway's turn (IBKR socket bridge).** `IbkrFeeds::connect` and `HistoricalFetcher::connect` (the `connect_data_client` path) dialled with no timeout and outside `GatewayGate`, so a gateway that took the socket and never answered hung them (the failure class that held the tradehub boot on 2026-10-17), and they could dial beside the exec connect on a gateway that admits one session. Both now go through `crates/bridges/vike-ibkr/src/market_feed/mod.rs`'s `dial_data_client`: one dial at a time per gateway, 2 s after the previous one, bounded by `OPEN_DEADLINE` into a typed `IbkrError::Connect`, and REFUSED at once (nothing dialled, the watchdog's words) while the gateway's rebuild schedule is armed, without spending the exec's hourly attempts. A data timeout does not arm the schedule; everything else is as before.
- **The cpapi reconcile read the quantity of every live open order as 0 and its status from an empty string.** The captured `GET /iserver/account/orders` row (paper, 2026-10-17) has no `totalSize` and no `status`. `parse_order_reports` (`crates/bridges/vike-ibkr/src/recon/cpapi.rs`) now reads the quantity from `filledQuantity` + `remainingQuantity`, else the `sizeAndFills` total (`"filled/total"`), else `totalSize`, and the status from `order_ccp_status`, else `status`. A listed order whose status word is missing or unknown is reported resting (`Accepted`/`PartiallyFilled`) with a warning instead of `Rejected`. The WebSocket and `/trades` rows are still uncaptured and unchanged.
- **A deribit perpetual the account was not started on is priced even when no mount names deribit.** v0.1.67's control door answered `no price feed for ETH-PERPETUAL: this node runs no price feed for deribit` on a deribit account with no strategy mount: `wire_price_only_feeds` (`crates/vike-tradehub/src/feeds.rs`) built price-only feeds for the CEX venues and Hyperliquid only. It now also wires deribit's (`feeds/deribit.rs`'s `wire_deribit_price_feed`: the strategy-lane dial, nothing subscribed), after the same datahub probe, and a refusal costs only deribit's foreign symbols. The price is a MAINNET reference for a testnet account (deribit's data host is public mainnet, exec signs on the testnet; unchanged). Deribit joins the price-only set (`price_only_venues`), so it gets the clock-driven sweep and the holdings-gated link dead-man like the other price-only venues (hub links only: `price_only_link_disclosure`).

## [0.1.67] - 2026-10-18

### Added
- **Deribit takes any perpetual of the money model its account is mounted on (decision 0174's reopener for deribit).** `SYMBOL_RULES` (`crates/vike-tradehub/src/server/symbol_admission.rs`) gives deribit `Admits(Spelling::PerpetualName)` instead of `MountedOnly`: the symbol is the venue's own perpetual name (`ETH-PERPETUAL`, `ETH_USDC-PERPETUAL`), a dated future, an option and a spot pair are refused by name, and an engine counts money one way, so an account mounted on an inverse perpetual (`BTC-PERPETUAL`, an amount of US dollars) admits other inverse ones and a linear one (`BTC_USDC-PERPETUAL`, an amount of the base coin) other linear ones (`money_model_refusal`; the adapter checks the same rule again). The deribit feed carries a price tap so the door prices an admitted perpetual (`QuoteFeed`, as Hyperliquid's), the gatewayhub's facts for deribit are the TESTNET book's futures rows (the grid the adapter signs on), the label lookups use the instrument's own settlement currency (a mount on a USDC instrument goes from refused to working), and `RECON_READS_ADMITTED` lists deribit. One account holding both money models stays out (`docs/ops/deribit-any-symbol-plan.md`). ⚠ A deribit engine now FAILS CLOSED on symbols like the other `Admits` venues (`admitting_venues`): an order on a symbol it was never told about is denied `symbol-not-admitted` (before, a strategy's order on an undeclared foreign symbol was signed on the MOUNTED instrument). `deribit_any_symbol_testnet_drill` is an ignored testnet drill (far orders, cancelled in the same run).

### Fixed
- **A gateway that lost its link to IBKR is declared dead after 5 minutes and rebuilt slowly, one connection at a time (IBKR socket bridge).** IB Gateway fell twice on 2026-10-17 with its API port still answering, and the daemon's port probe saw nothing. The exec connect now binds `ibapi`'s notice stream before it connects (the connectivity codes reach no other stream) and `crates/bridges/vike-ibkr/src/transport/link_watch.rs`'s `LinkMonitor` calls the link LOST when the server link (1100/2110, not restored by 1101/1102), the `ibapi` socket or a snapshot request has been broken for 5 minutes; a market data or sec-def farm down alone never does (the CI box: `[2103]` broken 136 s with the server link fine; a nightly 1100-to-1102 window ran 94 s). The existing stream-death path then refuses orders with a terminal reject and the daemon rebuilds, but `crates/bridges/vike-ibkr/src/transport/gateway_gate.rs`'s `GatewayGate` refuses the exec connect (nothing dialled) until about 60 s after the loss, then 2 and 5 minutes, at most 6 attempts an hour, and says `IB Gateway has no link to IBKR for N min: restart vike-ibkr-gateway (operator)` after three failures, at most every 30 minutes (the code never restarts the gateway). Every open step (exec, grid pre-fetch, reconcile; the start now dials exec first) takes one turn per gateway, 2 s after the previous, and waits for an abandoned open or a closing pump so a client id is not asked for twice; the pump closes the `ibapi` client when it ends. No setting, no variable. Not run against a real gateway.
- **A symbol an earlier run admitted gets its price lane and mark again after a restart, and the door asks the price lane for a symbol the engine already trades.** On v0.1.66 a restored flat ETHUSDT on the CI box was refused for ever for want of a reference price: marks are never journaled, and only a position or a live order made a symbol a boot re-admission target. Now every on-demand admitted symbol is a target, and an order on a symbol the engine already trades asks the price lane again and, if opening, waits for its first price. The detail is `crates/vike-tradehub/CLAUDE.md`'s account of the control door.
- **The control door's notional ceiling sizes a foreign symbol with its own contract value.** The engine's published grid has no row for a symbol it was never told about, so an INVERSE foreign symbol (an amount of US dollars) was sized as `qty * price`, refused by a ceiling it is under and never admitted. `ControlLimits::vet_notional` takes the value from the symbol's facts (`foreign_contract_value`; only asked when a ceiling is set), on the send, the preview and Telegram's preview.
- **A Hyperliquid unified (or portfolio-margin) account no longer reads as zero equity.** The reconcile balance was always the perp `clearinghouseState.marginSummary.accountValue`, which is 0 on an account whose collateral is its SPOT USDC, so the risk gate denied every order as `insufficient-margin` on a funded account. `HyperliquidReconClient::fetch_balance` (`crates/bridges/hyperliquid/src/exec/recon_client.rs`) now reads `userAbstraction` (cached five minutes) and, for `unifiedAccount` / `portfolioMargin`, takes the USDC `total` of `spotClearinghouseState`; every other account keeps the perp account value unchanged. A failed abstraction or spot read fails the balance (reported, never read as 0), and an unknown abstraction word reads perp-only with a warning. The network stays the account tier's (`live` mainnet, below it testnet). Funds deposited after the first sync reach the engine only on a re-mount or restart (later moves are held as `BalanceDrift`).

### Changed
- **A deploy is always a manual decision: `deploy.yml` is `workflow_dispatch` only (release tooling only; no binary changes).** The `workflow_run` arm (and its tag-run job `if:`) is gone, so a release going green deploys nothing by itself and the workflow stays enabled with no enable/disable around a deploy; deploy or roll back with `gh workflow run deploy.yml -f tag=vX.Y.Z` (`docs/ops/release.md`). `release_checkout_ref_gate` refuses a `workflow_run`, `push` or `schedule` trigger there. `mirror-publish.yml` and `release-image.yml` still follow a tag run.

## [0.1.66] - 2026-10-17

### Fixed
- **The IBKR Client Portal backend's open-order resync is now pinned to the shape the live gateway sends.** A paper-account capture (2026-10-17) showed a `GET /iserver/account/orders` row carries the client order id as `order_ref` and has no `cOID` key; `row_client_order_id` (`crates/bridges/vike-ibkr/src/transport/cpapi/decode.rs`) already reads `order_ref` first (with `cOID` and `local_order_id` as fallbacks), and a test built from the captured row now proves an order is bound back to its coid after a restart. The `/trades` and WebSocket row spellings remain uncaptured.
- **A symbol the account was not started on is priced on binance, okx, aster and hyperliquid even when no mount names that venue.** The control door's price handle existed only for a MOUNTED venue (`live_runtime.rs`'s feed loop), so a box that mounts bybit alone (the CI box) refused every foreign symbol of the others with `this node runs no price feed for <venue>`. `wire_price_only_feeds` (`crates/vike-tradehub/src/feeds.rs`) wires a price-only feed (the strategy-lane dial, nothing subscribed) for each admitting venue no mount names, after asking the datahub that it advertises a strategy lane for it; a refusal costs only that venue's foreign symbols. Hyperliquid's is ONE feed on `feed_tier`'s network (mainnet when a `live` account is armed), and a `live` Hyperliquid account is not hot-started beside a feed wired for the test network (`hyperliquid_feed_refusal`: NOT RUNNING, `restart required to start`, until the next restart), so it is never priced off the test book.

### Changed
- **The release is two phases: PREPARE the merged candidate before the owner's word, PUBLISH on the tag (release tooling only; no binary changes; decision 0176).** `just release-prepare <the merge commit>` dispatches `release.yml` with a new `sha` input, which runs the whole net (validate, every suite, `datahub-all`, `tradehub-all`, the API reference, the three asset builds) on that commit and ends in a new `seal` job: a `PREPARED.json` record beside the hand-off directory on the CI box and the commit status `release/prepared`. The tag run's `preflight` then looks for a valid prepare for the tagged commit on GitHub's side (a green `success` status younger than 2 days naming a green dispatch run started for exactly that sha, and the tag equal to `v` plus the version line); when it finds one the lanes are skipped and `publish` verifies the sealed record against the box and attaches those bytes without building anything, and when it finds none (no prepare, an older one, another commit, or the new `full` dispatch input) the lanes run in the tag run as before and `publish` goes through the same verification. A prepare that exists but disagrees with the box turns `publish` red with the command to run the full path. `release.yml`'s token is read-only except `seal` (`statuses: write`) and `publish` (`contents: write`); `mirror-publish.yml`, `release-image.yml` and `deploy.yml` follow only a tag PUSH run (`workflow_run.event == 'push'` and a `v*` branch). The age of a prepare counts from the PREPARE run's creation (a re-run does not refresh it), only statuses posted by `github-actions[bot]` count, and a fast-path `publish` supersedes the consumed prepare's status with a `failure` so a second run for that sha takes the full pipeline. `just release X.Y.Z [SHA]` tags exactly a prepared commit (an ancestor of `origin/main` with a valid prepare, in a worktree detached at it) when main moved after the PREPARE; without a sha it says whether HEAD was prepared. `just release` also REFUSES a tree `release-prep` did not prepare unless `ALLOW_OLD_FLOW=1` (it used to warn and push a `chore(release)` commit to `main` unprompted).
- **A release is server-only; the desktop app is built on request by its own workflow (release tooling only; no binary changes; decision 0177).** `release.yml` no longer builds, checksums or attaches `vike-desktop` and `vike-desktop.exe`: the job `gui` is gone, the job `windows` cross-builds `vike-cli.exe` alone (it stays in every release: the README, the binstall Windows overrides and the mirror's proof step name it), `validate` no longer runs `cargo test -p vike-desktop`, and `seal` and `publish` carry two lanes (`PREPARED.json` is schema 2, and `publish` refuses another schema, a manifest without `vike-backend`, `vike-cli` or `vike-cli.exe`, or one with a desktop line). The new dispatch-only `release-desktop.yml` (`gh workflow run release-desktop.yml -f tag=vX.Y.Z`) requires the published release, re-validates the crate, builds both desktop binaries in one the CI box job and attaches them with a second manifest `SHA256SUMS.desktop`; `SHA256SUMS` is never rewritten after publication, the desktop assets are not mirrored and the deploy needs no change. `release-image.yml`'s server image downloads `vike-backend` alone, and the client image is built only for a release that carries `vike-desktop` (verified against `SHA256SUMS.desktop`). The README's download paragraph and `docs/ops/release.md` say how to ask for a desktop build.

### Note
- **A price-only venue (binance, okx, aster, hyperliquid on a box that does not mount it) has no closed-bar feed and no link dead-man.** The engine's margin-call sweep, equity-drawdown latch and per-mount budget latch run only on a closed bar of the engine's own series (`crates/vike-core/src/runtime/strategy_drive.rs`'s `drive_strategy`); no tick-driven sweep exists, and no bar reaches an unmounted venue, so those sweeps do not run for it (they never did on such a venue). The link dead-man's venue set is the mounted venues, so the datahub's strat link or a pump dropping there cancels nothing. Mount the venue to get both (`docs/ops/trading-any-symbol.md`).

## [0.1.65] - 2026-10-17

### Fixed
- **The control door asks the engine that RUNS, not the boot's rows, whether a `live` account has an exposure ceiling.** `crates/vike-tradehub/src/tradehub_cli/live_runtime.rs` bound the door's `exposure_cap_armed` to a snapshot of the account rows read at boot, while a hot-started engine (decision 0169) is built from the rows read at its start. An account inactive at boot, or whose ceiling row was set later, was refused every new symbol with "applied at the next restart" (the restart 0169 rules out); an account whose ceiling was removed or loosened and then hot-restarted was still read as capped, so a `live` account could be admitted sixteen symbols with no account ceiling. `account_follow` now hands `LeaseFacts::engine_starting` the ceiling of each engine it is about to build, computed from the very rows the start uses (`rows_arm_a_cap`), BEFORE the engine exists (a start that builds nothing puts the previous answer back), and that answer outranks the boot's closure; a STOPPED account reads no ceiling until its next start, and rows that cannot be read arm nothing (the door refuses).
- **A boot re-admission that missed a symbol heals without a restart, and the cost while it stands is said.** One stalled facts look or `AdmitSymbol` commit put the whole account in the boot window's `missed` set; the symbols that failed were never told to the daemon, so the restore planner HELD their restored resting orders (not adopted, outside HALT's and cancel-all's reach) until an order on the symbol or a restart, with only a per-symbol line. `readmit::readmit_and_price` returns the targets still missing (facts not answered, or admitted with no price feed) and the boot logs them at `error` with the HELD consequence (`readmit::say_missed`), then a `boot-readmit-retry` thread (`readmit::spawn_retry`) asks again on a backoff (5 s, 15 s, 45 s, 2 min, 5 min), after the boot window. Every round is bounded by the boot bound on every gatewayhub round trip, for that thread alone (`LeaseFacts::begin_round`), and the whole schedule has a deadline. The retry is tied to the account's ENGINE GENERATION (`LeaseFacts::generation`, moved by every hot start and stop): a symbol whose engine was stopped or started anew since the boot is dropped untouched, so a retry can never admit a symbol into a fresh engine past the door's gates, and a symbol already admitted gets no second `AdmitSymbol`: its commit to the daemon is retried (`SymbolFacts::tell_daemon`, whose outcome is surfaced) and then its price subscription, and it counts as healed only when both took. It ends when they are healed, the generation moved, the facts registry or the process is gone. Not done here (core edge): the core still gives no per-order view of a HELD order.
- **Stopping an account forgets the symbols the node admitted into its engine.** `LeaseFacts::admitted` was never pruned, so after `account.active` went off and on the new engine (which starts with no admitted symbols) met the old entries: `release` returned early and `price` took the uncounted path past `MAX_WATCHED`, and the pump of such a symbol stayed open for ever. The watcher's stop calls `LeaseFacts::engine_stopped`, which drops the account's entries, hands a symbol no other account of the venue admitted to the idle reaper, moves the account's engine generation and forgets its lease session (nothing is asked of a dead lease).
- **A starting door that fails to start its thread leaves the listener blocking.** `StartingDoor::open` switched the shared listener to non-blocking before spawning its accept thread; a failed spawn left it that way and the real server then accepted on a non-blocking listener. Every error path gives it back blocking, and `ObserveDoor::stop_answering_starting` makes a door-less listener blocking too.
- **The price pump of a symbol the control door gave up on is closed, and the bound is on OPEN pumps.** `LeaseFacts::release` (`crates/vike-tradehub/src/remote_backend/facts.rs`) only forgot its `watched` entry, so every refused order (a 4 s no-price timeout on a quiet symbol) left a live tick pump feeding the price tap and the core's market lane until shutdown, and `MAX_WATCHED` bounded in-flight subscriptions, not pumps. A released symbol that no account admitted now keeps its pump and its place for `IDLE_TTL` (30 s, so the operator's retry finds it), then `LeaseFacts::close_idle_pumps` (a reaper thread every 5 s) closes it through `CexWatcher::close_ticks`; a full set closes its oldest idle pump before it refuses; the pump of an admitted symbol (any account of the venue) is never closed, and a pump is not closed while the venue's own socket has a gap open for the symbol (`PriceTap::gap_open`: the core's link dead-man window would be left without its `Live` and trip `CancelAllAndHalt`). The price tap now answers "none yet" for a price older than 60 s (`MAX_PRICE_AGE`; a dead pump used to leave its last price to mark an order for ever), and the negative cache of refused names is capped at 256.
- **The boot re-admission cannot hold `ready` for minutes on a silent gatewayhub.** Each facts look and admission commit of the restored symbols was a gatewayhub round trip capped only by the lease's 300 s `request_reply`, run serially on the boot thread. Inside a boot window (`LeaseFacts::begin_boot`) they now wait at most `config.gatewayhub_connect_wait_ms`, the bound `start_adapter` already uses (`TradeSession::symbol_facts_within` / `admit_symbol_within`; no new setting); a lease that missed it is not asked again before the node is ready. A symbol whose answer missed is NOT admitted (it lands in the boot re-admission's `failed` list at `error`): the engine keeps the admission and grid the journal restored, its position stays visible, the daemon's reconcile client is not told it so a restored order on it is HELD. As for any symbol whose look failed at boot, it has no fresh facts and no price feed until the next restart.
- **Three operator lines no longer say "no restart is armed".** The deployment-skew, daemon-lacks-the-verb and project-mismatch refusals (`crates/vike-tradehub/src/remote_backend.rs`) say the account is NOT RUNNING (`adapter failed`), what to fix, and that setting `account.active` off and on starts it again (a tradehub restart, where the cure needs one, belongs to the next batched restart). Behaviour is unchanged.
- **An account the watcher starts after boot is started like a boot one: the boot bound on its adapter-start wait, and its lease visible to the control door.** `RemoteBackend::refreshed` (`crates/vike-tradehub/src/remote_backend.rs`) did not carry `adapter_start_bound` or the shared `LeaseFacts` that `view` carries, so a hot-started account waited the lease's 300 s for a silent venue and registered its lease in a throwaway registry, where the control door (`symbol_facts`) and Telegram found "no open trade lease" and refused any symbol but the mounted one. The adapter-start timeout log no longer tells the operator to restart the tradehub: it says the account is NOT RUNNING and that setting `account.active` off and on retries it.
- **After a restart an OKX order resting on an admitted (non-mounted) instrument is no longer retired as "absent at venue" while it still rests.** OKX is an `Admits` venue but its reconcile client reads the mounted `instId` only; the node's lease claimed every admitted symbol it had told the daemon, so the restore step read the report's silence as "gone" and synthesized an `OrderCanceled` for an order that kept resting at OKX, outside the registry where HALT and cancel-all do not reach it. `vike_gatewayhub_client::trade::RECON_READS_ADMITTED` (binance, aster, bybit, hyperliquid) now lists the venues whose daemon client reads what it is told; any other claims nothing, so the restore step HOLDS such an order (`RESTORE ... HELD`), and `OkxReconClient::admitted_symbols` says the same (`Some([])`). The held order is still not adopted: cancel it at OKX by hand (`docs/ops/known-limitations-2026-10.md`).
- **The account exposure ceiling is fail-closed on a book it cannot price.** `max_account_exposure` counted a position or live order with no mark as 0.0, and marks are not restored, so after a restart a live account's admitted-symbol positions counted as nothing until their first quote: with a 10 000 ceiling and 9 000 of ETHUSDT held, a 9 000 BTCUSDT order passed. `ExecutionEngine::resolved_account_exposure_excluding` now answers infinity for such a book and the gate denies every order that is not a covered reduce with `account-exposure-unpriced` until it is priced (closing orders are never held back; the denial lifts on the first quote). Measured crate (`vike-exec`): the change is cold-path only (per order, ceiling armed); the the latency box latency A/B has not been run.
- **A foreign order's cancel and status no longer wait for the venue's grid (bybit, binance perp, okx).** After a restart the router built a client for the order's symbol by fetching `instruments-info` / `exchangeInfo` / `public/instruments` on the exec thread, so with that endpoint down or slow the cancel came back `CancelRejected` while the order kept resting, and a HALT mass-cancel over up to 16 uncached symbols queued behind one fetch each. `PerSymbolRest::with_light_factory` gives cancel and status a client built with no network (the full client when it already exists); amend and submit still need the grid. Binance spot and aster keep the old path.
- **A stopped account leaves nothing behind in the running node (decision 0169, slice S4).** Its held reconcile divergences are dropped (no pass would ever clear them, and a confirm would fold into a shut book); a stopped primary engine's seed leaves the capital base when the account starts again (it was counted twice until a process restart); a trade-link dead-man window shared with a running account of the same venue and symbol is forgotten unless that account's own link is down (it expired into a `CancelAllAndHalt` for the survivor); the observe frame carries one block per account, the newest, instead of a restarted account's stopped block ahead of its running one (the Trade window took the first); and the Telegram channel judges a chat order on the current list of accounts that do not run instead of the list the tradehub booted with.
- **Bybit signs an order, cancel, amend and status query on the ORDER's own symbol, on that symbol's own price/qty grid.** An order on a symbol other than the mounted one used to be signed on the mounted symbol and formatted on its grid. The exec thread's client is now a `PerSymbolRest` (`crates/bridges/bybit/src/per_symbol.rs`): the mounted symbol, or none, goes to the mounted client exactly as before; any other symbol gets a client built on first use from its own `instruments-info` row (an unknown symbol, a failed fetch or an inverse/dated contract is a terminal `OrderRejected` with the reason, never a guessed grid). Cancel, amend and status find the symbol from a coid map filled at submit; a coid the process never submitted (reconcile-adopted, or from before a restart) is still looked up on the mounted symbol. The order-rate window is shared by every symbol's client. Leverage is not posted for the new symbol.
- **Binance and Aster sign an order, cancel, modify and status query on the ORDER's own symbol, on that symbol's own grid.** The same fix for both Binance-grammar venues, through one `PerSymbolRest` (`crates/bridges/binance/src/per_symbol.rs`, used by Aster too) and each venue's `fetch_*_properties`. The engine's own lane (spot or USDⓈ-M perp) takes any symbol of that lane; an order on the OTHER lane is refused with a terminal `OrderRejected`, because only the mounted lane has a user-data stream and its fills would never arrive. The same limits as Bybit: a never-seen coid falls back to the mounted symbol, and leverage is not posted for the new symbol.
- **A plain order on a symbol the account was not started on is placed on THAT symbol: the control door admits the symbol into the account's engine first (bybit, binance, aster; decision 0173).** `trade order submit bybit ETHUSDT …` against a bybit account started on `BTCUSDT` used to be Acked and signed on `BTCUSDT`. `accept_command` (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `plan_submit_symbol`; the TCP command, both previews and the Telegram channel) now spells the symbol the venue's way (upper case; binance and aster keep the `.P` lane suffix, bybit's linear-only plane drops it), asks the account's own gatewayhub lease for the symbol's tick, lot and minimums (`TradeUp::SymbolFacts`, from the venue's public instrument list, cached an hour), and sends `Command::AdmitSymbol` then the order on the one command lane; a fill of that order folds into that symbol's position. A binance or aster engine is one lane (spot or USDⓈ-M perp), so a symbol of the other lane is refused by name. A stale admission is refreshed on the next order once it is old enough, a Preview reads and subscribes nothing, the Telegram channel never waits for a price, and the number of price subscriptions is bounded. The price the door saw is seeded into the journaled `AdmitSymbol` as the engine's first mark, so a limit order at many times the market is collared on a new symbol too and a limit order is never judged against its own price; a reduce-only first order, an order below the venue's lot, quantity or minimum notional, and (on a `live` account) any new symbol without an account-wide exposure ceiling (`policy.max_account_exposure` or the account's `max_exposure`, both unset by default) are refused before a slot is spent. A Preview that would look a symbol up takes a rate token. The node remembers (30 s) only the gatewayhub's DEFINITIVE refusal of a symbol (unknown, no tick size, not linear); a lost link, a timeout, a busy or restarting gatewayhub and a failed instrument-list fetch are never remembered by the node, so the retry asks at once (the gatewayhub itself keeps a failed fetch for 30 s per venue). Refused with the cause, nothing sent: a symbol the venue does not list, a failed lookup, a grid without a tick or lot, a paper account (one book), a 17th extra symbol, a surface with no lease to ask, and a symbol no price arrives for (the door subscribes the symbol's quote feed on its first order and holds an OPENING order up to 4 s for the first price; a reduce-only order is never held). The engine itself now FAILS CLOSED for these venues (`RiskLimits::deny_unadmitted_symbols`, set from `AdapterPolicy::fail_closed_symbol_venues`): an order on a symbol it was never told about is denied `symbol-not-admitted`, never rounded onto the mounted grid and signed; an admitted symbol's row is its WHOLE grid (no inherited min qty or min notional); a restart restores the admitted symbols; at most 16 extra symbols per account, admitted lazily on their first order. Deribit signs the mounted symbol whatever the order names, so it keeps the refusal, with the reason in `SYMBOL_RULES` (OKX is opened by its own bullet below); the per-order venues are fenced by the bullet below. On boot the node refreshes every ADMITTED symbol a restored position or live order holds (fresh facts plus a price subscription; the engine itself already restored the admission), before the reconcile driver starts (`crates/vike-tradehub/src/readmit.rs`).
- **A LABELLED account (`bybit#DEMO`, `binance#ALT`) trades a symbol it was not started on, priced like the default account.** The core handed a venue's quote, trade and book to the venue's default account's engine only, so a labelled engine admitted a symbol (`Command::AdmitSymbol`) held it with no price: a market order was denied `no-reference-price`, a limit order skipped the price collar and the position was never priced in equity, which is why the door refused a labelled account a new symbol by name. `CoreThread::admitted_fanout` (`crates/vike-core/src/runtime/routing.rs`'s `rebuild_admitted_fanout`) now lists the labelled engines that hold an admitted symbol, rebuilt only on the cold paths (assembly, so a restart or a replay lists them with no command, and an `AdmitSymbol` that admitted something); the quote, trade and book arms do the existing route unchanged, then write the same price-board slot of the listed engines that accept the symbol, so the default engine is never written twice and a core that admitted nothing into a labelled account pays one `is_empty()` per message. The door (`symbol_admission.rs`) admits a labelled account under its own route key, and after the admission is on the lane it holds an opening order until a quote noted after it has been seen (`SymbolFacts::engine_priced`; the earlier 4 s wait proved only the NODE had a price, taken before the engine accepted the symbol, so the first order could still find the engine unpriced). A declared leg or a combo leg keeps the lanes it had. Measured crate (`vike-core`): the the latency box latency A/B has not been run; the latency harness has a non-gating labelled fan-out variant for it.
- **Binance and Aster fills of a symbol other than the mounted one carry a symbol-qualified trade id** (`SYMBOL:t`). `t` is a per-symbol sequence on this grammar and the engine's fill ledger keys on the bare id, so once one account trades two symbols a genuine second fill could be dropped as a re-delivery. A fill on the mounted symbol keeps the bare `t` it always had (resync, reconcile and restored ledgers spell it that way). The coid-to-symbol map is never cleared and is taught by `adopt_restored`, a new symbol keeps the leverage the venue holds for it (posting ours would also run after a restart and move an existing position's liquidation price), and a failed grid fetch is remembered for 30 s. LOUD known limit: after a restart a RESTING order on an admitted symbol is not adopted (reconcile reads the mounted symbol only), so vike's cancel, mass-cancel and panic button do not reach it and it stays live on the venue (`docs/ops/known-limitations-2026-10.md`). Also: a foreign fill or terminal lost in a user-data gap is not recovered (history, resync and reconcile read the mounted symbol only), and a coid this process never saw nor was told of cancels on the mounted symbol.
- **The control door fences a foreign symbol on the per-order venues until admission supports them.** A plain Submit on a symbol other than the engine's own passed untouched on every per-order venue, though the engine judges it on the MOUNTED symbol's grid, multiplier and price (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `SYMBOL_RULES`). Now: a PAPER account is refused on oanda and the unsafe venues below (it is one book: the fill was booked on the mounted symbol); an OPENING order with no limit price is refused on oanda (no price watcher exists there, so it was judged at notional 0 and passed every ceiling; reduce-only is untouched); and the new `SymbolRule::PerOrderUnsafe` row refuses any foreign symbol on ibkr (multiplier and lot unknown), ctrader and alpaca (the engine rounds on the mounted symbol's grid), ig, fxcm and dukascopy (no price source). The engine's own symbol, no symbol, a declared leg and polymarket (whose engine judges every token) behave as before; the Trade window already kept these venues to one symbol. (Hyperliquid started on this row and now admits: see the entry below.)
- **After a restart, a resting order on an admitted (non-mounted) symbol of a bybit, binance or aster account is found by reconcile and adopted.** The reconcile client of each venue (`BybitReconClient`, `FamilyReconClient`) read the mounted symbol only, so the restore step, which takes the scope of a pass from the engine's symbols, retired a restored order on an admitted symbol as gone while it rested on the venue, out of reach of cancel, mass-cancel and the panic button. `ReconClient::admit_symbol` (default no-op, with `vike_exec::recon::AdmittedSymbols`, bounded by `MAX_EXTRA_SYMBOLS`) now tells the client which symbols the engine admitted, and each pass reads orders, positions and fills of every one with the mounted symbol's request shape, failing the whole fetch if one read fails. The node commits each admission to the gatewayhub after sending `AdmitSymbol` (`TradeUp::SymbolFacts` with `probe` false; a look, such as a Preview, never takes a slot), and the gatewayhub queues it on the account's reconcile worker ahead of the reply, so the boot re-admission (`crates/vike-tradehub/src/readmit.rs`) reaches the client before the first pass. A delisted admitted symbol reads as no rows and a rate limit pauses the pass for a minute. The residuals are `docs/ops/known-limitations-2026-10.md`'s.
- **OKX signs an order, cancel, amend and status query on the ORDER's own instrument, with that instrument's grid AND contract size, and converts its fills with the same contract size.** OKX sizes in contracts (`sz` = base / `ctVal`), so an order for another instrument signed on the mounted one would have been mis-sized by the ratio of the two `ctVal`s. The exec thread's client is now a `PerSymbolRest` (`crates/bridges/okx/src/per_symbol.rs`): the mounted `instId`, or none, goes to the mounted client exactly as before; any other `instId` gets a client built on first use from its own `public/instruments` SWAP row (own tick/lot/`ctVal`, own order-rate window). No row (unknown, not a SWAP, unreachable) or an inverse swap is a terminal `OrderRejected` with the reason; OKX still trades SWAP only. The private WS converts each fill with the `ctVal` of the instrument it names, loading the row once for an instrument with no client; a fill whose `ctVal` cannot be read is dropped with an error log rather than sized with another contract's. The same limits as Bybit: a never-seen coid falls back to the mounted instrument, leverage is not posted for the new instrument, and the resync/sentinel history replay covers the mounted instrument only.
- **OKX: what an instrument other than the mounted one needs to be safe to admit.** `adopt_restored` teaches the router which instrument each restored order rests on (the core announces them at boot and after a gatewayhub restart), so a cancel of one no longer goes to the mounted instrument, where OKX answers `51401` and the cancel reads "already gone" over a live order; the router's coid-to-instrument map is never cleared (it was cleared at 4096 entries, which sent an old foreign order's cancel to the mounted instrument). A foreign instrument's row read (the client factory's, and the `ctVal` load of a fill with no client, on the user-data pump) gives up after 5 s instead of 30 s and an unreadable instrument is remembered for 30 s, so a bad instrument stalls the exec thread and the pump once, not once per order or fill. A fill of any instrument but the mounted one carries the trade id `INSTID:tradeId` (the mounted one stays bare), and an instrument whose `ctMult` is not 1 is refused. The gatewayhub answers symbol facts for OKX (a linear USDT-settled SWAP, from a SWAP-only listing; spot, dated, option, inverse and USDC-settled rows are refused with a reason) with the grid converted from contracts to base units exactly as the mounted path does.
- **IBKR cpapi: the open-orders resync after an unknown place now finds the order.** The resync read the coid of a `GET /iserver/account/orders` row as `cOID`, while the Client Portal docs (secondary sources, not a capture) name it `order_ref`, so the one resync after an ambiguous place most likely found nothing and the stuck-order watchdog rejected a possibly live order. One reader, `row_client_order_id` (first non-empty of `order_ref`, `cOID`, `local_order_id`), now serves the exec resync, the executions rows and the reconcile parser. A first EMPTY answer (the first call of a session can be) earns one `?force=true` read and one more plain read, at most three, 250 ms apart. A live paper check of the field name is owed.
- **OANDA and Alpaca crypto: a fill books the position of the symbol the order was submitted under.** An order sent as `EURUSD` (OANDA) or `BTCUSD` (Alpaca) goes out as `EUR_USD` / `BTC/USD` and its fills came back in that spelling; the engine compares symbols by exact string and silently dropped the bare `Fill`, so the order read Filled and the position stayed flat. Each bridge now records the symbol at submit (and learns a restored order's from `adopt_restored`), shares it with its stream and confirm threads, and stamps every fill with it; an order submitted as `EUR_USD` / `BTC/USD` fills under that spelling, and a coid nobody recorded keeps the venue's. Seen by reading, not on the wire.
- **A restored order on an admitted symbol is never retired for a symbol the reconcile client was not told about, and a restarted gatewayhub is told the admitted symbols again.** The restore step took the scope of a pass from the engine's symbols, so a symbol whose facts the lease could not answer at boot (or a gatewayhub that restarted alone, whose new daemon knows nothing) made the venue's silence read as "gone": the mount heard `Canceled`, the ownership entry was forgotten and the order kept resting. Each pass now carries the admitted symbols its client had been told (`ReconClient::admitted_symbols`, stamped by the manager as the list held both before and after the fetch; `ReconcileReports::admitted_symbols`, additive in the journal, `None` for a client that keeps no list); the lease records a symbol when the daemon grants its COMMIT (`TradeSession::admit_symbol`, sent by `LeaseFacts::admitted`; a Preview's look is never claimed) and forgets them all at a new `gw_epoch` (`TradeSession::told_symbols`). On-demand symbols the pass did not read are HELD (neither adopted nor declared gone, one `RESTORE ... HELD` note per order) until a pass whose client was told judges them. The re-teaching worker (`crates/vike-tradehub/src/remote_backend/reteach.rs`) asks the new daemon for every symbol the old one was told (`TradeSession::retell_symbols`, at most `MAX_EXTRA_SYMBOLS`) before the reconcile pass that follows a restart, asks again on a capped backoff for a refusal, and the node's facts cache no longer answers for a symbol the daemon was not told.
- **IBKR: every network step of the adapter open is bounded.** The ibapi `connect`, handshake, `next_valid_order_id`, `managed_accounts` and `contract_details` calls wait with no timeout, so an IB Gateway that had lost its link to IBKR's servers held the open for ever (the hung `TRADE-OPEN` of the failed 2026-10-17 deploy). The exec connect now times out into `IbkrError::Connect` (the account is `adapter failed`); the reconcile and grid pre-fetch reads time out into their existing `None`.
- **One silent venue no longer holds the serial boot of the tradehub.** The boot starts the accounts one after another, and `RemoteBackend::start_adapter` waited for the gatewayhub's `AdapterStart` answer without the boot's bound, so a venue that answered nothing (ibkr's IB Gateway without its broker link) kept the control door from listening for minutes. The wait is now at most `config.gatewayhub_connect_wait_ms` (never more than the lease's `request_reply`; `TradeSession::start_adapter_within` takes the bound). A timeout detaches the lease and makes that one account NOT RUNNING with `adapter failed`, not `gatewayhub unavailable`, so the late arm does not restart the tradehub into the same silent venue; the other accounts start without it. Fix the venue's side, then set the account's `account.active` off and on to retry it.
- **The tradehub's node door listens before the engines start and answers `starting`.** The serial engine start can take minutes (and a silent venue used to hold it), during which the control and observe port was closed and the deploy helper could not tell a slow start from a dead one. The live arm now opens the listener first (`hub_server.rs`'s `open_observe_door`): every authenticated verb, read or command, is answered `starting: <phase; accounts up / starting / NOT RUNNING>` and the connection closes, nothing reaches the core (which does not exist yet), and the real server takes the same listener over once the mount is up. The stdout ready line is still printed only after that, and the deploy helper keys on the journal ready line, so an open port cannot read as ready. Each start step is also a `starting` line on stdout. `crates/vike-tradehub/CLAUDE.md` has the phases.

### Added
- **A key changed in the database under a RUNNING account is a warning on that account, not a restart (slice S4).** A restart cancels the account's resting orders, so it stays the operator's decision in his next batched restart; the account keeps its session and its old key, and shows `key changed in the database - the old key is still in use until a restart` until it starts again with the new one. The gatewayhub, the one process that reads key values, records a one-way keyed fingerprint (`vike_secrets::account_credentials::CredentialFingerprint`: no value, no `Display`, redacted `Debug`) of the key set each adapter started with and names the route keys whose key in the store differs now in the arming table (`ArmingTable::key_changed`, additive); on every change of the rows the account watcher asks, and lists them in the observe frame (`WireSnapshot::key_changed`, route keys only), in `vike-cli trade`'s equity view and in one `ARMING:` log line. The Trade window does not show it yet. `docs/ops/accounts-follow-the-active-flag.md`, "A key changed under a RUNNING account".
- **An account's engine can take a second symbol on demand (`Command::AdmitSymbol`, decision 0173, proposed).** The command carries the symbol's own grid and contract value into one account's engine (addressed by route key, journaled, replay-safe; journal format v19); the engine then snaps an order on THAT symbol's tick and lot, folds its fills into THAT symbol's position and publishes it as tradable (`VenueBlock::trades`), where before a foreign-symbol order was judged on the mounted symbol's grid. A grid without a tick or lot, an empty symbol and a 17th extra symbol are refused by name; the price collar rounds a trigger on the order symbol's tick; a market order on an admitted symbol with no reference price is denied `no-reference-price`. The node's control door sends it (below); the engine does not itself refuse an unadmitted foreign symbol, so every path that never sends it behaves as before.
- **The gatewayhub answers the venue facts of any symbol over a trade lease (`TradeUp::SymbolFacts`, feature `gw_trade_symbol_facts`).** A tradehub node holding only the trade key asks `TradeSession::symbol_facts` for a symbol's grid, minimums and contract value, so it can admit that symbol (`Command::AdmitSymbol`) without a read key; bybit (linear perpetuals), binance and aster (spot, and `.P` USDS-M perpetuals) and okx (linear USDT-settled SWAPs, the grid in base units) answer from the public instrument list kept ten minutes per venue, any other venue answers a reason, and an inverse perpetual is refused. `Scope::Trade` is not widened.
- **A `paper` account starts, and a tier changed under a running account restarts it, in the running tradehub (decision 0169, slice S4).** A `paper` account joins the core as the boot's simulator engine and needs neither the gatewayhub nor a mount that ever connected to it. A running account whose row states another tier (`set-tier`, or the last `demo`/`live` row deactivated beside an active `paper` one) is stopped as a deactivation is and started at the tier the row states in the same pass, listed `tier changed, restarting at <tier>` between the two; a refused start leaves it NOT RUNNING with the reason. A tradehub that booted with reconciliation off because nothing was armed starts the reconcile driver itself, with the boot's own settings, when the first `demo` or `live` account with a reconcile client starts (`flags.reconcile_off` still refuses it). The account-administration verbs wake the watcher at once. Still a restart: a venue whose price feed was not wired at boot, a deleted row, a changed credential (`docs/ops/accounts-follow-the-active-flag.md`).
- **Hyperliquid takes ANY core perp of the exchange on a `demo` or `live` account (decision 0174; the held "any symbol" row, flipped).** The control door's `SYMBOL_RULES` row moves from `PerOrder` to `Admits(Spelling::CaseKept)` and the engine fails closed for it like the other venues (an order on a coin it was never told about is denied `symbol-not-admitted`; `admitting_venues()`, the published `admits_symbols` and the Trade window follow the same table). The coin is spelled as the exchange lists it, case kept (`kPEPE`, never upper-cased; `BTC.P`, a spot pair `BASE/QUOTE` or `@N` and a builder-dex `dex:COIN` are refused by name before any lookup). `TradeUp::SymbolFacts` answers `hyperliquid` from the public catalog by the coin's EXACT name (linear, contract value 1.0) and refuses a delisted coin and an isolated-only one (`onlyIsolated`, or a `marginMode` of `strictIsolated` / `noCross`, read into a separate `no_cross` flag that only the catalog row's venue word uses: the margin mode of an already-mounted coin is unchanged, `only_isolated` still reads `onlyIsolated` alone). The venue's feed hands the door a price handle (`QuoteFeed`: a quote subscription on the existing dial, where a CEX venue's is the tick pump), so an opening order has a price to be judged on; like the CEX handle it is closed when idle: `QuoteShared::close` and `LeaseFacts::close_idle_pumps` close the lane of a symbol no account admitted `IDLE_TTL` after the door let go of it (an admitted symbol's lane is not closed). The rules every `Admits` venue shares apply unchanged: a `live` account is refused a new coin until `policy.max_account_exposure` (or the account's own ceiling) is armed, a reduce-only first order and an order under the coin's lot or minimum notional burn no slot, the door's price is the engine's first mark, a labelled account is admitted under its own route key, and the boot re-admits a restored coin. New in this flip: a hyperliquid engine mounted on a spot pair or a dex coin takes no core perp (one product per engine), and `RECON_READS_ADMITTED` lists hyperliquid because its reads span the whole address, so a restart judges a resting order on an admitted coin by the report instead of holding it. The adapter needed nothing for signing: it already builds an order from `req.symbol` through the loaded `Symbology` (asset index, `szDecimals`, five-significant-figure price clamp; a new test places a second coin and cancels it by the same asset), and a restored coin's cancel is taught by `adopt_restored`. Not covered, and said by the facts verb or listed in `docs/ops/known-limitations-2026-10.md`: isolated-only coins, HIP-3, spot, `updateLeverage` (a coin keeps the leverage the account holds), the facts being the MAINNET universe whatever the account's network, and two accounts of one network sharing one address. ⚠ No order has yet been placed on a second coin on any network: the first one on the testnet `demo` account is the proof owed before a `live` account is armed for it.

### Changed
- **The release is faster and no longer starts a duplicate CI run (release tooling only; no binary changes).** `just release-prep X.Y.Z` now cuts the changelog and bumps the version lines inside the last candidate PR, and `just release X.Y.Z` only verifies that tree (`scripts/release_verify.sh`) and tags it, instead of pushing a `chore(release)` commit to `main` that started a full second `ci` run and missed every crate's compile cache; the old flow remains as a loud fallback, and `just release` refuses a `[skip ci]` HEAD (`ALLOW_SKIP_CI_TAG=1` overrides) and a tree whose `[Unreleased]` still holds entries after the section is cut (`ALLOW_UNRELEASED_ENTRIES=1` overrides). `datahub-all` and `tradehub-all` run under `cargo nextest run --profile ci` with a `cargo test --doc` line each, in `ci.yml`, `release.yml` and the `just` recipes; `validate` runs on the `ci-test` runners; a red first attempt of a release is re-run once by `release-rerun.yml`; the image selftest copies a fixture tree instead of 86 MB of `crates/` and the queue selftest has a longer nextest ceiling. The `allow_empty_changelog` dispatch input is gone. The procedure is `docs/ops/release.md`.
- **⚠ ROLLBACK WARNING: the first order on another symbol makes the command journal unreadable to v0.1.64.** The command journal is format v19 from this release, but a journal is stamped 19 only when the first `AdmitSymbol` is written, not when the node starts (`CommandJournal::open` leaves a v18 header at 18; `append_cmd` raises it just before the first admission). So a deploy of this release that is rolled back before any second symbol was traded (or before a boot re-admitted a symbol a restored position holds) goes back to v0.1.64 as it is. After the first admission the v0.1.64 binary refuses the journal directory at start (`journal format version mismatch: got 19`), exits 1 and loops under systemd, and the deploy helper's automatic rollback ends incomplete with the tradehub down. Then: stop `vike-tradehub` (and `vike-datahub` where the journal materializer follows the directory), `mv "$J" "$J.v18"` with `$J` the `config.journal_dir` row, start the old build, and reconcile the account by hand (`docs/ops/tradehub-the CI box.md`, section 6, has the one-line version check). The deploy helper does not touch the journal.
- **The deploy helper no longer rolls back a tradehub that is slowly but visibly STARTING.** `healthy()` gave every unit a flat 45 s for its ready line, so one slow venue condemned v0.1.64's deploy. For `vike-tradehub.service` on the forward pass only, each new `{"kind":"starting",...}` journal line (printed by a tradehub from v0.1.65 on) now moves the deadline to now + 45 s, capped at 300 s (`STARTING_CAP`); a silent, inactive or older tradehub, an unreadable journal, every other unit and every rollback keep the plain 45 s. ⚠ `deploy/sbin/vike-trader-ci-deploy` is installed by hand: reinstall it before the deploy that needs it (`docs/ops/tradehub-the CI box.md`).
- **The Trade window trades the symbol you PICK, not only the one the account is mounted on.** A demo or live account of binance, bybit or aster takes an order on any symbol that venue's catalog lists (spot and perpetual alike; the order carries the picked symbol and account). A paper account, deribit, an older node that names no tier and every account whose node does not publish `admits_symbols` (next entry) keep the one symbol they were started on, and the window says why; a symbol the catalog does not list is refused with that cause. TP/SL stays on the engine's own symbol. The node's refusal of an order (for example "this node's engine trades X only") now appears on the sending window's strip in the node's own words instead of only in the clipped control segment.
- **The desktop sends a symbol an account was not started on only to a node that SAYS it admits it.** The node publishes one more key on each venue block, `admits_symbols` (`vike_exec::VenueBlock`, `WireVenueBlock`; additive: absent from the frame when false, read as false when absent), copied from the engine's fail-closed-on-symbols flag, which the mount sets from the control door's own table (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `SYMBOL_RULES` `Admits` rows: bybit, binance, aster, okx, hyperliquid), so the door, the engine and the capability are one table and `the_published_capability_is_the_doors_admit_set` pins it. The desktop's `symbol_follow` reads the key instead of a venue list. Against a node older than this (v0.1.63 and before) the key is missing, so the Trade window keeps binance, bybit, aster, okx and hyperliquid to the one symbol they were started on and says "this node does not offer to trade another symbol": an older node Acks a foreign symbol and signs it on the mounted symbol, which is the loss this closes. An older desktop ignores the key; a current node still refuses a foreign symbol on deribit, on the fenced per-order venues and on a paper account in the door's own words.
- **Publishing a snapshot for an engine with admitted symbols is about seven times cheaper (decision 0173).** `snapshot::build` allocated two `String`s per position row and per mark, twice for the primary's mirror, and copied the extra-symbol list each publish: about 120 allocations at the 16-symbol cap, a 6 us publish on the CI box against 0.7 us with none. A position and a mark now carry `vike_exec::Interned` (a `Copy` handle on the engine's own interned name that reads, compares and prints as the `String` it replaced), `VenueBlock::extra_symbols` is a shared `SymbolList` the engine rebuilds only when its symbols change, and `PriceBoard::cell` hashes past 8 cells instead of scanning once per open position. The published text is byte-identical (pinned against the `String` builder's); readers that owned a `String` say `to_string()`. the CI box, loaded and not the gate box, medians of 9: `snapshot-build-16sym` 7.7 us to 1.1 us, `submit-hop-16sym` 10.0 us to 2.9 us, the 0-symbol builds 0.75 us to 0.48 us; the the latency box A/B is not run.
- **A demo or live OKX account takes an order on any SWAP instrument, admitted into its engine on the first order (decision 0174).** The control door's table row for okx moves from `MountedOnly` to `Admits` and the engine fails closed on a symbol it was not told about, from the same table. The only spelling is OKX's own SWAP instId (`ETH-USDT-SWAP`, any case): a spot, dated-future or option id is refused by name before any lookup and `BTCUSDT.P` is not translated. The order is signed with that instrument's own grid and contract size, and its facts come from the gatewayhub (a linear USDT-settled SWAP). The Trade window follows the node's `admits_symbols` key, so against a node with this row an OKX account is offered any catalog symbol, and a symbol that is not an OKX SWAP instId is refused by name. Known limits (`docs/ops/known-limitations-2026-10.md`): reconcile and the lost-fill replay read the mounted instrument only, and funding settled on an admitted instrument is not folded.

### Docs
- **Two operator pages for the changes that ship next: `docs/ops/trading-any-symbol.md` (an order on any symbol of an exchange, from the Trade window and from `vike-cli trade order submit`: spelling per venue, which venues are finished and which are refused, the words of every refusal, what a restart does) and `docs/ops/accounts-follow-the-active-flag.md` (turning an account on or off with `account.active` without restarting the tradehub: the two-phase stop, the gates a start crosses, the 5/10/20/30 s retry, what still needs a restart, how to read the NOT RUNNING reasons).**

## [0.1.64] - 2026-10-17

### Fixed
- **Binance and Aster: any 5xx on a write is "outcome unknown", not a reject.** `UreqTransport::run` answered only a gateway 502/503/504/524 with no venue code as `E_TIMEOUT_AMBIGUOUS`; a bare 500, 501, 505 or 520-523 on a POST, PUT or DELETE became `code = status`, and the submit arms rejected terminally an order that may be live. Binance documents every 5xx as "execution status UNKNOWN and could have been a success", so all of them now take the `resolve_ambiguous_submit` re-query (both venues have the by-coid lookup), and so does a 5xx whose body is Binance's `-1000` UNKNOWN or `-1001` DISCONNECTED (they say no more than the status). A 5xx whose body carries any other code (`-1006` and `-1007` are already re-queried by `submit_outcome_unknown`; `-1016`, `-2010` and the rest still reject), a read, a 4xx and a 2xx are unchanged. The other venues' transports keep their narrower set (their docs say nothing about a bare 5xx).
- **Binance and Aster: an unreadable 2xx reply to a write is "outcome unknown", not a reject.** `UreqTransport::run` made a 2xx whose body is not JSON `code: 0` ("never left this host") on every method, so a submit the venue had answered was rejected terminally. On a POST, PUT or DELETE it is now `E_TIMEOUT_AMBIGUOUS`; a read keeps `code: 0`.
- **IBKR cpapi: the loopback agent follows no redirect and resolves DNS definitely.** `blocking_agent_loopback_insecure` kept ureq's ten redirects (a 301/302/303 turns a POST or DELETE into a bodyless GET that carries every header but `Authorization` and `Cookie`) and had no DNS resolver, so a failed lookup read as ambiguous. It is built through `venue_agent` now, like every other agent in `http.rs`; a 3xx on a cpapi write stays "outcome unknown", as the replayed GET always was.
- **Bybit and OKX: a failed active confirm no longer claims an order.** The core's stuck-order watchdog prods the exec thread to look an order up by client order id (`ExecActor::with_confirm`); both bridges answered a FAILED lookup (a bad key, a ban, a 5xx, a timeout) with an id-less optimistic `OrderAccepted`, a phantom when the order never landed. It now says nothing, like Binance and Aster, and the watchdog ladder, the user-data stream and reconcile decide. Found is unchanged; a submit whose POST timed out keeps its own re-query (`resolve_ambiguous_submit`).
- **Bybit and OKX: an active confirm no longer rejects a submit that has not been sent yet.** A submit still queued on the exec thread behind a slow cancel batch is not on the venue, so the lookup found nothing and the confirm rejected an order about to be placed, which then filled with no engine order to own it. "Absent" now rejects only a client order id whose submit the exec thread has finished dispatching (the one shared form, `vike_bridge_core::confirm`); an earlier absent answer says nothing.
- **Binance, Aster, Bybit and OKX: an active confirm beyond the fourth in flight waits for a slot instead of being dropped.** The stuck-order ladder asks the venue once per client order id and rejects at its backstop whatever is still unanswered, so a confirm skipped for lack of a slot (binance and aster did) was a live order rejected; bybit and okx would have queued behind one lock for the length of a slow two-endpoint lookup. Up to four lookups now run side by side and the rest queue (`vike_bridge_core::confirm`).
- **A venue whose accounts are all labelled no longer shows a phantom default account, and the tradehub's start text and mount flag are about the account the mount trades** (decision 0172). `vike-backend venues` printed `bybit - - - no NoAccountRow` and `bybit: 3 accounts - 1 not running` beside the two labelled accounts that exist, because `known_accounts` listed the default (unlabelled) account of every venue; it now lists it only when the venue has an unlabelled row or nothing else would be listed (an empty store still says why). The default account is still not running and a mount naming it is still refused. The feed arms asked `live_venues` (keyed by account route key) for the bare venue, so a mount on `bybit#DEMO` announced `EXEC IS PAPER ... mounted the paper exchange` with the badge `OTHER-ACCOUNT-LIVE`, a remedy that told the operator to create an unlabelled account, and the `MAINNET`/`DEMO` word of the default account; the announcement, the remedy, the network word and the `StrategyStatus` row's `live` now look up the mount's own account. The tradehub and the gatewayhub both change (restart both on deploy).
- **Every other credentialed ureq agent follows no redirect.** The alpaca SSE agent, the IG trade-stream agent (its `create_session` body carries the CST/XST pair; `run_once` now refuses a non-2xx `create_session` and subscribe by status), both OANDA stream agents, the alert webhook POST (the url is the secret), the `vike-ai` provider client, the `vike-agent-eval` Anthropic driver (`x-api-key`) and the Telegram long-poll agent (the bot token is in the URL path) kept ureq's default of ten redirects, which replays every header but `Authorization`/`Cookie` to a 3xx `Location` (and re-POSTs the body on a 307/308). Each now sets `max_redirects(0)`, so a 3xx surfaces as a definite non-2xx answer naming its status, and a loopback test per agent proves the `Location` is never dialled. Deribit's keyless option-chain read keeps the default.

## [0.1.63] - 2026-10-16

### Added
- **Binance and Aster resolve an UNKNOWN submit by an active by-client-order-id re-query (`ExecActor::with_confirm`).** Binance (perp and spot) and Aster (perp and spot) wire `with_confirm` over the `query_order_orderid` lookup their submit-timeout path already ran (`origClientOrderId`; `-2013` absent becomes a "venue confirms absent" reject, a failed query emits nothing, the stuck-order watchdog prods again; "absent" is trusted only once the exec thread has finished dispatching that coid; one REST client per exec client, bounded in-flight confirms, a per-coid backoff). Their `COID_LOOKUP` rows are now true, so the gatewayhub answers `ResolveOrders` with `Querying` for them.
- **FXCM echoes the venue's own order rate.** After a limit placement or an amend the adapter reads the Rate the ForexConnect Orders table holds for the order (new optional shim export `fc_order_rate`, bound by name, `FC_ABI_VERSION` stays 2, an older shim simply has no echo), logs it at info beside the requested and the sent rate, and WARNs past half a tick. The shim must be rebuilt and reinstalled on the box; the release gate and the unsafe ratchet list the new export.
- **`vike-indicators`: a second door that takes plain `f64`, with no `Bar` and no `vike-marketdata` in the call, for 210 of the 211 indicators — every one except `ichimoku`, which reads future bars (the candlestick patterns take `open, high, low, close`, the pair indicators two aligned series, `vwap` and `anchored_vwap` also the timestamps).** One function per indicator for a whole series, `vike_indicators::series::<name>(columns.., params..) -> Result<Vec<f64>, ParamError>` (`series::rsi(&close, 14.0)`, `series::atr(&high, &low, &close, 14.0)`, `series::macd(..)` returning one `Vec<f64>` per line), and `next_value` / `next_hl` / `next_hlc` / `next_cv` / `next_hlcv` / `next_ohlc` / `next_oc` on the concrete type for one value at a time (`Rsi::next_value(close)`, `Atr::next_hlc(h, l, c)`). Parameters are `f64` like `Type::new` and a value the registry refuses is the same `ParamError`. The `Bar` door (`next(&Bar)`, `on_bar`, `vectorize(&[Bar])`, `make`, `make_with`) is unchanged and `vike-marketdata` stays a dependency. All four ways in (`next(&Bar)`, the plain-number method, `vectorize`, `series::*`) run one step function per indicator and agree bit for bit; `tests/fast_*.rs` checks that over a range of periods, lengths 0 to 2600 and a NaN travelling through the input, and checks each indicator against a textbook reference at three price scales (level 100, 1e6 and 1e-3) to a relative 1e-9 or better. Exceptions to the 1e-9: `rvol_rogers_satchell` and `rvol_yang_zhang` on prices near a million (1e-6, they take three logarithms instead of four), and `ttm_squeeze`'s momentum (1e-9 plus 8e-16 of the price level, a difference of two numbers the size of the level). The README's new "Without `Bar`" section is `tests/readme_plain_door.rs`.

### Fixed
- **Bybit: a submit refused with a "timeout" code is re-queried, not rejected.** Bybit answers a backend timeout as HTTP 200 with a `retCode` (`10000` "Server Timeout", `170007`, `170146` "Order creation timeout"); the single, stop-conditional, whole-batch and per-row submit arms checked only the transport's timeout sentinel, so those codes became a terminal `OrderRejected` over an order that may be live. They now take the `resolve_ambiguous_submit` re-query (`error_codes::submit_outcome_unknown`), and a modify refused with one says "outcome unknown". Definite codes (`110007`, `10006`, ...) still reject. The re-query keeps its weak form: a failed query is still an id-less accept.
- **OKX: a submit refused with `50004` is re-queried, not rejected.** The venue says a `50004` "Endpoint request timeout" does not mean the request succeeded or failed, yet the single, whole-batch and per-row (`sCode`) arms rejected it (the algo stop now accepts optimistically, as it does for the sentinel), and a modify refused with it claimed the order kept its terms. All four now follow the sentinel (`error_codes::submit_outcome_unknown`). Definite codes (`51008`, `51000`, ...) still reject.
- **Binance and Aster: an ambiguous submit whose status query also fails claims nothing.** `family::confirm::resolve_submit` answered a failed by-coid lookup with an id-less optimistic `OrderAccepted`, a phantom when the order never landed (now far more common: every `-1006`/`-1007` submit goes there) that the stuck-order watchdog never sweeps. It now emits nothing, like the active confirm (`COID_LOOKUP` is true for both venues, so the watchdog's stage-1 confirm decides, and the user-data stream acks a live order). Found and absent are unchanged.
- **Binance and Aster: a modify refused with "execution status unknown" no longer claims the order kept its terms.** The binance spot cancel-replace re-queried the coid only on `-2021`, `-2022` and the timeout sentinel, so `-1006`/`-1007` came back as a plain `OrderModifyRejected` although the replace may have gone through (it now re-queries via `settle_replace`, and reports `OrderModified`, `OrderCanceled` or a rejection from what the venue holds), and the perp PUT modify (binance and aster) worded them as a refusal (it now says `modify outcome unknown`, like the sentinel). `vike_binance::error_codes::submit_outcome_unknown` decides both; `-1013`, `-2013` and every other code stay a plain denial.
- **IBKR cpapi: an unreadable 2xx reply to a write is unknown, not a reject.** A gateway reply with a 2xx status and a body that is not JSON (an HTML page, a truncated body) was `code: 0` ("the request never left"), so `place_order` rejected terminally an order the gateway had received. On a POST or DELETE it is now `E_TIMEOUT_AMBIGUOUS` and takes the `place_failed` path (a warning, no terminal event); a read keeps `code: 0`.
- **IBKR cpapi: an UNKNOWN-outcome place no longer rejects an order that may be live.** `CpapiTransport::place_order` turned every REST failure into a terminal `OrderRejected`, including a timeout, a reset after the POST or a gateway 502/503/504/524, where IBKR may hold the order (a phantom reject over a live order). The cpapi REST client now classifies a send failure by the shared allowlist (`classify_send_error`) and a gateway 5xx on a write as `E_TIMEOUT_AMBIGUOUS`; an ambiguous place or reply logs a warning and emits no terminal event, and asks for ONE open-orders resync 2 s later (`StreamResync`: the exec loop reads `/iserver/account/orders`, and a row naming the coid becomes the order's `OrderAccepted`). cpapi has no by-coid lookup (the watchdog's `confirm` is a no-op) and reconcile under `quarantine` folds nothing, so without that resync the stuck-order watchdog rejected the live order after about a minute; a resync that finds no row, or whose read fails, leaves that last resort in place. A definite failure (connection refused, a venue 4xx body) still rejects as before. A gateway 5xx on a write now carries a bounded head (512 bytes) of the gateway's own body in the error, and stays unknown.
- **Polymarket's signed calls follow no redirect.** The agent behind the order POST and DELETE, the heartbeat, the L1 derive, the signed reads, the relayer `/submit` and the Polygon RPC kept ureq's default of ten redirects, which replays every header but `Authorization` and `Cookie` (`POLY_API_KEY`, `POLY_SIGNATURE`, `POLY_PASSPHRASE`, `RELAYER_API_KEY`) to a 3xx `Location`. `crates/bridges/polymarket/src/egress.rs`'s `agent_with_timeout` and the RPC agent now set `max_redirects(0)`, so a 3xx surfaces as a definite non-2xx error naming its status.
- **IG order modify reaches the venue.** `IgExecutionClient` did not forward `ExecutionClient::modify`, so the trait's empty default ran: every IG amend vanished with no event and an unchanged price (seen on the CI box) while the IG amend code sat unreachable. It forwards now, and the new `modify_forwarded_gate` fails any venue whose caps say `supports_modify` while its bridge `ExecutionClient` impl lacks `fn modify(`.
- **A modify sends the grid-snapped price and quantity, like a submit.** `ExecutionEngine::modify_order` forwarded the raw `new_qty` / `new_price` to the venue and into the local order instead of what the risk gate normalized, so an off-grid price (66714.545 on a 0.01 tick) went to the wire on binance, okx and deribit. An omitted field stays omitted.
- **Deribit: the first order after an idle spell is no longer lost.** Nothing reads the order WebSocket between calls, so a socket the peer or a NAT dropped while idle took the first order frame into the void (the the CI box drill of 2026-10-16). A matching-engine call on a socket idle for 5 s (`IDLE_PROBE_AFTER`) is first preceded by a `public/test` probe (4 s wait), and a failed probe re-dials BEFORE any order frame exists, so nothing is ever re-sent. A failed re-dial is a definite pre-send error (one bounded dial per heal). The probe does not check authentication and the order socket never refreshed its ~900 s access token, so a socket whose auth is older than a third of the lifetime is re-authenticated before the next private frame; an order frame answered 13009 (`unauthorized`) is never re-sent (outcome unknown, resolved by label), only a read-only `private/get_*` is re-asked.
- **A refused modify shows its reason in `order ls`, and a qty-only modify is sized at the standing price.** `OrderView::reason` also carries the reason of a LIVE order's last `OrderModifyRejected` (cleared when a modify lands). The control door sizes a qty-only modify at the higher of the snapshot price and a recent modify's price and refuses only when the order has none (a market or stop order, an unknown coid; a zero or non-finite price is no price), instead of refusing every qty-only modify; the amend margin reservation is pinned by a test.
- **A send failure is ambiguous unless it provably preceded the write.** `classify_send_error` treated only a `ureq` timeout as ambiguous; an `Io` reset or `UnexpectedEof` after the POST body went out, `Protocol`, `LargeResponseHeader` and `RedirectFailed` were `code: 0` ("never left this host"), so the venue adapters rejected terminally an order that may be live (a stranded position, a later duplicate). Now only an allowlist (`send_error_is_definite`: DNS, connect refused or failed, proxy, bad URI, TLS policy, bad request) is definite and everything else is ambiguous (the caller re-queries, never rejects); a gateway-style 5xx is ambiguous too (`UreqTransport::run` and the bybit, okx and hyperliquid `/exchange` transports). Polymarket reads the shared predicate instead of its own copy.
- **Test races.** `lease_takeover` waits for the unknown set, not only for `Closed`; the gatewayhub-client `delivery_class` reconnect test waits for the replayed subscription's acknowledgement before relaxing its class (inside that window the relax sent nothing); the datahub `strat_lane` stalled-reader test bounds the sink latency by slow-call count (at most 20 calls over 100 ms, a 2 s ceiling; 2x CPU oversubscription was measured at up to 9) instead of one wall-clock maximum that a single scheduler preemption (176 ms in release CI) broke; the tradehub synthetic-cancel proof (`hop_kill_proofs`) counts the fill once instead of two terminal events, because the bridge's bare synthetic cancel legitimately follows a fill that won the race to the lane (about 4% of runs). Test code only. The okx page records that the engine symbol is the instId.
- **OANDA and IG no longer reject an order whose submit may have reached the venue.** OANDA resolves an ambiguous POST (a timeout or reset after the write, a gateway 502/503/504/524, an unreadable reply) with `GET /orders/@{clientOrderID}` (found -> `OrderAccepted` with the venue id; the terminal "venue confirms order absent" reject only when OANDA itself says the order does not exist on every attempt of a short ladder AND no transaction of that client id appeared since a watermark read before the POST; a found FILLED order delivers its fill, a CANCELLED one is read from its own transactions; only a `*_ORDER_REJECT` is the order's terminal reject; one 12 s deadline bounds the lookup; anything inconclusive -> no event, the order stays `Submitted`), and the same query answers `ExecutionClient::confirm` (which never rules an order absent: its POST may still be in flight), so the gatewayhub's `COID_LOOKUP` row for oanda is `true`. IG's open now sends its own `dealReference` (derived from the coid) and records it before the POST, so a create whose reply is lost is resolved through `/confirms` instead of rejected; IG cannot say "absent" (a 404 means "unknown"), an ambiguous CLOSE (no reference in IG's close body) is left UNKNOWN, and an open IG never acknowledged stays `Submitted` for the stuck-order watchdog instead of becoming a phantom live order (only an echo-confirmed reply accepts or rejects it); the deal-reference map is bounded. cTrader is unchanged: its submit has no ambiguous-reply class and no safe by-id lookup (the bridge page says why).
- **Binance and Aster: a submit refused with "execution status unknown" is re-queried, not rejected.** A gateway 5xx whose body carries the venue's own code (`-1007` TIMEOUT, `-1006` UNEXPECTED_RESP, both documented as "execution status unknown") is deliberately not the `E_TIMEOUT_AMBIGUOUS` sentinel, and the submit arms turned every other error into a terminal `OrderRejected` over an order that may be live (a stranded position, a later duplicate). `vike_binance::error_codes::submit_outcome_unknown` (the sentinel, or a code the table files under `ErrorKind::Timeout`; `-1006` moved there from `ServerError`) now decides every submit arm of binance and aster, perp, spot and batch (a whole-batch error and a single batch element alike), and a hit goes to `resolve_ambiguous_submit`. Every other code (`-2010` insufficient balance, `-1013`, `-1021`, ...) still rejects. Aster's published error tables list the same two codes; that the live venue emits them is not observed.
- **Send-error classification, review round.** Every agent built in `vike-bridge-core`'s `http.rs` except `blocking_agent_loopback_insecure` installs a DNS resolver (a DNS failure is definite `HostNotFound`) and follows no redirect (a signed query is never replayed to a `Location`); `blocking_agent_loopback_insecure` (IBKR's cpapi gateway on a loopback URL) and the agents a bridge builds itself (the alpaca, IG and OANDA stream agents, deribit's chain agent) install no resolver and keep ureq's default redirect limit, and polymarket's (`agent_with_timeout`, `PolygonRpc::build_agent`) install no resolver and follow no redirect, so a DNS failure on any of them reads as ambiguous; a TLS handshake that failed on the certificate or negotiation (ureq handshakes lazily, so it arrives as `Io(InvalidData)` wrapping a `rustls::Error`, now downcast) is definite; `BodyExceedsLimit` and `Io(NotFound)` are no longer definite; a gateway 5xx is ambiguous only on a POST/PUT/DELETE whose body carries no venue code (a read keeps the status-as-code); an ambiguous modify says "outcome unknown" instead of "order kept its terms" (binance and aster perp, bybit, okx); a Hyperliquid ambiguous cancel emits a non-terminal `OrderCancelRejected`. `vike-bridge-core` gains a `rustls` edge for the downcast.
- **OANDA: an inconclusive submit lookup leaves the order `Submitted`.** `resolve_submit` answered an id-less `OrderAccepted` when the by-client-id read failed, when no transaction watermark existed or the 12 s budget was spent, and when the transactions scan failed, which took a possibly never-received order out of the stuck-order watchdog's view (it watches only `Submitted`) and left reconcile to flag an orphan later. Those arms now emit nothing, as IG's `submit_outcome` does. A found order still accepts, and OANDA's own "order does not exist" with no transaction of that client id still rejects.
- **Hyperliquid learns the replacement oid after an ambiguous modify.** A modify whose reply timed out (or named no oid) left the order's oid on the old, possibly dead, one. The pump now records the oid of each open or resting `orderUpdates` row per registered cloid and the next modify adopts it (else one `orderStatus`-by-cloid read), clearing the unknown mark; the old oid stays retired so its stale `canceled` is still suppressed once. A WebSocket row alone, or an `orderStatus` that finds the order open under the held oid, never lifts that retirement inside `RETIRED_TTL`, and only `unknownOid` (not a failed read) spends the one-shot requery.
- **Polymarket's by-order lookup answers.** The `ExecutionClient` wrapper never forwarded `confirm`, so the `with_confirm` re-query was a silent no-op. It is delegated now, and a found `CANCELED`, `UNMATCHED`, `INVALID` or `CANCELED_MARKET_RESOLVED` order resolves to `OrderAccepted` plus `OrderCanceled` instead of a bare accept; the order row is read bare, under `data` / `order`, or as an array head.
- **FXCM: a request with no answer is UNKNOWN, not a refusal.** `fc_place` looks a timed-out or ID-less placement up in a fresh Orders table by `RequestID`, else reports the outcome as unknown; the adapter emits no terminal event for those and settles a lost amend reply by reading the order's rate back. `RespListener` sets the request id before `subscribeResponse` and guards its strings with a mutex (no race). Every wait on the order path is bounded (one placement fits under the ack timeout; the budget is `docs/ops/fxcm-forexconnect.md`'s); the lost-reply lookup verifies offer, side, amount, kind and rate and claims nothing on ambiguity, and FXCM now answers `ExecutionClient::confirm` for a placement whose reply this process lost (new optional shim export `fc_find_order`, no ABI bump; `COID_LOOKUP` row `true`). The shim must be rebuilt and reinstalled.
- **FXCM's `confirm` is forwarded by the wrapper, and a gate holds every venue to it.** `FxcmExecutionClient` did not forward `ExecutionClient::confirm`, so the trait's empty default ran and the lookup above never fired while `COID_LOOKUP` said `true`. It forwards now, and the new `confirm_forwarded_gate` fails any bridge that calls `.with_confirm(` (and any `COID_LOOKUP` `true` row) whose `ExecutionClient` impl lacks `fn confirm(`. A confirm that arrives after the worker gave up still delivers its events (the exec thread sends them, the worker only waits for an ack); an order id the session already holds under another coid is refused, one pass over the confirm lane is time-bounded, and the unknown-placement table evicts its oldest row at its cap.

### Changed
- **Operator docs from the the CI box sitting of 2026-10-16.** Shim install after an ABI bump, the two notional ceilings, labelled accounts, account retirement, the IBKR client-id leak and the control-door drill recipe (`docs/ops/`). No code.
- **A page of known limitations, per venue.** `docs/ops/known-limitations-2026-10.md` lists, verified against the code, what an operator can still see go wrong (IG's unconfirmed create and its close, the control door's price memory, the Deribit handshake read, redirects on catalog GETs, cTrader's missing lookup, off-grid wire prices, alpaca's amend) and what would lift each.
- **`vike-indicators`: 210 indicators run on rewritten step functions: 3 to 100 times faster on a whole series, and for those that used to re-run a batch kernel on every bar, hundreds to thousands of times faster per streamed bar (the same indicator on the previous code, 50,000 values, minimum of three runs on a shared build server).** Each is a small struct of plain numbers (running sums rebuilt every `ceil(512 / n)` laps, monotonic extremes, no allocation per bar, no `%` or division in the steady state), and the stream, `vectorize` and `series::*` all run its one `push`. The measured numbers (a table of whole-series times and streamed ns per value, before and after) are in the crate README, in "How much faster". Measured against `ta`, `yata`, `kand`, `wickra` and `rust_ti` on the same box, these indicators are first or tied in most cells in both regimes (of the 30 indicators of the second batch: the stream first or tied in 27, the whole series first in 26). Behind a competitor: `rvol_ewma`, `rvol_parkinson`, `rvol_garman_klass` and `rvol_rogers_satchell` trail `wickra` in both regimes, because they keep `libm::log` so the values are identical on Windows and Linux (the platform `ln` was measured to differ from one platform to the other in 0.3% of price-ratio inputs, `exp` in 0.5% and `log10` in 1.7%, by one ulp); and in the plain-number stream `envelopes` (2.10 ns against `wickra` 1.94) and `atr_trailing_stop` (4.38 against 4.26). **The last bits of many values changed** (running sums add in a different order, and several divisions became multiplications by a reciprocal); each indicator is held to a relative 1e-9 against a textbook form. The libm pin hashes of `hvol`, `chop`, `mcginley`, `alma`, `rvol_close_to_close`, `rvol_parkinson`, `rvol_garman_klass`, `rvol_rogers_satchell` and `rvol_yang_zhang` were re-recorded on a Linux lane; the Windows confirmation is pending.
- **`vike-indicators`: a one-bar bad tick (a print 1000 to 10 million times the price) no longer leaves a lasting error in the windowed indicators that keep running sums of squares or products, and `spread_zscore` and `correl` recover from a NaN or infinite close.** The running sums of `bollinger`, `bbands_pctb`, `bbands_width`, `hvol`, `relative_volatility`, `linreg_channel`, `ttm_squeeze`, `linearreg_slope`, `linearreg_angle`, `beta`, `cci`, `mad`, `chop`, `rwi`, `wma`, `awesome`, `qstick` and `eom` rebuild from the window on the spot when the term that has just left dwarfs the result (before: up to a few hundred bars at the scheduled rebuild, `bollinger` was off by up to 2% in the deviation). `tests/outlier_recovery.rs` holds a spike of 1e3, 1e5 and 1e7 against a textbook reference for these and for the sums that were already clean. `spread_zscore` and `correl` were poisoned for good by one non-finite close; they are now `NaN` while it is in the window and exact the update after it leaves, like `beta`, `correl_log` and `half_life`. **Not guarded, a known limit:** `hma`, `coppock`, `kst`, `dpo`, `ac` and `kama` keep the scheduled rebuild only, so a spike of 1e5 (`coppock`, `dpo`) to 1e7 (the others) leaves a relative error of 1e-8 to 5e-7 in them until it; the guard was written and measured (it costs 14% to 38% on a whole series for these six) and left out because the owner put speed first. `sma` is not guarded either, for the same reason (+44% in the stream).
- **`vike-indicators`: edge cases of the rewritten indicators that now answer differently (all on windows with no usable number, or on parameters the registry refuses and `with_params` still accepts).** `chandelier`, `donchian_width` and `ttm_squeeze` on a window with no number in the highs (or lows) answer `NaN` for what depends on that extreme (were `f64::MIN` / `f64::MAX`, or `-inf` in `donchian_width`); `midpoint` and `midprice` on a window with no number are `NaN` (were `0.0`); `bbands_pctb` and `bbands_width` answer a number on a band a few ulps wide (were `NaN` / `0`); a period of 0 reached through `with_params` is `NaN` or acts as 1 and no longer panics or underflows, and every raw period is capped at 65,536; `chop` streaming follows the full-history result; `aroonosc` on an all-`NaN` window is `NaN` (it panicked). `ichimoku` is untouched and stays batch-authoritative.

## [0.1.62] - 2026-10-16

### Added
- **Order modify works on IBKR and Dukascopy.** IBKR's socket backend re-places the order under its own `orderId` (TWS's amend) and believes it only on the venue's `openOrder` echo (`OrderModified`; a refusal or 10 s of silence is a non-terminal `OrderModifyRejected`; a stop amend now moves the trigger). Dukascopy's sidecar gains a `modify` command (`IOrder.setOpenPrice` / `setRequestedAmount`; needs the jar of this release, an older one is refused in words). `supports_modify` is true for both, and the IBKR socket/cpapi caps rows are one row again.
- **OANDA order modify is a v20 order REPLACE; a venue that cannot modify answers with an explicit rejection.** `ExecutionClient::modify` PUTs the replacing order at `/orders/@{clientOrderId}` under the same `clientExtensions.id`, so the platform keeps ONE order across the venue's cancel and new id; the reply maps to `OrderModified` (the new venue order id, the echoed units and price) and a refusal to `OrderModifyRejected`. The replaced order's `ORDER_CANCEL` (`replacedByOrderID`, `CLIENT_REQUEST_REPLACED`) no longer decodes to `OrderCanceled`, live or in the `sinceid` backfill. OANDA's caps now say `supports_modify`, and its amend row is `CancelReplace`. In the engine, `ExecutionEngine::modify_order` publishes a non-terminal `OrderModifyRejected` for a venue whose amend convention is `Unsupported` (the client is asked first, so paper mounts are not refused) instead of forwarding to the default no-op adapter.
- **FXCM: a priced limit rests at its price, and a resting entry can be amended (shim ABI 2).** `fcshim.cpp`'s only entry point for a resting limit took an int pip distance from the live quote, which is why every priced limit was refused. `fc_place_entry_at` (`FC_ABI_VERSION` 2) takes the caller's double rate, refuses one off the instrument's price grid and leaves wrong-side-of-market to the venue; `fc_edit_order` amends a resting entry's rate in place (`EditOrder`). The loader binds both optionally and only at `abi_version >= 2`, so an older installed shim answers with a named rejection instead of resting at a price nobody chose. Caps: FXCM `supports_modify` true (price only), amend row `Unknown`; `preflight_request` places only market and limit. The shim must be rebuilt and reinstalled on the box (a separate deploy step).

### Fixed
- **A failed cTrader amend is a modify rejection, not a terminal reject of a working order.** An amend's envelope id now carries its verb (`amend:<coid>`): its `ERROR_RES` used to read as a failed SUBMIT (terminal `OrderRejected`) and its `ORDER_ERROR_EVENT` as a failed cancel. An amended size is stepped onto the symbol's volume grid like an order's, and refused below the minimum.
- **An order's notional carries its KIND, so deribit's inverse perpetual is no longer priced at millions (decision 0170).** `BTC-PERPETUAL`'s quantity is US dollars, but the money path multiplied `qty · price · contract_size`, so a 10-lot was worth 6,570,000 and the control door refused it against a 100 ceiling, while the linear `BTC_USDC-PERPETUAL` (quantity in BTC, `contract_size` the 0.0001 lot) was understated 10,000x. `vike_model::ContractValue` (a `NotionalKind` plus a per-unit factor, derived from the contract's `Margining`) replaces the bare multiplier from the bridge (`LiveExec::contract_value`, `AdapterReply::Live`, behind the `gw_trade_contract_value` feature) through `Account`, `RiskContext`, the engine valuation, the margin call, the published `VenueBlock` and the remote GUI's `WireVenueBlock`; an inverse contract is sized by its quantity, books harmonic-average quote-currency PnL and a coin fee at the fill price, and never absorbs a coin wallet into its quote ledger. A linear contract is bit-identical. The control door's 1.0 floor is gone: it and the `RiskGate` call one function. An older gatewayhub that does not advertise the field leaves deribit and ibkr NOT RUNNING with the reason, and an unreadable deribit instrument row does the same. Strategy value-sizing verbs refuse an inverse mount, and an OKX inverse swap is refused at exec like bybit's. Old deribit-inverse journals refold to different PnL; nothing converts them.
- **Deribit order modify is `private/edit` instead of a silent no-op.** `DeribitRest::modify_order` edits the resting limit order in place (same venue id, `post_only` false, the new total size) and answers every outcome with `OrderModified` or a non-terminal `OrderModifyRejected`; an ambiguous failure is settled by reading the order back, never re-sent. The caps say `supports_modify`; the amend row stays `Unknown` until a partly filled edit is measured.
- **Binance spot order modify is an atomic cancel-replace instead of a silent no-op.** `BinanceSpotRest` took the default `modify_order`, so a spot modify was accepted by the control door, did nothing at the venue and emitted no event. Spot now sends `POST /api/v3/order/cancelReplace` under the SAME client order id (`STOP_ON_FAILURE`, `cancelRestrictions=ONLY_NEW`) and answers every outcome with an explicit event; the cancel half carries a marked throwaway id so the stream mapper drops its `CANCELED` report, and the history replay ignores the superseded `CANCELED` row of a re-used client order id.
- **IG order lifecycle: modify, confirmed cancel, reconcile and the symbol check.** `ExecCommand::Modify` was a silent no-op; it is now a price-only `PUT /workingorders/otc/{dealId}` answered through `/confirms` (`AMENDED` becomes `OrderModified`, anything else `OrderModifyRejected` with the venue's reason; a size change is an explicit rejection), and an `AMENDED` confirm is no longer read as a fill. A cancel is reported from the `DELETE`'s `/confirms` reply, not from the HTTP 200. The client order id to `dealId` map is ONE shared map, filled by the submit confirm and the confirm watchdog and read by the reconcile client, so this process's live working orders match their local orders (no `UnknownOrder` plus `OrphanLocalOrder` pair). The `/confirms` retry ladder is longer (7 x 400 ms after 3 x 200 ms), a dotless symbol (`EURUSD`) is refused before the network with the epic form named, and every create rejection logs IG's `errorCode`. IG's caps say `supports_modify`; its amend row stays `Unknown` (the partial-fill convention is unmeasured).
- **cTrader wire prices carry the symbol's digits, and an order view says why it was refused.** The RiskGate's tick rounding leaves f64 noise (`0.09000000000000001`) and the venue refused it (`more digits than symbol allows`); the new-order and amend paths now snap the wire price with `SymbolMap::round_price`. `OrderView::reason` carries the reason an `OrderRejected` or `OrderDenied` brought (a bounded engine side table, outside the snapshot and `state_hash`; additive on the wire as `WireOrderView::reason`) and `vike-cli trade order ls` shows it as a `REASON` column and a `reason` key.
- **Order-lifecycle holes on bybit, okx, hyperliquid and polymarket.** A refused bybit/okx perp amend is now a non-terminal `OrderModifyRejected` carrying the venue's reason (it was an empty event list, so the order looked amended). Hyperliquid's modify parses the `/exchange` reply (`status` err or `statuses[0].error` becomes `OrderModifyRejected`, a `filled` status carries the new oid) and a timeout after the send is an explicit "outcome unknown" advisory instead of only a WARN. Polymarket's cancel reads `not_canceled` as `OrderCancelRejected`, a submit whose transport failed AFTER the send (timeout, unreadable or unparseable 2xx body) stays UNKNOWN for reconcile instead of a terminal `OrderRejected`, and its Modify arm answers `OrderModifyRejected`.
- **The gatewayhub-client test rig fences the lease it cut, and `never_sent` names its double verdict.** The fake trade peer's `kill_lease` took the lease out of the shared slot, so the redial found no old lease to fence and could record a frame the ledger had already rejected as never received (a flaky `never_sent` run); `open_lease` now waits for the cut lease's handler (the real daemon's fence), the lease loop re-checks `killed` right before a read, and the double-verdict message names the reject reason, connection and cseq. Test code only.
- **The hop-kill synthetic-cancel proof no longer flakes.** It waits for the cancel's answer and pins "no cancel AHEAD of the fill" instead of "no cancel at all". Test code only.
- **Review findings on the IBKR, Dukascopy, cTrader and Deribit amend paths.** IBKR follows an amended quantity only on a confirmed amend (an unsolicited row keeps the fill tracking), allows one amend in flight per order, never terminally rejects an unacked order on an id-less rejection while an amend is open, refuses an amend for an order whose terms are not known, and the cpapi stop amend moves `auxPrice` without an unproven `OrderModified`. Dukascopy spaces JForex changes by at least 1.1 s and waits less. cTrader's amended volume steps down onto the grid. Deribit's edit price floors to the tick like a submit's.
- **Review findings on the crypto venue lifecycle.** Polymarket confirms an unknown submit and treats a wider set of ambiguous send errors as ambiguous; Hyperliquid answers an unknown modify; `get_order` is crate-private; the hop-kill proof has a barrier. Also in this wave: a degenerate contract value is folded at the bind, the grid and the observer and refused at the control door; an inverse engine drops a fill of unknown kind (decision 0170).

### Docs
- Decision 0170 (the notional of an order carries its kind, linear or inverse, from the contract to every consumer) is recorded, with its implementation plan (`docs/superpowers/plans/2026-10-16-order-notional-kind.md`).

## [0.1.61] - 2026-10-16

### Added
- **Dukascopy's trading path is connected in production (decision 0168).** The wiring table gives the venue an engine (`EURUSD`), so the tradehub's arming table lists it, its active `demo` account opens a trade lease, and the gatewayhub starts the JForex sidecar on `TradeOpen` (one sidecar per process: a named labelled account takes it, the default one yields; a `live` row is still `LiveTierNotWired`). Exec-only (no live feed, so a profile cannot name it as its own venue) and unreconciled: the adapter hands out no reconcile client with the reconcile gate on or off, because the only one it can build is positions-only. Nothing is measured on a deployed box yet: the first login under the gatewayhub's unit and the JVM's memory there.

### Fixed
- **A strategy mount that cannot pick its account no longer crashes the tradehub (0156).** A mount naming a venue and no account, on a venue with two active accounts (for example an unlabelled demo beside a `LIVE` one), panicked at start and restart-looped under systemd; that one mount is now simply not mounted, the refusal is logged with the labels to choose from, and every other mount and account keeps running. `account = "DEFAULT"` is now accepted in a profile `[[mounts]]` row and names the unlabelled account.
- **IBKR no longer holds its client id after a graceful stop.** The order-update pump kept the socket open after the session was retired, so the next start failed with `client id is already in use` until the gatewayhub was restarted; the pump now ends when its transport is dropped.
- **OANDA's transactions and quote streams no longer reconnect every ~10 s.** The response-head deadline leaked into the body read in the HTTP client; the agents now bound the request send instead, the stream lives as long as heartbeats arrive, and a leaving read loop is logged and paced.

### Docs
- Wave-2 sitting corrections in the gatewayhub runbook (the profile mount forces bybit demo on with binance, Dublin SOCKS egress for Polymarket orders, the known stream and seed limits). Decision 0169 (an account follows its `active` flag in the running tradehub) is recorded as `proposed`.

## [0.1.60] - 2026-10-16

### Added
- **The tradehub's live mount asks the gatewayhub for its venues (decision 0164, D7), and `config.gatewayhub_connect_wait_ms` bounds the wait.** The arming table, the preflight legs and each account's adapter, orders and reconcile reads come from `vike-gatewayhub` over the trade wire; this daemon no longer claims `LIVE-<route key>.lock` (the gatewayhub does, at the account's trade lease, and a lease another client holds refuses the whole live start). A start with the gatewayhub down waits `config.gatewayhub_connect_wait_ms` (1000 to 600000 ms; unset is a minute) only when an account is active at `demo` or `live`, then stops those accounts with the new block `gatewayhub unavailable` — never a paper client in their place — and a box that still runs a paper account arms them itself, once and never inside its first 90 seconds, by exiting 75 after a graceful teardown when the gatewayhub answers; the shipped tradehub unit declares that status a success and a forced restart (`docs/ops/tradehub-the CI box.md` has the lines), so the restart does not page. ⚠ A box whose EVERY account is `demo` or `live` (the CI box is one) has no engine to run while the gatewayhub is down: it exits at start and `Restart=on-failure` brings it back about every 65 seconds until the gatewayhub answers. A teardown that hits its hard cap after the late arm fired exits 0, and the waiting accounts stay NOT RUNNING until an operator starts the daemon. Prices come the same way (the strategy lane, below), so a live start also needs the datahub: the feed-gate entry below says what a start does while the datahub is away. `vike-backend venues` and `vike-backend catalog` ask the gatewayhub too (`venues` exits 1 when it does not answer; `catalog` uses the gatewayhub's client key pair). A downgrade needs `vike-cli config unset config.gatewayhub_connect_wait_ms` first.
- **The gateway daemon takes orders: `vike-gatewayhub` serves the trade lane, one lease per account (decision 0164, wave 2).** The tradehub opens a `Scope::Trade` connection per account and the daemon starts that account's venue adapter, dedups submits by `(account, client order id)`, applies the HALT sentinel at its own door (a reducing submit is admitted, any other submit is rejected with the HALT text, a cancel or confirm is never gated), claims the live-account lock of an armed live route key for the life of the session, and rings every venue event the adapter emits into a per-account ring of 20,000 events from which a reconnecting tradehub resumes by its journal cursor. The epoch is random per session and the ring and the dedup set live in the process, so a gatewayhub restart is a new epoch: the tradehub is answered `ReplayUnavailable`, re-teaches the daemon every resting order and reconciles at once, and a fill missed during the restart is recovered by that pass and by nothing else. A session outlives its lease (a link flap keeps the adapter, a final detach ends it, a lease nobody holds is retired after 30 minutes). Nothing is cancelled at the venue when a link is lost (decision D6); a command the daemon cannot take is answered with a terminal reject or an advisory, never silence; a risk-off cancel that arrives while an adapter is being restarted is held and sent only behind the teaching of every resting order (the Fixed entry on UNKNOWN submits says what that does and does not repair). Only bybit, okx and ig wire a lookup of an UNKNOWN submit by client order id after an epoch change, and none of the three states a fill (bybit and okx answer `OrderAccepted` for any order the venue still lists; ig needs a deal reference that lives in the adapter and is gone after a gatewayhub restart); the other eleven venues fall to reconcile and the stuck-order watchdog (each bridge page states its residual). Log prefixes `TRADE-OPEN:` and `TRADE-LINK:`. The lane is served only when the Trade key exists (next entry) and is negotiated by feature strings, with no version bump. `docs/ops/gatewayhub-the CI box.md`'s "Wave 2 release runbook" has the install order.
- **Three new platform keys, `vike-cli datahub setup --strategy-lane`, and a `gatewayhub setup` that mints three names.** `VIKE_GATEWAYHUB_TRADE_KEY` is the order-scope key the gatewayhub verifies and the tradehub holds; `VIKE_DATAHUB_TRADE_KEY` is the datahub's strategy-lane key (the datahub verifies it, the tradehub holds it); `VIKE_GATEWAYHUB_ADMIN_KEY` is named and minted by nothing (the account scope stays refused). `vike-cli gatewayhub setup` writes the observe/control pair and the Trade key in one call and refuses while any of the three is held, so on a box that has the pair `--rotate` is the only way to add the Trade key and it REPLACES the pair: from the gatewayhub's restart until the datahub's, the datahub is refused at the gateway's handshake (the runbook times that window and names the undo), and a restart of either daemon inside it leaves the datahub refused; an additive `gatewayhub setup --trade-key` on the model of `--strategy-lane` would remove the window and is requested, not built. `vike-cli datahub setup --strategy-lane` is additive: it opens only the lane key and refuses if it is held. `vike-cli secrets copy-hub-keys --only gatewayhub` copies the whole gatewayhub family and `--only datahub` the datahub's pair and the lane key; the desktop and the CLI admit none of the three. Mint with the release tag's own `vike-cli`: the installed one mints no Trade key and its `--rotate` would succeed without one.
- **The data daemon serves the tradehub's strategies their prices: the strategy lane (decision 0164, D8), and the tradehub's feeds are dials of it.** `vike-datahub` verifies the lane key and answers `StratOpen{venue, network}` with a market-stream tube per venue (the same frames, bounded queue and reconnecting client as the gatewayhub's data lane). Behind it the gatewayhub hosts the CEX tick pump (binance, bybit, okx, aster: one pump per symbol and tick size, shared across sessions), the credentialed streams of alpaca, ctrader, ig and oanda, and Hyperliquid on a named network, so a testnet Hyperliquid account gets testnet prices and the tradehub's own feed threads are gone. A book hole is told to a strategy as `Stale` (a non-`Live` status for `on_feed_status`, not a connection gap) and the paired `Live` waits for the next book anchor; a hole that has had no anchor for 6 seconds (three anchor periods) escalates ONCE to a `GapStart` on the book stream, which the anchor's `Live` closes. ⚠ That escalation is a NEW HALT PATH: a connection gap counts for the link dead-man, so a book hole that no anchor ends for 6 seconds plus the 120 s grace trips `CancelAllAndHalt` on an armed venue (before, such a book was silently stale for ever; a healthy pump anchors every 2 seconds, so it takes a real break). The sender keeps a timer per pump (checked every 250 ms) that re-anchors a quiet book with no pump message at all, and sends nothing while the pump says `Stale` or once the standing book is older than 20 seconds, except to a session that lost a book frame; the tradehub does not forward an anchor that restates the book it holds, because `SpreadMaker` amends a resting quote on every book it hears. A session that joins a lane is told where the lane stands (so an `on_feed_status` hook fires once more at start), and a whole book is capped at 1,000 levels a side. The in-tree XEMM maker latches its halt on any non-`Live` status, as before. Limits are START values, not measurements: 40 strategy connections (one per venue, polymarket one per token), 128 keys, 64 per venue, `Bye(TooSlow)` after 8 overflow episodes in 60 seconds or a write blocked for 10 seconds. The tradehub waits 25 seconds for a subscribe reply; a credentialed stream's handshake (ctrader's takes up to four replies) is bounded at 12 seconds inside the gatewayhub, at most 4 abandoned handshakes may run at once, and a ctrader first subscribe waits 13 seconds for a handshake in flight. A refused interval (oanda granularity, an IG scale, a deribit resolution code) still ends the start AFTER the core is built, and the core is now torn down in order (forwarder stopped, feeds released, core joined, all bounded at 10 seconds), with the bridge's own words inside the datahub's sentence.
- **The tradehub gates its feeds before it builds its core: a datahub that is away holds the `demo` and `live` accounts, never the process, and a restart loop is budgeted.** A live-gate start asks the datahub one authenticated question (no lane opened; a missing `VIKE_DATAHUB_TRADE_KEY` is the same answer at once, naming the key, and the key is re-read at every poll, so a key minted later is noticed) and reads what it serves for every mounted venue. When it does not answer, every `demo` and `live` account of a mounted venue is NOT RUNNING with the new block `feeds_unavailable` (badge `price feeds unavailable (datahub)`; an older peer reads the word as `adapter failed`), no feed is wired, `paper` accounts run without prices, one `PRICE FEEDS UNAVAILABLE` error line carries the datahub's words and the ready banner leads with `PRICE FEEDS UNAVAILABLE: <cause> - `; a start whose every account is `paper` no longer needs the datahub or its key. A credentialed-data venue (alpaca, ctrader, oanda, ig) whose default demo account has no credentials is refused before the core is built. ⚠ A strategy MOUNTED on a held account still refuses the node (a mount on an account that does not run is refused), so a profile whose mounts sit on `demo` or `live` accounts and a datahub that is down at start exits and the unit restarts it every 5 seconds until the datahub answers: on a box like the CI box (nine live mounts) a datahub deploy is a start-refusal window. The late arm polls the datahub as well, and the gatewayhub only when an account waits for it. A refusal after the core is up is SURVIVED where it honestly can be: a transient one (every link-class refusal carries a mark) on a venue with no running `demo`/`live` account leaves that venue priceless, and the late arm restarts the tradehub (exit 75, as above) when the datahub and the gatewayhub answer. That restart is budgeted: 3 per hour, counted in the state-directory file `late-arm-feed-restarts` (a ledger that cannot be read counts as spent and one that cannot be written orders no restart); with the budget spent the process stays UP and priceless, an `error` says so and the banner leads with `PRICE FEEDS UNAVAILABLE: the late-arm restart budget is spent`. A CAP refusal (the gatewayhub's pump or stream-session cap, the datahub lane's connection cap: each says `a cap, not a link`) needs an operator and orders no restart: a venue with no running live account stays up and priceless with one `PRICE FEEDS UNAVAILABLE (CAP)` error line and the cap in the banner; a venue WITH a running live account cannot be held after the core exists, so the start ends with exit status 78, and `deploy/vike-tradehub.service` now carries a `RestartPreventExitStatus=` line for that status so the unit stays `failed` and pages once instead of restarting into the same cap every 5 seconds. ⚠ An installed unit needs that one line added by hand (`docs/ops/gatewayhub-the CI box.md`'s "One more tradehub line"): until then 78 behaves as any other failure exit and loops. Every other refusal that ends a start (a bad interval, credentials, a lease another client holds) is still a plain failure exit. Not changed: the probe is all-or-nothing across mounted venues (one venue the lane does not serve holds every venue's live accounts), and oanda and ig stay unwatched by the dead-man.
- **The hop's cost is measured, not asserted (decision 0164, D9): `order_hop`, `event_hop` and `price_hop`.** Three ignored tests print `HOP-LATENCY` and `HOP-LATENCY-DELTA` lines against an in-process baseline (`cargo test -p vike-gatewayhub --release --test order_hop --test event_hop -- --ignored --nocapture --test-threads=1`, and `-p vike-datahub --test price_hop`); there is NO CI ceiling. Measured on the latency box (cores 28-31, `SCHED_FIFO`, 10 repetitions, the median of each repetition's p99 difference; the final run is 20 repetitions of the head `83da6d853` against 20 of `a4a51fb5f`, interleaved, and the only commits after it touch test files): an order's submit to the adapter +39 us paced, an event's adapter emit to core ingest +41 us (accepted) and +39 us (fill) paced, a price over both hops +68 us (quote) and +74 us (trade) paced and +282 to +349 us at the busiest feed rates (2,056 and 3,716 a second). The targets (+250 us orders and events, +1 ms prices) are MET at the mean rate and for every price shape (the worst median cell is +339 us; 7 of 300 price repetitions went over 1 ms, against 1 of 300 for `a4a51fb5f`), NOT MET in two stress shapes (16 orders back to back +360 us; 128 events back to back +732 us accepted and +1,084 us fill, throughput-bound), and INCONCLUSIVE for 2,000 events a second (+557 and +685 us under `SCHED_FIFO`, the method of record, +59 and +53 us for the same binary under `SCHED_OTHER`). Every verdict is the same as for `a4a51fb5f`; the two price cells that read 20 to 30% higher in an earlier run were box load (the same old revision measures 13 to 35% lower today), and the accepted-event and 16-order back-to-back cells run a little higher on the new head (a tail effect of about 30 to 50 us, p50 +1.4 us, not separable from code layout). Nothing was lost in any run. The core's own hop (`runtime_latency`, measured at `a4a51fb5f`, whose measured crates are unchanged to the head apart from one word in `crates/vike-core/CLAUDE.md`; 100 paired repetitions against a byte-identical control and layout controls) PASSES against the 10 us budget, with the baseline p99 about 0.5 us: p50 moves +10 ns (baseline) and +20 ns (journal, journal-snap), a layout-sized shift that a cold-code edit to the same code removes, and the journal-snap p999 moves +195 ns of 6.3 us (the snapshot now carries the gateway cursors; p999 has no gate).
- **The gatewayhub reconnects IBKR after the daily logoff, and HOLDS the session when it cannot see what rests (decision 0164, verdict 7(b)).** A supervisor probes the IB Gateway port (every 2 s, backing off to 60 s) and re-reads the account's rows on every attempt (`active = 0`, a changed tier or a `TierConflict` stops it and says why); when the port answers it rebuilds the adapter, the lease ends at the loss so the tradehub redials, learns the new epoch, re-teaches and reconciles, and every order in flight is answered with a terminal reject or an advisory. The bridge now reports its own death when its order-update stream ends, so a loss is acted on at once; the probes are the fallback, and a gateway that returns within about two probes while the bridge also did not notice is still missed (a restart of the gatewayhub is the cure). **The IBKR bridge now LEARNS the venue on every fresh socket connection** (the first of a daemon start included): it asks IB for the open orders, rebinds only the ones THIS client placed (an order made by hand in TWS, another API client's or another account's is ignored), then replays the day's executions (a rebound order's partial stays a partial), announces each rebound working order as accepted, and only then reports the link up. **The symbol is taught before the replay:** the replay waits for the gatewayhub's first teaching of the symbols its ledger holds (an empty one counts), so a replayed fill is booked under the symbol the order was SUBMITTED under and not dropped for the symbol IB echoes; a host that never hands over costs a 20 s fail-safe, after which the replay runs untaught with an `error!`. **The foreign-tag rule:** a tagged row whose client id or account decoded (another API client's order, a note typed by hand in TWS, another account's) is somebody else's and blocks nothing; the learning is judged blind, with no link-up and no replay, only when rows exist, none is ours, and some have client id 0, no account at all and a coid-shaped order reference (what a decoder that decoded nothing returns). A request that fails or is not answered (10 s per request, 30 s for the whole learning) reports nothing; **a failed learning is retried after 2 s, then 5 s (three attempts in all) and then logged `GAVE UP` at error**, and the gatewayhub HOLDS a supervised session whose bridge has not announced the link up within 120 s (every order verb refused with words, `TRADE-LINK: HELD:` at error, on a first start too), until a later announcement releases it. The connection refuses a submit with a terminal reject after waiting 3 s while the open orders are unknown. The gatewayhub keeps a ledger of the orders it sent and has not seen finish, and a rebuild over a non-empty ledger HOLDS the session: every order verb is refused loudly (`TRADE-LINK: HELD:` at error, nothing sent) until the bridge's report accounts for every order of the ledger (`TRADE-LINK: RELEASED:`). An order the bridge did not find lifts a DEMO session with an `error!` naming the coid, and keeps a LIVE session held until an operator cancels or confirms it in TWS and ends the session (the tradehub's final detach, the orphan timeout or a gatewayhub restart); a learning that fails or looks blind (rows returned, none attributed to this client) never lifts the hold. A coid the venue no longer lists is treated as finished and a cancel of it is refused; a fill it took during the outage is recovered from the executions replay by its order reference, as a partial, and books into the position ONLY for an order the gatewayhub itself sent (one only the tradehub knew, after a gatewayhub restart with an empty ledger, is replayed with IB's echo, dropped by the engine and NOT recovered). ⚠ The account's `CLIENT_ID` credential field must be 1 or more: the bridge refuses 0 (TWS's master client, which every order made by hand is bound to), the account does not start and the refusal names `CLIENT_ID`. A cancel is held for up to 5 seconds behind the commission of a fill in flight, and a fill whose commission never comes books with an UNKNOWN fee (0.0, no currency, an `error!` naming it); the fee of such a release-booked fill stays understated by the real commission (about 1 USD per order, nothing books it later), so reconcile fees from the broker statement. ⚠ None of this has run against a real gateway: the release runbook makes the IBKR DEMO account the FINAL step of the rollout, after the live tiers, an attended first run (it reads the position before and after and the bridge's `learned` line; its enabling restart sweeps every active account's book, live included), the LIVE tier stays held, and IBKR live is not in the rollout (`docs/ops/ibkr-socket-gateway.md`'s "After the daily logoff: the gatewayhub reconnects"). The bridge's connect-failure log lines stop saying the venue falls back to PAPER (the account is NOT RUNNING, decision 0156). Known residue: an order recovered only from the executions replay can stay `PartiallyFilled` in the engine though its position is right (a completed-orders request is not built), and an account whose gateway never answers the open-orders request is refused every submit, after the three attempts, until the connection is rebuilt (the host's hold says so after 120 s). A gatewayhub restart with an empty ledger loses the fills of orders only the tradehub knew.
- **`vike-indicators` and `vike-marketdata` get a public API and crates.io metadata.** Every indicator type is named at `vike_indicators::indicators::<Type>` with `new(..) -> Result<Self, ParamError>` (registry order and ranges; an out-of-range or `NaN` parameter is an error, where `make_with` still clamps), `next(&bar)` (an `f64`, or `[f64; N]` for N output lines) and `Default`. `compute(name, &raw, &bars)` returns each output line with its name, `next_named` returns a struct for the nine most used multi-line indicators, and `Bar::ohlcv(..)` builds a plain candle. Both crates ship a README, `readme`/`keywords`/`categories` and a copy of the licence text; `tests/readme_counts.rs` holds the README's counts to the registry and `tests/license_copy.rs` holds the licence copy to the root file.
- **26 new indicators (203 single-series, 211 with the 8 pair indicators):** `kama`, `vidya`, `frama`, `evwma`, `median_ma`, `chandelier`, `hilo_activator`, `atr_trailing_stop`, `ttm_squeeze`, `starc`, `acceleration_bands`, `linreg_channel`, `dx`, `plus_di`, `minus_di`, `plus_dm`, `minus_dm`, `rwi`, `qstick`, `chaikin_volatility`, `williams_ad`, `anchored_vwap`, `heikin_ashi`, `r_squared`, `autocorr`, `hurst`. TA-Lib 0.8.1 agrees with `kama`, `qstick`, `heikin_ashi`, `williams_ad`, `anchored_vwap` (bit for bit or within ulps) and with `acceleration_bands` and `chaikin_volatility` (within 1e-11); the directional rows differ by the seed convention and the gap decays (`crates/vike-indicators/tests/talib_probe.rs`, an ignored report).

### Changed
- **The tradehub links no venue bridge and holds no venue credential value (decision 0164, wave 2).** `vike-tradehub` and `vike-tradehub-core` depend on none of the 14 bridge crates and not on `vike-bridge-core` (the layer fences walk the normal edges). The venue half of `vike-tradehub-core` (the adapter registry, per-venue arming, the startup preflight probes, fee schedules, process claims, the `ExecutionClient` construction) moved into `vike-gatewayhub` as `BridgeTrade`; the tradehub keeps the fold, the core, the reconcile driver, the account plane and the hub server. Its credential store handle is opened with the new `NoVenueKeys` scope: no venue key VALUE (account credentials, venue-owned rows, the aster and polymarket deployment names) can be read in that process, while presence by name, account rows and hub keys still answer. The datahub boots with the same scope, and `VIKE_NO_LIVE_KEYS` no longer narrows the tradehub's handle (the stricter scope wins; no environment spelling selects it). What a bridge says about keys it does not use now appears in the gatewayhub's journal, once per process, not the tradehub's. `docs/crates.md` has the crate map.
- **The gatewayhub reads the account directory, every account's credentials, the five flags it folds and the venue settings anew at each tradehub step, and a read it cannot trust fails CLOSED.** A tradehub restart therefore picks up a filed key, an edited tier, a rotated deployment name, a flag or a venue setting without restarting the gatewayhub; a RUNNING adapter keeps the credentials it started with. A store that will not open, a read that errors or a store the loader marks reads every folded flag as off (`poly_exec`, `poly_reconcile`, `hyperliquid_hip3`, `allow_withdraw_keys`, `preflight_skip`: the guarded states) with an `error!` line, and an unreadable venue-settings table refuses every adapter start (`adapter failed` plus a sentence naming the read), so a transient store error can refuse a Polymarket start (`exec flag unset`) that the in-process code would have kept armed; the next start reads again. An unreadable account table at `TradeOpen` is a retryable daemon error, so the tradehub's first open of such an account reads `adapter failed` with the text where it used to read the table-miss word. The gatewayhub also states a project token (the `gateway-id` file in its state directory) at every `TradeOpen`, and the tradehub refuses to start accounts on a gatewayhub whose token is not its own project's; a gatewayhub that states none only warns.
- **Reconcile, restore and teardown over the gatewayhub hop (decision 0164, wave 2).** A pass the gatewayhub could not attempt (no adapter, link down, the venue's price feed mid-gap) is SKIPPED with an INFO line and never an empty answer or a fetch error, and a retry asks only the legs that were not ready. The startup pass that skipped a leg is retried for just the legs that were not ready, every 5 s for a minute, instead of waiting for the next 60 s tick. A restart presents the journal's cursor so the gatewayhub replays what the tradehub missed; the restore advances a gateway cursor only for a route key that an engine of the base `Snap` answers to (an account armed after the last `Snap` keeps no cursor and opens at the live position, and the startup pass and a held `MissingFill` recover what they can, as before this wave). The restored coids' wire ids are prefetched before the core exists, paged at 4,096 coids a frame with nothing cut, the journal's resting orders asked first and the ownership file folded as the core folds it (its TTL and cap); one failed page keeps the reconcile NOT READY. A gatewayhub restart under a live lease starts the adapter again, teaches the daemon every resting order FIRST, and only then sends the risk-off cancels that arrived meanwhile (they are held and released behind the teaching, after at most 30 s of patience, or before the final goodbye when a stop begins), and reconciles at once. A stop adds at most 3 s (an ack drain and the goodbye, concurrent across accounts) inside the existing shutdown deadline, and the tail now runs in this order: the reconcile driver is told to stop, the core is torn down (the cancel sweep), then the driver is joined within 1 s, so a reconcile fetch stalled inside a venue call cannot hold the cancel sweep back. ⚠ A stored `config.reconcile_policy` of `hybrid` or `synthesize` is OVERRIDDEN with `quarantine` at every start over the gatewayhub and said at `error` (a replayed fill would double-count a synthesized `PositionDrift`; the config write still accepts the words, and `quarantine` and `external-quarantine` are unchanged); a refusal at start would have turned a row written while the daemon runs into a restart loop with orders resting. ⚠ The first reconcile pass can miss the 120 s restored-tag window by arithmetic (2 s startup delay plus four calls at the bridges' 30 s timeout is 122 s); only four full-length timeouts reach it, the constant is untouched, and `docs/ops/reconcile-on-restart.md` has the numbers. The ring replay that a restart asks for is drained from before the core exists into a backlog (so a replay larger than the 4,096-slot lane no longer stalls the boot) and relayed in order once the core's senders arrive; the backlog is capped at 250,000 events (about 12 full rings, bounded at 100 MiB), and past the cap venue events are SHED with an `error` line, which IS a loss of order events in an extreme boot (the startup pass reads what the venue holds). An account that lost one relays without cursors for the rest of the process, so its journal cursor never jumps the gap and freezes at its pre-gap value (a restart replays up to a ring, which the core de-duplicates, or reconciles).
- **The command journal's format is VERSION 18, and an older build refuses a journal this build wrote (rollback: move the journal directory aside first).** It adds one variant to the nested ingest enum, `GatewayEvent` (a venue event that came over the gateway daemon's trade link, journaled as ONE record with its position in the gateway's per-account event ring), and one defaulted `Snap` field, `gateway_cursors` (where each ring stood, per account). Every live start now writes them, because every live venue's events arrive that way (a `paper`-only run writes the bytes it wrote at 17 and only the segment header's version moves), and every journal from 4 to 17 still reads and restores as before. A build that predates 18 reads a segment stamped 18 as a version above its own and refuses to start (`journal format version mismatch: got 18`), so before starting an older binary over a journal this build has opened, stop the trading daemon and move the journal directory aside (`mv "$J" "$J.v17"`; `docs/ops/tradehub-the CI box.md`'s "A rollback needs the command journal moved aside first"), which costs that daemon its replay and restore: reconcile the account out-of-band. A restore hands the cursors back (`BootRestore::gateway_cursors`), a gateway event is read as the event it carries by the tearsheet's fills, the crash tail's terminal facts, the data daemon's materializer and the live-event relay, and `replay_journal` on a production journal still answers `Unsupported` (multi-engine, wakers), so the release runbook records the offline fence as not verifiable.
- **The link dead-man keys its windows by source, and the datahub and the gatewayhub are now part of what it watches.** A window is `(venue, symbol, source)` with three sources: the venue's own lane, the gatewayhub trade link (reserved stream word `gateway-trade`) and the strategy-lane link to the datahub (`gateway-strat`). Each window acts once for its own outage, and a `Live` of one source never erases another's. An outage of ANY armed source longer than the grace (`DEFAULT_LINK_DEADMAN_GRACE_MS`, 120 s) trips `CancelAllAndHalt` on binance, aster, bybit, okx and polymarket as before, and now also on hyperliquid, deribit, alpaca and ctrader, which are armed for the datahub link; the HALT sentinel does not self-clear. The switch arms per venue, not per source, so a gatewayhub outage past the grace trips those four through the account's trade link as well (both hub links, over the 120 s grace; the startup report line and `docs/ops/kill-switches.md` now say so), and a book hole with no anchor for 6 seconds is a third way into the same window (the stale-book entry above). oanda and ig stay unwatched for a datahub or gatewayhub outage (their own socket closes at every weekend and the switch cannot arm a venue for one source only), and so does IBKR (its daily logoff would open the trade-link window every night): with the datahub away no bars arrive for them, the bar-driven margin-call watchdog and drawdown latch stop, and nothing escalates. While a trade link is down the stuck-order ladder and the in-flight confirm defer (counted from the link's return, no cap under a flapping link), so a restart of the gatewayhub or of the datahub is a trading event: the release runbook records the restart sum with the tradehub stopped (the CI box's journal holds single-digit seconds for the datahub's stop and the daemons' starts; the gatewayhub's graceful stop with sessions held is the one unmeasured term).
- **The deploy units change (installed by hand, in place; nothing installs or restarts anything).** `deploy/vike-gatewayhub.service` grants `settings/state` and `settings/db` (the live-account locks, `gateway-id`, parked confirmations, cTrader's rotated pair), raises `MemoryMax=` to 10G and `TimeoutStopSec=` to 35, and `Wants=`/`After=` the three IBKR units (still no `MemoryDenyWriteExecute=`). `deploy/vike-tradehub.service` `Wants=`/`After=` the gatewayhub and the datahub (never `Requires=`), raises `TimeoutStopSec=` to 20 (the CI box's tradehub took 6 to 10 s to stop on 9 recent stops and was killed at its old 10 s limit twice, so the new limit must be loaded before the release's stop; the gatewayhub's 35 is derived from its 18 session ceiling), carries the late arm's exit-status pair and a `RestartPreventExitStatus=` line for the operator-needed exit (see the feed-gate entry); the IBKR lines stay on both units until the soak ends. `deploy/vike-datahub.service` adds `MemorySwapMax=0`, `MemoryLow=5G` and a higher `CPUWeight=` (derived from one measured run, effect on neighbours not measured: the CI box swaps into zram, so the swap cap moves pressure into RAM and onto the gatewayhub and tradehub, and `MemoryLow=` protects nothing unless the ancestor cgroups carry protection). The deploy helper changes comments only. Every unit change is inert under the previous binaries, which is why a rollback needs no unit edit, and the installed units stay hand-derived (a literal install of the tracked data daemon unit would open an empty store); `vike-cli`'s bootstrap-daemon warning reads the tradehub unit's 20 s. A deploy that restarts the gateway under a live tradehub is now a drill of that restart (the helper restarts the gateway before the tradehub). `docs/ops/gatewayhub-the CI box.md`'s "Wave 2 release runbook" is the sitting, in order, with the undo of every half-done state.
- **The preflight report over the hop is the in-process report, and three things differ.** The startup preflight asks the gatewayhub one question per read (plan, clock, credential, network) so its clock re-reads and budgets keep their schedule, and the printed report is byte-identical to the in-process one under a fake clock (`crates/vike-tradehub/tests/preflight_parity.rs`). Differences: the round trip printed for a venue's clock is the question's (a few milliseconds wider, so a verdict can only be more lenient); an unreachable gatewayhub is a WARN on every row, never green; and a gatewayhub dialed at a non-loopback address makes the clock leg `UnmeasuredRisk` (the tradehub and the gateway would not share a clock), though the gatewayhub still refuses a non-loopback bind.
- **A stream session's queue is 16 MiB and the plane's ceiling 512 MiB (they were 4 MiB and 128 MiB), and the tube writes and reads in batches.** Drained frames are written as one batch (at most 250 us, 64 frames or 64 KiB) and the reader is buffered, a payload is scanned for non-finite floats before the session lock, and a `Book` is serialized by reference; the gatewayhub's queue worst case is about 768 MiB inside its `MemoryMax=`. These are the v0.1.59 the CI box findings (the split costs about 11 to 12 points of one core, 35 us a frame at 650 bytes, over half of it system time) answered without a wire change; the per-frame saving is not re-measured, and polymarket's folded `Book` is still about 80% of the tube's bytes.
- **The Dukascopy JForex JVM gets an explicit `-Xmx512m`.** Production starts no sidecar today (the venue is unwired), so this is the heap the day it is wired and for the live smokes; a dev box that relied on its default heap (a quarter of RAM) gets less. No `-XX:+ExitOnOutOfMemoryError` was added.
- **56 indicators stream from bounded state instead of re-running their batch kernel over a retained tail on every bar.** Measured per bar at 20k bars, before to after: `connors_rsi` 1.03 ms to 0.13 us, `t3` 316 us to 30 ns, `kst` 171 us to 0.13 us, `hvol` 5.6 ms to 0.3 us at period 200. `vectorize` and every registry row are unchanged: the 177 existing indicators hash to the same bits on a 1,200-bar adversarial series at three parameter sets, before and after (`tests/vectorize_hash.rs`), and `tests/all_indicators_exact.rs` and `tests/streamers_*_exact.rs` hold every `on_bar` fold to `vectorize` bit for bit at the smallest, default and largest parameters.
- **`next(&bar)` allocates nothing for the 82 hand-written indicators, and a batch kernel builds only the price columns it reads.** `on_bar` used to build a `Vec` and clone it on every bar (two allocations), and every `batch_*` kernel started with `Columns::from_bars`, six passes over ~120-byte `Bar`s even for a kernel that reads only the close. Each hand-written type now computes its value in an inherent `step`, `next` returns it directly and `on_bar` wraps it, and `Columns::needing` builds just the requested columns in one pass. Measured against the same series, machine and build for every library (50,000 bars, median of 41 passes, two runs that agree): streaming `Ema::next` 37 ns to 1.4 ns (ta-rs 1.4), `Rsi` 67 to 5 (ta-rs 4.3, wickra 7), `Macd` 41 to 9, `Atr` 51 to 4.7; a 50,000-bar batch `Ema` 2.4 ms to 0.45 ms, `Rsi` 3.5 to 1.0, `Macd` 5.4 to 1.4. `Sma` (10 ns) and `Bollinger` (32 ns) stay slower than libraries that keep running sums, on purpose: they fold the window again each bar so that `on_bar` stays bit-identical to `vectorize`. No value changed: all 203 indicators hash to the same bits as before at the default, smallest and largest parameters (`tests/vectorize_hash.rs` on both commits, 609 of 609).

### Removed
- **The `polymarket`, `ibkr` and `fxcm` cargo features of `vike-tradehub` are gone; they are features of `vike-gatewayhub` (with `data` and `trade`), and `vike-backend`'s `full` forwards them.** A build that named `-p vike-tradehub --features polymarket|ibkr|fxcm` must name the gatewayhub's. The tradehub's only feature is `telegram`. In a build WITHOUT the gatewayhub's `polymarket` feature, a profile naming `venue = "polymarket"` now passes `validate_for_live` and reaches the gatewayhub, whose arming table answers `FeatureAbsent` (the account is NOT RUNNING with that reason) instead of the tradehub refusing it up front. The shipped `full` build is unchanged.
- **The tradehub's own `LIVE-<route key>.lock` claim, its in-process venue registry and the bridge-fed reconcile health row are removed.** The lock is the gatewayhub's (above); the registry and the `LocalBackend` that read it are deleted, and a degraded CEX feed no longer suppresses a reconcile pass through a bridge status string: the gatewayhub's pump health answers `NOT READY` instead (a venue is degraded while ANY of its pumps is mid-gap, which is stricter than the old row).

### Fixed
- **A snap-only boot restore no longer hands the gateway a cursor ahead of the state it restored, and a redelivered gateway event is dropped.** When the refold cannot run (a labelled account engine, two engines of one venue), the restore folds no tail event into any engine, yet its gateway cursors had been advanced over the tail, so the gateway would never have re-sent a funding payment or fill the crash lost; they now stay at the checkpoint's. The core also drops a `GatewayEvent` of the held epoch at or below the held sequence before it is journaled or folded (it used to be journaled and folded twice; funding has no id to dedup on). Not covered, and pinned by tests in `vike-core`: a late event of an older epoch can still move the cursor back to it (one ordered link per account and a per-session epoch make it structurally unlikely), and a snap-only restore followed by a second crash before the next `Snap` can fold one funding payment twice.
- **An UNKNOWN submit's answer now lands, and a fill that beats its acceptance is no longer lost to the order's state.** When a tradehub restart or a gatewayhub restart loses an order's `OrderSubmitted`, the venue's answer arrives as a bare `OrderAccepted` naming a venue order on an order still `Initialized`; the old fold dropped it, so the order sat `Initialized` until the stuck-order watchdog rejected it locally while the venue held it. The fold now accepts that bare accept (an optimistic accept with no venue id is still refused, so the watchdog still sees that order), keeps a fill or cancel that arrives before the answer and replays it when the answer lands, refuses a fill whose trade id it already holds, and the watchdog asks the venue again, once per grace window, instead of rejecting an `Initialized` order that holds a deferred fill. `OrderSubmitted` alone no longer clears UNKNOWN. The frozen r5 transition table is untouched (`fixtures/r5/` byte-identical). The tradehub's client now repairs the lost prefix of a TERMINAL answer too: when a fill, cancel, expiry or other event the order table takes only from `Accepted` arrives for an order the client holds UNKNOWN, it first sends the implied `OrderSubmitted` and `OrderAccepted` (no venue id, the answer's own timestamp, journaled as plain events; a `warn!` per coid and a `prefixes_synthesized` counter), and for a filled or partially filled wrap it also states the bare `Fill` the wrap carries, so the position, PnL, fee and balance book (de-duplicated by trade id). An accept with no venue id (the resolver's optimistic answer to a failed lookup) leaves the order UNKNOWN, the reader of a restored ring starts only after the UNKNOWN set is marked, and an order whose ending the client already delivered is not marked UNKNOWN again. A bridge's SYNTHETIC cancel (the `OrderCanceled` with no venue time that a cancel of an order the venue no longer holds produces) is NOT a resolution: for an UNKNOWN coid the client has itself cancelled it is delivered as a non-terminal `OrderCancelRejected` advisory and the order stays UNKNOWN, because that answer also fits an order that FILLED (a REST-only venue whose cancel the venue really executed now waits for the lookup, the watchdog or reconcile). The deferred-fill replay is de-duplicated by trade id and the stuck-order watchdog asks the venue again, once per grace window, rather than rejecting an `Initialized` order that holds a deferred fill. ⚠ WHAT STAYS WRONG under the default `quarantine`, because no shipped lookup states a fill: an order that FILLED while the gatewayhub was down ends `Accepted` for ever on bybit and okx (their lookup answers `OrderAccepted` for any order still listed; position FLAT, no fee booked, a ghost live order whose cancel is a swallowed no-op, the coid-linked `MissingFill` HELD), an order cancelled or expired in the gap ends the same ghost, and on ig (its deal reference is gone with the adapter) and the eleven venues without a lookup the watchdog rejects the order with a phantom `OrderRejected("watchdog: no venue ack within confirm-grace")` at link-up plus 60 seconds (30 s, then 2 x 15 s) while the position stays flat; a LATER live fill books the position on that rejected order, and polymarket can never cancel such an order. Only a fill that happens after the new adapter spawned ends fully right. The operator action is `external-quarantine`, the one permitted policy that folds a coid-linked `MissingFill` (the fill only, the order's state untouched), under the root rollout rule; `hybrid` and `synthesize` stay overridden. The bridge fix (bybit's execution list by order link id, okx's fills by order id) is specified in the notes and not built, because it is a coordinated shared-home change. A journal whose live run dropped a bare accept refolds differently under this build (`HashMismatch` on an offline replay).
- **IBKR fills now move the account's position, PnL, fees and balance, for the first time on the socket path.** The IBKR bridge emitted only the order-state wrappers (`OrderFilled`, `OrderPartiallyFilled`) and never the bare `Fill` that `Account::apply_fill` books, so on the old in-process path an IBKR fill advanced the order and left the position flat (demo account on the CI box never moved its position). The bridge now emits the bare `Fill` and its wrapper under one trade id (de-duplicated by trade id), stamps the fill with the symbol the order was SUBMITTED under rather than the one IB echoes (a stock echoed on another exchange or a forex pair spelled the other way round read as a different instrument and the fill was dropped), and learns the symbol of a restored order from the teaching of the host (the gatewayhub's ledger, handed over before the executions replay) and the tradehub's later `adopt_restored`. ⚠ Consequences an operator sees: a position that was flat because the fills never arrived is now real, so read the venue position and the platform's before enabling the account and flatten the venue in TWS first if it is not flat (the runbook's IBKR demo step, the last); and its PnL now feeds the SHARED drawdown latch, which sums every engine of the core and trips on a fall of more than 25% from the summed peak (the seed cash at first), putting EVERY account into liquidate-only (the runbook has the arithmetic). The cpapi backend still has no fill path.
- **One overflow of a stream session no longer ends the session.** On v0.1.59 the CI box a single ~0.5 s stall of the gatewayhub's writer on a full queue produced nine holes (three symbols times `Book`, `BookDelta` and `Quote`), which exceeded the budget of 8 and closed the session, eight `GapStart`s and a re-seed. A lapse is now counted per overflow episode (the holes that start within a second of the first), and every hole is still written and still disclosed. The budget stays 8 in 60 seconds, so a chronic laggard is now cut about 9 seconds into continuous loss. The tube reader's retry wait also left its short read timeout armed after a due retry, so a quiet peer could end the session as "silent" and rebuild it with a spurious `GapStart` on every held key; the full deadline is re-armed.
- **A chart's cumulative indicators continued from the wrong state after a long history.** `ad`, `adosc`, `asi`, `mcginley`, `nvi`, `pvi` and `pvt` were seeded from a retained tail of the history, which is wrong for an indicator that integrates every bar, so the first live bar after loading 3,000 bars was off (`ad`: -88.1 instead of 346.9). They are hand-written streamers now and the chart folds them from bar zero (`crates/vike-chart/tests/indicator_continuation.rs`). The same change removes their O(n) per-bar cost and the unbounded history buffer they kept.

### Security
- **Only the gatewayhub holds venue credential values.** The tradehub's daemon and the datahub boot with scopes that cannot read one (`NoVenueKeys`; the tradehub's pager one-shot `notify_cli` still opens a full-scope handle and reads two deployment keys), the gatewayhub's Trade scope is a separate key that no CLI or desktop admits, and a text gate keeps a hub from opening its own full store and reading a named key. The trade-off is recorded in the runbook: the gatewayhub's `settings/state` grant (needed for the live-account locks) makes the HALT sentinel writable, and so deletable, by the daemon the sentinel is meant to stop; nothing in the gatewayhub's source removes a file outside its tests, and the tradehub already carried the same grant. A rotation of the gatewayhub pair has a CERTAIN refusal window for the datahub (above), and a project-token mismatch is checked on the tradehub's side only.

### Internal
- **CI, gates and the goldens.** `tradehub-all` builds the tradehub, the gatewayhub (`data`, `trade`, `polymarket`, `ibkr`, `fxcm`), the gatewayhub client and the bridges in ONE resolution in `ci.yml`, `release.yml` and the `justfile`, with two new `cargo tree` guards; the `windows-cross` suite cross-checks the gatewayhub with `trade`. ⚠ Three workflow files changed, so the merge queue refuses the PR: merge it by hand with `just merge`. The wire goldens (`fixtures/gatewayhub_wire/`, `fixtures/datahub_wire/`, `fixtures/stream_wire/`) were appended to and no existing line changed; no tolerance was widened. Decision records 0044, 0052, 0091, 0092, 0111, 0112, 0127, 0140, 0146, 0152, 0156, 0164 and 0165 carry dated `Amended` blocks (their `Status:` lines are untouched), and the crate pages carry what the wave learned. Test-only fixes in the wave: the `no_order_vanishes` rig now tells a lookup's re-stated terminal (marked, counted) from a genuine duplicate delivery, the `ibkr_reconnect` tests wait on conditions instead of a fixed sleep (three of them, and the `trade_ring` open-after-close, failed 11 to 30% of runs on a loaded box and now fail in none of 60 to 150 loaded runs), and the two `claim_cycle` tests no longer share a static (red under plain `cargo test`, which nextest hides); no production code moved for them.

## [0.1.59] - 2026-10-14

### Added
- **`config.gatewayhub_addr` is the address of the gateway daemon `vike-gatewayhub` (decision 0164), default `127.0.0.1:7883`.** One row for both sides: the daemon binds it and the data daemon dials it. The daemon refuses a NON-LOOPBACK value outright at startup, with no opt-in flag. Written with `vike-cli config set`, read at the next start. ⚠ A new `config` row is an unknown key to an older binary: `vike-cli config unset config.gatewayhub_addr` before a downgrade.
- **The gateway daemon ships: `vike-backend gatewayhub`, the unit `deploy/vike-gatewayhub.service` and its runbook `docs/ops/gatewayhub-the CI box.md` (decision 0164).** The tool is in the multicall's `full` set and the container image's links (the image supervises no gateway: it runs no data daemon to dial one), the unit carries the trading daemon's whole sandbox except `MemoryDenyWriteExecute=` (the JForex JRE cannot start under it; a gate holds the omission as a declared exemption), grants only its rolling log directory, and is stopped by SIGTERM within a budget a gate reads out of the server's drain constant. `deploy/vike-datahub.service` now `Wants=` and is ordered after it, never `Requires=`. NOTHING installs or enables the unit: it is a hand install, like every unit on the box, and the runbook gives the order (refresh the deploy helper, mint the node keys, install the unit, then release).
- **`vike-cli gatewayhub setup [--rotate]` mints the hub key pair of the gatewayhub, the third service to own hub keys (decision 0164, verdict 6).** The pair is two new platform keys, `VIKE_GATEWAYHUB_OBSERVE_KEY` and `VIKE_GATEWAYHUB_CONTROL_KEY`, appended after the five that existed in `vike_venues::credential_keys::PLATFORM_KEYS`: the gatewayhub verifies them, the datahub holds the same two names to dial it, and no key is shared across services. They are minted the way the other pairs are: CSPRNG bytes straight into the settings database's `hub_key` table, only each key's id printed, one journalled write, an absent store refused, and a pair already held refused without `--rotate`. `vike-cli secrets copy-hub-keys` takes `--only gatewayhub` (a copy with no `--only` now carries the pair too), and the refusals of `secrets set` and `secrets delete` for a gatewayhub key name `gatewayhub setup` rather than the tradehub's `backend setup`. This change only names and mints the keys; it alters no existing key, row or behaviour.

### Changed
- **The settings table `node_key` is `hub_key` from this release (decision 0166); operators rename it by hand BEFORE starting the new binaries, on every project folder that has a database.** No binary migrates the table, no code reads the old name, and there is no fallback: a binary started on an un-renamed database finds NO hub keys (the gatewayhub refuses to start, the tradehub starts without its hub server, the datahub and the backtesthub serve unauthenticated on loopback only). The one-line `sqlite3` command, its reverse for a rollback (run it BEFORE the old binaries start) and what a missed rename looks like are in `docs/ops/tradehub-the CI box.md`'s "The hub-key table is `hub_key` from this release". The platform key names (`VIKE_*_KEY`), the stored values and the `nk-` key ids do not change.
- **`vike-cli secrets copy-node-keys` is `vike-cli secrets copy-hub-keys` (decision 0166).** Same flags (`--from-settings-dir`, `--only`, `--replace`, `--dry-run`), same behaviour, and the old word is an unknown verb; it reads the `hub_key` table of the source database, so a source database must have been renamed first. `--node`, the MCP tools `node_snapshot` and `node_verdict` and the skill names keep their spelling.
- **The names change: "mount" and "node" each stop meaning several things (decisions 0165, 0166, 0167); the renames change no behaviour and no alias keeps an old name.** Crates: `vike-mount` is `vike-tradehub-core`, `vike-node-proto` is `vike-hub-proto`, `vike-connections` is `vike-gateway-panel`, and a new crate `vike-market-stream` (layer 22, on the new `data-plane` tier) holds the one live-data tube the gateway daemon and its clients share: the stream frames, the bounded queue, the writer and the reconnecting `RemoteDataClient`, moved out of `vike-gatewayhub` and `vike-gatewayhub-client` with their tests unchanged. Code: the bridge contract `VenueMount` is `VenueAdapter` (each bridge's `mount.rs` is `adapter.rs` and its struct is `<Venue>VenueAdapter`; the trait method is still `mount`), the datahub's `mount_*` lane starters are `start_*`, `build_node` is `assemble_runtime` (`Node` is `AssembledRuntime`, `NodeConfig` is `RuntimeConfig`), the strategy runs are `PaperMakerCore`, `LiveMakerCore`, `MultiStrategyCore`, `LiveXemmCore` and `PaperXemmCore`, `NodeKeys` is `HubKeys`, and `DataDest::Credentials` is `DataDest::Gateway`. Three display texts changed, and an operator who greps for the old words must follow: the arming word `mount failed` is `adapter failed` (display only, never parsed); the tradehub's `live mount failed: ...` line and the refusals of a live venue's start read `live start failed: ...` and `live start of venue '...' refused: ...`; and the Data Manager's rail row, hover and destination read "Gateway" where they read "Credentials" (same key glyph), with the screenshot slugs `gateway-compact`, `gateway-account` and `gateway`. The published `--help` and MCP tool descriptions say "tradehub" where they said "tradehub node" (no tool name or field changes). Added with the move: `vike_data::delivery` (the per-event delivery class, `Delivery`, and the table of which kind rides which class) and `DataClient::set_delivery` / `DataClient::is_dead` (default bodies, so no bridge changes), so the broker can ask for a stronger class when the recorder holds a key; `vike_venues::credential_keys::gatewayhub_key_pair()` is the one spelling of the gatewayhub's two key names, and the predicates `is_tradehub_node_key` and `is_datahub_node_key` are `is_tradehub_pair_key` and `is_datahub_pair_key`. Kept on purpose: the strategy-slot words (`StrategyMount`, the `mount` and `mount_param` tables, mounting a strategy), `--node`, the MCP tools `node_snapshot` and `node_verdict`, the `config.node_*` rows, the skill names, `vike-node-key-id`, `nk-` and the `venue_mounted` journal kind. The table rename above is the only operator step.
- **Log targets moved with the renames, and some log lines and machine-readable values changed text; update any `RUST_LOG` directive, any console or file level directive stored as a `preferences.*` row, and any Loki or Grafana query that names an old target or line.** A tracing target is the module path (or an explicit `target:` literal), so it followed the module: `vike_<bridge>::mount` is `vike_<bridge>::adapter` (Polymarket's explicit literal `vike_polymarket::exec_plane::mount` is `::adapter`), `vike_mount::*` is `vike_tradehub_core::*` (this carries the shared lines such as "DEMO credentials present → LIVE exec client", the fee schedule and the Binance withdraw gate), `vike_tradehub::node` is `vike_tradehub::hub_server`, `vike_cli::cmd::node` is `vike_cli::cmd::backend`, the datahub's `::mounts` is `::lanes`, `vike_node_proto` is `vike_hub_proto` and `vike_connections` is `vike_gateway_panel`. A directive that names an old target silently stops matching. Log line texts: the tradehub's `vike-tradehub node: ...` lines (connection opened, authenticated, auth denied, refused; about 14) now start `vike-tradehub: ...`, `tradehub node server listening` is `tradehub server listening`, `LIVE build_node core up` is `LIVE assemble_runtime core up`, the datahub, gatewayhub and backtesthub startup line prints `keys=HubKeys(...)` where it printed `keys=NodeKeys(...)`, and the gatewayhub's refusal for missing keys says "hub keys". The lines the deploy probes wait for are unchanged (`vike-gatewayhub: serving`, `vike-datahub: listening`, `backtesthub --addr: listening on`, `{"kind":"ready"}`). Machine-readable output: in `vike-cli config show --json` the `krate` value of every row owned by `vike-mount`, `vike-node-proto` or `vike-connections` is the new crate name (and `config show --env` filters on it), the release asset `bins.json` names `vike-tradehub-core` as the crate of the `incident` bin, one refusal message in `cli.json` says "hub keys", and the incident bundle manifest's `pkg_name` says `vike-tradehub-core`. The published MCP tool descriptions and instructions, the CLI help text and six skill descriptions changed only by "tradehub node" to "tradehub" and "node key" to "hub key"; no tool name, JSON field or skill name changed. Setting names, environment variables, database rows, HMAC domains and key ids are untouched.
- **The deploy helper's roster has a fifth unit, `vike-gatewayhub.service`, restarted immediately before `vike-datahub.service`; refresh the installed copy of the helper by hand BEFORE the first release that contains the gateway.** The helper is root-owned and installed out of band, so a box running the old copy replaces `vike-backend` while a running gateway keeps the old inode (its own `status` names such a unit OFF-ROSTER). A unit that is not installed, or not running, is skipped, so the new row changes nothing on a box that has not installed the gateway. The first release is a bootstrap (the binary with the verb arrives with the release that needs it): `docs/ops/gatewayhub-the CI box.md` says how to get through it.
- **The data daemon makes no venue call of its own: every one goes through the gateway daemon (decision 0164, wave 1).** Its live feeds and the recorder's, backfill and the chart seed, the venue catalog, the Deribit options poll, the polymarket family resolve and the read of the OANDA practice token moved into `vike-gatewayhub`, which `vike-datahub` dials over loopback at the `config.gatewayhub_addr` row. The data daemon links no venue bridge in any build, reads no venue credential and no `venue.polymarket.*` egress row (the gateway declares that egress at its own start, so on a box with two project folders the rows belong in the gateway's), and its features name which venues a build serves and records without linking anything. It signs with the gateway's client key pair, the two names `vike-cli gatewayhub setup` mints (a datahub in another project folder takes the same pair with `vike-cli secrets copy-hub-keys --only gatewayhub`), and the klines, funding and tick fetches behind backfill and the chart seed need the pair's control key. With no pair, or no gateway running, the data daemon still starts and serves its store, and every venue call fails naming the gatewayhub: the recorder records nothing, `--once` exits 4, and a venue none of whose subscribes ever took pages after the `--silent-secs` grace, explicit symbols included (its per-tick warning is `recorder: subscribe failed`). Two things a user sees change: the aster stream of the market-data plane now comes from aster's mainnet hosts (the data daemon's own feed read the testnet ones; the owner's ruling, verdict 7(a)), and the gateway's unit is capped at `MemoryMax=8G`, the data daemon's own cap, because the fetch answers that daemon used to buffer are buffered there now (the unit's comment carries the arithmetic and what an OOM kill does). Install the gateway and mint its keys BEFORE the release that contains this reaches a box; `docs/ops/gatewayhub-the CI box.md` gives the order.

### Fixed
- **A bybit or okx engine's orders now ride the venue's order-rate gate.** The gate existed (`vike_bybit::ratelimit::rest_rate_gate` and `vike_okx::ratelimit::rest_rate_gate`, sized from the per-venue table) and the reconcile clients and every smoke carried it, but the exec thread's own submit client was built on a bare transport, so a runaway submit/cancel loop (a requote storm, a mass cancel) was limited only by the venue's refusal: on bybit that is the per-UID order limit and then an HTTP 403 and a 10-minute IP ban, which the code reads as a bad key. Create, cancel, amend and the startup `set_leverage` of an engine now draw the gate (bybit 18 per second, okx 50 per 2 seconds, the values the table has always declared). A call under the budget is admitted at once, so a box that never bursts changes nothing; above it the call waits for a slot (one `warn` per throttle) instead of reaching the venue. The window is per engine, one symbol each, and the reconcile client keeps its own; the resync, fill-sentinel and confirm re-query clients stay on the gate-exempt path. No setting and no row.
- **The cTrader sockets of one process share one rotating grant.** cTrader replaces its refresh token on every refresh and ends sessions that use a superseded token; inside one tradehub the exec, data and reconcile sockets each held their own copy, so whichever refreshed first invalidated the others and a data-socket refresh could leave a dead pair in the store. An in-process cell now holds the current pair: it is the only place the refresh token is spent, a socket behind it adopts a sibling's pair, and the rotated pair is saved through the existing token store before any sibling can spend again. A data-only cTrader engine now saves its rotations too, and the reconcile socket is dialled after the exec socket. No credential writer was added and no store is read.
- **IBKR history seed: a documented availability letter outranks a positive `mktDataDelay`.** `mktDataDelay` is the milliseconds the request took in IBKR's REST history pages, so a real-time symbol could be labelled Delayed by a slow request; an explicit R, Z or Y letter now wins and a positive number stays the fallback when the letter says nothing. **Alpaca:** the market-data socket now logs the venue's error frame (code and text) instead of a bare "connection lost"; the redial is unchanged.
- **FXCM: the shim's table reads now return CURRENT data, and a failed read is an error, never an empty list.** The ForexConnect shim read Orders, Trades, the account balance and the quote through the login-time snapshot (`getTableRefreshResponse`), which is NULL for Orders and Trades on the demo login: the reconcile client always reported zero orders, zero fills and a flat position (and the restore planner reads an empty report as cancellation), the balance stayed at its login value and a resting order was priced from the login-time quote. `fc_orders`, `fc_trades`, the balance read and the limit-entry quote now send a refresh-table request and wait for the answer; a failed or timed-out refresh returns the reason instead of `[]` and a limit entry is refused before it is sent. Exported signatures, the JSON shape and the ABI version are unchanged; the fix reaches a box only with a rebuilt `libfcshim.so` in a release. The FXCM demo account is a hedging account, so the market round-trip smoke leaves two open trades (documented, not changed).

## [0.1.58] - 2026-10-13

### Added
- **Seven client-side tunings that were compiled-in constants are settings rows now, each defaulting to the value it had, so a box that writes none changes nothing.** `config.node_ack_wait_ms` is how long `vike-cli trade` (the REPL and every one-shot write verb) and the `vike-cli mcp` write tools wait for the node's verdict on a command they sent before saying "outcome NOT known" (default 2000 ms, range 500 to 30000: the ceiling is the control client's own reply timeout; running out of it only changes the wording, never cancels or re-sends the command, and `mcp` serves one request at a time, so the row is also how long a write tool can hold it). `preferences.observe_poll_ms` is the desktop's observe-bridge poll, which caps how often a chart refreshes from the node (50, range 10 to 1000; a backend switch still stops the bridge within one short slice). `preferences.chart_history_bars` is how many bars back a chart reads from the backend's store (3000, range 300 to 4000, the ceiling held under the GUI bar store's per-series cap by a compile-time assertion; the server's own seed depth is unchanged). `preferences.backfill_lookback_days` is the Data Manager's default backfill window for a series with no recorded gap (30, range 1 to 365; still clamped per venue). `preferences.ai_anthropic_model`, `preferences.ai_cerebras_model` and `preferences.ai_timeout_secs` are the Studio chat's two model ids (up to 100 characters of letters, digits and `._:/-`; an id the provider rejects shows as an HTTP error line in the chat pane and changes nothing else) and its whole-request timeout (120, range 30 to 900 seconds). Each value is checked at `config set` and again at the load, and one outside its range is refused, never clamped. `vike-cli config show` lists the six desktop rows as read by `desktop`. **A store holding any of these rows is refused by a binary older than this change, which does not know the key: unset the key before downgrading.** In the code the values are parameters (`vike_ai::make_client`, the store-read and observe-bridge entry points in `vike-app-core`); the `ACK_WAIT` constants of `trade` and `mcp`, `STORE_READ_BARS` and the two `MODEL` constants of `vike-ai` are gone.
- **Five settings rows lift numbers the trade daemon's mounts hard-coded; a box without the rows behaves exactly as before.** Each is written with `vike-cli config set`, read at the next start, and refused outside its bound at the write and at the load (nothing is clamped). `config.paper_slippage` is the slippage, as a fraction of price, a `paper`-tier account's simulated exchange fills at (fees stay the venue's own; read only with `flags.tradehub_live` on, since the daemon's all-paper variant and the cross-exchange maker take theirs from their own mount config). `config.preflight_disk_warn_mib` and `config.preflight_disk_fail_mib` are the startup preflight's free-space floors for the journal directory, in MiB; warn must stay above fail, a key not written counts at its default, and a refusal names both keys. `config.inflight_confirm_ms` turns on the core's fast in-flight confirm, which re-asks the venue about an order stuck unacknowledged on that cadence with a reject-free read and never invents a terminal state (off by default and at `0`; the daemon warns at startup when it is at or above the effective `submit_ack_timeout`, where it can never act). `config.strategy_state_save_ms` makes the core re-save every stateful strategy mount's state on that cadence while it runs, so a crash no longer loses a breaker's trip count or an A-S accumulator (off by default and at `0`, leaving only the save at a clean shutdown; both the live and the paper variant read it). **One-way door:** an older binary refuses a store that carries any of these keys, so unset the key before downgrading (decision 0111, verdict 8).
- **Seven per-box rows tune the data daemon without editing its unit.** `config.recorder_tick_secs` and `config.recorder_silent_secs` are what `--tick-secs` and `--silent-secs` set (the flag still beats the row), `config.recorder_flush_max_rows` and `config.recorder_flush_max_age_secs` are the recorder's flush window, `config.datahub_catalog_ttl_secs` is how long the venue-catalog lane reuses an instrument list, `config.datahub_options_poll_secs` is the options lane's Deribit REST poll and `config.datahub_chain_cadence_secs` is the option-chain recorder's snapshot bucket. An unset row is the compiled-in value, so a box that writes none behaves exactly as before; a value outside a row's range, or one that is not a whole number, is refused by `config set` and, if it got into the database another way, by the unit's `config check` pre-flight, never clamped. The ranges, what each value costs, and the chain cadence's idempotency-key caveat (a near-equal cadence change can drop a snapshot) are on `docs/ops/recorder-deploy.md` and `docs/ops/datahub-the CI box.md`. **Writing any of them is a one-way door:** an older binary refuses a settings store that carries a key it does not know, and no verb deletes a row, so going back past this release means restoring `vike.db` from the nightly backup. `VIKE_RECORD_CHAINS_CADENCE_MS` still refuses startup; its row counts seconds, so nothing is converted.
- **Two connection timings of the data and backtesthub daemons are settings rows.** `config.node_idle_read_timeout_ms` is how long a daemon lets a connection go without a request before it closes it (the data daemon and the backtesthub both read it), and `config.md_heartbeat_ms` is how often the data daemon's market-data stream writes a heartbeat when it is quiet. A box without either row runs the values it has always run, and each is read at the daemon's next start. Both are bounded: a value outside the range in `docs/ops/datahub-the CI box.md` is refused by `config set` and, if a store already holds one, by the daemon's start, with no clamping. The idle timeout bounds the gap between requests, never a request's length, and its shortest accepted value is twice the slowest poll cadence of a client that keeps a connection warm; a client that holds one connection across a longer pause (the desktop's history store) is closed on and fails its next read once, so shorten it knowingly. The heartbeat period is already sent to every client in the stream's reply, so changing it needs no client change. An older binary refuses a settings store that carries either row, so unset the key before downgrading.
- **Per-kind retention and the tool-feed cadences are settings rows of the data daemon.** `config.recorder_retention_days_by_kind` gives each stored kind its own lifetime (`kind:days` entries joined by commas): until now the recorder profile's single `retention_days` applied to every series in the store, backfilled history and the heavy book and depth tapes alike, and deleted whatever was older. A kind the row names uses its own days instead of that scalar, longer or shorter; any other kind keeps the scalar; an absent or empty row changes nothing. **Deletion is irreversible** (the only way back is to fetch the window again), and the row switches it on for a kind even with no scalar set, so the daemon logs a warning at every start while it is on. `exec_fill`, `exec_order` and `exec_funding` are refused by name (execution history is not tape), as are an unknown kind, a repeated kind, zero days, spaces and empty entries, at `config set` and at the load, never clamped. `config.datahub_news_poll_secs`, `config.datahub_calendar_poll_secs` and `config.datahub_equity_poll_secs` set how often the daemon re-polls the news feeds, the economic calendar and the three equity calendars, from a minute to a day, defaults unchanged; the provider quotas they spend are the operator's. A snapshot now reports its feed's cadence (`cadence_secs`, an additive reply field: no version bump, an older client ignores it and an older server's reply decodes as unsaid), and the desktop flags a feed STALE after three of those cycles, never sooner than before, so a slower cadence does not make every window say STALE. Nothing changes on a box that writes none of the rows. **An older binary refuses a settings store that carries any of these four rows: unset the key before downgrading.** Operator pages: `docs/ops/recorder-deploy.md` and `docs/ops/datahub-the CI box.md`.
- **`policy.max_leverage` is a leverage ceiling, and an absent row means no ceiling.** The row was accepted, bounded and read by nothing, with a default that would have capped every box at no leverage had anything read it. It now narrows the run profile's `[risk] max_leverage` once per mount, before the buying-power gate and each perp bridge's account leverage read it, so the two cannot disagree: a profile above the ceiling runs at the ceiling, a profile at or under it is unchanged, a profile that names no leverage is unchanged, and a box with no row behaves exactly as before. A value that is not a leverage (below one times, zero or not a finite number) is refused by name at `config set` and at the daemon's load, never clamped. `config show` and `config set` report the key as enforced and print it unset when no row exists (it printed a default of no leverage that was never a cap), and the boot anchor records it. The daemon's flag-off paper arm builds its core without a `MountPolicy` and is not reached, like every other `policy.*` ceiling.
- **`guards.initial_trading_state` starts the process halted or reducing.** The run-profile key was accepted and applied by nothing. The daemon now sends the spawned core a `SetTradingState` command on both its live and its paper arm before any feed is wired; `active`, the default, sends nothing, so a profile that never names the key is unchanged. A stricter state the journal restored is never loosened by the profile (a halted core stays halted under a profile that says reducing), and the key applies at every start, so a halt set here is undone by an operator's resume only until the next restart. It is the in-process gate state, not the HALT file.
- **`vike-cli config unset <key>` removes one settings row and keeps the integrity seal valid in the same transaction (decision 0161).** A key a release retires (or a row the loader refuses) used to make `vike-tradehub`, `trade`, `mcp` and `config check` refuse the whole store, with nothing able to remove the row: a hand `DELETE` trips the seal's row count and the nightly backup still holds the row. `config unset policy.max_total_exposure` (any `policy.*`, `config.*`, `preferences.*` or `flags.*` key, including one the loader no longer knows) deletes that row and lowers the seal's count by one in one transaction, so the store loads again; `config check` now prints the exact line for each refused row. It never shows or journals the removed value, refuses a credential-shaped key and a `venue.*` key, refuses a store whose seal count is already off (that case is still a restore from the backup, because moving the count would hide the erase), and is restart-to-apply like every local write. The refusal texts and the retired-key messages now name this verb instead of saying the removal is owed. Operator page: `docs/ops/config-unset.md`.

### Changed
- **The client's patience on a bad node link is two settings, raise-only.** `preferences.observe_stall_timeout_ms` is how long the desktop's and `vike-cli`'s observe stream may be silent before the client ends the link and reconnects; `preferences.control_reply_timeout_ms` is how long a control connection waits for the node's reply to a command before giving it up as outcome-unknown. A flaky Wi-Fi or VPN link that tears the stream down repeatedly wants the first raised. Each window can only be RAISED above the built-in one: a value below it, or above the ceiling, is refused at `config set` and at the load (a daemon or tool whose store already holds one refuses it; nothing clamps), because a shorter wait tears healthy links down. A box without the rows behaves exactly as before. The node's heartbeat, the handshake bounds, the pre-auth caps and the one-shot reply waits stay protocol facts, not settings; the `vike-tradehub-client` `liveness` page now says which numbers are which. The desktop reads the rows at start (a changed row takes a restart); `vike-cli trade` and `vike-cli mcp` read them on their own box. **Rollback caution:** an older binary refuses a store that carries either row (the loader rejects a key it does not know), and no `vike-cli config` verb deletes a row, so "unset the key before downgrading" is not possible today: keep these rows out of a store an older binary reads if you may roll back.
- **The link dead-man's way back to its default grace is `config unset`.** The warning for a `policy.link_deadman_grace_ms` row set to zero used to say to write the current default number back with `config set`, which froze today's default into a new row; it now says `vike-cli config unset policy.link_deadman_grace_ms`, which removes the row so the default applies whatever it is.
- **Every byte count an operator reads is now one IEC text (decision 0141, amended).** Base 1024 with the honest labels `B` `KiB` `MiB` `GiB` `TiB`, one decimal above `B`, `0` as `0 B`. The GUI data screens (the Data-Manager grid, the stored-series views, the data rail) said `KB`/`MB`/`GB`/`TB` for a base-1024 figure, so 1536 bytes read `1.5 KB` and now reads `1.5 KiB`. The `vike-cli data hist import` plan's store-growth line divided by 1000 and printed `≈ +1.3 GB`; it now divides by 1024 and prints `≈ +1.2 GiB` for the same estimate. The startup preflight's disk check printed a plain count below 1 MiB (`2048 B`) and `3072.0 GiB` for 3 TiB; it now prints `2.0 KiB` and `3.0 TiB`. The disk floors still compare in bytes, so no threshold moved. The three functions `binary_kb`, `decimal_kb` and `binary_mib_gib` are one, `vike_util::bytes::iec_bytes`.
- **The compute daemon is renamed for its job (decision 0159, #2887, #2890).** The crate `vike-compute` becomes `vike-backtesthub`, the daemon binary and `vike-backend` tool `backtest` become `backtesthub` (`vike-backend backtesthub --addr`), the systemd unit `vike-backtest.service` becomes `vike-backtesthub.service`, its ops page is `docs/ops/backtesthub.md`, and the recipes `just backtest-serve` and `just backtest-tunnel` are `just backtesthub-serve` and `just backtesthub-tunnel`. No alias keeps an old name. `vike-backtest` stays the harness library and `vike-cli backtest` stays the command; the `Run*` wire verbs, the `config.backtest_addr` row, stored `produced_by` strings and the `backtest` cargo feature keep their names. **One-time step on a box that runs the old unit (the CI box):** the deploy installs no unit files and skips a unit that is not installed, so the renamed unit reaches a box only when an operator installs it and stops and disables the old one by hand; decision 0159 leaves that step to the owner's word, and `docs/ops/backtesthub.md` has the install notes.
- **`vike-cli config activate` refuses a `run` body the daemon's own loader would refuse (decision 0157, #2885).** A stored body must load and validate through the same run-profile materialiser the daemon boots from before it is made active; a stale `mode` row or a misspelled key is refused with the loader's own words and nothing is written, and `--dry-run` asks the same question. The arming warning stays. A daemon or recorder body cannot be checked there; `bootstrap-daemon` and `bootstrap-recorder` run their loaders before they store it.

### Removed
- **Four run-profile keys that no mount ever applied are retired, and a stored body that still carries one is now REFUSED by name.** `guards.freshness_ms`, `sinks.gui`, `sinks.recorder` and `sinks.raw_capture_dir` were stored, validated and read by nothing (recording is a recorder profile run inside the datahub, the store's one writer). The daemon's load and `vike-cli config activate` refuse a body holding one, naming it and the cure: re-run `vike-cli config bootstrap-run <name>` without that flag, which replaces the stored body. `bootstrap-run` no longer takes the four as flags, and the shipped sample profiles no longer set them, so a profile started from a sample no longer warns at boot. The table that disclosed such keys is deleted with its last row. Check any box before deploying: a stored run body that carries one of the four refuses the daemon's start until it is re-written.
- **`vike-cli config mirror` and `config activate --proves` are gone (decision 0157, #2884)**, with the eleven example `.toml` files under `docs/ops/`: settings and profile bodies reach the database only from `bootstrap-run`, `bootstrap-daemon` and `bootstrap-recorder`, and `activate` selects a stored profile with no file to prove it against. The run-profile operator page is `docs/ops/run-profile.md`; the venue arming notes that only the example files carried (OANDA's `TimeoutStopSec`, the cTrader warnings) are in git history.
- **The recorder's per-subscription `backfill` policy is gone, and anything that carries it is now REFUSED (#2889).** Nothing ever read it: the recorder loop ignored the field and real gap filling is the datahub `Backfill` verb, which never looks at a subscription, yet `--backfill` filed a word and `record ls` printed it as a column, so an operator saw `backfill=archive` and believed backfill was on. Now `vike-cli data realtime record add --backfill` and `vike-cli config bootstrap-recorder --backfill` fail as unknown flags, a recorder profile body whose `[[subscribe]]` entry has a `backfill` key fails at load with an `unknown field` error naming it (delete the key), and the column is gone from `record ls` in its table and JSON forms. This overrides the accept-what-was-accepted rule of decision 0155 for this one flag, on purpose. No migration: a store keeps its old nullable `subscription.backfill` column and nothing reads it, and a build older than this change cannot read a `vike.db` created after it (its query still names the column).

### Fixed
- **A derivative chart on any venue reaches the fold (#2891).** The desktop kept venue lists from the days it opened its own venue sockets, so a perp, future or option chart on any venue outside okx, binance, bybit, aster and hyperliquid never had its series folded: a Deribit `Option` chart (what the Symbol picker sets) showed `NO FEED` ("this backend publishes no bars for this series") while the log said the backend's store had seeded it. Every class on every venue now reaches the fold (`Combo` still does not) and the chart shows `LIVE`. The dead venue-fed bar plane is deleted with the lists.
- **The deploy pre-flight probe works on a box whose accounts are `demo` or `live` (#2883).** The probe runs `vike-tradehub` in its paper arm over a COPY of `vike.db`, and since the account-tier change in 0.1.57 that arm refuses a mount on a `demo` or `live` account, which is every real box, so no release could be installed. The probe's copy now has its account rows set to tier `paper` (the `paper-accounts` field of the deploy helper's `unit_shape`, set for the trade unit only); the live database is never touched. The helper installed on the CI box already carries this change; the repo copy now agrees with it, so the next deploy's provenance check passes. `/smoke.log` is gitignored: a live-smoke run before a release left it in the runner checkout, and `release.yml` refuses to build a dirty tree.
- **`config.reconcile_startup_delay_ms` has a ceiling (#2893).** The core refuses a restarted strategy's tagged submit while an order it left resting is not yet back in the registry, and lifts that refusal two minutes after its first refusal if no reconcile pass has come. A first-pass delay about that long let the refusal expire before the pass and left two live quotes on one side, one of them unaddressable by tag. A delay above half of that window is now refused at `config set` and at the load (a daemon whose store already holds one refuses to start; nothing clamps it); the ceiling is in `docs/ops/reconcile-on-restart.md`. Nothing changes for a delay at or below it, and the default is far below.
- **A box that refuses reconcile no longer mutes its restarted strategy for a pass that cannot come.** With `flags.reconcile_off` and the restore left on, the core refused the strategy's tagged submit over an order it had left resting "until the first reconcile pass" and lifted the refusal two minutes after the first refusal, so the order was orphaned all the same, later. The live mount now closes the order-restore venue gate for every venue when the operator refused reconcile (`reconcile_config::restore_venue_gate`): nothing waits, the ring names each venue whose restored orders stay unconfirmed, and the refusal's startup line says they are never adopted. This does not save the order: with reconcile refused (or `flags.reconcile_restore_off` set) the order a previous session left resting stays at the venue, unknown to the engine, which is what `flags.reconcile_off` has always cost. A box that reconciles, the CI box included, builds the same gate as before. Also pinned by a test: a restarted maker's own restored-quote clock running out before the first pass cancels nothing at the venue, so it needs no tie to the first-pass ceiling.
- **The trade daemon's client dials with a connect bound (#2889).** `vike-tradehub-client`, the CLI's and the desktop's link to the trade daemon, used a plain `TcpStream::connect`, which takes minutes to give up on a black-holed host. Every node client now dials through `vike_node_proto::frame::connect_bounded` (`CLIENT_CONNECT_TIMEOUT`, then the handshake deadline), and the client's own `HANDSHAKE_REPLY_TIMEOUT` is deleted. The write bound the dial arms is cleared after the handshake, so a session behaves as before.

### Security
- **One classifier decides which names hold a credential (#2892).** Two lists answered that and disagreed in both directions: `config show`, the boot lines, the settings write fence, the trade daemon's settings-write audit and the MCP agent transcript used a suffix list that missed names containing `PRIVATE`, `SEED`, `MNEMONIC`, `WALLET`, `CREDENTIAL` or `_PK`, while the incident bundle's environment dump used a substring list that printed `FXCM_DEMO_USER`, `*_LOGIN` and `*_SIGNATURE` verbatim. `vike_config::redact`'s `SECRET_RULES` is now their union: the incident bundle masks every name it ever masked, or more, and the other surfaces mask what they did plus the new names, except an exact-name exemption list (`VIKE_NO_LIVE_KEYS`, `flags.allow_withdraw_keys`, `flags.datahub_chart_seed`). Visible side effect: the optimiser tool's `seed` argument is recorded as `<redacted>` in the MCP agent transcript.

### Internal
- Hand-copied cross-crate facts have one owner below their readers, with no change to published JSON, serde tags, TOML grammar or values (#2886): `DivergenceKind::resolves_to_events` in `vike-exec`, `Event::TABLE` in `vike-model`, `P99_BUDGET_NS` as a `vike-core` constant (`vike-docs` now depends on `vike-core`), `vike_model::MarginCallConfig` (`ProfileMarginCall` is gone) and `vike_data::source::FillSource` (was `vike_backfill::caps::Source`).
- The node connection limits follow one scheme, `SERVER_*` beside `CLIENT_*` in `vike_node_proto::limits`, and `connect_bounded` is one function in `vike_node_proto::frame` instead of two verbatim copies (#2886, #2889).
- One venue wiring table in `vike-model` replaces the docs class table, the daemon's wired-market table and five bridges' hand-typed probe symbols, and the table the feed plan dispatches on is the one list of feed venues; `venues.json`, the mount and engine order and both refusal messages are byte-identical (decision 0158, #2888).
- The run profile schema and its stored-row loader move from `vike-core` to `vike-config`, its core glue to `vike-mount`; `core_sink`, `journal_view` and `recon_manager` leave `vike-core` for `vike-mount`, and two of `CoreLaneSink`'s log lines now log under the target `vike_mount::core_sink`; `vike-cockpit` becomes `vike_panels::cockpit`; the dead profile-plane code (`plan_active_row`, `ActivePlan`, `WithholdReason`, `write_profile_risk`, `read_profile_risk`) is deleted (decision 0157, #2884, #2885).
- The desktop's dead venue-fed bar plane (`AppMode::ObserveWithFeeds`, `BarPlane::VenueFeeds`, `DIRECT_BAR_VENUES`) is deleted (#2891).

## [0.1.57] - 2026-10-13

### Changed
- **An account runs at its tier or not at all.** An account row says `paper`, `demo` or `live`; a `paper` row runs the simulator, a `demo`/`live` row runs only when it armed at exactly that tier, and every other outcome (no row, `active = 0`, no keys, two active tiers, a connect failure, a declined key, `flags.tradehub_live`/`flags.poly_exec` off) is NOT RUNNING with its reason: no paper engine stands in for it and an order to it is refused. The Trade window shows the tier chip and the reason. `EngineMode` and `WireEngineMode` are gone (the wire carries the tier word and a `not_running` list; the key `mode` is now `tier`, so an older desktop shows `MODE ?`), `VenueMode` is `AccountTier`. **A daemon whose mounted strategy runs on an account that does not run refuses to start**: check that every demo/live account the box mounts has an active row and working keys.
- **A credential belongs to its account, and its name carries no tier.** Bridges read an account's keys by field from `credential.account_id`; the `SIM`/`DEMO`/`LIVE` key-name tokens and `Environment` are gone. `vike-cli secrets set --account <ID> <FIELD>` and `set --venue <V> <FIELD>` write them (value on stdin); a credential write creates no account and picks no tier, and an unknown account is refused. With two unlabelled same-tier dukascopy accounts only the lowest id runs.
- **The run profile loses `mode`.** The kind of run is the program that runs and the tier is the account's; the grid source follows the mounted account's tier, and a demo/live mount whose profile sets venue-owned `[risk]` fields (`tick_size`, `lot_size`, `min_qty`, `min_notional`) is refused. `vike-cli config bootstrap-run` has no `--mode`. **A stored `mode` row now stops the daemon at start; the error prints the `bootstrap-run` command that re-writes the profile without it.**
- `vike-cli secrets account add|set-tier` print the venue's own word for our `demo` (IBKR "paper account", Alpaca "sandbox", Binance/Bybit/OKX "demo trading", ...) and warn when `--tier paper` is used on a venue whose word is "paper".
- Every command line is read by clap through `vike_util::cli`; the DataFusion engine is its own crate, `vike-store`; the HALT sentinel's file half lives in `vike-mount`; `vike-compute` leaves `vike-backtest`; the run-request vocabulary is `vike_model::run_request`; one credential-store handle with `VIKE_NO_LIVE_KEYS` and a typed key schema; the helpers, the project layout, the change journal and the run store are crates.
- Internal: the hyperliquid, okx, ig, oanda, dukascopy, fxcm, aster, polymarket and ibkr crates are split into role folders (no behaviour change); CI: the release validate job runs under nextest, a comment-only YAML edit starts no heavy CI, and the merge queue's tier D and merge-lane read plan logs and verify merged trees better.

### Fixed
- The boot-restore scratch lives under `<settings>/state/restore-scratch`, inside the tradehub unit's `ReadWritePaths`.
- The deploy pre-flight probe confines `config.journal_dir` to its own directory; `build_plugin` prunes scratch crates untouched for two days.
- News sources are reachable from the datacenter and log one line per provider failure.

## [0.1.56] - 2026-10-12

### Added
- **News, economic calendar, earnings, dividends and IPO come from the datahub (decision 0143).** The
  desktop no longer fetches any of them itself: the data daemon polls the five providers lazily on its
  own schedule, keeps the last good rows when a provider fails (the old desktop replaced them with an
  empty list), and answers three Observe verbs (`NewsFeed`, `EconomicCalendar`, `EquityCalendar`)
  negotiated by the `tool_feeds` feature. An old server is refused by name, a build without
  `tool-feeds-serve` answers "not armed". The Finnhub and FMP keys are read from the credential store
  on the datahub box every minute (no restart); `FINNHUB_API_KEY` and `FMP_API_KEY` are now settable
  with `vike-cli secrets set`. MarketWatch is fetched over https; Finnhub's key travels in the
  `X-Finnhub-Token` header. The 26 flags and 16 news-site icons ship inside the desktop (the icons are
  publishers' marks used only to identify a headline's source; `crates/vike-desktop/assets/news-logos/README.md`).
- **Credentials: add, edit, delete, save (decision 0144).** `vike-cli secrets delete KEY --confirm KEY`
  retires a credential (the row and value are kept at rest and every reader sees the name as absent;
  `secrets set` brings it back). The Connections editor has a two-click **Remove** beside each held
  key, and Data Manager > Providers > Data providers sets and removes the Finnhub and FMP keys on the
  server through the new `RetireCredential` / `HeldCredentials` account verbs (they need the node's
  account-admin barrier, `credential-admin` capability).
- **Datahub client reply timeouts.** Every datahub verb has an idle read bound (300 s for
  server-bounded verbs, 10 s Options poll, 30 s catalog route; long runs stay unbounded) and the
  tradehub client's one-shot verbs are bounded at 60 s; a failed exchange marks the client unusable
  so the caller re-dials.

### Changed
- Credential writes name their intent (`UpsertIntent::Replace` / `Add`) instead of passing a
  classifier closure; `Replace` refuses a name the store does not hold.
- The light half of the credential and HALT code lives below the venue plumbing (decision 0135); the
  Options tool reads the datahub (decision 0128) and the desktop opens no Deribit socket.
- CI: `ci.yml` is judged job by job and the feature-suite script arm by arm (decision 0142); the
  datahub and trade daemon builds are verified by one all-features job each.

## [0.1.55] - 2026-10-11

### Added
- **`vike-datahub` can run the journal materializer (off by default).** With the settings row
  `flags.journal_materializer` on and a `config.journal_dir` row naming the trade daemon's command
  journal, the data daemon folds that journal's fills and order lifecycle into the store's
  `exec_fill` / `exec_order` series. Its checkpoint stays in the journal directory, so the data
  daemon needs read and write access there; it warns when it falls behind the journal.
  `docs/ops/datahub-the CI box.md` has the steps.
- **The trade daemon restores its core from the command journal at boot (decision 0127).** When the
  journal is on, `vike-tradehub` reads it once before mounting a venue and restores each engine's
  positions, PnL, fees, balances, order-id counters, conditional and held-exit orders and arm owners.
  The ownership file still decides who owns an order, and the first reconcile pass still decides what
  rests at the venue. A torn tail is kept aside under `torn/` and the read stops cleanly. A journal
  that cannot be read is moved aside, loudly. A journal this build cannot read at all refuses the
  start. With the journal off, nothing changes.
- **Trade history in the desktop.** The Tearsheet window gains Fills, Orders and Summary tabs over
  the datahub's `exec_fill` / `exec_order` series, with venue, symbol and time filters. Each empty
  state says why it is empty: no datahub, a datahub too old to serve it, or recording off. Stored
  fills now also keep the venue's order id and the account, and are left empty where a venue gives
  none; an import from a venue export keeps both ids.
- **The desktop's Options tool reads its chains from the datahub (decision 0128).** The desktop
  opens no Deribit socket of its own any more. The Greeks tool builds its option index from the
  datahub's venue catalog, which now also lists Deribit's USDC-settled options (SOL among them).
  An older datahub does not serve the chain verb, so deploy the datahub first.
- The settings row `flags.record_chains` records option chains into the store, now proven end to
  end. A process that runs no store maintenance refuses the recorder with a warning (it would
  never compact what it writes) and still serves the chains.
- `vike-indicators` gains one realized-volatility family: EWMA, close-to-close, Parkinson,
  Garman-Klass, Rogers-Satchell and Yang-Zhang, as six registry indicators. The option-basket legs
  name their right as a call or put, and the Deribit option ticker keeps every field the venue
  sends (sizes, last, bid and ask IV, the venue's own greeks, index price).

### Changed
- The journal materializer moved from `vike-journal` to `vike-datahub`, the one process that writes
  the store (decision 0084). `vike-journal` no longer depends on `vike-data`.
- **The command journal's directory is the `config.journal_dir` row alone, for both daemons
  (decision 0111 amended).** A run profile's `[sinks.journal]` only tunes the journal that row turns
  on. A stored run profile that still carries `sinks.journal.dir` refuses startup by name, with the
  fix in the message.
- **The command journal's format is VERSION 17**, which adds the route key the boot restore matches
  engines by. Version 16 journals still read. Rolling back to an older build needs the journal
  directory moved aside first; the trade daemon's ops runbook has the step.
- **An unknown order kind is refused instead of becoming a market order.** The order type is now
  one typed value. Alpaca, OANDA, IBKR, IG and Binance perpetuals used to send an order kind they
  did not know as a market order; the pre-trade check, the trade daemon's command door and the MCP
  `submit_order` tool now refuse it by name. An order adopted from a venue keeps the venue's own
  word. A resting Polymarket CLOB order now reports its type as a limit, not as its time in force.
- **One account-tier vocabulary (decision 0129).** An account tier is exactly `paper`, `demo` or
  `live` everywhere, and `sim` is no longer accepted as a tier word; the `SIM` token in a credential
  key name still means paper. The IBKR gateway's `--tier` is strict too. Order reports carry a
  typed status and order type, and a venue status word a bridge does not know is warned and treated
  as still live, never as a fill or a cancel. The trade daemon sends order statuses as the order
  state word (such as `FILLED`); the desktop and the eval harness read both spellings.
- **IBKR bars take any interval IB can build.** The live feed used to turn every interval outside a
  five-word list (15m, 4h, …) into one-minute bars. It now accepts any positive multiple of IB's 5 s
  bar and refuses the rest; a history request IB has no exact bar size for is refused instead of
  being filled with a nearby size.
- cTrader instruments take their class from the broker's asset class and their base and quote from
  its asset list, not from the symbol's name: BTCUSD is a crypto CFD. Deribit option names are
  looked up in the venue's catalog instead of being parsed.
- Telegram's `/submit` accepts `b` and `s` for the side, as the CLI does.
- Fewer dependencies: 14 unused edges dropped, and the IDN host adapter is back to its
  zero-dependency stub (21 packages fewer). A non-ASCII host name no longer parses; every host
  the workspace dials is plain ASCII.

## [0.1.54] - 2026-10-11

### Added
- **Orders a previous session left resting go back into the engine registry at the first successful
  reconcile pass (decision 0120).** For a venue whose row in the declared table is enabled, the
  strategy that placed an order hears its cancel or rejection after a restart, the engine can
  cancel it by id, and an order the venue no longer holds is reported to the strategy as a cancel.
  It is switched off by the settings row `flags.reconcile_restore_off`. `SpreadMaker` now saves the quote it has
  resting and continues it after a restart instead of placing a second one. The ownership file
  gains optional venue, symbol, account and tag fields on each record; older lines still load.

### Removed
- **The last old-path reads (decision 0117, there are no migrations).** `vike-tradehub` no longer
  reads `<exe_dir>/telegram_updates.log`; the Telegram ledger is `settings/state/telegram_updates.ledger`
  only. The Studio no longer reads `studio_workspace.json` from the hist store root, only from
  `settings/state/`. The never-called one-time conversion of `studio_strategies.json` into
  `user_data/strategies/` is deleted.
- The `migrate_to_group` backfill tool and its library half `DataFusionHist::migrate_series_to_group`
  (decision 0117: no migrations). A per-symbol tick series stays readable as it is; nothing on any
  measured store was left for the tool to fold.
- Decision 0117: a credential row named after a retired arming switch (`BINANCE_MAINNET`,
  `BYBIT_MAINNET`, `OKX_MAINNET`, `HYPERLIQUID_MAINNET`, `POLY_EXEC`, `POLY_RECONCILE`) no longer
  refuses startup, since nothing reads it; and `vike-report`'s unused store-first fill read with
  its journal fallback is gone (a tearsheet's journal is `--journal DIR`).
- `VIKE_LOG_FILE_LEVEL` refuses startup: the rolling log's level is the `preferences.log_file_level`
  row on every binary, the backfill tools and one-shot `backtest` runs included (they keep their own
  default when the project has no row). Write it first, then delete the unit line:
  `vike-cli config set preferences.log_file_level warn`.
- History manifests in the old v2 format are no longer read; every series on the deployed box was
  converted first. A v2 file now fails with a message saying this build reads only v3 and that the
  series' manifest can be rebuilt from its parts with `vike-cli data hist repair`.

## [0.1.53] - 2026-10-10

### Removed
- **Decision 0111, completed: no setting is read from the environment any more.** Each variable
  below now refuses startup, naming the row or argument that replaces it. Write the row with
  `vike-cli config set <key> <value>` BEFORE removing the line; `docs/ops/` has the order for a
  running box.
  - Reconciliation: `VIKE_RECONCILE` -> `flags.reconcile`, `VIKE_RECONCILE_OFF` ->
    `flags.reconcile_off`, `VIKE_RECONCILE_BALANCE` -> `flags.reconcile_balance`,
    `VIKE_RECONCILE_GENERATE_MISSING` -> `flags.reconcile_generate_missing`,
    `VIKE_RECONCILE_POLICY` -> `config.reconcile_policy` (an absent row is `quarantine`),
    `VIKE_RECONCILE_INTERVAL_MS`, `_AUDIT_MS`, `_LOOKBACK_MS`, `_STARTUP_DELAY_MS`,
    `_BALANCE_TOL_ABS`, `_BALANCE_TOL_REL` -> the matching `config.reconcile_*` rows.
  - Trading daemon: `VIKE_TRADEHUB_LIVE` -> `flags.tradehub_live`, `VIKE_TRADEHUB_CONTROL` ->
    `flags.tradehub_control`, `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND` ->
    `flags.tradehub_allow_public_bind`, `VIKE_TRADEHUB_ADDR` -> `config.tradehub_addr`,
    `VIKE_TRADEHUB_ADVERTISE_ADDR` -> `config.tradehub_advertise_addr`,
    `VIKE_TRADEHUB_ACCOUNT_ADMIN` -> `config.tradehub_account_admin`,
    `VIKE_TRADEHUB_CONTROL_RATE` -> `config.tradehub_control_rate`, `VIKE_TELEGRAM_CONTROL` ->
    `flags.telegram_control`, `VIKE_PREFLIGHT_SKIP` -> `flags.preflight_skip`,
    `VIKE_CANCEL_ORDERS_ON_SHUTDOWN` and `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT` -> their `flags.*`
    rows, `VIKE_PIN_CORES` -> `config.pin_cores`, `VIKE_JOURNAL_DIR` -> `config.journal_dir`,
    `VIKE_JOURNAL_SNAPSHOT_EVERY` -> `config.journal_snapshot_every`, `VIKE_INSTANCE_ORIGIN` ->
    `config.instance_origin`, `VIKE_STYLE` -> `preferences.chart_style`, `VIKE_LOG` ->
    `preferences.log_level` (`RUST_LOG` stays).
  - Data daemon: `VIKE_DATAHUB_ADDR` -> `config.datahub_bind_addr` (a client dials
    `config.datahub_addr`), `VIKE_DATAHUB_ALLOW_PUBLIC_BIND` -> `flags.datahub_allow_public_bind`
    (the compute daemon reads the same row), `VIKE_DATAHUB_LIVE` -> `flags.datahub_live`,
    `VIKE_DATAHUB_LIVE_RESIDENT` -> `config.datahub_live_resident`, `VIKE_DATAHUB_CHART_SEED` ->
    `flags.datahub_chart_seed`, `VIKE_DATAHUB_ADVERTISE_ADDR` -> `config.datahub_advertise_addr`,
    `VIKE_DATAHUB_VENUE_CATALOG_OFF` -> `flags.venue_catalog_off`, `VIKE_DATAHUB_STORE` and
    `VIKE_HIST_STORE` -> `config.store_root`.
  - Compute daemon and tools: `VIKE_BACKTEST_ADDR` -> `config.backtest_addr`, `VIKE_SWEEP_THREADS` ->
    `preferences.sweep_threads`, `VIKE_BACKTEST_NAMED_RUN` -> `backtest --addr --named-run`,
    `VIKE_API_BASE` -> `vikedata_backfill --api-base <URL>`, `VIKE_COUNTERS_FILE` -> the
    `vike_stat` argument.
  - Desktop: `VIKE_EXPORT_DIR` -> `preferences.export_dir` (new row), `VIKE_MAX_ORDER_QTY` ->
    `preferences.max_order_qty`, `VIKE_RECORD_PROPERTIES` and `VIKE_RECORD_CHAINS` ->
    `flags.record_properties` and `flags.record_chains`.
  - Dropped with no row: `VIKE_STATE_ROOT` (the state tree is always `<settings>/state`),
    `VIKE_ALERTS` (`<settings>/state/alerts.json`), `VIKE_RECORD_CHAINS_CADENCE_MS`; the warnings
    for `VIKE_RECORD_DVOL` and `VIKE_DATAHUB_VENUE_CATALOG` became refusals.
  - Still read: `VIKE_SETTINGS_DIR`, `VIKE_CREDENTIAL_SCOPE`, `VIKE_LOG_DIR`, `VIKE_LOG_FILE_LEVEL`,
    `RUST_LOG`, the credential and node-key variables, and the test, build and desktop developer
    switches.

### Changed
- **The strategy builder takes `--port`, `--out-dir`, `--retain` and `--workspace-root` on its
  command line** (from `ExecStart=`) instead of `VIKE_STRATEGY_BUILDER_PORT`, `_OUT_DIR`, `_RETAIN`
  and `_WORKSPACE_ROOT`, which now refuse startup. A malformed value is an error naming the flag.
- **`vike-backend datahub --store DIR`** serves (and records into) DIR for that run, above the
  `config.store_root` row. A key-less datahub on local files is now
  `vike-backend datahub --store DIR`.
- The container image no longer bakes `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND`: its entrypoint starts
  `trade --allow-public-bind`, gives the builder `--workspace-root`, and the thin image takes its
  node address as the container argument.
- The tracked units (`deploy/`) carry no retired variable; the data daemon's store is `--store` on
  its `ExecStart=`. `deploy/sbin/vike-trader-ci-deploy` must be reinstalled by hand: its probe
  confines the data daemon with rows and passes the builder its arguments.

### Added
- **MCP requests are cancellable.** `notifications/cancelled` aborts a `run_backtest`, `run_sweep` or
  `run_walk_forward` call: the server closes its connection to the compute daemon, sends no response
  for the cancelled request, and keeps answering everything else. The daemon notices the closed
  connection and stops the run: a sweep stops scheduling grid points, a walk-forward stops at its
  next window or grid point, a single backtest before its next data load or replay (a run already
  inside the engine finishes). No wire change.

### Fixed
- **bybit `publicTrade` frames deliver every trade**, not only the first row of the frame.
- **OKX `trades` frames deliver every trade** too (an unreadable row is skipped, the rest stay).
- **`vike-tradehub`'s stdio loop no longer acks a command the core never received**: it answers
  `{"kind":"error","error":"core_gone"}`; an execution actor logs one warning when the core is gone
  instead of dropping events silently.
- **`events_api_backfill` holds a bounded number of fetched days in memory** (a slow store used to
  let finished fetches pile up without limit).

## [0.1.52] - 2026-10-10

### Added
- **Decision 0111: no setting lives in the environment or a TOML file.** Settings are rows in the
  settings database. This release adds the rows and the registry gate; the environment still wins
  where a variable is set (the next release deletes those reads).
  - `config.reconcile_policy` (`hybrid`, `synthesize`, `quarantine` or `external-quarantine`; any
    other word is refused), `config.reconcile_interval_ms`, `_audit_ms`, `_lookback_ms`,
    `_startup_delay_ms`, `_balance_tol_abs`, `_balance_tol_rel`, `config.tradehub_control_rate`,
    `config.pin_cores`, `config.journal_snapshot_every`, `config.datahub_bind_addr`,
    `config.datahub_live_resident`, `flags.datahub_live`, `flags.datahub_chart_seed`,
    `flags.datahub_allow_public_bind`, `preferences.max_order_qty`.
  - `preferences.log_file_level` is read by the data and compute daemons too (default unchanged).
  - **`vike-cli config bootstrap-run <name> --mode <m> [--risk.<key> <value> …]`** writes the
    daemon's run profile into the database and activates it (`--dry-run` only shows what would be written).
- **A second IB Gateway for the LIVE (real-money) IBKR account.** `vike-cli secrets ibc-start
  --tier live` (default `demo`, unchanged) logs in from `IBKR_LIVE_USERNAME`/`IBKR_LIVE_PASSWORD`
  (new credential names `secrets set` accepts) with IBC `--mode=live`, and
  `deploy/ibkr-gateway/{write-ibc-config,start-gateway,stop-gateway}.sh live` run it as its own instance:
  API port (`IBKR_GW_LIVE_API_PORT`), its own ini, Gateway settings directory, log and pid file.
  The live pair is on the child's argv for the gateway's lifetime (the same accepted exposure as the
  demo's). The login needs a phone tap and is never retried; the start refuses while a Client Portal
  Gateway runs on the box, and `stop-gateway.sh` without an argument no longer touches the live one.
  The scripts are copied to a box by hand (`docs/ops/ibkr-socket-gateway.md`, "The LIVE gateway").

### Changed
- **`vike-cli secrets migrate --init` is now `vike-cli secrets init`** (with `--dry-run`). It creates
  the EMPTY settings database exactly as before; plain `secrets migrate`, which created nothing, is
  gone. Images up to 0.1.51 still spell it `secrets migrate --init`.

### Removed
- **`--profile` and `VIKE_RUN_PROFILE` for the daemon's run profile.** The profile comes only from
  the active `run` row; the variable is refused at startup naming `vike-cli config bootstrap-run`.
  `vike-cli config mirror --profile` is gone. Backtest profiles passed to `vike-cli backtest` are
  unchanged.
- The legacy `MAINNET` credential tier. Venues take `DEMO` or `LIVE` credential names only; a `{VENUE}_MAINNET_{FIELD}` name is an unknown name.
- **There are no migrations** (`docs/decisions/0117-there-are-no-migrations.md`).
  `vike-cli secrets move-venue-config` is gone. A credential row under a venue setting's old name
  (`IBKR_DEMO_PORT`, `POLY_RATE_GATE`, …) is read by nothing and no longer stops the trading or data
  daemon, and `vike-cli config check` no longer fails on one; `secrets set` still refuses to write
  one and prints the `config set` line instead. `vike-cli config compare`, `adopt` and
  `migrate-store` are unknown verbs now.

## [0.1.51] - 2026-10-08

### Changed
- **The IB Gateway's login is taken from the settings database at every launch, and no file keeps a
  copy.** `deploy/ibkr-gateway/start-gateway.sh` now launches IBC through the new
  `vike-cli secrets ibc-start`, which reads `IBKR_DEMO_USERNAME`/`IBKR_DEMO_PASSWORD` and runs IBC's
  `ibcstart.sh` with them as `--user=`/`--pw=` words, so a password rotated with `vike-cli secrets
  set` is the one the next launch sends instead of a stale plaintext copy that IBKR answers by locking
  the account. ⚠ **The pair is visible in `ps` to every user on that box for as long as the gateway
  runs**: IBC can take a login privately only through a file, and the owner ruled that no file of any
  kind may hold it. The verb itself prints, logs and journals no value and no script holds one.
  `write-ibc-config.sh` now renders `ibc/config.ini` with the login BLANK (and says so when it
  replaces a file that held a plaintext password); `IBKR_GW_SECRETS` is gone. There is no tier flag:
  the gateway logs in to the DEMO (paper) account, the one tier the ibkr mount arms; a store with only
  a live pair, or a deactivated demo account, is refused. The Client Portal login is `vike-cli
  secrets ibkr-cp-login` (the pair in the driver's environment only), and `run-login.sh` no longer
  sources a `.cpcreds`. Remove the old `ibc/config.ini` password (re-run `write-ibc-config.sh`),
  `bin/ibkr-gateway/settings/secrets.env` and `bin/ibkr-cpapi/.cpcreds` once the first launch from
  the database has worked.

## [0.1.50] - 2026-10-08

### Added
- **`vike-cli secrets set` writes a closed list of credentials that are not venue keys** into a store
  that never held them: the collector and data-API keys (`VIKE_API_KEY`, `VIKE_ARCHIVE_API_KEY`,
  `DATABENTO_API_KEY`, `TARDIS_API_KEY`), the pager (`VIKE_ALERT_*`), the Telegram control bot
  (`VIKE_TELEGRAM_*`), the builder fees, the JForex tools, aster's TESTNET wallet and the Studio chat
  keys (`ANTHROPIC_API_KEY`, `CEREBRAS_API_KEY`). The list is fixed in code; a typo of a listed name
  is answered with the real spelling. The value still arrives on stdin or from a named variable,
  never on the command line.
- **`vike-cli secrets copy-node-keys --from-settings-dir DIR [--only tradehub|datahub] [--dry-run]
  [--replace]`** copies the node keys from another project's settings database into this one, so a
  service moved to a new project keeps the keys its clients already hold instead of minting a new
  pair. Database to database: no file is read or written, nothing prints a value, and a differing
  key already in place is refused unless `--replace`.
- **`vike-cli secrets migrate --init`** creates the EMPTY settings database on a fresh box by an
  explicit act, so a new install no longer has to migrate a blank credential template.

### Changed
- **⚠ The credential FILE store is removed.** `secrets.env` and `node.env` are no longer read as a
  store: the settings database (`<project>/settings/db/vike.db`) is the only one. A box that has never
  migrated must run `vike-cli secrets migrate` once (it still reads the old files, read-only, to carry
  them over); a keyed credential file with no database is reported as NOT READ by the daemon, `config
  check`, `secrets list` and `secrets path`, and nothing is armed from it. `secrets template` and the
  file writer are gone.
- **The Studio chat's keyless message** points at `vike-cli secrets set ANTHROPIC_API_KEY` instead of
  a `.env` file.
- **The container image** puts `/opt/vike/bin` on `PATH`, words the no-database warning correctly and
  treats `settings/state` as the daemon's state directory.
- **Internal:** crate-layout and test de-duplication refactors in `vike-tradehub`, `vike-node-proto`,
  `vike-secrets` and `vike-ops`, and CI/merge-queue changes. No behaviour change.

## [0.1.49] - 2026-10-07

### Fixed
- **`vike-cli backtest` reads `--archive` correctly.** A path containing a space was split in two, and
  `backtest sweep.toml --archive dir` took `dir` for a second profile. An `--archive` path that does
  not exist is now refused instead of running a report over no data, saving a run directory and
  exiting 0.
- **The daemon's "cannot read the settings store" messages state the real reason.** They claimed the
  settings directory was read-only in the daemon's mount namespace; the cause is that the daemon opens
  the store read-only, and a read-only open cannot replay a rollback journal. The message text is the
  only change.

### Changed
- **The four tracked `settings/*.toml` files are gone.** No binary read them since decision 0086 made
  the settings database the only store; editing one never changed anything.
- **Internal:** crate-layout refactors (no file over 1,000 lines, role folders, a page per crate) in
  `vike-datahub`, `vike-tradehub`, `vike-marketdata`, `vike-config`, `vike-backtest`, `vike-analytics`,
  `vike-alerting`, `vike-mount`, `vike-buildinfo` and `xtask`, plus CI planning changes (the planner
  selects less, the release runs every suite, the `test` job has its own runner). No behaviour change.

## [0.1.48] - 2026-10-06

### Added
- **The data server serves Deribit's order book.** The Trade window showed "NO DEPTH FEED FOR THIS
  VENUE" for Deribit's BTC-PERPETUAL; the datahub now serves a depth lane for it (the top 200
  levels, folded from Deribit's 100 ms book channel on a second socket), and the Deribit catalog
  lists every perpetual, dated future and spot pair, with spot classified as spot rather than as a
  derivative. A subscribe Deribit refuses (an unknown instrument is answered with an empty result
  and then silence) is now reported as a gap with one throttled warning instead of an idle feed.
  - **This needs the release on the data server's box.** An older server keeps answering "no depth
    feed" for Deribit.
- **The ladder remembers its Group for each instrument.** The Group (1 / 2 / 5 / 10 / 25 / 50) used
  to fall back to 1 whenever the symbol or the account changed; it is now kept per venue and symbol,
  so setting BTC to 10, looking at ETH and coming back leaves BTC at 10. Changing only the account
  keeps the Group, and an instrument seen for the first time starts at 1.
- **Each tool in the launcher has its own icon.** Chart (two candles), Account (a person), Trade
  (a bolt), Polymarket (a pie with a slice) and Greeks (a delta) no longer borrow the chart's or
  the options icon.

### Changed
- **Connections is part of the Data Manager.** The Connections window's two tabs, Credentials and
  Backend, are now destinations in the Data Manager's left rail, under CONFIGURE, in the place of
  its old Venues destination. Credentials shows, for one venue, the ceiling toggle, whether a
  credential is present and what actually mounted, so one screen states all three. The separate
  Connections window is gone: the launcher's Connections row opens the Data Manager, and the Trade
  ticket's Connect click opens it at Credentials.
- **The Trade ticket grows with the text size, and the window opens as much wider.** The ticket is
  260 pt at Small, 286 at Standard and 338 at Large, so the five quick sizes stay on one row from
  Standard up. The window opens 26 pt wider at Standard (626 pt, and 678 at Large) rather than the
  ladder paying for it, and the ladder keeps the width it has at Small.
- **The older windows follow the live theme.** Account, Calendar, News, Greeks, Tearsheet,
  Connections, the indicator picker and the symbol row took fixed colours and now read the active
  theme: on Graphite and Classic the neutrals, accent shapes and market colours are unchanged, and
  on the other themes these windows now follow the theme. An oscillator's level lines follow the
  theme too, and the indicator dialog gains a "Use the theme's colour" revert for a level you
  coloured by hand.
- **DEMO is outlined in the info blue**, as in the v3 design, where it was amber. LIVE keeps the
  accent fill and PAPER the grey outline, so the mode still never rests on colour alone.
- **A word is no longer painted in the accent colour.** Twelve places drew a word in the accent;
  the word now takes the text colour and the accent stays a shape (an edge, an outline, an
  underline). Six draws drop to regular weight and three bold ones to semibold, under the ruled
  font weights.
- **Spacings and line widths that sat off the design's scale moved onto it.** The gaps and line
  widths typed as numbers in the windows now name a step of the design file, and the few that were
  between steps moved by 1 to 4 pt (ties go up); study lines are 1.5 wide where they were 1.4. The Trade
  window's view-toggle gap and Vol bar inset are the design's 5 pt, and the ladder ring and the
  crosshair, minus and plus glyph strokes take the design's widths.
- **A rail row held down is filled like the selected row**, without the accent edge, instead of
  looking like a hover: it reads as "about to become selected", and a press dragged off the row
  stops showing it.
- **The IBKR socket bridge moves to ibapi 4.2.0.** The port keeps behaviour (an absent size grid
  still reads as the inert zero, and the renamed good-till time-in-force values map across). It was
  run against a real paper gateway: delayed
  quotes arrived, and a far-from-market order was submitted, accepted and cancelled with its
  client order id intact on both terminals. A fill was not observed, as before.

### Fixed
- **The Data Manager's Activity log footnote no longer lands on the foot line.** The log panel took
  every pixel the body had left, so the footnote drawn after it fell below the body's bottom edge
  and overprinted "N series indexed · M with gaps".
- **Picking a level colour no longer moves the indicator dialog.** The revert button is taller than
  the colour well, so the row grew by 3 pt when a level gained a colour and everything below it
  moved under the pointer; every colour row now keeps the same height with or without an override.
- **Calendar numeric columns keep an 8 pt gap** from the left-aligned column after them, so
  "0.52%Quarterly" no longer touches in Earnings, Dividends and IPO.

### Note
- **The live core was reorganised with no change to what it does.** The runtime's files were split
  by role (core assembly, the reconcile pass, the routed-intent arms), with the bodies moved
  verbatim, and characterization tests now cover reconcile and apply branches no test reached. The
  one code change is that the accounts-epoch sort runs on the stack, so a publish no longer
  allocates for it (the value is unchanged and the measured latency is neutral).
- **Crate layouts changed with no behaviour change.** The model, data, indicators, secrets,
  connections, CLI, studio and test crates had their oversized files split by role, as pure moves
  with their tests grouped, and each gained or updated its own crate page.
- **The minor and patch cargo group (two crates) and the CI Rust toolchain action were bumped**,
  beside the ibapi change above.

## [0.1.47] - 2026-10-05

### Added
- **The Trade window is rebuilt to the owner's v3 design.** The instrument bar, the ladder, the
  ticket and the tick chart are drawn the way the design draws them: a one-line account menu (the
  venue, then its account in a dimmer, smaller type, then the mode chip), a flat symbol picker with
  one row per instrument and venue, a ladder with the Vol column and order pills, a compact ticket,
  and a tick chart whose heatmap runs from cyan to amber on a percentile scale, with market-coloured
  lines and bubbles. The chart's price rows are the ladder's.
- **Text size has three sizes: Small, Standard and Large.** Small is the scale Standard used to be
  (10 / 11 / 12 / 14 / 18 / 24 / 40 pt), Standard is now the scale Large used to be and is the
  default (11 / 12 / 13 / 15 / 19 / 26 / 40), and Large is new (13 / 14 / 15 / 17 / 22 / 30 / 44).
  The setting now reaches every window: Connections, its Backend tab, Account, Calendar and News took
  their sizes from numbers typed into them and ignored it until now.
- **The design values live in one file, and the brand book is drawn from it.** Colours, type
  scales, densities, the window bar, market colours, the heat ramp, the brand orange, the corner
  radius and the spacing, stroke and strength steps are data; the Rust constants, the CSS and a
  brand-book page are generated from that file and compared with it byte for byte by a test. The
  rules of use (which face and weight for what) name the tests that enforce them, and a new
  semibold or bold outside the ruled places fails a gate.

### Changed
- **Every window has the same 25 pt title bar** at every density, with a small name and 20 pt
  controls, instead of the shell's 30 pt bar beside the Trade window's own.
- **The Data window loads its stored coverage 2.3 times faster** (5.9 s to about 2.7 s on the
  measured store): the walk's gap probes now run on a connection each, six at a time.
- **A control keeps its density's height at every text size.** At Large text a Compact button or
  field grew up to two points past its density's height; its padding now takes only what the text leaves room for, and
  an icon never outgrows its button.
- **The Trade window opens tall enough to show its whole form at every text size.** The opening
  height counted two rows of quick sizes and one row of cancel buttons as constants, and at Large
  text both wrap further. The compact ticket's position strip drops the P/L's unit before it cuts a
  digit, and a refused TP/SL falls back to a warning mark before its words are cut.

### Note
- **A settings store that holds the text size `small` is refused by an older build** (the loader
  rejects a word it does not know), and a stored `standard` now means the larger scale it did not
  mean before. Nothing is written until a trader picks a size.

## [0.1.46] - 2026-10-04

### Changed
- **Compaction keeps a grouped series sorted by instrument, so a one-instrument read decodes a
  small slice of a compacted part.** The grouped layout (the one the live recorder uses for
  Polymarket quotes, trades and book updates) is written instrument by instrument so that a read of
  one instrument can skip the others, but the data server's compaction re-sorted every merged part
  by time and undid that. It now merges a grouped series by instrument, then time. Measured on a
  part of 1,000,000 rows and 400 instruments: a one-instrument read decoded 20,480 rows instead of
  all of them, for a file 12% larger. The rows every read returns are unchanged, and an older build
  reads the new parts exactly as it reads the ones the recorder writes. A part an earlier build
  compacted is re-sorted only when its date is merged again; nothing is rewritten for this.
  - A grouped folder of a kind whose rows carry no instrument (no writer makes one) is merged in
    time order with one warning per series per pass; only a merge under a store source policy
    refuses it, and then as one failed series, not a stopped pass.
- **The `vike-cli trade` reads answer an idle node at once, and a placeholder frame is marked.**
  `trade order ls`, `trade position ls` and the REPL's reads waited for a frame past the node's
  pre-fold placeholder, so against a node that had not folded yet (an idle one) they sat out the
  whole 2 s deadline; any frame from the node now ends the wait. A frame from before the node's
  first fold is marked wherever it is shown: each REPL table header reads `[seq 0 PRE-FOLD]` under
  a `[pre-fold]` line, and the one-shot `ls` commands print that line on stderr. The MCP read
  answers' pre-fold note now says whose placeholder it is: the node's own (identity stamped) or
  this client's, when no node frame arrived in time.

### Fixed
- **Compacting a grouped series under a store source policy no longer drops other instruments'
  rows.** A store source policy (the hand-written `_sources.json` that ranks the writers of a
  series) is applied to grouped series too, and the precedence contest ran per timestamp rather
  than per instrument and timestamp. So where two ranked sources wrote the same date, a
  lower-ranked source's rows of every OTHER instrument at a timestamp the higher-ranked source also
  wrote were dropped as duplicates. The contest is now per instrument. Nothing writes that file,
  and a store without one was never affected; this release does not restore rows an earlier
  compaction dropped.
- **Rows that share a timestamp come back in the order they were written, in reads and in
  compaction.** For order-book updates those rows are the levels of one event. The store could read
  a part in several pieces at once and keep the pieces in the order they finished, so the levels of
  one book event could be read, and compacted, in a different order (no level was lost or changed).
  Each part is now read in one piece, in file order. That also cut the CPU of a store read to
  between half and three quarters wherever a part used to be split, and no measured read got
  slower. A part an earlier build already compacted is not repaired.

### Note
- **v0.1.45 was tagged and never published.** Its release run was stopped on purpose before
  anything was built, so no Release and no deploy exist for it. This release carries its changes
  (the [0.1.45] entry below) plus the ones above. The v0.1.45 tag stays as a record.

## [0.1.45] - 2026-10-04

### Changed
- **The CLI's REPL, one-shot and MCP order paths now refuse an order that cannot be sized, at
  parse.** A quantity that is zero, negative or not a finite number, a price that is not a finite
  number, and (MCP only) a limit order with no price are refused with the same reason on all three
  paths, instead of reaching a preview that said "within limits". Before this the REPL and the
  one-shot command already refused a zero, negative or NaN size but let an infinite size and a NaN
  price through, and the MCP `submit_order` tool refused none of these. The limit check stays
  advisory: a sizeable order over a limit is still built and, once confirmed, still sent, and the
  node remains the gate. Nothing the node would accept is newly refused; a zero or negative price is still passed
  through.
- **A `Modify` is now sized with its order's contract multiplier, and the daemon's notional
  ceiling never counts a multiplier below 1.** `policy.max_notional_per_order` sized a `Modify` as
  quantity times price, because a `Modify` names only a client order id, so "submit small, modify
  up" was checked without the contract multiplier on an instrument whose multiplier is not 1. The
  daemon now finds the open order that id names in the state it already publishes and sizes the
  modify with the multiplier of the instrument that order rests on; the refusal names the
  multiplier when there is one. An order the daemon does not yet know (placed since its last
  publish, already finished, or never existed) is sized as before, and the core's risk gate still
  judges it with the real multiplier. Separately, the ceiling now treats a multiplier below 1 as 1,
  for a `Submit`, every priced bracket leg and a `Modify` alike, and so a zero, negative or
  non-numeric one from bad venue data: the daemon is never weaker than quantity times price, and
  for a contract below 1 (none ships) it is stricter than the desktop, which keeps the true value.
  Of the shipped venues only a Deribit future reports a contract size, so only that is affected.
  With no ceiling set nothing changes; a daemon restart applies it.

## [0.1.44] - 2026-10-04

### Changed
- **The daemon's per-order notional ceiling now counts the instrument's contract multiplier.**
  `policy.max_notional_per_order` is enforced only at the daemon's control edge, which sized an
  order as quantity times price while the desktop's order preview and the core's risk gate also
  multiply by the contract multiplier, so on an instrument whose multiplier is not 1 the daemon let
  through orders the desktop would have refused. A `Submit`, and every priced leg of a bracket, is
  now sized with the multiplier of the engine the command addresses, read from the engine block the
  node already publishes, and the refusal names the multiplier when there is one. Of the shipped
  venues only a Deribit future reports a contract size, so only that is refused sooner; a contract
  with a multiplier below 1 (none ships) is sized smaller, as the desktop sizes it. A size or price
  that is not a finite number is now refused instead of compared with the ceiling, since NaN is
  never above it; the wire and the Telegram parser cannot send one, so this closes an in-process
  path. A `Modify` names no instrument and is still sized without the multiplier, and zero or
  negative sizes are still refused by the core's risk gate. With no ceiling set nothing changes; a
  daemon restart applies it.

## [0.1.43] - 2026-10-04

### Added
- **One Trade window replaces the DOM window and the old Trade ticket (desktop).** The ladder, the
  order ticket and TP/SL live in one window per instrument. TP/SL goes through the node's bracket
  command, and the window shows the node's reason before the click when the engine cannot hold the
  stop (the shipped daemon's binance engine is spot). Money-path guards: a held confirm prompt for
  a Reverse or market order is dropped with a "Not sent" note if the price for its side disappears
  while it waits; a press decided on a stale ladder places nothing; dragging a marker whose orders
  went away sends nothing; the status strip says why Buy/Sell would send nothing instead of
  "Ready". A LIVE window is marked by the LIVE chip in the theme accent, the word LIVE and an
  accent wash on a held prompt.
  - ⚠ The DOM window is gone. `VIKE_SHOT_WIN=trade` opens the new window (`account` shows the old
    Trade content), `VIKE_DOM_*` variables are ignored, and `VIKE_TOOL=dom` falls back to Calendar.
  - Desktop only: install the new `vike-desktop` build; the daemons need nothing for this.

### Changed
- **A node round trip takes about 0.03 ms instead of about 80 ms.** Every node frame went out as
  two writes (length, then body) and the second waited for the peer's delayed ACK. A frame is now
  one write, and every node socket sets `TCP_NODELAY`. The bytes on the wire are unchanged, so old
  and new clients and servers still talk to each other; the server side needs the daemon restart
  the release deploy performs.
- **The `vike-cli mcp` node snapshot answers at once (2 s → 0.05 s).** `node_snapshot` and the
  snapshot resource no longer wait for the node's first fold. Every answer carries `pre_fold`; when
  it is `true`, balances, equity and the venue list are the node's placeholder (an empty venue list
  does NOT mean nothing is mounted) — ask again. The order path is unchanged.
- **Internal code layout, no behaviour change.** Large modules are split by concern and grouped in
  directories (vike-model venues, paths, time and accounts; vike-cli mcp and secrets; vike-tradehub
  server; vike-datahub hub and server; vike-data recorders; vike-strategy strategies), and
  duplicated bridge helpers live once in vike-bridge-core. Code that names one of the moved modules
  by its full module path needs the new path; crate-root names are unchanged.

### Fixed
- **Perp fills on aster and binance now move the position.** The perp stream labelled every live
  fill, partial fill and liquidation with the exchange's bare symbol (`BTCUSDT`), while the engine
  is mounted on the catalog spelling `BTCUSDT.P`, so the engine dropped them from its position
  bookkeeping: the order showed FILLED, but the position, realized PnL and commission did not move.
  A market order was usually booked about 5 s later, when the trade-history replay that runs after
  the last order command delivered the trade again under the right label; a resting limit order
  that filled later stayed unbooked until the next order command or a stream reconnect. A venue
  liquidation did not close the position when the stream delivered it; at best that replay would
  later book it as an ordinary closing fill, where the venue's `allOrders` list includes the
  `autoclose-` liquidation order (not measured in this tree; see
  `crates/bridges/binance/src/family/history.rs`'s `map_perp_history` doc). Reconcile could not
  repair it: the fills it synthesizes
  carried the same bare label and were dropped the same way, and once a replay had booked a local
  position, the position check raised a `PositionOnlyExternal` (venue `BTCUSDT`) and an
  `OrphanLocalPosition` (local `BTCUSDT.P`) on every pass. Every perp fill, liquidation and
  reconcile report is now labelled with the symbol the exchange names plus `.P` (a fill on another
  symbol of the same account keeps its own label and never lands on this engine); the spot lane is
  unchanged. A trade the stream already booked and the replay delivers again is refused by its
  trade id, and a replayed liquidation is now handled as a liquidation rather than as a fill, so
  neither is booked twice. Aster was exposed (its wired market is the perp `BTCUSDT.P`); the
  shipped binance engine is spot, so binance perp was affected only where a perp engine was
  mounted.
  - ⚠ **Restart required:** the fix is in the daemon binary. The daemon restores no position at
    startup, so after the restart an aster perp position that is open on the venue is not in the
    daemon's book, and the first reconcile pass reports it as one held `PositionOnlyExternal`
    (under the default `quarantine` policy nothing is written to the book). That is what any
    restart did before this fix too: right after a restart the book was flat, and the alert pair
    above appeared only once a replay had booked a local position.
  - ⚠ **Each venue liquidation raises one FALSE `MissingFill` per liquidation trade**, held under
    `quarantine` and `external-quarantine` for the 1 h reconcile lookback. The book has already
    closed the position; reconcile does not yet recognise a liquidation trade as booked. Do not
    "fix" it by trading — let it age out of the lookback. This rests on the venue's `userTrades`
    list carrying liquidation trades, which is expected but not measured in this tree; if it does
    not, no such alert appears. Before this fix the replay's closing fill would have hidden it
    wherever `allOrders` lists the liquidation order (also not measured), which is why it may be
    new to an operator.
  - ⚠ **Do not set `VIKE_RECONCILE_POLICY` to `hybrid` or `synthesize`** until the follow-up that
    lets reconcile see booked liquidations lands (the engine's reconcile view exporting its
    liquidation ids): under those two policies that false `MissingFill` is folded as a fill. On
    the already-flat book it first OPENS a phantom position on the opposite side; a later pass's
    `PositionDrift` then rewrites that position back to the venue's, and that is when the bogus
    realized PnL is booked.
  - **Journal:** where the command journal is enabled it is write-ahead, so it still holds the
    bare-labelled fills written before the upgrade: a `tearsheet --journal` over that period lists
    those trades under `BTCUSDT`, and again under `BTCUSDT.P` where the replay delivered them a
    second time. After the upgrade a replayed trade is still journaled twice, now both times under
    `BTCUSDT.P` — not new; the two labels have merely converged.

## [0.1.42] - 2026-10-03

### Added
- **A running backfill can be stopped, and it stops when its client goes away.** Killing
  `vike-cli data hist fetch` used to leave the data server fetching with nobody listening (four
  OANDA pairs over 21 years ran on until the server was restarted). The server now checks between
  chunks whether the peer is still there; if not, it stops at the chunk boundary, keeps what it
  wrote and closes the connection without a reply, and a re-run resumes. Two operator verbs cover a
  client that cannot be reached: `vike-cli data hist running` lists the running backfills and
  `vike-cli data hist cancel VENUE:SYMBOL:INTERVAL` stops one at its next chunk boundary (a lane
  that runs one batch per request is listed, and its cancel says it cannot stop). On the wire these
  are `ListBackfills` (read) and `CancelBackfill` (control, served wherever `Backfill` is), with the
  `backfill_cancel` capability and no protocol-version bump; a client refuses locally when the
  server does not advertise it. ⚠ A data server from v0.1.41 or older has none of this: only a
  restart stops its backfill.
- **`vike-cli data hist import` loads Dukascopy's official daily tick archive.** `import FORMAT
  DATASET [--from DAY] [--to DAY] [--bars IV[,IV...] | none] [--dry-run [--verify]] [--yes]
  [--json]` reads the vendor's daily `.bi5` files from the imports directory under the data
  server's project folder, prints the plan, asks you to type `import N days` unless `--yes` is
  given, then imports one calendar month per request with a progress line each and a summary.
  `--dry-run` stops at the plan and `--verify` decodes without writing. A day belongs to one lane
  only: a day the HTTP `fetch` already holds is refused, and a day imported here is skipped by
  `fetch`. Exit codes: 1 for a day refused for what its file holds or any server refusal, 2 for a
  bad command line or no terminal without `--yes`, 3 when the server cannot be reached. A server
  without the import capability (every box before this release) is refused locally with a message
  saying what to do. Proven on real vendor files (2026-10-02); the Dukascopy archive import
  operator page carries the steps, the measured throughput and what each refusal means. The import
  is stopped with Ctrl-C: it is not listed by `data hist running`.
- **The data server says how far back each venue's history can be fetched and whether this server
  can fetch it.** A new read-only `HistoryChannels` request (observe key, advertised as the
  `history_channels` capability, no protocol-version bump) answers every venue on the roster with
  the catalogue's documented depth, limits and cost, plus this server's own state: whether the
  lane is mounted, whether its credential is Present, Absent, Unreadable or NotChecked (a word,
  never a value, and nothing is called Absent that was not read), and what the store already
  holds for it. `vike-cli data source show VENUE --addr HOST:PORT` includes the overlay, and the
  desktop's Data Manager By-venue table gains a HISTORY column. The default lookback is clamped to
  a venue's depth only where that depth is documented or measured; no row qualifies today, so
  nothing changes yet. Refusing an impossible window up front is deliberately not built: the venue
  answers with its own error or data from its earliest date.
- **An OANDA re-run no longer asks again for days that held no candles.** A day with no candles
  wrote no rows and so spent no key, so every re-run of a multi-year fetch asked for every weekend
  again (613 Saturdays over 2015 to 2026, one request each). The chunked fetch now records a
  key-only `:empty` marker for such a day, and only when a later day of the same request is known
  to hold data (fetched, or already stored), so a day the venue had merely not published yet is
  never marked. `data hist ls` and `rm` treat the marker as part of the series, and the repair
  plan counts it as a note, not a loss. ⚠ The first run after this release asks every Saturday
  once more to write its marker; a request that ends on an empty day still asks for it each run,
  and a manifest rebuild drops the markers.
- **Node-side bracket command: TP/SL from the desktop (default account, one engine).** New
  bracket capability, protocol version unchanged. The node serves a bracket (entry, take-profit and
  stop-loss) only on the default account of a venue with exactly one engine, for a symbol that
  engine trades; binance and aster only when that engine is a perp. Anything else is refused
  before the acknowledgement, with the reason. A client-side refusal is now latched in the status
  bar's control segment until dismissed. ⚠ On a daemon whose binance engine is spot (the shipped
  one) desktop TP/SL on binance stays refused with the node's reason until a perp engine is
  mounted. A bracket frame delivered twice places two brackets.
- **`vike-cli config bootstrap-daemon` carries the bar width and the `[daemon]` table.**
  `--interval-ms`, `--summary-ms` and `--shutdown-deadline-ms` store the mount's `interval_ms` and
  the daemon profile's `daemon.summary_ms` / `daemon.shutdown_deadline_ms`, so the OANDA recipe's
  10 s teardown deadline is one command rather than a mirror-and-activate pair over a copied file.
  Each takes a whole number of milliseconds above zero, `--interval-ms` must agree with
  `--interval` whenever vike can measure the interval, and a flag left off stores no row (the
  daemon's own defaults stay the daemon's). The report prints the values it stored. A
  `--shutdown-deadline-ms` that is not under the shipped unit's 10 s `TimeoutStopSec=` prints a
  one-line warning on stderr (the OANDA recipe's 10 s deadline needs the unit's stop timeout, and a
  container's `--stop-timeout`, raised to 20 s).

### Changed
- **Every store read the data server makes for research is bounded.** The cohort, perp-metrics and
  equity reads, and the head of the exec-fills read, no longer materialise a whole range on every
  page: each takes a row budget through the same block walk as `LoadBars`. Every read kind has a
  row ceiling derived from the frame size, and a request with no limit that would exceed it is
  refused by name instead of being read in full (the four tick scans included). Every reply is
  also counted in bytes, exactly, and cut on a whole timestamp so a page is never empty while
  rows remain. `LoadBars` now runs through the same path. No wire change. Known residual: the
  shortest real bar is 124 bytes against the 129 the bar ceiling assumes, so a reply that fits can
  be refused when its bars are priced at exactly zero.
- **`vike-cli config show` says where a policy ceiling lives.** The pre-trade ceilings table's
  LIVES IN column and the JSON `home` field read `policy (settings database)` instead of
  `policy.toml`, a file no binary has read since decision 0086. The variant behind it is
  `CeilingHome::PolicySettings` (was `PolicyFile`).
- **Comments, help text and refusals stop naming files nothing reads.** The text sweep after
  decisions 0086 and 0095 rewrote what still presented `policy.toml`, `flags.toml`, `config.toml`,
  `preferences.toml`, `tradehub.toml` or `secrets.env` as the daemon's input or the operator's edit
  surface, in Rust comments, docs, examples and the messages an operator reads. The refusals of
  `alpaca`, `ctrader`, `oanda` and `ig` credentialed-data mounts, the `config check` live-arming
  sources, the removed-key refusals and the account-rename warnings now name the settings row or
  `vike-cli secrets set`. Text for a box with no settings database (the file credential store) and
  history that explains why a legacy file is ignored stay as written.
- **The settings database links venues by number only (release 2 of 2).** `account`, `credential`
  and `venue_setting` no longer carry a text `venue` column. ⚠ A rollback across this release needs
  the binaries of the release that introduced `venue_id` as the link (v0.1.41) or newer. An older
  binary reads these tables by the text column, which is gone.
  - **What it changes.** The text-keyed uniqueness rules go with the column, and the credential
    table's scope check is now over the venue's number. Every reader and writer already named the
    venue by number (release 1); a writer now writes the number alone on these three tables. The
    venue-arming table keeps its text column, and keeps writing it, until it is deleted in a later
    plan. `SCHEMA_VERSION` does not move.
  - **What contracts a store.** `vike-cli config migrate-store`, on purpose (see the Fixed entry
    below): one transaction of the settings database's write path drops the column and says so.
    Without it, the first write through that path does the same in its own transaction, whichever
    write that is. Either way the venue links are carried onto the number first, so a store still on
    the shape before release 1 is carried and then contracted by the same run or write.
  - **A venue-scoped credential row whose venue the store's venue table does not hold refuses the
    contraction,** by table, row id and venue, and commits nothing: dropping the text would erase
    the only venue that row names. It blocks every write to that store, a credential rotation
    included, until it is repaired. The refusal is trap 7's text (the refusal a store with unnumbered
    rows meets), which sends the operator to a SQLite client; `migrate-store` exits 1 on it.
  - **A credential key filed under a venue the store's venue table does not hold is refused by
    name** instead of landing as a row that names no venue (which every reader would have taken for
    an infrastructure key). Only a classifier answering a venue outside the roster reaches it.

### Fixed
- **`vike-cli config migrate-store` now carries the venue links on purpose, and contracts the
  store.** After decision 0095's lines (printed first, so a later failure cannot swallow the report
  of a rewrite already committed) it prints what it did to the links, one line per step:
  `venue links: carried onto venue_id` when the account, venue-setting or venue-arming table still
  let the link be missing or empty, and `venue links: text venue column dropped from account,
  credential, venue_setting` (the tables it found) when the text column was still there; both
  lines when it did both, in one transaction of the write path that committed, measured by the same
  read-only probe before the commit. `venue links: already carried and contracted` means a
  read-only probe found nothing owed, and the probe opened nothing for writing. On a store where
  decision 0095's migration is still pending, `migrate-store` is not the only door: any `vike-cli`
  verb, read-only ones included (and a daemon that can write the store), applies 0095 at startup
  in a transaction that also carries and contracts the store before the verb runs, with no line
  naming the contraction, so that line can follow a run that wrote. It
  prints `venue links: no settings database`, or, for a store at schema 1, that there is nothing
  to carry and nothing was written; with no settings database at all it prints one line for both
  steps. A refusal of the
  store's own repair (a row naming a venue the store's venue table does not hold, a key two rows
  would share, a dangling reference) exits 1 with the refusal's own text and without the "run this
  as the user that owns the store" hint, which every other failure still carries. (The v0.1.41
  entry said this verb could report the store current without carrying the links.)

## [0.1.41] - 2026-10-01

### Added
- **The backend publishes what each venue engine trades and what stands behind it (Trade window,
  step 1).** The snapshot now carries, per engine, whether it is paper, demo or live and the symbols
  it trades, and every order view names its account (absent for the default account). The mode is
  set only where a real venue client is built: a restored engine reads paper until its caller sets
  it, and a live client whose tier went unrecorded reads as live. The wire carries all of it as
  optional fields with no protocol-version bump: an older client ignores them, a newer client
  reading an older node sees them as not said, and an unknown future mode word reads as not said
  instead of dropping the observe link. The desktop's observe bridge keeps the account label, route
  key, symbols and mode it used to drop. Nothing in the desktop consumes them yet (the Trade window
  itself is a later release), so the desktop behaves as before.
- **A read-only Directory request on the trading daemon's node protocol.** It answers the venues on
  the node's own roster, each with its own spelling, and the active accounts (venue, label, mode and
  the broker's own account number). Either key may read it, the observe key included (the owner's
  ruling of 2026-09-30; decision 0065 records the amendment). It carries no key name, no credential
  and no arming flag, and every error is a fixed sentence with no filesystem path. A node
  advertises it as the `directory` feature and a client refuses to send it to an older node. A node
  with nothing to list accounts from (no settings source, no settings database or one that predates
  the account table, or one that will not read) answers with an error, never with an empty
  directory.
- **`venue.title`: each venue's own spelling in the settings database** (Binance, OKX, cTrader,
  Interactive Brokers and so on, fourteen rows). It is written once and never rewritten, so a title
  an operator cleared is not refilled. A store has no titles until its first write by this release
  (see the settings-store entry below); until then the Directory reports a missing title as absent
  and a client falls back to the venue key.
- **`vike-cli data hist fetch` cuts an OANDA window into one request per calendar year and prints
  one progress line after each,** so a long fetch shows it is alive. A failed year stops the run and
  names the year and the rows already written; a re-run does zero new work for the years that
  landed. Only OANDA is cut, because its commit keys are whole UTC days that do not depend on the
  request; every other venue still goes out as one request, byte-identical to before. The date help
  now says what the parser takes: epoch milliseconds or a UTC date `YYYY-MM-DD` (it said
  `YYYY-MM-DDTHH`, which `fetch` refused).

### Changed
- **The settings store links venues by number: release 1 of 2.** The settings database now keys
  every venue link on the venue's number instead of its name. `venue_id` is `NOT NULL` on the
  account, venue-setting and venue-arming tables, their uniqueness rules and indexes are keyed on
  it, and every reader and guard in the credential and settings crate reads and filters by it. The
  text `venue` column is still WRITTEN and nothing is dropped, so the release before this one reads
  and writes a carried store. A second release drops the text column; it is not in this one and
  waits until this release runs on the production box and on the dev box and both stores are
  carried. The earlier ceiling migration now reads a store as it finds it and rewrites the rows it
  read by row id, and a refusal from the new pass reaches the boot as its own message instead of
  being blamed on that migration.
  - **What carries a store.** The first write through the settings database's write path does it,
    in that write's own transaction: the venue roster is topped up, the columns are added and
    filled, then the three tables are rebuilt. An older binary's writes cannot undo it. Until a
    store is carried the new readers fall back to the text column, so an uncarried store keeps
    working; what it lacks until then is the venue titles. A daemon started against a store whose
    earlier ceiling migration is still pending also runs the pass at start.
  - **What an operator does AFTER the release.** On the production box the trading daemon cannot
    write the settings directory, so the first write is an operator's `vike-cli` (the release's
    own, one that carries this pass). Run one no-op write there and on the dev box, repeating the
    value the key already holds: `vike-cli config set venue.polymarket.proxy_enabled <its current
    value>`, the value read from `vike-cli config show`. That one write adds the venue titles and
    carries the links in one transaction; no daemon needs a restart. Then read the store with an
    independent reader, not `vike-cli`: all fourteen titles present and none empty, the link column
    `NOT NULL` in the three tables, the integrity and foreign-key checks clean, and the account
    count unchanged. `vike-cli config migrate-store` is NOT a way to carry the store yet: it can
    report the store current without carrying the links (a follow-up).
  - **A row that names a venue the store's own venue table does not hold refuses the write,** by
    table, row id and venue, and commits nothing. It blocks every write to that store, a credential
    rotation included, and no `vike-cli` verb repairs it (the repair text sends the operator to a
    SQLite client). Measured before the release: neither the production box's store nor the dev
    box's store holds such a row.
  - **Rollback.** The release before this one reads and writes a carried store. Measured with the
    v0.1.39 binary on copies of the production store, whose store code matches v0.1.40's but for
    four test lines: `config check`, `config show` and `secrets list` read the same as the new
    binary, `config set`, `secrets set` and a venue-policy write all succeed, and a new account
    can be minted. Rolling forward again moved no row, id or digest. Not measured: a daemon running
    on a carried copy, an older binary's insert of a NEW venue-arming row (its arming write ran on
    an existing row), and a rollback across more than one release.
- **A live-tier key set that an arm cannot use is now named, not reported as "no credentials".**
  Alpaca, IG, Deribit, cTrader, FXCM and IBKR mount only their demo tier; with only live-tier keys
  they stayed paper with the same cause as an empty store and no log line (OANDA logged an error
  beside a probe that still said "no credentials"). They now report a new cause, `live tier not
  wired`, in the trading daemon's `venues` verb and the arming screen, and log one `error!` line per
  venue at daemon start carrying the venue, account and `found_tier` (no key, no value). What
  mounts does not change in any cell: a 14-venue by ceiling by key-set matrix was written green on
  the unmodified code first, and exactly 12 cells print another cause. Measured read-only on the
  production box before the release: it holds no such keys, so its journal does not change.
- **Every live-mount line carries `venue`, `account` and `tier`, and a live tier starts with the
  `⚠ REAL-MONEY: ` prefix.** Eleven of the fourteen bridges announced a mount with no venue or
  account field and the prefix existed in three; one convention now holds for all fourteen, and a
  gate iterates the roster. A box-local alert or grep that matches the start of one of those lines
  sees the prefix on a live tier.
- **A venue's feed network is asked of the same registry row as its exec side.** The Hyperliquid
  feed chose its network from the venue's ceiling line and each CEX bridge re-derived its own, while
  every account's exec network comes from the registry row, so a labelled account capped below the
  venue line traded one network against a feed on the other. Every unlabelled configuration is
  unchanged (a 60-cell table test copies the removed derivations verbatim), and the production
  box, which has no labelled account, is unchanged. For labelled accounts 4 of 192 grid cells move
  (venue line `live`, labelled account capped to `demo` and armed on its demo keys, default account
  not armed live): the Hyperliquid feed was mainnet and is now testnet. Armed accounts on both
  networks keep the mainnet feed, start normally and log a warning naming the testnet accounts
  priced off the wrong book.
- **The IBKR API client comes from crates.io instead of a vendored copy.** `ibapi` is a registry
  dependency pinned `=3.2.1`, with default features off and only `sync` on (its default is the
  async tokio client, which stays out of the blocking bridge). After the same rustfmt the published
  crate is byte-identical to the vendored copy except for three dropped test module declarations,
  so no behaviour changes, and both IBKR transports are untouched. The vendored copy (283 files),
  its drift workflow and its drift script are deleted, `ibapi` leaves the CI exclusion table, and
  the lockfile drops four packages (mio, socket2, utoipa and utoipa-gen).

### Fixed
- **Paged reads of recorded Polymarket ticks, trades and order-book updates no longer lose rows.**
  Since v0.1.34 (2026-09-23) a read through the data server of a series stored in the grouped
  layout (the only kind the live recorder stores grouped: Polymarket quotes, trades and book
  updates) could return only part of the data, often nothing at all, with no error. Reproduced
  through a loopback datahub: 8 of 11 planted shapes lost rows, for example 0 of 50, 10 of 30 and
  15,000 of 15,100. The store now runs one row budget across a symbol's own series and every group
  directory, reading blocks until it has enough rows or reaches the end of the range; all four
  capped verbs (quote, trade, book and depth) share it, and the wire is unchanged. The fix is in
  the datahub's store code, so it takes effect when the datahub is upgraded. The files on disk were
  never damaged, and bars, other venues and `data hist export --addr` were not affected. Results
  that backtests, studies or the `cheap_np_*` tools produced from grouped Polymarket ticks through
  the datahub since v0.1.34 are suspect and cannot be repaired, only re-run; this release does not
  re-run them.
- **`LoadBars` no longer reads a whole range before applying its limit.** It asks the store for a
  bounded head, so a small limit over a multi-year range holds only what it returns, and a client
  that pages through a long range no longer makes the server re-load the rest of the range on every
  page (about 14 hours of datahub CPU for a full 5-second OANDA range, by the design's estimate).
  Every capped reply is byte-identical to before. A request with no limit over 520,223 bars is
  refused by name on a connection that stays open; it used to be read in full (about 6 GB of rows
  for a full 5-second OANDA range, in a unit capped at 8 GB) and then die as a dropped connection.
  The ceiling is derived from the smallest serialised bar, so it refuses nothing that fits a frame
  today. No wire change.
- **Dukascopy: each recent chunk of a multi-chunk window is stored under its own provisional
  key,** straight after its fetch (provisional-commits follow-up 9), so when those days settle the
  settled commit supersedes exactly that chunk and the ticks are no longer stored twice. A recent
  chunk whose provisional key is already spent skips its download and resamples from the stored
  ticks. Two residuals stay: a store that already holds an old multi-day tail keeps its doubled
  ticks until the series is deleted and fetched again, and the partial `[today, now]` end of a
  `--days N` request still sits beside its day once that day settles.
- **`vike-cli config check` fails a store that the daemons refuse to start on.** A credential row
  still carrying a venue setting's old name used to pass the check, so a container whose entrypoint
  runs only this check started a daemon that then refused at its own boot. The check now reports the
  same rows the daemons name (one shared function, so there is no second list), and never prints a
  value or a neighbouring credential's name. This replaces the v0.1.39 note that the check does not
  detect a stranded row.
- **The trading daemon's `venues` and `catalog` verbs report a stranded store instead of
  refusing.** They mount nothing and read none of those rows, and `venues` is the command that
  answers "why is this venue paper"; it refused exactly when its answer was most needed. Both now
  print the finding on stderr (names only, and that the daemons will refuse) and carry on. A clean
  store prints nothing, so a clean box's output is unchanged. The daemons still refuse.
- **The deploy pre-flight reads a unit's `Environment` by systemd's own quoting.** The helper cut
  the property on spaces, so systemd's quoted assignment for a value holding a space (`"NAME= 1"`)
  arrived as a broken word and the judge was handed an empty value, which it reads as not set. A
  padded or two-word value of a retired variable in a unit's `Environment=` therefore passed the
  check that exists to refuse a release before anything is installed; the daemon's own boot still
  refused it afterwards, so the cost was a deploy that installed and then failed. The property is
  now decoded one character at a time by systemd's quoting rules and nothing is evaluated (no
  `eval`, no `xargs`, no format taken from the data, since the helper runs as root over unit-file
  text). Output for an ordinary value is unchanged; a line terminator inside a value is written as a
  space, so one value cannot forge a second assignment line. The helper is a root-run script
  installed by hand outside the tree, so merging changes nothing on a box by itself; this is the
  repo copy of a helper that was installed by hand on the production box, digest-checked, before this
  release was cut.
- **The retired-variable judge takes a value as the daemon receives it.** `vike-cli config
  retired-env`, which the pre-flight runs from the staged release, trimmed every value before
  judging, so a padded `1` got the ordinary "write this row" line while the daemon's own boot,
  handed the value as received, said to write nothing. It now removes only quoting that is syntax
  (the pair `systemctl show` wraps around a whole assignment and the pair systemd strips from a
  quoted environment-file value), through the same function the daemons call.
- **Stored venue values are trimmed on write, and `config show` tells a misscoped field from an
  undeclared one.** A padded argument, or a piped line with trailing spaces and CRLF, used to be
  stored with the padding (the desktop's proxy box had the same gap). A declared field stored at a
  scope it does not use is now shown as `misscoped`; `undeclared` is left for a key no field
  declares.
- **Operator pages and recipes say what is true now.** The step after the default `quarantine`
  reconcile policy is `external-quarantine`, never `hybrid` (decision 0043); the operator pages for
  reconcile and the production box said "graduate to hybrid". The nine per-venue live-mount recipe
  files said to copy them to a daemon-profile path no binary reads; each now carries the `vike-cli
  config bootstrap-daemon` line that builds the profile row from its own values and says what that
  verb cannot carry, and the run-profile recipe names the policy settings rows instead of the
  policy file. The trading daemon's unit `Description=` now says paper-versus-live comes from the
  settings database and that `.env` can override it. The OANDA note in the history-channels table
  and its generated page, the operator page's time estimate (measured 3,620 s for EUR_USD and
  3,828 s for USD_JPY, 2015-01-01 to 2026-09-30 at 5 seconds, one request each before the per-year
  cut) and the get-market-data skill's claim that fetch needs no credentials (OANDA's lane reads one
  practice token the datahub holds) now say what is true. Text only.

### Internal
- The root project guide is cut from 152 KB to 33 KB after a line-by-line audit against the code at
  v0.1.40: only rules a reader needs before touching anything stay, the stale claims it carried are
  fixed in the pages that own the fact, the 19 KB feature-lane block became a pointer to the lane
  script, and a new size gate (`root_claude_md_size_gate`) lets it shrink and never grow. The
  `default:` text of the reconcile-policy settings row follows the `external-quarantine` correction
  above; no other code moves.
- Archive import lane groundwork for Dukascopy, no command uses it yet: the daily `.bi5` decoder
  with the decompression limits and time-base checks, and the price-scale table for the 28 G10
  pairs (a refusal quoting the vendor's warning for any other instrument); the import wire (the
  `ImportArchive` request and `ArchiveImported` answer, the `archive_import` capability with one
  `import_format=` entry per format, the shared validators and three local refusals that send
  nothing), with the protocol version unchanged at 7. The datahub answers the new request only with
  the capability error and the compute server refuses it by plane. Decision 0100 records the lane's
  three rulings.
- Gates: the Polymarket egress declaration must be a statement of the mount's own body; the
  field-reader table follows the read's binding to its consumer; every roster venue must sit in each
  of five venue-subset tables or an excluded row with a reason; the venue-free mount crate may
  import no venue-keyed table beyond one pinned allowance; a text gate iterates the roster for the
  live-mount line convention; a test holds the bridges' probe symbols against the wired markets.
- Dead code and doubles: `load_workspace_dotenv_scoped` (no caller anywhere) is deleted with its
  gate row; one owned planted-mount double replaces eleven planted venue-mount impls, so adding a
  declaration field costs one edit; three Hyperliquid items narrow to crate-private; four weak
  tests were strengthened so they fail when the behaviour they name is removed.
- The per-event snapshot build now clones one short string per venue, and one label per order for a
  labelled account. On the CI run of the change that added it (#2370) the latency gate's baseline
  and snapshot-build harnesses stayed inside main's own spread; no harness runs a labelled engine,
  so the label clone is unmeasured.
- `vike-cli trade order ls` still refuses a labelled book. Its message now says an order row names
  its account only when that account is labelled, and a node that predates the field sends labelled
  orders without it, so the read cannot attribute a row for certain; the old text said the wire had
  no such field.
- Design records, none of them built here: the Trade window spec and its 20-task plan, the
  links-by-number plan, the bounded `LoadBars` read, the tick-scan paging defect, and the proposed
  cancel of a Backfill request when its client drops (a proposal that still awaits the owner; the
  datahub still cannot cancel a Backfill when its client drops).

## [0.1.40] - 2026-10-01

### Changed
- **⚠ BREAKING for a box that sets it: the HALT sentinel's location is no longer an environment variable.**
  The kill-switch file is unchanged: an existing file halts new orders, `touch` over ssh stops the
  very next submit, and the default location is `<project>/settings/state/HALT`. What went is the
  override variable `VIKE_HALT_FILE`, which named the file outright and duplicated what the boot
  already resolves. A box that still sets it, in a unit's `Environment=`, a drop-in or a container
  image, now REFUSES startup and names the variable, because ignoring it silently would send an
  operator to `touch` a file nothing watches. The installed trading unit on the production box does
  not set it (measured before the release); the tracked unit and the container image no longer do.
  The refused-variable list is now 45 names.
- **The sentinel's path reaches every venue mount as data.** The boot's resolved path is carried to
  the live execution clients by the mount contract's process facts, so a bridge no longer resolves
  a process-global path itself; the paper books, the live clients and the daemon's startup advisory
  watch the one file. A gate now fails a bridge that reads a process-global path.
- **`vike-cli secrets move-venue-config` writes a change-journal record** when it moves something:
  the moved credential names and the database file, never a value. A dry run, a store with nothing
  to move and a refused move write none.
- **The supersede step is one piece of code for the live commit and the crash-recovery replay,**
  with a test on the replay side that the stamp is applied. Behaviour is unchanged for every record
  a writer produces.
- **A labelled OKX account's half-credential report names its own keys.** A labelled account holding
  a key and secret but no passphrase was never reported under its own key name, and a labelled
  account holding a complete trio was reported for the default account's missing passphrase. The
  report now reads the mounting account's keys and ends in `vike-cli secrets set <KEY>`.
- **The FXCM refusal says what is true.** Without the ForexConnect shim every order is answered with
  a terminal rejection (nothing is placed and nothing vanishes); the gate keeps a box that holds
  credentials but no shim on paper instead of mounting a client that rejects everything. The
  paper-cause label reads `SDK not loaded` (it said `SDK not linked`, false since the shim became
  runtime-opened). No behaviour change.
- **`vike_mount::EquitySampleHook` is deleted;** nothing in the tree named it.

### Fixed
- **A rollback no longer loses the datahub's pending plain writes.** Since v0.1.36 every keyed
  append, every recorder flush included, wrote its write-ahead record in the new frame shape, which
  v0.1.35 and older delete unread on their first open. A plain append (one that supersedes nothing)
  goes back to the old frame shape, so a rollback keeps those records; only a superseding Dukascopy
  commit still needs the newer reader, and that commit can be fetched again. Both directions are
  pinned by tests, one of them a frozen copy of the v0.1.35 reader.
- **A refused supersede during the datahub's crash recovery no longer keeps the store from opening.**
  The replay of one series used to fail the whole open, so the datahub and the recorder inside it
  restarted in a loop. That one series now stops replaying, keeps its records and logs the reason,
  and the store opens.
- **Provisional commits: a canonical commit also retires its provisional twin when the twin lands
  late,** and a superseding call without a commit key is refused before any I/O instead of sealing a
  part that cannot be told from the one it removed.
- **`data hist rm` no longer lists every day of a chunked series as an unknown producer.** A series
  written one UTC day at a time (the OANDA 5-second history lane: about 7,800 keys per 21 years)
  printed one plan line per key, each saying "no declared producer", although the producer is
  declared. Such keys are now recognised by their shape, fold into one counted group per venue,
  symbol and interval, and name the klines producer; look-alike keys (property records, end-of-day
  keys, other producers' namespaces) are still refused.
- **The kill-switch page tells a database-backed box how to disarm a venue.** On a box whose
  settings live in the database, the credential, policy and flags files are not read, so the page's
  instruction to remove a key from the credential file disarmed nothing. It now leads with
  `vike-cli secrets list`, whose source line names the store that answers, says that no `secrets`
  verb deletes a credential, and adds the disarm that works on either store: `vike-cli config set
  policy.venues.<venue> paper` and a restart, which mounts that venue paper without reading its
  credential. The Telegram allow-list row names the credential store that answers. Text only.
- **The pages an operator arms from say what `live` means.** For the four venues whose network is the
  ceiling, `live` means mainnet: with live keys in the store it mounts real money, with only demo
  keys it mounts paper. The runbook, the go-live recipes, the MCP arm prompt, the aster remedy and
  the OANDA history page said the older meaning or a state that no longer holds. Text only, plus two
  message strings.
- **The operator pages for the 0.1.39 settings change give the recovery in order:** where the new
  `vike-cli` comes from (the asset a tag publishes), to stop a refusing unit first, what a labelled
  row does and what the move overwrites. A mount whose bridge answered with a LIVE identity or a
  LIVE-bound startup probe under a ceiling that does not permit live is now refused; no current
  bridge does either.

### Internal
- A flaky bridge-core resync supervisor test now waits on the list the supervisor emitted rather
  than on the fetch counter, and CI stops retrying that one test (three other tests stay retried).
  The change is to the test alone; no production code moves.
- Gates: a bridge may not read a halt helper that resolves, reads or declares a process-global path,
  and every halt function is classified. The merged mount crate's prose follows decision 0098.
- The the latency box verification roster has `lane3` and `lane4`.
- Design record: the Dukascopy archive import lane (history channels, section 5).

## [0.1.39] - 2026-10-01

### Added
- **The IBKR Client Portal gateway URL is a venue setting, `venue.ibkr.<tier>.cpapi_url` (#2359).**
  Write it with
  `vike-cli config set venue.ibkr.demo.cpapi_url https://127.0.0.1:<port>` and restart the trading
  daemon; unset, it is the local gateway's own address, so a plain tunnel to the gateway's usual
  port needs no setting. It used to be a credential row, which is one of the rows the breaking
  change below refuses.

### Changed
- **⚠ BREAKING: a credential row that holds a venue setting under its old name refuses startup
  (decision 0095, #2359).** Until now the settings store also rendered every
  venue-setting row back into the credential map under its old credential name, and a credential row
  of that name won over the row. Every venue mount now reads its settings from the settings rows
  alone, so the rendering is gone, and a credential row still carrying a declared venue setting's old
  name would be read by nothing. It refuses startup instead.
  - **Who refuses:** the trading daemon (with its catalog and venues verbs) and the data daemon,
    which exits with status 2. The desktop starts and warns once, because it mounts no venue.
    `vike-cli` starts, because its verbs are the remedy. The refusal names each row and the setting
    it stands for, points at `vike-cli secrets move-venue-config`, and never prints a value.
  - **The names, derived from the settings catalog:** the IBKR gateway's host, port, backend and
    Client Portal URL for each tier (a labelled `__<LABEL>` spelling included); FXCM's url and
    connection; the Dukascopy demo server; the nine Polymarket machine fields (the five egress
    fields, rate gate, exec markets, pre-submit register and socket batching); the mark-stream
    toggles of Aster, Binance, Bybit, Hyperliquid and OKX; Binance's trade-lite fill; and Bybit's
    fast exec.
  - **Operator step, with the NEW release's `vike-cli`.** The asset exists once the tag publishes,
    and a green release deploys itself, so on a box whose store is clean there is nothing to do and
    on any other the step is also the recovery after a refused deploy. Take the release's `vike-cli`
    asset (it does not need installing) and point it at each store a daemon boots from:
    `VIKE_SETTINGS_DIR=<root>/settings ./vike-cli secrets move-venue-config --dry-run`. If it lists
    rows, run the same command without `--dry-run`. Where it reports two different values for one
    setting, make them equal first with `vike-cli secrets set`. A box with no settings database runs
    `vike-cli secrets migrate` first, because a credential file can no longer carry a venue setting
    at all.
  - **The installed tools cannot do this check.** The `vike-cli` already installed (v0.1.38 and
    older) cannot see eight of the names: the five mark-stream toggles, Binance's trade-lite fill,
    Bybit's fast exec and the IBKR Client Portal URL. Its dry run says `nothing to move` over a store
    the new daemons refuse, and its move cannot clear them. Its `secrets list` is no check either,
    because it also lists the names the old store renders out of settings rows.
  - **If the step is skipped,** the deploy pre-flight boots the new binary against a copy of each
    probed unit's settings root and refuses the release before it installs anything, so the running
    build stays untouched until the store is clean. That is how the pre-flight is written; no test
    starts a daemon against a stranded store. A container's entrypoint check, `vike-cli config
    check`, does not detect a stranded row, so there the daemon refuses at its own start. Recovery is
    the same two steps with the new `vike-cli`. The operator page is `docs/ops/upgrading.md`.
- **Venue settings are read from the settings rows alone (decision 0095, #2359).**
  What an operator can see besides the refusal above:
  - **A labelled IBKR account dials its tier's gateway** (`venue.ibkr.<tier>.host`, `port`, `backend`
    and `cpapi_url`). A label names an account; the gateway belongs to the machine and the tier. A
    labelled account used to read its own `__<LABEL>` gateway credential rows, which are among the
    refused rows.
  - **The bridges read these fields from the settings rows:** Binance's trade-lite fill, Bybit's fast
    exec, Polymarket's rate gate, exec markets and pre-submit register, IBKR's gateway, FXCM's url and
    connection, and the Dukascopy demo server (one row for both demo accounts).
  - **Polymarket egress:** the one-release fallback that still honoured an unmoved egress credential
    row, with its warning, is deleted. Such a row now refuses the data daemon's and the trading
    daemon's start, as above.
  - **`vike-cli secrets set` refuses to create a row under any of these names** and prints the
    `vike-cli config set venue.<venue>[.<tier>].<field>` line that writes the setting instead. A row
    the store already holds can still be rotated, which is how two rows are made equal when the move
    refuses a divergence.
  - **`vike-cli secrets move-venue-config` moves the machine toggles too** (the mark-stream toggles,
    Binance's trade-lite fill and Bybit's fast exec), and `vike-cli secrets migrate` marks the same
    names as pending venue settings.
  - **The desktop's Connections form no longer offers the Dukascopy JNLP server.** Its demo cell
    writes the two login pairs; set the server with `vike-cli config set venue.dukascopy.demo.server
    <url>`.
  - **One boot line is gone.** The line saying a credential value was the one in force over a
    settings row no longer exists, because that precedence does not. Dukascopy's warning for a server
    that is not a JNLP one now names the `venue.dukascopy.demo.server` setting.
- **vike-run is merged into vike-mount (decision 0098, #2362).** The workspace has one
  crate fewer: the node assembly (the venue mounts, the xemm and maker mounts, the incident bundle)
  is part of vike-mount, which keeps its name, still names no venue and moves from layer 30 to 35.
  The wired markets moved up to the trading daemon as data, a table of rows (venue, symbol, whether
  the venue is offered the reconnect poke, and the engine's rank) that the node mounts in table order
  and holds engines in rank order. No change of behaviour is intended: both orders are pinned by
  tests that passed on the old code first, and every test that existed still runs. That evidence
  reaches no live mount. What an operator or a builder can see:
  - **Log targets of the moved code are its new module paths:** the node's lines that carried a
    `vike_run::node` target carry `vike_mount::node`, and likewise for the rest. No directive, unit
    or script in the tree names a `vike_run` target; update any box-local `RUST_LOG`, `VIKE_LOG` or
    `VIKE_LOG_FILE_LEVEL` directive that does.
  - **An incident bundle records its package as `vike-mount`.** The `incident` binary is built with
    `-p vike-mount`; its name is unchanged and nothing ships it. The release's `bins.json` lists its
    row under the new crate, the API reference has no `vike_run` crate, and the next public-mirror
    snapshot has no vike-run directory.
  - **Building:** the `ibkr`, `polymarket` and `fxcm` features of the trading daemon's crate no
    longer forward a vike-run feature, and the feature lanes in the scripts, the justfile and the
    workflow no longer name the crate. The `vike_mount::halt` re-export is deleted; callers name
    `vike_bridge_core::halt`.

### Fixed
- **The container image job's toolchain smoke no longer bind-mounts the checkout (#2358).**
  The smoke ran the image's
  cargo against a bind mount of the release job's checkout. That checkout exists only inside the CI
  runner's private mount namespace, which the container daemon cannot see, so the container got an
  empty directory and the smoke died before it judged any toolchain; the v0.1.38 image was never
  published for that reason. The smoke now copies the manifest and crate sources into its own scratch
  directory. ⚠ The fix is proven only by the script's self-test and a stand-in runtime, not by a real
  image build, and a defect may sit one stage later. This tag's image job is the first that can show
  it passing; the images for v0.1.36, v0.1.37 and v0.1.38 were never published and cannot be rebuilt
  from their tags.

### Internal
- The maker's outcome-token preset takes its venue from its caller, so no node
  source names one; the merged crate's venue-free text gate reads every moved source file, and the
  deploy-layout and desktop local-core gates, which would have passed blind after the move, were
  re-keyed and proven red by planting a violation.
- New merge gates: no bridge reads a declared venue field from the credential map,
  and the boot refusal and the move verb agree on every name across venues, tiers and labels.
- Design records: decision 0098 and its four-task plan (#2357).

## [0.1.38] - 2026-10-01

### Added
- **OANDA candle history goes straight into the datahub's store (#2352).** `vike-cli data hist fetch
  oanda:EUR_USD:5s --from 2005-01-03 --to 2026-09-29` asks the datahub to pull OANDA's mid candles
  (5 seconds and every coarser step OANDA serves) into its store. Decision 0097 is the verdict.
  - **Operator step: the practice token.** Store your OANDA practice account's API token as
    `OANDA_DEMO_API_KEY` in the datahub's own store, value on stdin: `vike-cli secrets set
    OANDA_DEMO_API_KEY`. Check with `vike-cli secrets path` that the store is the one the datahub
    unit reads. A key under a live name is never read, and a live-account token does not work.
  - **How the datahub holds it.** It reads that one key when a Backfill request for OANDA arrives
    and drops it when the request ends. Only the Control-scoped Backfill verb can reach it: the chart
    seed, the venue catalog and every Observe verb never read it. A token stored or rotated after
    the daemon started works with no restart. The OANDA arming ceiling is not consulted, because
    the lane places no order. At startup the datahub logs one line saying whether the lane is
    armed, and it never logs the value.
  - **What a fetch does.** It stores one UTC day at a time and skips every day already stored, so
    an interrupted fetch resumes where it stopped. A day is stored once it has settled, about ten
    minutes after it ends in UTC, so today's candles are not stored and nothing tops the range up
    by itself. Expect roughly one to two hours per pair for the whole span (an estimate from probe
    measurements, not yet timed end to end), with no output until it finishes; run one year at a
    time to watch it advance.
  - **Refused before any request:** a symbol not spelled the OANDA way (`EUR_USD`, with the
    underscore), and a window of seconds-sized candles that starts before 2005-01-03, where OANDA
    serves about one candle a day under the 5-second label. No tick history and no bid or ask
    series exist. The operator page is `docs/ops/oanda-history.md`.
- **`vike-cli data source show <venue>` says how far back a venue's history goes, and through which
  door (#2352).** For a roster venue it lists each history channel: the venue's own request API, a
  bulk archive or a vendor. Each channel says whether it serves bars or ticks, its depth, cost and
  limits, what this tree can do with it today, and where and when each answer was read. `--json`
  carries the same rows. Dukascopy, OANDA and IBKR are filled from the vendors' own pages; a venue
  nobody has measured says so instead of guessing, and an unstated depth is never shown as
  unlimited. A generated reference page carries the same table. The `data hist fetch` help no longer
  calls every venue public market data; it says a venue that needs a credential is marked there.

### Changed
- **⚠ BREAKING: seventeen more environment variables refuse startup (#2354).** They are
  `CTRADER_REDIRECT_URI`, `CTRADER_SCOPE`, `CTRADER_TOKEN_FILE`, `CTRADER_CLIENT_ID`,
  `CTRADER_CLIENT_SECRET`, `POLY_EGRESS_PROBE_URL`, `POLY_EXPECT_EGRESS_COUNTRY`,
  `POLY_CHAIN_WATCH`, `POLY_CHAIN_RPC_URL`, `POLY_CHAIN_MAX_SPAN`, `POLY_CHAIN_PROXY`,
  `POLY_AUTO_REDEEM`, `POLY_REDEEM_HALT`, `POLY_HEARTBEAT`, `VIKE_PM_RESOLVE`, `VIKE_HL_OUTCOME` and
  `VIKE_RECORD_DVOL_CADENCE_MS`.
  - **None of them changed what a running daemon did.** Their only readers were the cTrader
    authorize tool, the ignored live Polymarket smokes, and code no daemon starts (the Polygon chain
    watcher, the auto-redeem, heartbeat, market-resolve and Hyperliquid outcome pollers, and the DVOL
    recorder's cadence). They refuse anyway, so a leftover line is named instead of silently ignored.
  - **Who refuses:** `vike-cli` (every subcommand), `vike-desktop`, the trading daemon and the data
    daemon, when one is set non-empty in the PROCESS environment. The deploy pre-flight fails on one
    left in an installed unit, its drop-ins or its EnvironmentFile, before anything is installed.
    The auto-redeem halt variable refuses even when empty, because its readers halted on presence.
  - **What to do:** unset the variable. No refusal here pastes a `vike-cli config set` line: the
    cTrader three became flags of the authorize tool, its app pair lives in the credential store
    (below), the two egress variables became constants of the order-placing smokes, and the rest
    became parameters nothing passes. The five settlement flag rows stay, and nothing reads them.
  - **Watch your shell.** A variable left exported from running a smoke, such as the expected
    egress country, now stops `vike-cli` and `vike-desktop` at startup. A credential-store row under
    one of these names is not refused, and a Polygon chain row in the store is read by nothing now.
  - **The full set refused at startup is now forty-four names:** the seventeen above, plus
    `VIKE_MAX_ORDER_NOTIONAL`, `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL`, `VIKE_STATE_DIR`,
    `VIKE_TRADEHUB_RECORD`, `VIKE_TICK_STORE`, `VIKE_SECRETS_PASSPHRASE`, `BINANCE_MAINNET`,
    `BYBIT_MAINNET`, `OKX_MAINNET`, `HYPERLIQUID_MAINNET`, `POLY_EXEC`, `POLY_RECONCILE`,
    `POLY_PROXY_ENABLED`, `POLY_PROXY_HOST`, `POLY_PROXY_PORT`, `POLY_SOCKS_PROXY`,
    `POLY_WS_PROXY_ENABLED`, `POLY_RATE_GATE`, `POLY_EXEC_MARKETS`, `POLY_PRESUBMIT_REGISTER`,
    `POLY_WS_TOKENS_PER_SOCKET`, `HYPERLIQUID_HIP3`, `VIKE_ALLOW_WITHDRAW_KEYS`,
    `VIKE_BINANCE_TRADE_LITE_FILL`, `VIKE_BYBIT_FAST_EXEC`, `VIKE_MARK_STREAMS_ASTER` and
    `VIKE_MARK_STREAMS`. `vike-cli config check` reports a set one as an error, and `vike-cli config
    retired-env` judges an environment fed on stdin the way the deploy pre-flight does.
- **The cTrader authorize tool takes flags, and reads its app pair from the store (#2354, #2355).**
  - Its redirect URI, scope and token file are `--redirect-uri`, `--scope` and `--token-file`, each
    with its old default. Read the redirect URI, authorize URL and token path it reports to confirm
    a flag took.
  - The Spotware app pair comes from the credential store, the same rows every cTrader mount reads,
    so the tool authorizes the application the mount authenticates as. Write each half once with the
    value on stdin, `vike-cli secrets set CTRADER_CLIENT_ID` and then `vike-cli secrets set
    CTRADER_CLIENT_SECRET`, with the two variables UNSET first: `vike-cli` refuses to start while
    either is exported, so `--from-env` with the same name is refused before it runs.
  - The tool refuses to run while one of its five retired variables is set, naming each and never
    its value. Its token lands in the project that `VIKE_SETTINGS_DIR` names when that is set, the
    project the app pair is read from; a run with the override from outside a project used to write
    the grant to the working directory.
- **`vike-cli secrets move-venue-config` moves four more Polymarket fields (#2355).** Credential rows
  named `POLY_RATE_GATE`, `POLY_EXEC_MARKETS`, `POLY_PRESUBMIT_REGISTER` and
  `POLY_WS_TOKENS_PER_SOCKET` now move into the matching `venue.polymarket.*` settings rows, like the
  five egress names before them, and `--dry-run` lists them.
  - For the first three the move changes no value in force: the trading daemon already let the
    credential row win.
  - **Socket batching is the exception.** The datahub reads that setting from the row alone, so a
    credential row of that name was read by nothing, and once moved it takes effect at the next
    datahub restart. If batching should not change, write the value you want with `vike-cli config
    set venue.polymarket.ws_tokens_per_socket <n>` after the move and before that restart.
  - `vike-cli secrets migrate` and `vike-cli secrets set` file a NEW row of these four names as
    Polymarket venue configuration rather than as the Polymarket account's credential. Rows already
    in a database keep their placement.
  - **After upgrading,** run `vike-cli secrets move-venue-config --dry-run` against each box's store.
    The move is finished when it answers `nothing to move`.
- **Every venue now mounts itself through one contract (decision 0096).** All fourteen bridges carry
  their own mount behind one shared contract, the trading daemon's registry lists them, and the mount
  crate names no venue (#2334, #2338, #2344, #2347, #2353, #2356). No change of behaviour is intended:
  the arming rules, ceilings, networks, credentials, reconcile and paper fallbacks are the same.
  What an operator can see:
  - **Log targets moved.** The lines a venue's mount logs, such as "DEMO credentials present → LIVE
    exec client", the Binance withdraw gate and a venue's paper fallback, now carry that bridge's own
    mount target, for example `vike_binance::mount` or `vike_aster::mount`, where they carried
    `vike_mount`. Polymarket's carry `vike_polymarket::exec_plane::mount`. The Dukascopy sidecar's
    held-elsewhere and already-claimed refusals come from `vike_mount::contract`. The effective fee
    schedule and the other shared lines keep `vike_mount`. Update any `RUST_LOG`, `VIKE_LOG` or
    `VIKE_LOG_FILE_LEVEL` directive or log query that names the old target for a mount line.
  - **The one intended difference:** a live (mainnet) Aster mount is recorded at the live tier, not
    the demo one, when it records which account it authenticated as. Nothing changes today, because
    Aster's reconcile client reports no account identity.
  - **A new interlock:** if a bridge ever answers live under a ceiling below `live`, or hands back a
    live session under one, that account mounts PAPER and one error line names the venue and the
    account. No bridge in this release does either.
  - **OKX, in one configuration only:** a labelled account whose own ceiling caps it below the venue
    line no longer logs an error about the default account's half-entered keys before the risk-budget
    refusal. Startup still fails the same way.
  - **Building:** the `ibkr`, `polymarket` and `fxcm` cargo features moved from the mount crate to
    the trading daemon's crate. Spell them `-p vike-tradehub --features vike-tradehub/ibkr` (and
    likewise); `-p vike-mount --features ibkr` no longer resolves. The multicall's `full` build, which
    the release ships, is unchanged. The public functions `vike_mount::build_recon_client` and
    `vike_mount::startup::authed_read_clients` are deleted.
- **Reminder for a box upgrading from v0.1.35 or older:** read the v0.1.36 entry first. Its decision
  0095 changes apply unchanged here and are not repeated: the four mainnet switches are retired, so a
  box that still sets one refuses to start until the line is removed; the arming ceiling alone picks
  the network, so `live` with live-tier keys is mainnet; and a one-time store migration lowers those
  four venues' `live` ceilings to `demo`. Re-set `live` only where mainnet is intended, then restart.

### Fixed
- **A long Backfill can no longer exhaust the datahub's memory (#2352).** After a Backfill the datahub
  read the whole requested range back into memory only to report its first and last timestamp, so a
  multi-year request at a fine interval could exceed the unit's memory ceiling. It now asks the store
  for the range's edges alone. Measured on 5.1 million rows: 41 MB against 881 MB. The reply is
  unchanged.
- **Dukascopy: a chunk the store refuses to supersede no longer aborts the whole download (#2336).** The
  refused chunk is logged at error level with the remedy and skipped, and every later chunk and the
  recent tail still run. The request then ends in an error that names every skipped chunk, so it
  never reports success over a window with a hole in it. A fetch error or any other store error
  still stops the request at once.
- **Deploy: a running strategy builder no longer fails the pre-flight (#2339).** The pre-flight gave
  the builder's probe the live credential's path, which a CI-invoked deploy cannot read, so a deploy
  with the builder running stopped at the pre-flight (safely, with nothing installed). The probe now
  brings a throwaway key of its own. The deploy helper is installed out of band, so a box needs the
  new copy installed before it takes this release.
- **The container image job finds its toolchain (#2339).** The image build's FXCM smoke ran `cargo`
  as root through sudo, where it does not exist. The unprivileged runner now plants the smoke's
  settings row before the privileged build. The fix takes effect from this tag: the v0.1.36 and
  v0.1.37 images cannot be rebuilt from their own tags.

### Internal
- New merge gates: no venue setting may be read from the process environment by a library or a
  binary, no bridge crate's production source may call the process environment (the two one-shot
  tools pinned), and every root that can build a Polymarket client must declare its egress first.
  A text gate holds the mount crate free of venue ids, and it drops from layer 60 to 30, the run
  crate from 62 to 35.
- A chunked, settled-only kline ingest that a backfill lane opts into (the OANDA lane is the first).
- Design records: the history-channels design, the OANDA history lane, and the provisional-commits
  follow-ups for sign-off (#2337), none of them built beyond what is listed above.

## [0.1.37] - 2026-09-30

### Fixed
- **Dukascopy: a second bar interval over an already-ingested window now writes its bars.** A settled
  day whose ticks an earlier request stored was skipped whole for any other interval, so
  `vike-cli data hist fetch dukascopy:EURUSD:5m` over days already fetched at 1m reported success
  with 0 rows written. The skip now stops at the network fetch: the new interval's bars are
  resampled from the stored ticks, and a retry also writes the bars a request that died after
  storing the ticks never did. A chunk whose bars at that interval already exist still fetches and
  writes nothing. The most recent two days are still fetched every time.
- **Dukascopy: a flaky feed no longer sinks a day's download on its first burst of errors.** One hour's
  download used to retry four times within 1.5 seconds, but the feed fails in bursts that last seconds to
  minutes, so a day (24 hourly downloads that must all succeed before it is stored) got through on about one
  attempt in six in a measured run. It now waits 4, 10, 20 and 40 seconds between attempts, honouring a
  server's `Retry-After` up to a minute: about 74 seconds of patience per hour before it gives up. A
  download cut off mid-body is retried too, where it used to fail at once.

## [0.1.36] - 2026-09-30

### Added
- **Venue history reaches a store only through the datahub's Backfill request, and that request gained two lanes
  (decision 0094, #2298).**
  - **Funding rates.** `vike-cli data hist fetch hyperliquid:BTC:funding --days 30` or
    `binance:BTCUSDT.P:funding` asks the datahub for the market funding-rate series. Binance and
    Hyperliquid are the two sources; rows land under the same `funding` interval label as before. A
    Binance perpetual must be spelled `SYMBOL.P` (a bare spot-style symbol is refused before any
    request, COIN-M perpetuals go to the right endpoint, a dated delivery contract is refused as
    having no funding), and a Hyperliquid spot pair is refused before anything is sent. The lane is
    negotiated by a new `backfill_funding` capability, and `vike-cli node ping` shows whether a
    daemon serves it. The old program's `--interval`, `--symbols` mapping and `--store` flags have no
    equivalent.
  - **Dukascopy ticks and bars.** `vike-cli data hist fetch dukascopy:EURUSD:1m --days N` stores the
    window's ticks as quotes and answers with bars resampled from them, so a bar backfill on
    Dukascopy is a tick download (also from the desktop's Data Manager, whose pane says so). Only
    whole bars are resampled, so an unaligned window stores every tick but writes no partial edge
    bar. Opening a chart never starts a tick download.
  - **Long windows are fetched in day-sized chunks** (#2310), each fetched, stored, freed and resampled
    before the next, so a long window no longer risks the data daemon's memory ceiling on a major pair.
    The most recent tail (inside the two-day margin below) is gathered and stored as one batch. A failed
    chunk of a multi-chunk window names itself and leaves earlier chunks written; retry with the same
    bounds. A retry or overlapping request skips settled chunks it already ingested (#2311).
  - **Data inside the last two days is stored under keys of the request itself**, because the feed
    answers a not-yet-published hour like an hour with no trades. Two different recent-window
    requests therefore store their shared ticks twice.
  - **Known limit:** the skip check reads only the quote key, which carries no interval, so asking
    for already-ingested settled days at a second bar interval can report 0 rows written with no
    error. It is recorded in the deferred backlog and not fixed.
- **Settings window with five appearance settings (decision 0093, #2313).** File, Settings opens a window
  for Theme, Market colours, Gradient in window headers, Density and Text size; a choice re-applies
  to every open window at once and is saved as one row (`preferences.theme`, `preferences.market_colors`,
  `preferences.header_gradient`, `preferences.density`, `preferences.text_size`). `vike-cli config
  set preferences.theme midnight` writes the same row; the daemon never reads them.
  - **Accepted words** (any other word is refused by name; note the market-colour word is `colorblind`
    although the window says Colour-blind): theme `graphite|midnight|dusk|carbon`; market colours
    `classic|tradingview|exchange|colorblind`; header gradient `true|false`; density
    `compact|normal|comfortable`; text size `standard|large`.
  - **Four dark themes** (Graphite by default, Midnight, Dusk, Carbon; there is no light theme and
    the app stays dark on a light-mode OS) and **four market-colour sets** (Classic by default,
    TradingView, Exchange, Colour-blind). Status colours are the same in every theme.
  - The app never creates the settings database. With no project settings directory or no database a
    choice applies until the app closes and the window says so; a container started without a
    mounted project has no project settings directory to persist to. Do not create a database just for
    this, because a new settings database replaces the credential files beside it as the credential store.
  - **Rollback caution:** v0.1.35 and older treat the five keys as unknown and mark the store
    unsound, and the v0.1.35 daemon refuses to start on that mark. No `vike-cli config` verb deletes
    a single row, so keep these rows out of a store an older daemon reads if you may roll back.
- **The Vike mark, an app icon and a Linux launcher (#2314).** The title bar draws the orange Vike mark and
  the window carries the icon and the permanent app id `io.vike.Trader`. On Linux,
  `vike-desktop --install-desktop-entry` writes a launcher entry and icons under the user's data
  directory, started with the project's settings directory; `--uninstall-desktop-entry` removes
  exactly those files. Run it from the project folder, and again after moving the binary or the
  project. The Windows release build embeds the icon; cross-building vike-desktop for the Windows GNU
  target now needs x86_64-w64-mingw32-windres (package binutils-mingw-w64-x86-64) and a missing
  windres fails the build. MSVC builds warn and carry no icon.
- **`vike-cli config set venue.*` is validated and journalled (#2312, #2330).** A declared catalog lists every
  settable venue field (Dukascopy server, FXCM url and connection, IBKR backend, host and port, and
  Polymarket's proxy, rate-gate, exec-market, pre-submit and socket-batching fields). An undeclared
  field, a wrong scope or a value failing the field's grammar is refused before the store opens, so a
  typo no longer creates a row nothing reads. Secret fields take their value on stdin only and the
  journal records `<secret>`. On a box with no settings database the verb never creates one: it refuses
  and points at `vike-cli secrets move-venue-config` (run `vike-cli secrets migrate` first).
  `vike-cli config show` gains a venue-settings block and flags stored rows nothing declares. Scripts
  that wrote free-form `venue.*` keys must use declared names.
- **New repair and check verbs:** `vike-cli config migrate-store` (see decision 0095 below) and
  `vike-cli config retired-env`, which the deploy pre-flight uses.

### Changed
- **⚠ BREAKING: the per-venue backfill programs are deleted (decision 0094, #2290, #2298).** The aster,
  binance, bybit, deribit, hyperliquid, okx, dukascopy and funding-rate backfill programs no longer
  exist. The datahub already served the six kline venues; funding rates and Dukascopy ticks and bars
  are now lanes of its Backfill request, so no coverage is lost for these eight.
  - **Replace any script or cron** that ran one with `vike-cli data hist fetch VENUE:SYMBOL:INTERVAL
    (--days N | --from D --to D)`, pointed at the datahub with `--addr` or the configured datahub
    address. The old programs wrote straight into a store directory; the fetch needs a reachable
    datahub of this release with the `backfill-serve` feature (the shipped one has it) and a key
    with write scope. `--addr` now works on a venue fetch, and the "built without backfill-serve"
    hint appears only when the server never advertised backfill. The vendor-data programs (databento,
    tardis and the others) still write into a store directly.
  - The datahub uses the venues' plain pagers, not the deleted program's concurrent lanes. Only a large
    Binance spot backfill changes (the code counts 6 lanes on spot and 1 on the perpetual host); the
    code's own estimate is about five times slower, computed from published budgets and not timed.
  - **Four one-shot programs are also deleted** (`collector_supervisor`, `ibkr_backfill`,
    `hyperliquid_funding_backfill`, `poly_reparse`), each measured unused. There is no replacement:
    0094 says a resident collector would return only as a datahub plane driven by settings rows,
    and an IBKR or account-address history lane would need a credential read the data daemon
    deliberately lacks. The `ibkr`, `poly-reparse` and `venue-backfill` features of the backfill
    crate go with them, and `VIKE_PACE_BOOK` (the pace-file override of those programs) has no reader
    now; it is not refused, so remove it from any unit or script.
  - A zero-width interval such as `0m` is refused for every lane before any fetch.
- **⚠ BREAKING: `live` now means mainnet, and four venue switches are retired (decision 0095, #2319).**
  - **`BINANCE_MAINNET`, `BYBIT_MAINNET`, `OKX_MAINNET` and `HYPERLIQUID_MAINNET` refuse startup**
    when set non-empty (desktop, every `vike-cli` subcommand, the trading daemon and the data daemon),
    printing `vike-cli config set policy.venues.<venue> live`. A `{VENUE}_MAINNET` line in the
    credential store is refused too. An empty value still starts. Remove each line from every installed
    unit, its drop-ins and its EnvironmentFile (`systemctl cat` shows the unit and drop-ins, not
    EnvironmentFile contents), then reload and restart.
  - **The ceiling alone picks the network.** A `live` ceiling with LIVE-tier keys in the store is
    mainnet; a `demo` ceiling is the demo network; a `live` ceiling with only demo-tier keys stays on
    paper with block `LiveCredentialsAbsent` instead of falling back to demo. The Hyperliquid feed
    and startup probes follow the ceiling. The `vike-backend venues --json` block strings changed
    (`POLY_EXEC unset` is now `flags.poly_exec off`, and `mainnet switch unset` is gone), so update any
    script matching them. Aster's and Polymarket's exec network choice is unchanged, but aster's startup
    clock probe and the venue-arming report now honour the ceiling: under a ceiling below live they no
    longer measure or report the mainnet host.
  - **A one-time migration rewrites `live` ceilings of these four venues to `demo`**, the network
    those rows actually traded unless the switch was set. It runs once per store, is journalled per
    row and runs at boot before any ceiling is read. A box that really traded mainnet through the switch
    is on demo after the upgrade. **After upgrading, check `vike-cli config show` and re-set `live`
    only where mainnet is intended**, then restart. Do not write `live` for these venues before the
    upgrade.
  - **Which process does what with a pending migration:** the trading daemon and `vike-backend venues`
    refuse to start if it is pending and the store is not writable; `vike-cli` and the desktop continue
    and show a store warning, so `config show` still answers; the data daemon reads no ceiling and does
    not migrate. Any settings write to a not-yet-migrated store migrates it first. Run
    `vike-cli config migrate-store` as the user owning the settings database to apply it explicitly.
  - **Rollback residual:** a pre-0095 binary that writes a `live` ceiling into a migrated store
    writes the old meaning, which this release reads as mainnet. Check `config show` first.
  - **The deploy pre-flight fails on retired variables.** It checks each unit's merged environment,
    EnvironmentFile and the running process's own environment, and names the unit and line. **Install
    the new deploy helper on each box before the release**, or the check is silently skipped. A
    release whose `vike-cli` lacks `config retired-env` is skipped with a warning, so rollback stays
    deployable.
- **⚠ BREAKING: Polymarket's mounted paths read no environment (decision 0095, #2330).**
  Eleven variables refuse startup when set, and the refusal prints the paste-ready `vike-cli config set`
  line: `POLY_EXEC`, `POLY_RECONCILE`, `POLY_PROXY_ENABLED`, `POLY_PROXY_HOST`, `POLY_PROXY_PORT`,
  `POLY_SOCKS_PROXY`, `POLY_WS_PROXY_ENABLED`, `POLY_RATE_GATE`, `POLY_EXEC_MARKETS`,
  `POLY_PRESUBMIT_REGISTER` and `POLY_WS_TOKENS_PER_SOCKET`. They become `flags.poly_exec`,
  `flags.poly_reconcile` and `venue.polymarket.*` rows. The bridge is not yet free of environment reads
  overall: the settlement watcher, heartbeat, auto-redeem, market-resolve and egress-probe reads remain
  in code nothing starts, and credentials such as the private key still come from the credential map.
  `POLY_SOCKS_PROXY` takes its value on stdin. `POLY_SOCKS_PROXY=` and
  `POLY_WS_PROXY_ENABLED=` refuse even when empty, since starting would silently dial the built-in
  local SOCKS proxy. If `POLY_EXEC` and `POLY_RECONCILE` were the only thing arming Polymarket, set
  `flags.poly_exec` and `flags.poly_reconcile` to true or the venue stays disarmed.
  - **The data daemon's unit no longer sets `POLY_PROXY_ENABLED=false`, and the daemon now refuses
    retired variables instead of ignoring them.** Polymarket's proxy defaults ON, so a box reaching
    it directly needs `venue.polymarket.proxy_enabled = false` as a row. A proxy value still held
    under the old name in the credential store is honoured for now with one startup warning (row,
    then credential, then default); a later release will refuse it.
  - **If the settings database exists but cannot be read, neither daemon refuses to start.** They log an
    error and take Polymarket's egress from the credential store's legacy names, then from the built-in
    default (the local SOCKS proxy). A box that reaches Polymarket directly then fails every Polymarket
    dial until the store reads again, while the daemon otherwise looks healthy. After removing the
    unit's proxy line, check the startup log line "polymarket egress declared".
  - The desktop Data Manager's Polymarket proxy box now saves to the `venue.polymarket.socks_proxy`
    settings row (journalled) instead of the credential store, and reads its starting value from the
    settings directory. A proxy previously saved as a credential is honoured with a startup warning
    until `vike-cli secrets move-venue-config` moves it.
  - **Roll out in this order:** confirm the row exists (`vike-cli config show`); run `vike-cli secrets
    move-venue-config --dry-run` and ensure a row and a credential do not both exist with different
    values; delete the `Environment=POLY_...` lines from the installed unit, drop-ins and EnvironmentFile
    of both daemons, reload, restart, and confirm Polymarket is still reached (if the data daemon cannot
    reach Polymarket after the restart, put the line back and stop); deploy; then run
    `vike-cli secrets move-venue-config` and restart until `--dry-run` prints `nothing to move`.
  - `POLY_REWARD_WEIGHT` has no reader any more (#2293), so setting it does nothing (it is not refused).
- **⚠ BREAKING: HIP-3 and five more venue toggles leave the environment (decision 0095, #2332).**
  - **Six variables now refuse startup when set non-empty**, and the refusal prints the paste-ready
    `vike-cli config set` line: `HYPERLIQUID_HIP3`, `VIKE_ALLOW_WITHDRAW_KEYS`,
    `VIKE_BINANCE_TRADE_LITE_FILL`, `VIKE_BYBIT_FAST_EXEC`, `VIKE_MARK_STREAMS` and
    `VIKE_MARK_STREAMS_ASTER`. A blank value still starts. `VIKE_MARK_STREAMS=0` prints one line per
    venue. The deploy pre-flight catches a leftover line in an installed unit before anything is
    installed.
  - **Where each one went.** `flags.hyperliquid_hip3` and `flags.allow_withdraw_keys` were already
    settings rows and only lose their environment layer. New rows: `venue.binance.trade_lite_fill`,
    `venue.bybit.fast_exec` and `venue.<venue>.mark_streams` for binance, bybit, okx, hyperliquid
    and aster.
  - **Deleting an `=0` export can arm what it held off.** For both flags the environment beat the
    row, so a unit exporting `VIKE_ALLOW_WITHDRAW_KEYS=0` over a row of true ran with the override
    off. Write `false` to the row (`vike-cli config set flags.allow_withdraw_keys false`, the line
    the refusal prints) before you delete the line; likewise for `HYPERLIQUID_HIP3=0`.
  - **A row reaches every daemon on its settings directory; a variable reached one unit.** Check
    which unit set the variable before you write its row. A daemon that must differ needs a
    settings directory of its own.
  - **The datahub's Hyperliquid venue catalog now reads the same `flags.hyperliquid_hip3` row as the
    trading daemon's mount**; it used to read the variable alone.
  - **A padded or commented value never acted and is refused as the default it ran.** The old readers
    compared the value exactly, so `" 1"` or `0 # comment` ran the default. The daemon's refusal says
    so; the deploy pre-flight trims values before it judges, so follow the daemon's message.
  - **Upgrade order:** the previous release reads the two flag rows but none of the new `venue.*`
    fields, so write the flag rows before the release and the `venue.*` rows after it. A box with no
    settings database cannot take these rows: run `vike-cli secrets migrate` first.
- **⚠ BREAKING: unattended agent sessions change no setting, and settings writes lose the typed confirm
  (decisions 0086, 0040, #2309, #2297, #2288).**
  - Under `vike-cli mcp --unattended`, which the scheduled agent runner now passes on every launch,
    every `set_setting` call is refused before a preview exists, whatever the key. Change settings
    with `vike-cli config set` or the GUI, or run the agent attended. A runner pointed at an older
    `vike-cli` refuses the unknown flag and the run ends as Failed (the Failed outcome is inferred from
    the runner's exit table).
  - The `vike-cli trade` REPL's `set-setting` takes `<full.dotted.key> <value>` like `config set`
    (a leading file token such as `policy.toml` is refused), prints `change: <key>: old -> new`, and
    asks one y/N. The MCP `set_setting` tool drops the `file` and `policy_confirm` arguments and its
    preview gains a `change` object. Older daemons and newer clients work together in either order.
  - The desktop's Backend settings editor saves every row, ceilings included, with one click:
    `Save` or `Save anyway`, with no typed confirm.
- **⚠ BREAKING: Rust module paths moved and alias re-exports were deleted (code-layout phase 1, #2240 to
  #2260).** Out-of-tree code or forks must rename imports: order and money modules of the model crate under
  `orders::` and `money::`, the Polymarket exec plane under `exec_plane::`, the backfill crate's
  collectors under `venues::`, 42 app-core modules into `backend/`, `data/`, `orders/` and `ui/`,
  trade aggregation into the orderflow crate, and the tearsheet core from the report crate to the
  analytics crate. Several alias re-exports are deleted with no replacement.
  - Two run-profile validation messages and one `--html` help string changed, so the vendored
    profile and CLI fixtures changed; the documentation site, which vendors them byte for byte,
    needs the new copies.
  - The report crate's `journal` feature is deleted (its default is now empty), and the `vike-cli`
    tree loses that crate. Drop `--features journal` from any build line, and do not pass the
    retired `alerting-standalone`, `report-journal-off` or `backfill-ibkr` CI suite keys (they now
    exit 1). A `backfill-serve` suite key is new; it compiles the datahub with `backfill-serve` so a
    bridge-only change is covered.
  - Building the recorder crate directly with `--features polymarket,binance` no longer works; the
    datahub's `record-polymarket` and `record-binance` features remain.
  - `vike-deribit` gains a default-on `exec` feature; its keyless half builds with
    `--no-default-features`, and the datahub's `backfill-serve` build now uses that half.
  - The cTrader live-mount smoke moved to the cTrader crate:
    `cargo test -p vike-ctrader --test ctrader_live_mount_smoke -- --ignored --nocapture`, with a new
    leave-flat twin `ctrader_leave_flat_smoke` (demo orders, not yet run against a demo account).
- **The datahub's recorder and live market-data plane now hold their venue feed clients from one broker
  (decision 0092, #2277).** A key both want (for example a Binance depth stream) is subscribed once from
  the box instead of twice, which removes a second depth re-seed against the weight budget the
  order-signing daemon shares. The startup warning about double subscription is gone. Binance, Bybit,
  OKX, Aster and Hyperliquid feed clients are shared; Polymarket keeps one per holder so a window closing
  cannot put a gap in a recorded book. No operator action.
- **The desktop GUI moves onto one design system (decision 0093, #2300 to #2328).** Bundled Inter and
  JetBrains Mono replace system fonts (the light weight is gone), one bundled icon font gives each meaning
  one icon, and the main window, charts, DOM, Polymarket cockpit, Data Manager and Studio follow the chosen
  theme, market colours, density and text size.
  - **On the default look with nothing touched:** captions are a lighter grey, "connected" and error
    states use fixed status colours, default chart axis labels and value tags are smaller (12 to 10),
    window titles are one point smaller, and untouched charts change their candles to the Classic pair
    (#40BA50 / #F85249). A colour picked in Chart settings stays that chart's override, with a revert
    button. In the DOM, LIVE is an accent-filled pill (no longer red) and PAPER a grey outline; choose
    Market colours, Exchange for the previous bid/ask pair.
  - The Polymarket ticket's BUY buttons are disabled with a reason while disarmed, and the cockpit's
    amber countdown and current-window border are gone. Studio's violet accent is gone and eleven
    disabled controls now say why. Icon-only buttons carry their tooltip words as accessible names, so
    re-key any UI Automation lookup that matched an old glyph such as the cross.
  - **Some screens do not follow the theme yet:** Connections, Backend settings, Trade, News,
    Calendar, Tearsheet, Greeks, indicator series colours and compare-overlay colours still paint
    their own colours and sizes (the list is not exhaustive), so expect mixed looks on Midnight, Dusk and
    Carbon (Graphite is unaffected). The Settings window says so.
  - The thin desktop container image no longer installs a font package; v0.1.36 is the first image
    built without it, so look at its first window for text.
- **Log targets changed for events from moved modules.** An event logged without an explicit target
  takes its module path, so moved modules report new targets, for example `vike_polymarket::user_ws`
  is now `vike_polymarket::exec_plane::user_ws`, and the footprint aggregator's warnings moved to
  `vike_orderflow::bar_agg`. Two explicit Polymarket targets also renamed: `heartbeat` and `mount`
  now sit under `exec_plane`. Update any `RUST_LOG`, `VIKE_LOG` or `VIKE_LOG_FILE_LEVEL` directive or
  log query naming an old target. The pinned `vike_tradehub::audit` target did not move.
- **The tradehub unit no longer passes `--config` (#2228).** The daemon profile comes from the active
  daemon-profile row (`vike-cli config bootstrap-daemon`, `config activate daemon <name>`). The binary
  still accepts the flag, ignores it and warns; a later release refuses it, so drop it from installed
  units and confirm the active profile row exists with `vike-cli config show`.
- **Paper mounts capped by an arming ceiling now enforce the per-account exposure ceiling (decision
  0088, #2238).** A venue on paper because its ceiling refuses it applied only the box-wide exposure
  limit, a more permissive engine than the same venue on paper for lacking credentials. It now takes the
  minimum of both ceilings as a live mount does, so a paper rehearsal with `policy.account_exposure`
  rows may refuse orders it used to admit. Live mounts are unchanged.
- **Unused binaries deleted (#2293).** `ibkr_mount` and `polymarket_maker_paper` (measured unused, not
  release assets) go with the Polymarket market picker and the flow-toxicity producer. As a
  consequence, no binary mounts IBKR live market data, and the maker's flow-toxicity guard has no producer.
- **Internal:** venue mount seams for aster, Hyperliquid, cTrader and Dukascopy moved from the mount
  crate into their bridge crates (0088), and a venue mount contract and registry were added with every
  row still on the legacy path, with no behaviour change (#2333). New merge gates: mojibake, shell
  backtick and broken intra-doc links, plus the named-crate dependency fences of decisions 0089 to 0091.
  Dependencies: datafusion 55.1, rustix 1.1.5 and cc 1.4.6; ureq is held below 3.4.1 (currently 3.4.0),
  whose body idle timeout becomes a total ceiling, with a monthly release watch. Runbooks and shipped
  skills now give `vike-cli config set` where they said to hand-edit settings files, and the Windows
  daemon runner and MCP smoke script follow the settings database.

### Fixed
- **A Dukascopy window fetched while still recent no longer loses data for good (#2327).** A request whose
  whole span fit in one chunk shared its commit key with the later settled fetch, so ticks the feed
  published after the early fetch were silently dropped. An early fetch now commits under separate
  provisional keys, and fetching the same window (same bounds and interval) after it settles replaces
  the provisional rows atomically with no lost or doubled volume.
  - **The write-ahead log frame gains a supersede field (magic VWL2) for every commit, not only
    Dukascopy.** Old frames still decode, but an older datahub cannot decode a new frame, so stop the
    datahub cleanly (no pending log record) before rolling it back.
  - **Sharp edges:** if compaction merges a provisional part into a multi-key part first, the settled
    commit refuses, permanently for that exact window; if the bar-side supersede refuses after the
    quote side succeeded, later retries report success while bars stay on provisional data. The
    remedy is to delete the symbol's quote and bar series and re-backfill.
  - Delete by producer with the bare `--produced-by dukascopy`; with a trailing colon, series holding
    provisional or superseded parts are refused.
- **Operator-facing startup and refusal messages of the live node show their dashes again (#2299).** Seven
  messages (the preflight SKIPPED, NO-GO, FAILED and DEGRADED lines, the second-account mount line,
  the LIVE execution warning and the no-live-account-lock refusal) had a double-encoded em dash in
  v0.1.35.
- **The desktop's Backend settings editor no longer opens cut off**, with its Save button out of
  view, on a row near the bottom of the table; it is scrolled into view once per opening.
- **The Data Manager's store-bar buttons for other stores are disabled with a reason** instead of
  being live buttons that switched nothing. Stored-series coverage bars now match their legend, and
  every disabled control says why on hover.
- **A DOM opened from the launcher's 320 pt window fits (#2329, #2331).** Header, toolbar and footer stack
  onto as many rows as needed, so STALE, Reduce and Recenter are no longer cut off and the footer no
  longer covers the position readout. The Elite heatmap's empty caption splits onto two lines instead of
  running over the ladder.
- **The Windows daemon runner warns instead of throwing** when no profile file exists, since a box on
  the settings database has none; on a fresh box create the profile row with `vike-cli config
  bootstrap-daemon` before `start`.

## [0.1.35] - 2026-09-27

### Added
- **Studio's Run works against a keyed compute daemon.** The desktop now holds the datahub CONTROL
  key for Studio's compute dial — the owner's ruling on decision 0083's question 1 — and for nothing
  else. `just studio` reads it on the server box with the new `vike-cli datahub control-key` and
  hands it to the desktop as `VIKE_STUDIO_COMPUTE_KEY`, beside the builder key it already hands
  over, so one command now gives Studio both Build and Run.
  - **The key's reach is narrow by construction.** It arrives under its own variable (never
    `VIKE_DATAHUB_CONTROL_KEY`), from the environment only, as a type no other desktop dial accepts:
    the datahub store, the chart seed, the venue catalog, the named run and the trading-node observer
    cannot be handed it. Studio signs it only toward a server whose pre-auth handshake is the compute
    daemon's; pointed at the datahub, Studio refuses before sending anything, so the key can never
    open a session that backfills or deletes store data.
  - **`vike-cli datahub control-key` is the one verb that hands a key VALUE out**, and only to a
    pipe: it refuses a terminal, prints the value and nothing else on stdout, names the key's id on
    stderr, and reads exactly the key that box's datahub verifies. No MCP tool offers it.
  - **Run needs this release on the server box.** Against an older box the launcher warns that it
    read no compute key and starts the desktop for Build alone.
  - **`just studio` builds the desktop before reading either key**, then starts the built program,
    so neither key reaches cargo or any build script. Passing a built `vike-desktop.exe` works as
    before.

### Changed
- **⚠ BREAKING: settings live only in the settings database (decision 0086).** Every binary reads
  its settings from the rows in `<project>/settings/db/vike.db` and from nothing else, and a write
  changes exactly one row.
  - **A leftover `policy.toml`, `config.toml`, `preferences.toml` or `flags.toml` is not read.**
    Nothing warns about it and nothing compares it with the database. `vike-cli config show`
    names where every value comes from, so check there that no key you rely on comes from its
    default.
  - `vike-cli config set <key> <value>`, `vike-cli backend setup`/`connect`, the desktop's arming
    Save and the daemon's own control channel all write through the same one-row path. It checks
    that the result still boots cleanly before it commits. **The daemon writes settings rows
    now.**
  - **`config set` no longer takes `--confirm`**, for any key and ceilings included. A script that
    still passes it gets an error that names the flag as removed.
  - **`vike-cli config compare` and `vike-cli config adopt` (including `--undo`) are retired** and
    fail with a message saying what replaced them. `config mirror` now carries profiles only.
  - **No command repairs an unsound store.** Restore `vike.db` from the box's backup.
  - **A box with no active daemon profile refuses to start**, naming
    `vike-cli config bootstrap-daemon <name> --venue <v> --asset-class <c>
    (--symbol <sym>|--token-id <id>)` as the fix — the writer that builds a profile row from
    arguments, never from a file, and activates it in one act. `vike-cli config
    bootstrap-recorder` is the same rung for a first recorder profile.
  - **The daemon's `--config <file>` and the datahub's `--record <file>` are accepted and ignored
    for this one release.** The ACTIVE profile row in the database is what runs. On a box with
    no profile row yet, `vike-cli config bootstrap-daemon` and `vike-cli config bootstrap-recorder`
    create the first one from arguments. Drop both flags from installed units before the next
    release.
  - The `settings/*.example.toml` files are gone from the source tree and the container image.
    The image entrypoint warns instead of refusing when `settings/db/vike.db` is absent, and names
    the bootstrap verb.
- **⚠ BREAKING: `--store DIR` no longer reads history.** Every history reader — `backtest` runs and
  its compute daemon, the `cheap_np_*` tools, `tearsheet`, `vike-study` and
  `vike-cli backtest run --local` — refuses it by name and reads through a datahub instead
  (decision 0084). For files on your own machine, start a key-less one beside the run:
  `VIKE_DATAHUB_STORE=DIR vike-backend datahub`. It binds loopback only, and every reader dials loopback by
  default. `--archive` is unchanged, and the `data` verbs that WRITE a store keep their `--store`.
  - A parameter search started before this release recorded a local path as its store, so resuming
    one is refused — the identity it recorded named a directory the run never read.
  - `tearsheet` and `vike-study` no longer link DataFusion.
  - **`data export` is a reader too, and closes the same way.** `vike-cli data hist export` and the
    engine's `backtest data export` refuse `--store` by name; the Parquet file is written from bars
    read through the datahub the settings name (`config.datahub_addr`, or `VIKE_DATAHUB_ADDR` above
    it; loopback by default), and it is the same file a local export wrote. `vike-cli data hist`'s
    read verbs (`ls`, `get`, `coverage`, …) now refuse `--store` with that same sentence. To export
    files on your own machine, start a key-less datahub on them first, exactly as for a run. `rm`,
    `repair`, `fetch --source starter|demo` and `seed-demo` WRITE and keep their `--store`.
  - The store-side `DataFusionHist::export_bars_parquet` is removed; `vike_data::write_bars_parquet`
    over the store's own `load_bars` is the same file.
- **⚠ The strategy builder's unit is a filesystem jail, and its key moves into a credential file.**
  A caller's `build.rs` runs inside the builder, so what that unit can see is the boundary around
  arbitrary code. `deploy/vike-strategy-builder.service` now mounts an empty, read-only tmpfs over
  `/mnt`, `/srv`, `/opt` and `/media` and binds back only the binary, the toolchain, the source
  tree, the cargo cache and `user_data/plugins`. It drops `AF_UNIX`, so a build cannot read other
  units' environments over the system bus. It loads the key with `LoadCredential=` from
  `settings/strategy-builder.key`, the file the Studio launcher already reads, instead of an
  `Environment=` drop-in that any local user could read over D-Bus.
  - The daemon accepts `VIKE_STRATEGY_BUILDER_KEY_FILE` beside `VIKE_STRATEGY_BUILDER_KEY` and
    **refuses to start with both set**. Delete the old key drop-in in the same step as installing
    the new unit. The container accepts either form too.
  - The `cargo` it spawns gets a cleared environment plus a short allowlist, so the key no longer
    reaches build scripts. A proxy or CA setting belongs in the toolchain's
    `cargo-home/config.toml` now; an `Environment=` line or `.env` no longer reaches cargo.
  - A project root under any other top-level directory must add that directory to the unit's
    `TemporaryFileSystem=` line. The unit's header lists what the jail still does not close.
  - The daemon now refuses to start if `VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT` names a tree that
    carries its own `settings/` — the jail binds that variable's target back read-only into every
    compile, so a project root there would hand every `build.rs` the credential store the jail
    exists to hide. The unit's header warns against it at the same sentence that first sanctions a
    hand-maintained checkout as an alternative to the release asset.
- **The backtest daemon's unit stops setting `VIKE_HIST_STORE`.** That daemon has read its history
  from the datahub since 0.1.34, so nothing the unit runs read the variable and the line configured
  nothing. The unit now documents what does pair it with the data daemon: the address it dials and
  the node keys it presents, both of which one project root for the two units keeps equal. An
  installed copy that still carries the line is harmless — a deploy installs no unit file — and can
  lose it at the next hand edit.
- **Machine-wide venue settings are stored as `'any'` instead of an empty tier** (the settings-store
  plane's step 7). One uniqueness rule now covers every `venue_setting` row, where two partial ones
  split the table on the empty value. The settings read exactly as before, under the same names. No
  version constant moved: the store is carried over on its first write, triggered by its shape.
  - **⚠ OPERATORS: a rollback past this release breaks Polymarket egress unless you pin it first.**
    Measured against a copy of a real migrated store: an older binary reads the three
    `POLY_PROXY_*` settings under names nothing looks up, so Polymarket falls back to its built-in
    default, which is proxy ON. On a box with no SOCKS proxy listening, every Polymarket connection
    is refused and a recording daemon stops taking that venue's tape. `vike-cli config check` exits
    0 and notices nothing. **Before rolling back, set `POLY_PROXY_ENABLED` (plus the host and port,
    if you do use a proxy) in the process environment of every unit that dials Polymarket** — a
    systemd drop-in where the unit loads no `EnvironmentFile=`. Remove it after rolling forward,
    because the environment beats the store.
  - The older binary also cannot write a machine-wide venue setting: `vike-cli config set
    venue.polymarket.proxy_enabled …` fails with `NOT NULL constraint failed: venue_setting.tier`.
    Roll forward rather than editing the table. Credentials, per-tier settings and onboarding a new
    account all keep working after a rollback.
  - **The carry-over is not tied to a release.** The first write by ANY binary that carries it
    migrates the store it points at — a hand-built `vike-cli` included. Do not point a newer CLI at a
    store whose installed daemons are older.

### Fixed
- **Studio no longer checks a Rust plugin's source as Rhai.** With the Strategy pane on
  `Plugin (Rust)`, the editor header showed a red `● error · line N` (and an error banner) for
  valid Rust source, next to the `● built <sha>` from that same source's successful Build. The
  header ran the Rhai compile check on the editor whatever the strategy source was. In Plugin mode
  it now shows a neutral `● Rust · Build to check`, and the Build result in the Strategy pane is
  the verdict. The Rhai check no longer runs on that buffer, and switching back to `Rhai script`
  checks the buffer again. Rhai and Native modes are unchanged.
- **Studio no longer lets Run use a plugin artifact older than an edit you made during its Build.**
  When a Build's sha arrived, the Studio recorded the editor text at that moment as the built
  source, not the text it had sent. A Build can take minutes. Anything typed while it ran counted
  as built, Run was not blocked, and Run then used the older artifact for the newer code without
  any error. The Studio now keeps the exact text it sent with the Build, so an edit made during the
  Build keeps Run blocked with the "changed since the last Build" reason.
- **Templates and the AI copilot no longer write Rhai over a plugin's Rust source.** In
  `Plugin (Rust)` mode, the Sweep pane's template `Load`, the `Browse templates` cards, the empty
  results panel's `Load a template` and the copilot's `Apply to editor` all overwrote the editor
  with Rhai while Plugin stayed selected. Because a Plugin save keeps no copy of the source, that
  lost the only copy. These four controls are now disabled in Plugin mode, and hovering one says
  why. Pick `Rhai script` first to use them.
- **The `● unsaved` chip now clears after saving in Plugin mode, and its tooltip says what Ctrl+S
  saves there.** A Plugin save stores the plugin's name and its Build's sha, not the editor text.
  The save never updated the chip's comparison point, so the chip never cleared, and the tooltip
  promised that Ctrl+S would save the edits. A save now clears the chip when the saved sha was
  built from the current editor text. After a later edit, the chip stays lit and its tooltip says
  to Build first.
- **Studio remembers the strategy source across a restart.** The editor text was restored but not
  the Rhai/Native/Plugin selection, so a plugin's Rust source came back in Rhai mode. The Studio
  now saves the selection with its workspace, along with the Native strategy (by name), its
  parameters and the plugin's name. A `studio_workspace.json` written by an earlier version still
  loads and opens in Rhai mode. A plugin's sha is not restored: after a restart, press Build again.
  The builder caches by content, so rebuilding unchanged source is fast.
- **Loading a saved Rhai row no longer overwrites a Plugin's Rust buffer.** The same hazard the
  four writers above were fixed for reached the Saved list too: in `Plugin (Rust)` mode, `Load` on
  a saved Rhai row switched the strategy source and wrote the saved script over the only copy of
  the Rust source. Loading a Rhai row is now disabled (with the reason on hover) while a Rust
  buffer with no other copy is selected; a Native or Plugin row's `Load` is unaffected, since
  neither touches the editor buffer.
- **Native mode's `● unsaved` chip now tracks the actual Native selection, not the parked Rhai
  buffer.** The dirty check and the chip's tooltip were shared with Rhai mode
  (`editor.source != saved_source`), so editing a param row never lit the chip (nothing in
  `editor.source` had changed) and an unrelated stale Rhai buffer could light it instead. Native
  mode now compares its own `(name, params)` baseline, set on Save and on Load, and the tooltip
  names what a save actually captures.
- **`vike-cli datahub --help` exited 2**, printing its usage to stderr behind
  `vike-cli help requested: vike-cli datahub`, and every real usage error on that verb read
  backwards (`vike-cli <the error>: vike-cli datahub`) — its parse error reached the shared exit
  helper with the arguments swapped. Help is exit 0 on stdout now, as on every other verb, and
  `crates/vike-cli/tests/help_cli.rs` now also sweeps every top-level verb the roster lists, beside
  its per-verb tests — a verb added after those were written had none, which is how this one was
  missed.
- **`vike-cli backtest run --local` saves its run where `backtest ls` looks.** With
  `VIKE_SETTINGS_DIR` naming one project and the working directory inside another, the engine saved
  the run in the project it was standing in, and `ls`/`show`/`path` never found it. The engine is
  now told the `user_data` directory the reading verbs use; a `VIKE_USER_DATA_DIR` you set still
  wins.
- **`vike-cli` finds the backtest engine on a release install again.** A release ships the engine
  inside `vike-backend`, as its `backtest` tool, but `vike-cli` looked for that binary under its
  pre-rename name, `vike`. So `backtest run --local` and every `data` verb that drives the engine
  reported it missing while it sat beside `vike-cli`. The CLI now looks for `vike-backend` in
  `<project>/bin/` and beside itself, after any standalone `backtest`.

### Fixed
- **The container image carries the FXCM runtime, and the shim binds inside it.** Every
  `vike-tradehub` image up to 0.1.34 shipped an empty `/opt/vike/lib`, so FXCM stayed paper inside
  Docker whatever the operator configured. The release now attaches the ForexConnect runtime — the
  pinned SDK libraries plus the shim — as one checksummed asset, `fxcm-runtime-linux-x64.tar.gz`. The
  image build stages it at `/opt/vike/lib`, refuses a release build that lacks it, and refuses to
  publish an image whose own loader cannot open the shim and bind its entry points. That is what is
  checked: the runtime is present and loadable. No FXCM session has been made from a container. FXCM
  still arms only with credentials and a `policy.toml` ceiling above `paper`.
  - **This publishes FXCM's proprietary runtime libraries** inside every image, on the author's
    distribution grant from FXCM. The public source mirror does not re-attach the asset.
  - A deploy downloads the new asset (it verifies the whole `SHA256SUMS`) and installs none of it: a
    box's `<project>/lib` stays whatever `just fxcm-package` put there.

## [0.1.34] - 2026-09-23

### Changed
- **The settings store becomes a relational plane, and the migration reaches a store that already
  exists** (stages 3–5). `venue_arming` folds into `account.armed`; every table gains
  `AUTOINCREMENT` so a freed account number stays freed; the account tier `sim` is renamed `paper`;
  one measured-dead column is dropped; `AccountEdit::SetTier` lands.
  - **⚠ OPERATORS: the upgrade is a ONE-WAY DOOR for NEW ACCOUNTS.** Measured against a copy of a
    real migrated store: a rolled-back older binary still READS every credential and can ROTATE one,
    but **cannot mint a new account** — it fails with
    `CHECK constraint failed: tier IN ('paper','demo','live')`. If you roll back, rotating keys
    works and onboarding a venue does not; roll forward rather than editing the CHECK by hand.
  - **No version constant moved.** Every migration triggers on the store's SHAPE, asked of
    `sqlite_master`, never on `SCHEMA_VERSION` — because `READABLE_SCHEMA_VERSIONS` is
    `[1, SCHEMA_VERSION]`, so a bump to 3 would silently drop 2, the version live boxes hold, and
    every venue would fall to paper with nothing erroring.
  - The rebuild runs inside the caller's transaction and rolls back whole: verified on a live-shaped
    store to preserve every row, every id and every `REFERENCES` clause, restore all indexes, leave
    no scratch table, and complete on re-run after an interruption. It is idempotent.
  - **Fixed before release:** `upsert_rows` — the writer `vike-cli secrets set` goes through — was
    the one credential writer that never ran the repair, so minting a key on an unmigrated store met
    the old CHECK and showed a bare constraint failure naming nothing.
- **A user-authored strategy can no longer name the live core** — it is a compile error now, rather
  than a convention. Several crates moved to the layer they actually occupy, and the write-ahead
  journal became its own crate below the core that writes it.
- **The backtest daemon reads history over the wire** rather than touching the store directly
  (decision 0084), and the datahub grew the verbs that serve it, including `series_facts`. Four scan
  verbs now carry a row budget.
- **Dukascopy's JForex home lands in the project folder, not `$HOME`.**
- A settings remedy now follows the store that actually answered, not the file that used to.

### Fixed
- **The release stops shipping silently wrong.** The FXCM shim is built again and is now REQUIRED on
  the release runner rather than skipped where no SDK is staged — it had vanished from scratch space
  and a release shipped without it, silently. A failed smoke can no longer publish.

### Added
- **A portable Temurin JRE ships as a release asset** — the JVM that runs the JForex sidecar jar, so
  a deployment no longer has to find one.

## [0.1.33] - 2026-09-23

### Changed
- **A runtime-loaded Rust plugin receives the whole `Strategy` seam, not two hooks of it**
  (`ABI_VERSION` 2 → 3). `PluginVTable` carried `warmup` and `on_bar`; `build_plugin` REFUSED to
  build a user file overriding anything else, which made the gap loud but left every strategy that
  needs a fill or a book stuck in the build-time tier. Thirteen hooks are now delivered —
  `on_start`, `on_quote_tick`, `on_trade_tick`, `on_order_book`, `on_schedule`, `on_fill`,
  `on_feed_status`, `on_mark`, `on_reference_quote`, `on_flow`, `on_order_event`,
  `on_params_updated`, `on_stop`.
  - **The refusal mechanism STAYS and now refuses exactly three**: `params`, `save_state`,
    `load_state`. Not an oversight — two of them RETURN owned data, which needs a
    plugin-allocates/host-frees ownership contract this ABI has never had, and `load_state` is
    grouped with its read half because a save/load pair half-delivered is worse than one plainly
    absent. `vike_strategy_plugin::host::UNWIRED_HOOKS` carries the argument;
    `tests/hook_roster.rs` still holds both lists exactly equal to the trait.
  - **`on_order_book` crosses as a CURSOR** because `L2Book`'s level maps are private `BTreeMap`s —
    there is no flat mirror to build. The host flattens once per dispatch so `level_at` is O(1) (a
    per-index `BTreeMap` seek would make a full walk O(levels²)); the guest rebuilds an `L2Book`
    through `apply_snapshot`. ⚠ The copy is not avoidable by any payload shape: the trait hands user
    code a concrete `&L2Book`.
  - **`on_params_updated` crosses as JSON, not TOML — measured, not preferred.** `StrategyParams`
    reaches six `Option` fields with no `skip_serializing_if`, and `toml`'s serializer refuses a
    `None` outright, so a TOML hop would have failed for the ordinary case (a controller bag with no
    take-profit set). `serde_json` adds no crate to a plugin's link — `vike-model` has carried it
    since `save_state` existed — and the cdylib template's own dependency table is unchanged.
  - **The equivalence test grew rather than the fixture standing still**, which is the whole
    difference between wiring a hook and proving one: the fixture now overrides every wired hook
    with an override that CHANGES BEHAVIOUR, the comparison runs down BOTH engine lanes (`run` is
    the only source of `on_bar`/`on_schedule`, `run_ticks` the only source of the three tick hooks),
    and `on_stop` is compared through `SimBroker`'s `pending` list because the run is over when it
    fires. ⚠ Six hooks no backtest engine emits get a NARROWER witness that says so by name.

## [0.1.32] - 2026-09-21

### Changed
- **A venue setting is a ROW, not a field on `Config`.** The ten config-shaped values ruling 10
  relocates (the JForex servers, the FXCM connection, the IBKR gateway host/port, the polymarket
  proxy trio) live in the `venue_setting` table, keyed `(venue, tier, field)`, rather than as
  `setting` rows under `config.venue.…`.
  - ⚠ **The `setting` shape reached production and failed TWICE.** `vike_config::Config` carries
    `#[serde(deny_unknown_fields)]` and has no `venue` field, so the whole `config` section stopped
    deserializing — first on a value that was not a JSON scalar, then on the field name itself. Both
    times every setting fell back to its compiled-in default, which caps every venue at `paper` with
    nothing recording that an arming was ever stated. The running daemon held what it booted with,
    so nothing traded differently; the next restart would have brought the fleet up on paper.
  - ⚠ **The earlier ruling's stated reason did not survive measurement.** It chose a `section.key`
    path because `CONSUMPTION` would refuse an unread key — but that table is a fixed list of
    literal keys and `is_consumed` answers `true` for one it does not carry, so an unread
    `config.venue.*` key was never gated under either shape. What the column table actually buys is
    real: the DDL refuses a mistyped tier at write time, and two partial unique indexes keep the
    machine-scoped and tier-scoped namespaces apart. A map field on `Config` would have made the
    venue subtree the one place in the settings model where a typo is accepted in silence.
  - **`vike-cli config set venue.<venue>[.<tier>].<field> <value>`** is the operator's writer.
    Without it these values would be read-only: `secrets set` writes the credential table, so it
    would create a shadow row the fold reports as a collision and resolves in the credential's
    favour — the new value ignored, no error anywhere.
  - ⚠ **`config mirror` deliberately does NOT clear that table**, and adding a `DELETE` beside the
    other two would destroy data: venue settings have no file to be re-derived from, so one mirror
    would remove a box's JForex server, IBKR gateway and polymarket egress while reporting success.
    Gated, and the gate is mutation-proven.

### Fixed
- **The move's collision check stopped colliding with itself.** It was fed the whole live credential
  set including the rows being moved, so every rendered name matched its own source and the verb
  refused all ten keys — a refusal nothing could satisfy. Found by running it against the real store.
- **The Data Manager's store load stops waiting forever and says what it did** (#2071) — a hung peer
  left the window on "Loading stored data…" with no deadline, no stop flag and no log line, so
  *hung*, *empty* and *slow* were indistinguishable from outside.
- **The QA sheet's store criteria stop reporting a structural fact as a bug** (#2070) — the desktop
  has carried no local store engine since `hist-datafusion` went with `fat`, so those surfaces
  capture empty by construction and the criterion was flagging it as a product defect.

### Added
- **The Data Manager rail's twelve destinations get their control layer** (#2068) — ten of the
  twelve had never been rendered once; `by_venue` and `store` carried no interactive widget at all.
- **The CEX boundary, the account plane, and groups in the data CLI** (#2072), with rulings 0079 and
  its siblings written down — Hyperliquid spot is stored under its core symbol and the bridge
  converts at the wire.

### Dependencies
- The `egui` family and `toml_edit` take their patch bumps (0.36.2 / 0.25.15). **`ureq` is held at
  3.4.0**: 3.4.1 turns `timeout_recv_body` from a per-read idle bound into a total body ceiling, and
  `vike-backfill` arms it for the first meaning. Measured 3.4.0 PASS at ~5.0s against 3.4.1 FAIL at
  3.01s, three for three. The root `Cargo.toml`'s pin carries the mechanism.

## [0.1.31] - 2026-09-21

### Fixed
- **A moved venue setting is stored as a JSON SCALAR, and reading it back decodes.** MEASURED on
  the CI box: `secrets move-venue-config` wrote each credential's plain string straight into
  `setting.value`, a column that holds ONE JSON scalar. The settings loader hit
  `expected value at line 1 column 1` on the FIRST such row and **stopped parsing the whole `config`
  section**, so every setting fell back to its compiled-in default — which caps every venue at
  `paper` with nothing recording that an arming was ever stated. The running daemon still held what
  it booted with, so nothing traded differently; the next restart would have brought the fleet up on
  paper, in silence.
  - ⚠ **Why no test caught it: both halves were wrong in the SAME direction, so they agreed.** The
    fold read that column back raw, so writer and reader round-tripped perfectly and the venue
    bridges saw exactly the right value. Only a DIFFERENT reader of the same row — the settings
    loader — ever objected. And the verb's `--dry-run` writes nothing, so the rehearsal could not
    see a write-format defect at all.
  - ⚠ **A row that does not parse is returned AS-IS**, because a box that already ran the unencoded
    move holds raw rows and refusing them would take the venue configuration away from exactly the
    boxes that need it back (`docs/decisions/0013`'s degrade-rather-than-refuse rule).
  - The check that bites asserts the STORED FORM parses as JSON and that the raw form does not —
    not a round trip, which is what passed while the bug shipped.

## [0.1.30] - 2026-09-21

### Added
- **`vike-cli secrets set` writes every key this workspace reads, not just the enumerable grid.**
  On a migrated box (`docs/decisions/0054`) the credential FILES stopped being read at all, and
  `set` refused every name outside the fixed `{VENUE}_{TIER}_{SUFFIX}` grid while advising an
  EDITOR — a route that edits a draft nobody loads. **Half the store had no writer anywhere**: an
  operator could see a credential in `secrets list` and rotate it nowhere. Three admission rules
  replace the grid-only test:
  - the **grid**, unchanged;
  - a name the settings registry proves a reader for **and** the credential classifier can PLACE —
    which is what admits the bespoke per-bridge keys (the FX logins, the per-venue server/host
    keys, the prediction-market proxy trio). ⚠ Both halves are load-bearing: the registry alone
    admits a store PATH read out of the process-env sweep, which is not a credential and would be
    the dead line the refusal exists to prevent;
  - **rotation** — any name this box's store ALREADY HOLDS. This is what reaches the deployment's
    own secrets (the Telegram trio, a Cloudflare token) without a hand-kept roster that would rot,
    and it cannot invent a dead line, because the row is already there.
- **A LABELLED account (`KEY__LABEL`) is written rather than refused**, when its base is a grid key.
  ⚠ The hazard the old refusal was written about does NOT go with it — it was the suggestion list
  offering the UNLABELLED base, which an operator then set, overwriting the DEFAULT account's
  signing key. Writing the typed name is what ends the temptation; the base is left byte-for-byte.
- **`vike-trader-ci-deploy verify-tools` — the deploy now proves the runtime tools it placed.** It
  walks the tool table, compares each destination against the release manifest the deploy itself
  verified, and FAILS the deploy run on drift or absence.

### Fixed
- **A stale deploy helper stopped being silent.** MEASURED on the CI box: the installed helper predated
  the runtime-tool table, so for eight days every deploy installed the two binaries, placed **no
  tool**, and reported success — the JForex sidecar jar three commits stale (both Dukascopy demo
  logins timing out at 240s) and no LightGBM on the box at all, with nothing red anywhere. The
  helper is installed BY HAND and nothing deploys it; the new check catches exactly that, and the
  mechanism is the verb's own ABSENCE, since an old copy dies on its usage arm.
- **`scripts/fetch_release_tools.sh` survives a repository RENAME.** It derives its slug from the
  checkout's `origin`, which a rename does not update, and GitHub answers the old path with a 301 —
  so the API read came back empty and the run died with `no release 'latest' … : Moved Permanently`,
  a message about a TAG for a problem with the NAME.
- **`secrets move-venue-config` stopped colliding with itself.** Its collision check was fed the
  whole live credential set, including the rows being moved, so every rendered name matched its own
  source and the verb refused ALL TEN keys — a refusal nothing could satisfy. It now evaluates
  against the rows that stay.

### Changed
- **The pmxt Polymarket-L2 archive is OBSOLETE — it stopped publishing on 2026-08-10.** The last
  object is `polymarket_orderbook_2026-08-10T00.parquet`; everything after is 404, and the
  attributed site refuses connections. ⚠ The collector never noticed: a 404 is mapped to "the hour
  hasn't been published yet, skip it", which is correct for an archive on a lag and wrong for one
  that has ended — so a run over any later range skips every hour and **exits 0 having ingested
  nothing**. The module is kept (the ingested history is real and its mapper is the reference for
  that Parquet schema); what is dead is the SOURCE.
- **`ureq` is held at 3.4.0 and the pin carries the measurement.** 3.4.1 turns `timeout_recv_body`
  from a per-READ idle bound into a TOTAL body ceiling, which is not what
  `vike-backfill`'s HTTP module documents it as. Measured A/B on one test: 3.4.0 passes at ~5.0s,
  3.4.1 fails at 3.01s, three for three. ⚠ The gate that caught it is the `coverage` workflow, not
  the CI fast lane — `vike-backfill` is excluded from the roster, so a green `ci` says nothing here.

## [0.1.29] - 2026-09-21

### Added
- **`vike-cli secrets move-venue-config [--dry-run]` — ruling 10's move, as an operator act.** Ten
  config-shaped keys that were never credentials (an IBKR gateway host, an FXCM connection name, a
  JForex server, the polymarket proxy trio) leave the credential store and become
  `config.venue.<venue>[.<tier>].<field>` settings rows, which `config show` renders and
  `config set` writes.
  - ⚠ **An operator act, not a step inside `secrets migrate`**, for the reason `config adopt` is
    one and a sharper one: it DELETES rows from the only copy of a box's venue keys. `--dry-run`
    answers the whole verdict and writes nothing.
  - ⚠ **Two refusals, both writing NOTHING**: a collision (a rendered name a live `credential` row
    already holds — SQLite cannot constrain a name across two tables) and a divergence (two names
    collapsing onto one key while holding DIFFERENT values, which would hand one dukascopy account
    the other's JForex server).
  - ⚠ **Every reader keeps finding the legacy name it looks up.** The credential map folds the
    settings rows back in, at BOTH read paths — the inner read every composition root reaches, and
    the SCOPED read `polymarket`'s egress uses directly. Without the second, the proxy family would
    have been the one thing the move broke while every other reader kept working.

### Internal
- `ScopedSecrets::fold_in` — the seam that makes the scoped fold possible without rebuilding the
  type from a map, which would have dropped its findings (the permission warning, the shadowed
  file, the source) in silence. It answers three states rather than a `bool`, because a caller must
  tell a COLLISION — a half-done migration, to report — from NOT DECLARED, which is the scope doing
  its job: folding in a name nobody asked for would widen a read that was deliberately narrowed.

## [0.1.28] - 2026-09-21

### Added
- **A venue can be asked WHICH ACCOUNT a key trades, and the answer is recorded.** `vike-mount`'s
  `book_identity` table said, for binance/bybit/okx/deribit, that the credential store names no
  account and only an authenticated call could — and there was no such call. There is now:
  `vike_exec::recon::ReconClient::fetch_account_identity`, implemented for binance (the `uid` rides
  `/api/v3/account`, a body the fee read already pulls, so it costs that venue NOTHING), okx (one
  signed `GET /api/v5/account/config`) and deribit (one `private/get_account_summary` with
  `extended: true`). The mount folds the answer through the same `vike_model::account_confirmation`
  handshake dukascopy and hyperliquid already use, and `vike-cli secrets confirm` files it into
  `account.venue_account_id`.
  - ⚠ **It runs at TWO sites and neither covers the other**: the startup preflight, for every ARMED
    venue whether or not it is mounted, and `make_engine_for_account`, for every MOUNTED venue.
    Measured on the deployed box: 16 `account` rows across 13 venues against a profile that mounts
    ONE. A mount-only rung would have recorded one venue's book while the store asked about
    thirteen; a preflight-only one would never reach deribit, which builds its client inline.
  - ⚠ **The identity read is ordered AFTER the fee read, and that ordering is load-bearing.** On
    binance spot both ride `/api/v3/account`; asking first turns one signed request into two.
  - ⚠ **bybit is deliberately NOT implemented.** Its demo key answered `retCode 33004` (*Your api
    key has expired*) when the probe went to measure what its response names, so nothing was
    measured and nothing was written. Filling that row from the venue's documentation instead is
    the `canWithdraw` trap this workstream exists not to take.

### Changed
- **The three `vike-tradehub` node scopes say what they GRANT**: `Observe`/`Control`/`Admin` become
  `Read`/`Write`/`Account`. ⚠ **The variant names ARE the wire** — `Request::Auth` carries `Scope`
  through derived serde — so this is a protocol break between a node and a client of different
  vintages, not a rename. `Scope` remains NOT a ladder: `Account` is not a superset of `Write`.

### Fixed
- **The paper-mount warning stopped telling an operator two false things.** It read *"no DEMO
  credentials for it were found in the settings store … add BYBIT_DEMO_API_KEY + _API_SECRET to
  `<project>/settings/secrets.env`"* — on a box where the credentials ARE in the store (the startup
  preflight had demoted the venue over an expired key, and said so one screen earlier) and where
  `secrets.env` is NO LONGER READ because the settings database answers. It now states the outcome,
  names BOTH causes, points at the preflight line that separates them, and names the VERB rather
  than an artifact: `vike-cli secrets set` writes whichever store answers, `vike-cli secrets path`
  prints which one that is. Four sites carried the hardcoded path; they now share one helper.

### Security
- **The capture sanitizer redacts the PARENT account and the human behind it, before anything
  captured them.** `REDACT_KEYS` gained `parentUid`/`mainUid`/`masterUid`/`subAccount` and their
  spellings — the field that separates a sub-account from its master, which is precisely what the
  blind-CEX probes went looking for — and then `email`/`username`/`system_name`, because a deribit
  `get_account_summary` was MEASURED to answer all three while `redacted_paths` came back EMPTY.
  - ⚠ **One residual cannot be closed and is PINNED rather than left implied**: deribit's account id
    is spelled `id`, and `id` may never join that list — it is every order id, trade id and combo
    leg, and redaction is by KEY, not by PATH. A test fails a later author who "fixes" it and says
    what it would cost.

### Internal
- **Ruling 10's move gets all three of its prerequisites, and moves nothing.**
  `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §12 forbids the ten config-shaped
  keys leaving `credential` until §6.2's ordering (A) exists. It does now: a name renderer
  (`venue_setting_names`, one row to possibly SEVERAL legacy names — the ten names are nine rows
  because the dukascopy pair was measured equal), the collision check §6.2 says the migration owes,
  the read-side fold that keeps every loader finding the name it looks up, and `move_pending_rows`
  itself. No verb calls it yet.
  - ⚠ **The values go to `setting` rows, NOT to `venue_setting`** — owner's ruling. That table would
    have been a THIRD key shape for one question, and the spec's own open items say it arrives
    "with no gate of its own … outside [`CONSUMPTION`]'s vocabulary because its keys are not
    `section.key` paths". A `config.venue.<venue>[.<tier>].<field>` key costs nothing and is visible
    to `config show`, writable by `config set`, and covered by that gate.
  - ⚠ **Both refusals write NOTHING**: a collision (a rendered name a live credential row holds —
    SQLite cannot constrain a name across two tables) and a divergence (two names collapsing onto
    one key with DIFFERENT values, which would hand one dukascopy account the other's JForex
    server).
- Studio's Local backtest backend is deleted — every backtest goes through the compute daemon
  (0078); a study may ask for one backtest and `poly_mm_batch` becomes a user study (0076); the
  Bayesian sampler comes down to `vike-ml`; one offscreen rasterizer replaces two in the PNG
  harnesses; `vike-chart`'s libm edge is a DEV edge, as its own comment demanded.

## [0.1.27] - 2026-09-20

### Added
- **An account is addressable end to end — from the wire, through the mount, to the risk ceiling.**
  A strategy mount names the account it wants (`MountStrategy`, behind its own capability, so a
  client naming one refuses an old node rather than being silently routed elsewhere), an
  order-carrying command names its account, and `vike_model::account_keys` gains the wire spelling
  that admits the reserved word. ⚠ **A mount that names a venue and no account is now REFUSED where
  that venue runs more than one engine** — it used to pick one. A single-engine venue is unchanged.
- **`policy.accounts.<venue>.<label>.max_exposure` — the open-notional ceiling can be stated PER
  ACCOUNT**, not only per venue. It composes as a `min` with the box-wide figure, so adding one can
  only ever narrow. ⚠ It is enforced per ENGINE, and two accounts sharing one venue book therefore
  carry it separately — which is what the shared-book warning below exists to tell you.
- **`vike-cli venues` — a read-only screen that says WHY a venue is still on paper**, per account,
  in the same projection the mount selects with. It answers the question the boot banner could only
  hint at.
- **A venue's own handshake can now NAME the account it authenticated as, and the store can keep
  it.** Hyperliquid asks `userRole` at mount instead of assuming the key is the account —
  the assumption was false for every agent wallet, and a wrong address there answers
  `accountValue: "0.0"` rather than an error, so the mount looked FLAT and HEALTHY while reading
  somebody else's book. The answer is parked into the state directory and folded by
  `vike-cli secrets confirm`, exactly as dukascopy's already was.
- **`vike-cli backend admin-key`** mints the third node key, the one 0065's `Admin` scope needs.
  ⚠ Minting it ARMS NOTHING: the account verbs also need `config.toml`'s `tradehub_account_admin`
  (`loopback`, which the daemon CHECKS against its own bind at boot, or `contained`, which you
  assert) and a restart. Until then the node holds no account writer at all.
- **The node wire can write a book** (`AccountVerb::SetBook`) — the twin of
  `vike-cli secrets set-book`, so `account.venue_account_id` is settable from a desktop and not only
  from a shell on the daemon's box. Repointing a row that already names a different book takes the
  typed confirm; writing onto an empty column does not.
- **A mount says WHAT IT TRADES to somebody who can read it**, over the wire.
- **The store records whether a series is spot or perp** (0061 step 2), five more bridges name their
  instrument's kind, and `vike-cli data list --class` reads it back. An options chain names its
  underlying's kind rather than an "asset class".

### Changed
- **The shared-book report is capped and says the thing no individual pair can.** N accounts on one
  wallet is N(N-1)/2 findings — at fifty accounts that is a flood in which the fact worth knowing
  (that fifty of them are ONE wallet) appears in no line.
- **The book a mount reports is the RECORDED one first, and only then a derived one.** Until now the
  answer came solely from the credential store, so the five venues whose keys name no account could
  never report a shared book however much the store had been told — `secrets set-book` wrote a column
  that changed no decision anywhere.
- **The compute plane stops reaching a venue.** `vike-cli data fetch` asks a datahub, the backtest
  path no longer dials a venue, and the spot collector retires. **No tool in this tree reaches
  ClickHouse for data any more** — the last direct reader is gone.
- **The asset-class taxonomy moved below the store** (vike-catalog → vike-model), and the `.P`
  suffix question gained ONE authority which the bridges ask OF THE VENUE rather than deciding
  locally.
- **The Polymarket takers left the simulator and the engine gates came home.**
- **The leaf libraries can be published to crates.io** (0071) — 0037's scope error, measured.
- **The boot banner names the credential store that actually ANSWERS**, rather than the one a path
  suggests. On a migrated box those are different files.
- **The layer graph straightens: the store comes down below the engine, and the recorder comes off
  it** — acquisition is a rank of its own rather than a leftover. The workaround binary that
  existed only to route around the old edge goes with its cause.
- **Eleven copies of the libm walk became one** (0074), hosted in `vike-model`. ⚠ The WALK is
  shared; the gate over it is deliberately not — 0074 carries why, and a later correction records
  what the first shape of that record cost.
- **`LIVE_CAPABLE`'s permissive arm carries its argument** rather than resting on the reader to
  supply one.

### Fixed
- **The backtest unit could not write a single run — the grant was one directory short.**
- **A release binary reported itself `dirty`** because the pinned tool archives landed in the
  checkout.
- **An FXCM login is not a book, and the account is not even fixed for the session.** The shim
  re-picks the first eligible account on every FFI call, so order flow moves to another account the
  moment the first enters a margin call. Recorded rather than patched — no CI runner stages that
  SDK — and the identity table stops asserting a book it cannot know.
- **A written `HYPERLIQUID_{TIER}_ACCOUNT_ADDRESS` is now audited against the venue.** It was the
  one input in that venue's path nothing checked, and the best-configured account was the only one
  never verified. The mount still uses what you wrote — a disagreement reports and re-routes
  nothing.
- **A parked confirmation can name a LABELLED account.** The address is a credential-key prefix, and
  a labelled account has none, so such a record could never be folded.
- **A symbol becomes a directory name, so it may not contain a path separator** — and the catalog
  renders the core symbol without one, saying so when it cannot.
- **An UNREADABLE account epoch is not a CHANGED one**, on the MCP confirm path.
- **The recorder's family GLOB is not a directory name**, so the group name is rendered.
- **Studio's Remote backend dialled the daemon that refuses its verbs.** The compute verbs live on
  the data daemon; the trading node answers `required_scope` for them and always would have.
- **A flake diagnosed, a rotting number retired, and an annualisation rule made structural** rather
  than restated in prose that drifts.

## [0.1.26] - 2026-09-19

### Added
- **`vike-cli config mirror` writes the four settings files into the settings database** (0057
  phase 1). ⚠ **This is MIRRORED, not crossed: the FILES still win and nothing this box resolves
  changes.** The rows are a regenerable copy, which is what makes the phase safe to land on a live
  box. Nothing deletes a `.toml`, and 0057 is explicit about the order — *"the read-back path must
  land BEFORE the files are retired"* — so retirement waits on a later phase.
- **The `account` table gains a lifecycle, and the wire gains the barrier 0065 designed** — an
  account is a row, addressed by the venue's own number, and a daemon refuses an ambiguous one by
  name instead of guessing.
- **A venue catalog is fetched ON by default** (0066), with a baseline instrument list shipped so a
  box with no network still addresses the common symbols.
- **`vike-cli backtest --html`** and the batch-B flag doors on the backtest CLI.

### Changed
- **The settings store's `value` column holds a JSON scalar** rather than a TOML rendering.
  ⚠ **Run `vike-cli config mirror` before the new binary's unit restarts.** Every row both live
  boxes carry was measured identical across the change, so neither needs the re-run to keep
  booting; the ordering rule is what makes that true of the next box too.
- **The market-data vocabulary moved out of `vike-model`** into a crate of its own.
- **rhai 1.26.1**, and the licence comment stops saying proprietary.
- **The Data Manager's seven tabs and nested sidebar became one rail.**

### Fixed
- **A stale or wrongly-typed settings row degrades instead of bricking the box.** A row the new
  reader could not parse was a hard startup refusal that took down `vike-cli config mirror` — the
  one command the refusal told the operator to run — and `sqlite3` is installed on neither
  deployment box, so there was no way back. A row that cannot be read now DEGRADES by name: the
  files still answer, and every command keeps running.
- **`config.datahub_addr` reached the GUI and no CLI dialer at all**, so a box that set the key
  moved some of its dialers and silently left the rest on the compiled-in default.
- **The kline collectors refused a venue at the wrong seam** (0059 phase 1), so the one-shot
  backfill bins and the `backtest data fetch` verb disagreed with the supervisor about what is
  collectable. Four hand-copied venue rosters became one registry (0059 phase 3).
- **`vike-cli backtest --html` claimed nothing was recomputed while the monthly table recomputed.**

## [0.1.25] - 2026-09-15

### Added
- **The dukascopy mount addresses the ACCOUNT it is mounted for** — the last venue whose arm read
  one account's credentials whatever account it was asked to mount.
  `crates/vike-mount/src/engine.rs`'s `make_engine_for_account`
  had a `("dukascopy", _)` arm that hardcoded `DukascopyAccount::Demo1` and ignored its own
  `account` parameter, and `arm_addresses_accounts` refused the venue outright so nothing could ask
  it to do otherwise. `docs/superpowers/specs/2026-09-14-the-credential-schema.md` ruling 13 refused
  every KEY-GRAMMAR fix for that and said to wait for the database — *"once the account is a COLUMN,
  a name carries no account, no tier and no index"* — and §12 barred the list until the arm threaded
  `account` into an account-aware loader. The column, its reader and its key-name reader have all
  landed, so this is that work.
  - **The mapping is keyed on the ROW, and specifically on its credential-key OWNER PREFIX** —
    `DUKASCOPY_DEMO1_` or `DUKASCOPY_DEMO2_`, through the new
    `vike_dukascopy::DukascopyAccount::from_key_prefix` (the exact inverse of its new `key_prefix`).
    That is the store's OWN account identity — `vike_secrets::schema`'s `Classification::owner_prefix`
    says an account's owner prefix is recoverable from any one of its rows, which is why the
    migration needs no stored discriminator for the one pair sharing `(venue, tier, label)`. The
    ADDRESS an operator writes is `account.venue_account_id`, the venue's own number, matched first
    and then a `label` if a row carries one. ⚠ **No label is required, invented or read from a key
    name**: both dukascopy rows carry `NULL` and the owner refused the provisional `DEMO1`/`DEMO2`
    spellings twice.
  - ⚠ **`AccountLabel::Default` is `DukascopyAccount::Demo1` on every box, whatever the database
    holds.** Argued at `crates/bridges/dukascopy/src/account.rs`'s `resolve_account` (moved there
    from `vike-mount` by decision 0088, 2026-09-27): re-pointing it
    would move which LEGAL ENTITY an order reaches (Dukascopy Bank SA vs Dukascopy Europe IBS AS)
    with no operator act, and after the migration there are two unlabelled `(dukascopy, demo)` rows
    and nothing marks either "the default" — so the store has no better answer to give. A
    `Backend::Files` box, a database older than the `account` table, and a process whose root
    declared no project all behave exactly as before: the default account mounts, every labelled one
    is refused.
  - ⚠ **An account the store cannot identify is REFUSED to paper with a named `error!`** — never
    coerced onto Demo1. Five refusals, each naming what an operator can do: no `account` table, no
    such row, an ambiguous address, a row whose key names name no dukascopy family (or both), and
    the sidecar claim below. `vike_config::ArmingBlock::AccountNotInStore` is the new arming row, and
    the arming PROJECTION consults the same resolver as the mount so the two cannot disagree about
    what will arm.
  - ⚠ **ONE JForex sidecar per process, refused rather than reported.** Two concurrent sidecars are
    UNPROVEN and the risk falls on the account that already worked: `Bridge.java` sets no platform
    cache directory, so two JVMs of one user share the per-user default, and a corrupted cache is
    the INSTANT-`login failed`-every-time failure until the directory is deleted by hand. That is
    why this is not a `docs/decisions/0013` degrade — it would degrade a capability that already
    worked, not the one being added. What retires it is a measurement, named at that module: a
    per-sidecar cache directory, then two real concurrent logins on an SDK-staged box.
  - `vike_config::ArmingBlock::NoAccountSupport` is KEPT although no roster venue now produces it: a
    venue scaffolded by `just new-venue` is deliberately not added to `arm_addresses_accounts`, so it
    is the first refusal every future venue's second account meets.
    `crates/vike-mount/tests/unaddressable_account_warning.rs` drives the message from a PLANTED row
    (through the new `vike_mount::unaddressable_accounts_text`) instead of testing a thing nothing
    can produce, and pins the retirement over the whole roster.
  - `crates/vike-mount/src/book_identity.rs`'s dukascopy row keeps `Undeterminable` and its REASON is
    rewritten — the correction §9 of the spec records as owed. The old text rested on an arming
    refusal that no longer exists and on "unknowable", which was never true: the sidecar's ready
    handshake carries an account.
- **Settings-database SCHEMA 2 — the account is a ROW.**
  `docs/superpowers/specs/2026-09-14-the-credential-schema.md` (ACCEPTED, signed 2026-09-14) §4's
  `account` and reshaped `credential` tables and §6's `venue_setting`, plus the in-place migration
  that carries a schema-1 store into them. The account used to live in the key NAME
  (`DUKASCOPY_DEMO1_LOGIN` bakes an account INDEX into the tier token), so it could go stale with
  nothing in the store able to contradict it; it now has a permanent opaque `id` and a column for
  the venue's own answer. **Dukascopy's two demo accounts are two rows** — ruling 1, and the one
  `(venue, tier)` pair in the store that yields two accounts.
  - ⚠ **A schema-2 binary READS a schema-1 store** (`vike_secrets::READABLE_SCHEMA_VERSIONS`), and
    that is what makes the version bump deployable on its own. A bare bump would have been a silent
    all-venues-to-paper event on both live boxes: the reader errors, the infallible wrapper every
    composition root reaches through returns an EMPTY map, and an empty credential map is not an
    error downstream but the LIVE GATE. So the bump and the reshape are not one event — the upgrade
    is performed by `vike-cli secrets migrate`, with `--dry-run` in front of it, and deploy order is
    free.
  - **The upgrade is ONE transaction**, DDL and version stamp included, because `PRAGMA
    user_version` and DDL are both rolled back with the transaction that set them — MEASURED and
    PINNED (`db_tests::the_schema_stamp_and_the_ddl_are_both_transactional`) rather than assumed.
    It has to be: schema 2 keeps `name` and `value`, so `SELECT name, value FROM credential` is
    still valid SQL against it, and a half-applied reshape stamped 1 over schema-2 tables would be
    ACCEPTED by an older binary and answered from. A run that cannot carry EVERY row it read fails
    the whole run and leaves the working schema-1 store — a per-key skip would have committed a
    store missing a credential that exists nowhere else.
  - ⚠ **No reader notices.** `resolve_project` returns the same name→value map before and after,
    proved by comparing the two maps rather than by asserting it
    (`a_reshaped_store_answers_byte_for_byte_like_the_flat_one_it_came_from`). That is only true
    because §11's steps 3 and 4 — the fold of the ten book keys into `account.venue_account_id` and
    the move of the ten config keys to `venue_setting` — are deliberately NOT performed: §12 forbids
    them until a map renderer exists, and the third ordering (move the rows, then fix the readers)
    is the one it calls inadmissible. Every such row is classified and REPORTED BY NAME, so the next
    change takes its work-list out of a run rather than out of prose. `venue_setting` is created
    EMPTY for the same reason — a table is a shape, and a ROW nothing reads is the defect
    `vike_config::CONSUMPTION` exists to refuse.
  - **The account classification is INJECTED**, exactly as `is_node_key` already is:
    `vike-secrets` declares no `vike-*` dependency and stays a layer-15 leaf, so
    `vike_bridge_core::credentials::classify_credential_name` supplies it. §5.2's `POLY_*` split,
    §11 step 2's hand-mapped venue families, §6's non-secret set and §7's book keys are tables
    there, each row citing the section that measured it.
  - **§4.2's superseded values are rescued**: a `#KEY=VALUE` comment line in the credential file
    becomes a `superseded_at` row, and prose comments become `notes`. Both come from a
    comment-ONLY scan that produces no live value, so it cannot disagree with the one parser about
    anything that reaches the map. A commented key with no live row is REPORTED, not written.
  - **The write path learned the shape.** `credential.name` is a PARTIAL unique index now, so
    `ON CONFLICT(name)` no longer names a constraint; the upsert is an explicit UPDATE of the live
    row plus a classified INSERT for a name the store has never held. A NEW name with no classifier
    is refused BY NAME rather than filed as a deployment-level credential belonging to no venue,
    and a NEW name against a schema-1 store is refused with the verb that fixes it. Replacing a
    KNOWN key needs no classifier at all — which is what keeps the venue's own cTrader grant
    rotation working, on both schemas.
  - ⚠ **`{VENUE}_LIVE_*` and `{VENUE}_MAINNET_*` are ONE credential under two names, and the store
    now says so instead of refusing to exist.** `vike_model::account_keys` normalizes the legacy
    `MAINNET` tier onto `LIVE` and §4.4 removes the store's own tier token from `field`, so both
    spellings resolve to one `(account_id, field)` — which `credential_one_live_value` admits once.
    MEASURED end to end on a planted schema-1 store holding both: the dry run said it would be
    upgraded, and the apply failed with `UNIQUE constraint failed: credential.account_id,
    credential.field`, out of `write_rows` as a raw engine error naming no key, i.e. BEFORE the
    count guard that would have named one. On the create path the half-built database is unlinked
    and the verb then fails permanently, with hand-editing `secrets.env` as the only repair — the
    file this whole design promises never to touch. It is reachable rather than theoretical: both
    spellings are legal names `secrets set` will write, `load_credentials_from` reads LIVE and falls
    back to MAINNET, and `save_credentials` never deletes a line, so a box that renamed its keys
    holds both. **The schema is unchanged.** Identical values file the LEGACY spelling as the
    canonical one's rollback copy (`superseded_at`, which is what §4.2 put that column there for),
    so both `name` rows survive and `resolve_project` still answers for either — the reader admits a
    superseded row whose name no live row carries, which the §4.2 rollback copies can never be.
    DIFFERING values are refused with BOTH names in the message, never a value, and the repair is
    an operator deleting one of the two lines. ⚠ That refusal is per-KEY on the FILE path — every
    other key in the edit lands — and whole-RUN on the reshape, which is the disposition
    `reshape_into` already took for every refusal and is not a second rule: its source is the table
    about to be DROPPED, so a skipped row is a credential destroyed.
    `vike_secrets::RowReport::aliases` reports what was filed, by name and never by value.
  - `PRAGMA foreign_keys` is now set AND verified on every connection: it is off by default and
    per-connection, so schema 2's two `REFERENCES` clauses would otherwise enforce nothing.
  - ⚠ **Two deviations from §4's printed DDL, both argued in `crates/vike-secrets/src/schema.rs`:**
    `account.label` is NULLABLE (the signature rules the provisional `DEMO1`/`DEMO2` labels are not
    written at all and that labels are "informative and optional" — and a `NOT NULL` label makes
    dukascopy's sixteenth row un-insertable, which §4.1 says in its own words), and `tier` carries a
    `CHECK` because `STRICT` constrains types and not values.
- **`vike-cli secrets migrate` — the verb that CREATES the settings database and moves the
  credential store into it.** `docs/decisions/0054`'s credential half shipped a schema and a reader
  and nothing that could bring the store into existence, so no box could reach either: nothing
  outside tests called `vike_secrets::migrate`. It reads `secrets.env` and `node.env` and writes
  NEITHER — retiring them stays an operator act — and re-running it inserts nothing.
  ⚠ **The first successful run is irreversible in practice**: from then on
  `vike_secrets::backend_at` answers `Database` for every process on the box, the credential file
  stops being read, and the node-key classification is baked into two tables there is no repair
  verb for. So `--dry-run` is a REAL dry run rather than a courtesy —
  `crates/vike-secrets/src/db/migrate/planner.rs`'s `plan` is the ONE classifier and `migrate` is that plan plus the
  write, so a rehearsal cannot describe a migration different from the one that follows it.
  ⚠ **`preview` is that plan PLUS the row classifier**, and it was the plan alone until a reviewer
  measured what that left out: `plan` decides which table a name belongs to and what is already
  stored, and every decision `vike_secrets::schema`'s fill makes PER ROW — which account, and what
  to do when two names resolve to one — was invisible to it. On a store holding both
  `ASTER_LIVE_API_KEY` and `ASTER_MAINNET_API_KEY` the preview printed *would be UPGRADED* and
  *"every key name would still answer exactly as it does today"*, and the apply that followed it
  failed. It now runs the REAL classifier over an in-memory REPLICA of the store (`preview_rows`),
  which creates no file, opens nothing for writing, and — `temp_store = MEMORY` — cannot spill a
  plaintext credential to a temp file, so the two refused previews below stay refused.
  It returns `MigrationPlan` and deliberately not `Migration`, whose
  `database_exists` is a claim about a run that SUCCEEDED. The three cheaper previews that were
  refused — open-for-write-and-roll-back, migrate-a-copy-in-a-temp-dir, just-run-it-it-is-idempotent
  — are argued at `preview`, along with the four things a dry run still cannot promise.
  The migration is journalled as ONE `credential_write` for the ACT (`venue` = `multi`, the untiered
  tier, key NAMES only), which `crates/vike-cli/src/cmd/secrets.rs`'s `record_migration` argues
  against one record per venue and tier. `crates/vike-ops/tests/credentials/credential_writer_gate.rs` watches
  the migration by name — the blindness 0054's *What must land* predicted, since a gate keyed on the
  two upsert names goes green rather than red at a store that grows a CREATOR.
  `docs/decisions/0036` carries a fourth amendment narrowing *an ABSENT store is refused, not
  created* to what it always was: a claim about `secrets set`.
- **A remote parameter search names its own method, and a daemon too old to hear that says so.**
  `Request::RunSweepProfile` carries a `search` selector (`vike_datahub_client::proto`'s
  `WireSearch`: the method plus that method's own knob, as the TOKENS the operator typed), so
  `--optimizer euler|tpe|genetic`, `--euler-depth`, `--trials`, `--seed` and `--rank-by multi` work
  on `--addr` as well as `--local` — and `genetic`, which was refused during arg PARSING on both
  routes, joins the roster. Stage 7 of
  `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`. ⚠ The field alone would have
  been the defect rather than the fix: `Request` has no `deny_unknown_fields`, so a daemon
  predating it DECODES the frame, drops the selector, runs the exhaustive grid and answers a
  well-formed report — a Bayesian search and an exhaustive one, indistinguishable from the answer.
  So the client checks `FEATURE_SEARCH_METHOD` in the handshake and refuses BY NAME with nothing
  sent; `PROTO_VERSION` is deliberately NOT bumped, because a bump is strict equality inside
  `connect` and would report "a service could not be reached" for a service that answered, while
  breaking the signed auth mac against every keyed peer not upgraded in lockstep. The ownership
  rule, the value parsers and their refusal texts moved ONCE into
  `vike_backtest::harness::search_select`, which the engine's argv parser and the compute server
  both call — so `--trials` under the wrong method is refused with the same sentence whichever
  route ran, and the three optimizer rosters §5.4 recorded (engine four, `--local` three, wire one)
  are now one const with five test legs, each mutated to prove it bites. MCP's `run_sweep` grew the
  same four arguments: before this, every agent was a grid-search user. A searched sweep also
  reports its own cost — euler's budget line, tpe's trial line — which a remote caller could read
  nowhere at all, because the engine prints it to a stderr no socket carries.
- **`vike-cli research study` runs a study instead of explaining why it cannot.** Ruling R1 of the
  same design makes `research` a fourth PLANE with `study` as its first sub-verb, and a plane whose
  only verb refuses on every box is not a plane — so the wire request that verb was built against
  finally exists: `vike_datahub_client::proto`'s `Request::RunStudy` (the study name, the recipe's
  TEXT — never a path, because the backend is a different machine — and the window as typed) and
  `Response::StudyReport`, the minted run as JSON. `--json` is accepted now that there is an answer
  to shape, which is what its own refusal promised. ⚠ The capability string `FEATURE_STUDY` did not
  change and was never version-gated; what changed is that a daemon can now advertise it — **and
  only when a study runner is MOUNTED.** That runner is INJECTED, because
  `vike_studio_core::study_dispatch`'s `run_study_plan` sits ABOVE the compute daemon in the layer
  graph and the edge cannot be reversed; `crates/vike/src/main.rs` is the one composition root that
  can see both sides, exactly as it already is for the Studio runners. So `vike-backend backtest
  --addr` serves a study, a bare `cargo run -p vike-backtest --bin backtest` refuses it BY NAME,
  and both refusal paths still print the `vike-backend study` invocation with this run's own
  arguments filled in. The store root and the trainer path stay the daemon's configuration and are
  NOT wire fields — a client may not steer a filesystem it cannot see, which is what `--store` and
  `--lightgbm` have been refused by name for since this verb shipped.

- **`vike-cli config set <key> <value>` — the change journal stops being empty.** A live box's
  journal held, across two months, twenty-nine boot records, twenty-eight venue mounts and ZERO
  settings writes, while the credential store's own backup files proved it had changed at least
  eight times. The ledger was never broken: the only journalling settings writers an operator could
  reach were two `backend` sub-verbs, each writing one fixed key, so every real change was made with
  an editor over ssh. The verb writes through `vike_config::set_setting` (comment-preserving,
  loader-validated before a byte lands, atomic) and records through `vike_model::change_journal` —
  the path `vike-cli backend connect` already proves works. `set_setting_journalled` moved out of
  `crates/vike-cli/src/cmd/node/mod.rs` into `crates/vike-cli/src/cmd/settings_write.rs`, so the
  decisions a journalled settings write makes have one home rather than two. A credential-shaped key
  is REFUSED rather than redacted (`vike_config::is_secret_key`), which is what keeps a value that
  would need hiding from `ps` off the command line; there is no `--file`; and a policy RISK CEILING
  demands the retyped key while the ARMING ceilings do not — a line no site in this tree could draw
  before, because the daemon's `apply_set_setting` and the GUI's `can_save_fields` both key on the
  FILE and the two classes share one. `vike_config::requires_typed_confirm` is that line, drawn on
  the KEY and matched by segment rather than by prefix. Every local write is restart-to-apply and
  the verb says so, because nothing in this tree watches a settings file; it also prints the key's
  READ verdict from the same table `vike-cli config show` renders, and names the ABSOLUTE file it
  wrote plus which rung resolved that directory — the project walk has captured the wrong tree three
  times on record and the live box has two project roots.

### Fixed
- **The Dukascopy second account could not be mounted by ANY policy — the feature above shipped
  unreachable — and four defects behind it.** The fan-out mounts the DEFAULT account first
  (`vike_mount::known_accounts` puts it first, `accounts_to_mount` preserves row order), the
  one-sidecar claim was FIRST-COME, and `vike_config::VenuePolicy::account` is `min(venue line,
  account line)` — so raising `venues.dukascopy` to `demo`, the only way to get an
  `[accounts.dukascopy]` line above paper, NECESSARILY arms the default account too. It therefore
  took the sidecar every time and every labelled account hit the claim refusal. On the owner's own
  box, holding both `DUKASCOPY_DEMO1_*` and `DUKASCOPY_DEMO2_*`, there was no configuration that
  traded the second account.
  - **The holder is decided by the POLICY, before anything is mounted** —
    `crates/vike-mount/src/dukascopy.rs`'s `sidecar_holder`. An `[accounts.dukascopy]` line above
    `paper` whose account would arm on its own merits takes the session and **the DEFAULT account
    declines**; a box that names no account keeps mounting `DUKASCOPY_DEMO1_*` byte-identically; an
    account that is named but cannot arm (no such row, no credentials, an unreadable store) takes
    nothing from the account that works. So the whole of mounting Demo2 is now two lines of
    `policy.toml` beside `vike-cli secrets set-book`. ⚠ The cost is stated rather than discovered:
    on a box that arms a labelled dukascopy account, a strategy mounted on the venue's DEFAULT
    account runs on a PAPER engine — loudly (an `error!` naming the holder, and a
    `vike_config::ArmingBlock::SidecarHeldElsewhere` row), but `refuse_unarmed_mount_accounts`
    deliberately does not refuse a mount that names no account.
  - **The arming PROJECTION and the mount agreed about the store and not about the sidecar.** Both
    resolved the account; only the mount consulted the claim, so a two-account box projected `Demo`
    for BOTH accounts, `vike_run::refuse_unarmed_mount_accounts` saw a non-Paper row and passed, and
    the mount then built a paper engine — **a strategy believing it was armed running on paper**,
    the exact failure that refusal exists to prevent. The holder rule is one function both sides
    call, so the disagreement is now unrepresentable.
  - **A FAILED spawn burned the sidecar claim.** It was an `AtomicBool` nothing ever released, and
    `DukascopyExecutionClient::spawn` fails on four ordinary paths (absent jar, `Command::spawn`, a
    `Fatal` envelope from a bad login, the 300s ready timeout) — `crates/bridges/dukascopy/CLAUDE.md`
    records a JNLP 404 firing on roughly every second mount on a box without the retry jar, so this
    was the common path. Every later account was then refused with *this process already runs a
    JForex sidecar* when it ran none. The claim is an RAII guard now: released on drop unless the
    spawn committed it, and exercised over a caller-supplied flag so the release is actually tested.
  - **A store-read FAILURE was swallowed and re-reported as a bad row.** The account KEY-NAME read
    went through `.ok().flatten()`, so a database that existed and would not open produced *this
    row's key names do not say which broker it is* about a row nobody could read the key names of —
    sending an operator to hunt a row that was correct. Both halves of the read now carry their
    error, and `DukascopyRefusal::StoreUnreadable` names WHICH read failed.
  - **`CREDENTIAL_STORE_PIN` had gone BLIND.** The dukascopy resolution opened the credential store
    from a `vike-mount` LIBRARY file, at a location taken from a process global
    (`vike_bridge_core::halt::declared_project_state_dir`) — precisely the class that ratchet exists
    to hold down — and every test stayed green, because neither account reader was one of
    `CREDENTIAL_STORE_READERS`' keyed names. Both halves are fixed and in that order: the family is
    KEYED now (four `vike-bridge-core` wrappers plus `vike_secrets::resolve_accounts` /
    `resolve_account_keys`), and the library read is GONE — the composition root performs it beside
    its credential load and hands the snapshot down as `vike_mount::MountPolicy::accounts`
    (`vike_mount::AccountDirectory`). The pin did not grow: the one root that reads it,
    `crates/vike-tradehub/src/tradehub_cli.rs`, was already pinned. It also makes the whole
    resolution drivable from a test — `crates/vike-mount/tests/dukascopy_sidecar_holder.rs` is a
    two-account box written down as a value, with no database on disk and no process state.
  - **A test that could no longer fail was retired rather than left standing.**
    `crates/vike-mount/tests/unaddressable_account_silence.rs` asserted that a single-account box
    emits no unaddressable-account warning — over a projection that cannot produce one for ANY
    roster venue since dukascopy joined `arm_addresses_accounts`, so it passed for a reason that had
    stopped existing while its own doc claimed it was doing more work than before. It now proves the
    refusal an operator actually meets (the one-sidecar decline), in both directions, over the real
    mount; the retirement it depended on stays pinned by name in its twin.
  - Stale docs corrected: `load_workspace_accounts_from_env` and `vike-secrets`' crate doc both
    still said nothing mounts, arms or signs from the `account` table and that nothing could join
    `arm_addresses_accounts` — false since #1845, and the worst kind of stale doc on a reader that
    decides which LEGAL ENTITY an order reaches.
- **Six unread settings told operators to export an environment variable that is equally dead, and
  `vike-cli config show` printed the sentence.** `vike_config::CONSUMPTION`'s `Consumer::Not` rows
  admit that nothing reads a key and name "the reader that owns the variable today" instead. For
  `flags.{hl_outcome, pm_resolve, poly_auto_redeem, poly_heartbeat, poly_redeem_halt,
  record_chains}` that reader sits inside a poller (or a recorder constructor) **no composition
  root ever builds**, so setting either spelling changes nothing — while the paragraph read as
  *use the variable instead*, printed to an operator who had configured the key. That is
  `Policy::max_total_exposure`'s defect one level down: positive confirmation of something false,
  from the command added to stop it. The gate could not see it —
  `crates/vike-config/tests/settings_are_consumed.rs` checked a `why` for LENGTH and for the
  absence of "todo", and never opened the file it named — and the tree already knew about one of
  them in another gated table (`crates/vike-ops/tests/docs/kill_switch_gate.rs`: "nothing outside tests
  constructs this poller").
  The claim is now DATA: `vike_config::Reader` says whether the environment spelling `Live`s (with
  a caller OUTSIDE the defining file as proof), is `Uncalled` (naming every entry point that must
  have no call site), or is `Nothing`; direction 5 of that gate checks it, so wiring one of these
  features turns its own row red instead of leaving it lying. `config show` and `config set` lead
  with `Reader::verdict` — one derived line, one home, both surfaces — before the paragraph.
  Nothing was deleted: both tombstone shapes would print a SECOND falsehood
  (`REMOVED_FLAG_KEYS` says "export `<VAR>=1` instead"; a `REMOVED_ENV` refusal would stop a daemon
  over a spelling that changes nothing while the adapter's live `env::var` call site stands), and
  each feature needs a MOUNT decision, not a threading change. Four doc comments that asserted the
  deleted wiring outright — `crates/vike-data/src/chain_rec.rs`'s and
  `crates/bridges/deribit/src/dvol.rs`'s "PRODUCTION WIRING", `crates/bridges/deribit/src/chain.rs`'s
  "PRODUCTION PATH (live, not aspirational)", and `crates/vike-app-core/src/tools.rs`'s
  `options_provider` — are corrected; they are how the claims survived the desktop cut. So is
  `tools.rs`'s `spawn_tool_fetchers`, the function `options_provider`'s corrected doc NAMES as its
  caller and which went on asserting the same wiring one screen away.
  `preferences.sweep_threads` stays the table's one `Reader::Live` — its variable genuinely still
  caps concurrency — with its stated obstacle corrected: it had named `vike-datahub`'s backtest
  serve arm as one of three consuming processes, and ruling 7 of the datahub market-data-wire
  design deleted that crate's `vike-backtest` and `vike-studio-core` edges outright, so that daemon
  cannot reach the code at all. The real block is a process-wide resource knob read at rayon pool
  construction from three front doors in two crates, not a missing settings load.
- **…and the gate that was supposed to make all of that un-shippable could not fail where it
  mattered.** Every scan in `crates/vike-config/tests/settings_are_consumed.rs` stopped at the first
  line whose trimmed text began `#[cfg(test)]` — an attribute on an ITEM, not a marker for a test
  module, so a `#[cfg(test)] use`, `fn` or `const` ended the scan having cut nothing.
  `crates/vike-tradehub/src/tradehub_cli.rs` carries one at line 304 of 7,684, so the daemon's whole
  live mount — every `FoldTier` row, `make_engine`, `spawn_recon` — was invisible to the no-caller
  search that makes a `Reader::Uncalled` row mean anything, and "wiring one of these features turns
  its own row red" was false in the one file a mount would be wired in. A/B-measured against the
  real gate binary: the same planted `AutoRedeemPoller::spawn(` call read GREEN at line 2000 and RED
  at line 100. Tree-wide, 823 `crates/**/src/*.rs` files carry a `#[cfg(test)]` and 231,897 lines
  sat after the first one in their files. The fixture-only kill proof beside it passed throughout,
  because the disabling text appears in no three-line synthetic string. `production_lines` is now
  the one notion of production code every direction shares (the cut is at a test MODULE, and an
  inline one is skipped body-and-all with the scan RESUMING after it), which also closes the mirror
  hole — `Reader::Live` accepted a `caller` that existed only inside its file's own test module —
  and `Reader::Uncalled` additionally searches the READ's own name, a door an entry list cannot name
  because `Type::method` qualification structurally excludes a `pub fn`. All three are
  mutation-proved against real files.
- **`vike-cli config show` contradicted itself inside one invocation**: its file half printed
  "NEITHER SPELLING DOES ANYTHING" for six flags while its environment half listed the same
  variables under a header promising "READS = what the reader consults". The env table now names
  the dead rows beneath itself and `--json` carries `reader_verdict`, both derived from
  `vike_config::env_verdict` (FLAG_REGISTRY × CONSUMPTION) rather than a second list.
- **`flags.poly_exec` joined the typed-confirm ceremony.** Its exclusion from `CONFIRMED_FLAG_KEYS`
  rested on "a write of it arms nothing", which stopped being true when the same sweep wired the
  key: a write of it now arms LIVE Polymarket execution, on a venue with no testnet at all.
- **A planted binary survives `ETXTBSY`, everywhere `vike-cli`'s tests plant one — and `[0.1.10]`'s
  claim that CI could not see this is CORRECTED below.** A test that writes an executable and
  immediately causes it to be exec'd fails with `Text file busy (os error 26)`: `fs::write` closes
  its own descriptor, but a binary's cases run as parallel THREADS of one process and a
  `fork`/`posix_spawn` in ANY other thread hands the forked child an inherited duplicate of that
  still-open write descriptor until it reaches `execve`. Measured twice on 2026-09-14, on two
  unrelated PRs that could not touch the file — `data_cli.rs`'s
  `json_is_the_whole_of_stdout_and_the_engines_report_moves_to_stderr` (errno 26 on stderr) and
  `backtest_cli.rs`'s `the_project_bin_engine_is_the_one_that_runs`, which surfaced as an EMPTY
  value under "the child's stdout is INHERITED, not captured:" because the child died before
  producing anything. ⚠ **That second shape is why curing the losers is curing nothing**: the
  planted engine is exec'd by `vike-cli`, one process away, so the errno never reaches this process
  as a number and eleven cases (three in `backtest_cli.rs`, eight in `data_cli.rs` — not the two
  that had lost) were all holding the same coin. ONE shared helper now owns both the plant and the
  spawn (`crates/vike-cli/tests/common/mod.rs`), matching `raw_os_error() == Some(26)` for the
  spawns this process makes and the `(os error 26)` `std` renders for the ones a child reports —
  the same errno, read back out of text because that is the only way it crosses a process boundary.
  Every other outcome is handed back on the FIRST attempt, a missing binary included: a retry that
  masked a real spawn failure would be worse than the flake, and `data_cli.rs`'s
  `a_missing_engine_is_the_connect_rung` exists to prove that failure reaches the operator.
  `crates/vike-cli/tests/planted_binary_retry.rs` measures both directions — a missing binary fails
  in under a quarter of the retry budget, a PERSISTENT busy panics rather than being returned as an
  answer — and `a_case_that_plants_an_engine_may_not_spawn_the_cli_itself` makes "a new case joins
  the cure by construction" a check rather than a hope, because `backtest_cli.rs`'s dominant idiom
  is a direct spawn that would opt straight back out.
- **The settings-write lock budget belongs to the CALLER, so a GUI frame can no longer freeze on it.**
  The advisory lock below shipped its spin budget as a private constant every caller inherited, and
  the argument for that ~3 s was written for exactly one of the three: a CLI at a prompt with no
  supervisor above it. `crates/vike-app-core/src/ui/tool_views/venues.rs`'s `apply_arming` runs INSIDE
  an egui frame, where three seconds is a frozen window with no repaint, no spinner and no cancel;
  `crates/vike-tradehub/src/server/settings.rs`'s `apply_set_setting` runs on a connection thread, where the
  same wait holds a socket and the rate token the peer paid for. `vike_config::LockBudget` is now a
  parameter of `set_setting_within` and each caller argues its own at its own site: the CLI keeps
  the whole-process budget, the GUI takes ONE non-blocking attempt (and the acquire no longer sleeps
  after its final try, so that means zero rather than 2 ms) folding a `Busy` into the refusal row it
  already renders, and the daemon takes 1 s — bounded at both ends by a compile-time assertion
  against the client's own `CONTROL_REPLY_TIMEOUT`, because blocking past a peer's deadline would
  land a write nobody is left to hear about. A caller that says nothing still gets the bounded
  default — and no production caller does, which that spelling's own doc now says, so a fourth one
  arriving through it is a deliberate act rather than an inherited shape. `LockBudget::from_millis`
  also SATURATES: it takes a `u64` and counts attempts in a `u32`, and the cast that used to sit
  there answered a SHORTER wait than asked for above ~8.6e9 ms — silently, on a public API, in the
  one direction a caller cannot detect.
- **A stranded settings write is no longer journalled as `refused`.** `Outcome::Refused`'s own
  definition is "the change was refused and nothing was written", and `SettingsWriteError::Stranded`
  means the previous file was moved to `<file>.toml.bak` and the target is ABSENT — a larger change
  than the write would have been. Both journalling callers mapped it there, reproducing inside this
  branch's own error path the confidently-wrong accountability record the lock was taken to remove.
  `vike_model::change_journal::Outcome::Stranded` is the fourth outcome and
  `vike_config::journal_outcome` the one mapping. The same state's cheap second consequence is
  closed too: an absent target with a `.bak` beside it is REFUSED rather than treated as a fresh
  project and written over with a one-key file. ⚠ Two honesty notes ride with it, both declared on
  the types rather than fixed by changing a row shape.
  `vike_model::change_journal::SettingTarget` now states that on a row whose outcome is not an
  applied one the `old`/`new` cells describe the ATTEMPT and not the file: a refusing writer
  established no previous value, so an absent `old` there means "none was established", never "the
  key was unset" — which on a `Stranded` row would be a false claim beside a now-true outcome.
  And the daemon's wire path appends NO row for a refusal at all: `accept_command` returns through
  a `?` before the audit call, which is this under-recording direction stated where it happens,
  kept because that surface journals no refusal for ANY control verb and recording one for
  `SetSetting` alone would make its own trail inconsistent.
- **The live-arm, control-opening and safety-override FLAGS demand the retyped key.** `config set`
  drew its ceremony on `policy.*`, so `policy.max_leverage` — a field nothing in the tree reads —
  required a retype while `flags.tradehub_live`, whose own doc calls it "the single largest blast
  radius on this list", and `flags.tradehub_control`, which OPENS the headless daemon's
  authenticated control scope — a remote order-origination path — were one-line non-interactive
  writes. All live in `flags.toml`, so no file-keyed rule in this tree could have drawn that line.
  `vike_config::typed_confirm_reason` carries the class, and the refusal names which class it is
  refusing rather than calling a flag a policy ceiling. ⚠ The class's membership rule is the REASON
  that function renders — real money on a real wire, or a default-on guard turned off — and not a
  list sourced from `crates/vike-config/src/flags.rs`'s declared safety overrides: the narrower
  sourcing shipped first, and it left `flags.tradehub_control` OUTSIDE a ceremony that
  `flags.tradehub_allow_public_bind`, which only widens that same surface's bind ADDRESS, was
  inside — the inverse of the ordering the class claims. ⚠ Relatedly, the ruling that lets the
  generated skill pages advertise this verb rested on "every key it writes is restart-to-apply so
  nothing it does takes effect in a running process", which is FALSE on a deployed box:
  `deploy/vike-tradehub-project.service` carries `Restart=on-failure`, the tagged deploy restarts
  the roster unattended, and the upgrade window restarts daily — restart-to-apply is a delay, not a
  gate. The ruling now stands on the description-vs-callable argument alone and says outright that
  the containment is the shell grant.
- **A settings refusal says which of write / refuse / partly-happened occurred.** A failure to open
  the writer's never-unlinked sentinel was blamed on the DIRECTORY alone, which is a dead end when
  the directory is innocent — `SettingsWriteError::Lock` now names the sentinel too, and leads with
  the cause the shipped units GUARANTEE and an `ls` answers wrongly: a settings directory that is
  read-only IN THE WRITER'S OWN MOUNT NAMESPACE (`ProtectSystem=strict` plus a `ReadWritePaths=`
  grant naming only `settings/state`), with the foreign-owned sentinel kept as the second
  possibility rather than the asserted one. MEASURED 2026-09-13 inside the live daemon's namespace:
  the sentinel `touch` is EROFS, `settings/state` is writable. The consequence is declared where it
  bites — every `WireCommand::SetSetting` on a deployed box refuses there before any of
  `vike_tradehub::server::SETTINGS_LOCK_BUDGET` is spent, so that budget governs a container or dev
  configuration rather than the the CI box deployment, and whether the daemon should be able to write its
  own settings at all is an owner ruling in flight rather than a grant to widen. The loader
  `Validation` refusal renders a message naming a real file
  and nothing else, which reads as "I have just broken policy.toml" when not a byte moved, so it and
  the unknown-section refusal now lead with "nothing was written". And `config set`'s `wrote …` line
  is absolutised, because the one rung that can be relative is the one a human types.
- **A settings write is now serialised against the GUI and the daemon.**
  `vike_config::set_setting` was a read-modify-write with NO cross-process serialisation while the
  ledger that records it IS serialised (`vike_model::change_journal`'s `append_record` takes an
  `AppendLock`), so two of its three production callers racing on one file produced TWO journal rows
  both stamped `applied_pending_restart` and ONE surviving value — an accountability record that is
  confidently wrong, which is worse than recording neither. The whole read → edit → validate → land
  sequence now runs under an exclusive advisory lock on `vike_config::SETTINGS_LOCK_FILE`, the
  `AppendLock` idiom (`std::fs::File::try_lock`, kernel-released when the holder dies). It SPINS
  with a bounded budget and then REFUSES rather than blocking: the change journal's blocking acquire
  rests on a microsecond critical section with no caller code inside it, while this one is a read, a
  parse, a loader validation and two renames held against processes this one cannot see, and a CLI
  that hangs during an incident has no supervisor above it. ⚠ It closes the LOST-UPDATE window, not
  the two-operation one: the file write and the ledger append are still separate, so a crash between
  them leaves a changed file and no row — the under-recording direction, unchanged from before.
- **A failed atomic landing no longer DELETES the settings file.** `set_setting`'s rename fallback —
  there for a destination another process holds open on Windows — did `remove_file(&path)` and
  retried, so a second failure left `policy.toml` gone with nothing in its place: the box boots on
  compiled-in defaults, with no file to inspect and no backup. It now moves the previous file aside,
  retries, and puts it back; the one case where the restore itself fails is its own error variant
  naming where the operator's bytes are.
- **`vike-cli config set`'s missing-VALUE refusal no longer echoes the operator's token.**
  `KEY=VALUE` is the dotenv muscle memory that produces a one-positional command line, and the
  credential gate ran after the parser — so `config set BINANCE_LIVE_API_KEY=sk_live_…` printed the
  key in full, twice, on stderr. The token is now tested both whole and pre-`=`, and a match quotes
  nothing. `--` also ends option parsing, so a VALUE may begin with dashes and a value spelled
  `--help` is written instead of printing usage and exiting 0.
- **The credential fence moved from the leaf caller to the shared writer**
  (`crates/vike-cli/src/cmd/settings_write.rs`), so a third caller inherits it instead of
  re-proving it; a mistyped SECTION is diagnosed as a section error rather than as a credential; and
  both refusals now name `vike-cli config show` as well, so an operator who mistyped a SETTINGS key
  is no longer routed into a credential store that refuses it too.
- **A parameter search now leaves a run artifact.** Before this the sweep branch of
  `crates/vike-compute/src/backtest_cli.rs`'s `run` returned success one statement before the clock
  read that mints a run id, so a grid, euler, TPE or genetic search — the runs that cost hundreds of
  backtests and carry a seed and a budget — wrote nothing at all. A search now mints ONE parent run
  under `<project>/user_data/runs/` holding its identity, a per-trial JSON-Lines ledger written as
  each evaluation completes, a resolved trials document and the common manifest. `<id>#<n>` is a
  LEDGER LINE index rather than a directory, so a 512-point sweep is one Studio listing row and not
  512 — pinned from the listing's own side by
  `crates/vike-studio-core/src/listing_tests.rs`'s `a_search_parent_lists_as_one_row_whatever_it_holds`.
- `backtest trials <id>`: read a finished or interrupted search's trials — `--sort`, `--top`,
  `--json`, `--export-params`. Artifact-only: no store, no profile, no network.
- `backtest --resume <id>`: continue an interrupted search, evaluating only what its ledger is
  missing. It is a memoising `PointEvaluator` rather than a resumed searcher — every method's loop
  is seed-deterministic, so replaying it against the original scores reproduces the original
  trajectory — and no `Optimizer` signature, `SearchOutcome` field or `SweepRow` field changed.
  Refused by name, naming the field that moved, if the profile bytes, the store path, **what the
  store HELD**, the searching binary, the optimizer, the seed or the budget differ from the run
  being resumed. The data witness is the load-bearing one: without it, a search killed part-way, a
  backfill inside the profile's own window and a resume would rank cached scores computed on one
  dataset against fresh ones computed on another, with no error and no field in any document from
  which a reader could detect it. `--resume` with `--keep-trials none` is refused as a
  contradiction, because it would otherwise rewrite the resumed run's header to say it kept no
  ledger and make every later resume of that id refuse for a reason that is not true.
- `backtest --keep-trials none|scalars`. `series` is refused by name, with the reason: a trial's
  equity curve is dropped before a sweep row exists.
- **A box name in a shipped string literal now fails on the PR instead of at TAG time.**
  `crates/vike-ops/tests/release/shipped_box_name_gate.rs` scans every `crates/**/src` file for the same
  token set `scripts/forbidden_tokens.ere` carries, and it is a TOKENIZER rather than a line
  scanner because the incident that motivated it put the tokens on backslash-continuation lines
  holding no quote character at all — the axis a key-matching line scanner cannot see, and the same
  one the `toml` dev-edge was added to close for build settings. `proc-macro2` lexes standalone and
  discards comments, so only real string `Literal`s are judged; doc comments desugar to
  `#[doc = "…"]` and get their own arm; a `cfg` is EVALUATED rather than matched, so
  `cfg_attr(test, …)`, `cfg(any(test, feature = …))` and `cfg(not(test))` are each classified by
  whether the code SHIPS. No exception table.
  ⚠ **Why no existing gate could see it:** `scripts/publish_mirror.sh` REDACTS those tokens out of
  every text file before it greps, `api-docs` documents that redacted copy, and
  `compile_time_path_gate` polices `env!` embeds only. The one check that would have caught it,
  `scripts/refuse_box_paths.sh`, runs over BUILT RELEASE ASSETS inside `release.yml` — and a
  tag-push run executes the sources as of the tag, so a fix landing afterwards cannot be re-run into
  it and the tag has to be cut again.

### Changed
- **`vike-cli secrets template` is backend-aware.** Its documented use is
  `vike-cli secrets template > settings/secrets.env`, and every doc and skill in this tree names it
  as HOW TO CREATE THE STORE — so on a box whose settings database answers, that redirect wrote a
  file nothing reads and exited 0, the live gate wearing the fresh-install answer. The grid still
  goes to stdout (it is a SHAPE, and reading key names is not a write) and a finding now goes to
  stderr naming the database and the two verbs that act on it. ⚠ On an unmigrated box the output is
  BYTE-IDENTICAL, stdout and stderr both, and a test asserts it: that grid lands in real stores.
- **The profile's parameter-search section is `[paramscan]`; `[sweep]` keeps loading, permanently.**
  Ruling R2 of the backtest-CLI-surface design renamed it on a measurement rather than a
  preference: nineteen competitor CLIs were surveyed and exactly one uses the word "sweep" (an npm
  package), while QuantRocket calls it `paramscan`, LEAN `optimize` and Freqtrade `hyperopt` — the
  last of which would LIE here, because it names Bayesian search specifically and this engine has
  grid, euler, tpe and genetic. ⚠ **The old spelling is a `#[serde(alias)]`, not a deprecation with
  an end date.** `BacktestProfile` is `deny_unknown_fields`, so without it every profile already on
  an operator's disk would fail to load with "unknown field" — the `VIKE_MAX_ORDER_NOTIONAL`-removal
  shape applied to a file. `both_the_new_section_and_the_legacy_one_load_and_parse_identically` is
  the warranty. Every operator-facing string — the usage text, `vike-cli init`'s scaffolded
  profiles, the refusal messages, the MCP tool schema and both generated agent skills — says
  `[paramscan]` now. ⚠ **The Rust identifiers moved too, in the same release**: the client-side
  presence check is `declares_a_paramscan_grid`, `BacktestProfile::is_sweep` is `is_paramscan`, and
  the wire verbs are `Request::RunParamscan`/`RunParamscanProfile` and
  `Response::ParamscanResult`/`ParamscanReport`. What did NOT move is every NEGOTIATED TOKEN — the
  serialized wire tags (pinned with `#[serde(rename = "RunSweep")]` and its three siblings), the
  `"sweep"` field key inside `RunParamscan`, the capability strings `run_sweep_profile`/`run_sweep`,
  the MCP tool name `run_sweep` and `WalkforwardCfg`'s `search = "sweep"` value. A token is
  compared literally by a peer that shipped before the rename, so renaming one refuses that peer;
  an identifier is compared by nobody. The `harness::sweep` MODULE path is unmoved for the
  third reason — a file rename is a separately-scoped edit this tree's path-keyed gates make
  expensive.
- **`vike-cli mcp`'s compute tools read `config.backtest_addr`.** They resolved
  `vike_config::DEFAULT_BACKTEST_ADDR` as a compiled-in default with only their own
  `--backtest-addr` above it, so a box that set the key moved `backtest` and `research study` onto
  it and silently left an agent's `run_backtest`/`run_sweep`/`run_walk_forward`/`list_strategies`
  on `7880`. §18 row 9 of the design recorded it; the ladder is now the same three rungs every
  other compute dialer has.
- **The settings keys that nothing read now have readers, and the environment still beats the file.**
  `config.journal_dir`, `flags.poly_exec`, `flags.poly_reconcile`, `flags.hyperliquid_hip3`,
  `flags.record_properties`, `flags.allow_withdraw_keys`, `flags.preflight_skip`,
  `flags.reconcile_balance` and `flags.reconcile_generate_missing` were declared, validated, and
  reported by `vike-cli config show` as the origin of an effective value while their variables were
  read in libraries no settings file could reach. Each is now resolved once by the daemon and folded
  into the map its reader already consults.
  ⚠ **Precedence is the whole of the design and it is machine-checked**, because
  `docs/decisions/0054` makes "one image, many containers, configured by `Environment=` lines" a
  constraint and the CI box carries its reconcile policy and balance-seed flag in a root-owned systemd
  drop-in. `vike_config::load` applies the environment over the file, and the fold OVERWRITES every
  key whose reader had no credential-store tier — the two Polymarket gates keep theirs, and keep it
  only because a `POLY_EXEC`/`POLY_RECONCILE` line in `secrets.env` REFUSES startup rather than
  outranking anything. `a_process_env_value_beats_a_file_value_for_every_wired_key` is the
  table-driven proof; the two safety overrides (`allow_withdraw_keys`, `preflight_skip`) carry a
  second one each on the reader's own side, where an exported `=0` over a file `true` must resolve
  to REFUSE.

### Removed
- **Seven settings keys nothing read were DELETED, and a deployed box can be refused by one of
  them.** ⚠ **OPERATOR ACTION BEFORE UPGRADING** — a green tag deploys to the CI box automatically, and
  these refusals are hard startup failures:
  - `<project>/settings/flags.toml`: remove `poly_presubmit_register`, `poly_rate_gate`,
    `poly_chain_watch`, `poly_chain_proxy`, `bybit_fast_exec`, `binance_trade_lite_fill`. The daemon
    refuses to load a `flags.toml` carrying any of them, on `true` AND on `false`.
  - the process environment (a unit's `Environment=`/`EnvironmentFile=`, a shell export):
    unset `VIKE_STATE_DIR`. A set, non-empty value refuses startup by name.
  The six flag keys were only ever MIRRORS — the resolved field had no consumer, while the
  variable each mirrored is still read today by the venue adapter that owns it, so the VARIABLES
  keep working and only the file keys are gone (`vike_config::flags::REMOVED_FLAG_KEYS` is the
  tombstone, and the refusal names the variable to export instead). `VIKE_STATE_DIR` is the
  opposite case: its one reader, the desktop's strategy-state sidecar resolver, went with the
  desktop's local core, so the variable itself is a `vike_config::REMOVED_ENV` row.
  ⚠ `VIKE_STATE_ROOT` is a DIFFERENT directory and is not a replacement for it.
  The tracked surface is clean — no `deploy/` unit sets any of the seven, and the repo's own
  `settings/flags.toml` had them commented out — so what has to be checked is the live,
  operator-edited `<project>/settings/flags.toml` and any drop-ins on the deployed boxes.

### Fixed
- **Both committed marketing transcripts had gone stale, and one of them showed a command that no
  longer runs.** `.trader/shots/agent-cli-backtest.txt` typed
  `vike-cli backtest --local …`; `backtest` grew a mandatory sub-verb, so on today's `main` that
  spelling is an EXIT 2 — a published page demonstrating a command a reader cannot run. Its
  `--help` roster was stale in the same edit's wake (it listed `sweep`, `walkforward` and
  `strategy-status`, and none of `report`, `research`, `backend`, `datahub` — eleven verbs where
  the binary prints twelve). ⚠ The sibling `agent-mcp-session.txt` was stale too, and INVISIBLY:
  every command in it still works, but the tool roster it prints had grown from 17 to 23 and the
  handshake now returns the server's `instructions` block, so nothing about running it would have
  said so. **Both were RE-CAPTURED, not edited** — `VIKE_SMOKE_CAPTURE_DIR=.trader/shots
  scripts/cli_mcp_smoke.sh` on a the CI box lane, the one sanctioned route
  (`scripts/marketing_shots.sh`'s `*.txt` arm VERIFIES the committed file and deliberately
  re-derives nothing), transferred by the sha256 that script prints beside each file and checked
  against it. Hand-patching the command line would have left a transcript claiming a session that
  never happened, with output no command above it produced; `.trader/shots/manifest.json` calls
  that "the same lie in monospace". The manifest's two `captured` rows carry the new date and
  commit, and both `claim` counts move with the captures they evidence.
- **The desktop's whole hist-store plane dialled the datahub unauthenticated**, so the Data Manager
  and Studio read an empty store from every remote deployment while the log said exactly why.
  `RemoteHistStore::with_keys` had no production caller at all: both dial sites used
  `RemoteHistStore::new`, whose own doc says to use the keyed constructor against a keyed server.
  ⚠ The severity is structural rather than situational — `vike_datahub_client::bind::bind_decision`
  REFUSES to start a datahub bound to a non-loopback address without keys, so every datahub
  reachable off-box is necessarily authenticated, and the loopback dev case kept working and hid it.
  Same defect CLASS as the DOM-ladder fix one layer over: the market-data half had been wired to
  `connect_authed` and the store half was left behind. One helper (`remote_hist_store`) now serves
  both sites so they cannot answer differently, with the observe-key NAME resolved on the frame
  thread beside the ADDRESS — both follow the ACTIVE backend, and reading them at different instants
  signs one backend's datahub with another's key.
  ⚠ A third dial stays unauthenticated ON PURPOSE and now carries a tombstone saying so:
  `vike_app_core::data::backfill_wire`'s `run_wire_backfill` sends `Backfill`, a CONTROL-scope verb, and
  control scope also carries the verbs that compile client-supplied Rhai — so this binary must never
  hold that key. It fails closed, naming the missing keys, rather than doing nothing in silence.

## [0.1.24] - 2026-09-12

### Added
- **`vike-backend datahub` serves a live DOM and a live trade tape over the node protocol.** §4 and §8
  of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`: the shared vocabulary
  (`MdSpec`, `MdFrame`, `BookSnapshot`, `WireStreamStatus`, `MdRefusal`, `MdBye`) lives in the LIGHT
  crate, `crates/vike-datahub-client/src/market.rs`, so both ends cannot drift on a spelling; the
  daemon side is `crates/vike-datahub/src/md/hub.rs`'s `MdHub` plus its per-session mailbox. The
  plane is ARMED by the exact string `VIKE_DATAHUB_LIVE=1` and COMPILED by `live-feeds` plus one
  `live-<venue>` feature per bridge — but `MdHub` itself is deliberately feature-FREE, because it
  appears in `serve_authed`'s public signature and cfg-ing the `MdSubscribe` arm away is exactly what
  the decode-vs-drop contract forbids: a build with no plane must DECODE the verbs in order to refuse
  them cleanly, answer `Response::Error` and leave the connection POSITIONAL. That also puts the hub
  suite on the default build, in the roster lane, on every PR. Every constant carries its measurement
  at its declaration (§12 of the design, four one-hour windows of real BTCUSDT depth and tape), and
  the mailbox is bounded in BYTES as well as frames: a frame cap alone makes the box's ceiling a
  CLIENT-chosen parameter — 200 levels a side is 11.2 MB to 42.9 MB and breaks the under-32 MB claim
  `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` leans on — and cannot bound
  a `Trades` frame at any depth, whose size is a function of how far behind the publisher fell.
  `MD_TAPE_BATCH_MAX` was RETIRED rather than given a number, because splitting a drain would break
  the one-frame-per-key-per-tick property the whole derivation rests on. The memory claim is a
  `const _: () = assert!` rather than a paragraph.
- **The desktop's DOM ladder, trade tape and off-daemon charts come back — over that wire, not over a
  local venue mount.** The `fat` deletion took the desktop's local market-data plane and left
  everything downstream of it standing: `BookStore`, `TradeStore`, `GuiFeedSink`, `TickVolAgg`,
  `OrderflowAgg` and the whole of `feed_lifecycle.rs` all compiled, all CI-tested, and fed by nothing
  — `GuiFeedSink`, the struct built precisely to be the core-free market-data seam, had ZERO
  production constructors, and `ensure_depth` took its `no feed registered for venue — the DOM ladder
  will stay STALE` branch every frame for ever. So the desktop was a monitor: positions, orders and
  equity over the node wire, plus bars for the ONE pair the daemon happens to be trading
  (`crates/vike-tradehub/src/publish.rs`'s `project_bar_series` filters on the primary mount). One
  connection to the backend's datahub now feeds all three. The state machine is in
  `crates/vike-app-core/src/data/md_session.rs` and `datahub_feed.rs`, never in the GUI shell, because
  `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` names `vike-desktop` — a thread-and-socket state
  machine there is one nobody runs. Three decisions are load-bearing rather than stylistic: the
  desired/served diff compares `MdSpec::key()` and never `MdSpec`, since the client sends
  `depth_levels: None` and the server echoes its AUTHORITATIVE spec with the depth RESOLVED, so a
  whole-struct diff differs on every key for ever and re-dials the live server once per pass; every
  `TradeTick` is re-stamped from the envelope, because the hub blanks `tick.symbol` on the way in and
  `TradeStore::push` keys on that field, so passing wire ticks through puts every print under
  `(venue, "")` and silently empties every tick/volume chart and orderflow overlay; and receipts are
  stamped with the LOCAL clock, never `BookSnapshot::venue_ts`, since two boxes' clocks differ and a
  wire stamp makes every ladder permanently stale or falsely live across a tunnel. The dial is
  AUTHENTICATED for the first time — `BackendRecord::datahub_observe_key`, resolved
  process-environment-first then `<project>/settings/node.env` on the same ladder
  `crates/vike-cli/src/lib.rs`'s `datahub_keyring` walks — and only the OBSERVE half is ever
  populated, structurally: the datahub's CONTROL scope carries the verbs that compile client-supplied
  Rhai, and a GUI that wants a DOM ladder must never hold that key. ⚠ **Not a visual verdict**: no CI
  runner has a GPU, so whether the ladder, the tape and the chart actually PAINT is `VIKE_SHOT` and
  `just qa-shots` against a real datahub, with a human looking.
- **`backtest --optimizer grid|euler|tpe|genetic`, all four on one `Optimizer` seam.**
  `crates/vike-backtest/src/harness/optimize.rs` was READ OUT OF the three existing searchers rather
  than designed ahead of them — they share exactly one evaluation unit, one ordering rule, one
  profile builder and one bounded executor, and none of the four you would expect — so
  `crates/vike-backtest/src/search.rs` does not change by one line, which is the sharpest available
  evidence the seam is real. What it retires is the plumbing each lane re-rolled: three hand-rolled
  copies of the ordering rule each commented as matching the others, and ONE error (`strategy.params`
  not being a table) with THREE different fault timings — the grid failed before any backtest, euler
  latched it and re-raised after the whole depth budget, TPE failed at trial 1 and discarded every
  row it had collected. The shared preflight collapses those into one and is what makes
  `PointEvaluator::evaluate` infallible BY TYPE. `harness/genetic.rs` is the first implementation
  written AGAINST the trait and cost `optimize.rs` no change at all; its generator is COUNTER-BASED
  rather than a stream (every decision is a pure hash of `(seed, purpose, generation, slot)` through
  MurmurHash3's frozen `fmix64`), so how many draws one operator consumes cannot move another's — the
  FORK failure a stateful stream already produced once — and its genome is INDICES, so every operator
  is integer arithmetic and "does the searcher find the grid's argmax" is an EQUALITY rather than a
  smoke test. `crates/vike-ops/tests/hygiene/clock_pin.rs` PERMITTED that with no exemption row of any kind:
  it refuses a manifest line, not randomness, and a seeded generator whose seed is a parameter is an
  INPUT. Hence `GeneticConfig::new(seed)` takes the seed as a required parameter and `--optimizer
  genetic` with no `--seed` is REFUSED rather than defaulted — a defaulted seed is a hidden constant
  that changes results silently — while tpe keeps its default of `0` because that default is a
  SHIPPED WIRE (`crates/vike-cli/src/cmd/backtest.rs` forwards `--seed` only when the operator wrote
  one). `--seed` is therefore the first flag with two owners, and `METHOD_FLAGS` is keyed on a SET of
  owners so the refusal names every one of them.
- **`vike-cli report` and `vike-cli study` ship as client halves that currently REFUSE on every box,
  and the refusal is the deliverable rather than a shortfall.** Ruling 16 states the rule `vike-cli
  <X>` asks the backend to do X, and measuring the two rosters found two operations with a backend
  verb and no client side — `report` sharpest of all, because a tearsheet is computed from the LIVE
  JOURNAL the trading daemon writes on ITS box, so getting a report on your own live trading meant
  opening an SSH session to production. Neither can be served by what exists, and they need new
  requests on DIFFERENT daemons, which is why the halves are asymmetric: `report`'s wire verb ships
  here (`Request::Tearsheet` / `Response::Tearsheet` / `FEATURE_TEARSHEET` plus `remote_handle`'s
  `tearsheet`), with the node's one arm declining honestly while `served_features` advertises
  nothing; `study`'s request variant is deliberately NOT added, because ruling 7 was moving every
  compute verb off the data server in a sibling change and adding it would have landed it on the
  surface it is about to leave. Each refusal names the missing capability, states that nothing went on
  the wire, and prints the `vike-backend` invocation that does the same work on the box holding the
  data with this run's own flags carried across — a `--seed` dropped in translation rescales every
  return ratio in the answer without saying so. The recipe crosses the wire as TEXT, never as a path,
  because the backend is another machine and a path would resolve against ITS filesystem;
  `--store`/`--lightgbm` are refused BY NAME for the same reason.
- **A recorded series can now be judged on its RATE, not only on its silence.** The binance perp depth
  lane below recorded at 4% of its true rate for forty days and every watchdog read healthy, because
  `crates/vike-recorder/src/alerts.rs`'s `watchdog_tick` measured RECENCY and that lane never went
  30 s without a row against a 300 s default. `Liveness` already carried a monotonic `rows`, so the
  observed rate was always free; the other operand did not exist.
  `crates/vike-data/src/series_cadence.rs`'s `SERIES_CADENCE` now declares one row per recorded
  series naming a CLASS and, where the code declares one, a CEILING — and `Cadence::ceiling_per_s`
  answers `Some` only for a `Sampled` row carrying an interval, which is the whole safety property:
  an unclassified or unmeasured series yields no number, so a consumer has nothing to threshold and
  cannot invent one. It is NOT a field on `StoreKind`, and the evidence is in this tree: inside one
  kind, `trade` spans binance BTCUSDT.P at 20.3–62.7 items/s and one polymarket token at ~0.25/s, and
  inside one (kind, venue) cell `DEPTH_FRESHNESS_THRESHOLD`'s 2026-07-11 sweep found liquid pairs at
  ~0.1 s and the thinnest actively-traded pairs at 44–47 s — 0.0067 updates/s, sixty times slower
  than the lane this work exists to catch. So the judgement is licensed rather than absolute: a
  sampled lane is judged only while its own instrument's trade tape ran at or above the sampling
  ceiling for the whole 900 s window, and then the expectation IS the ceiling with a floor at 20% of
  it. `@depth@100ms` is a periodic SAMPLING of an event stream, not a periodic publish — it declares
  a ceiling, and the floor is the instrument's liquidity, which no static table can hold. What it
  would have done, from measured numbers: the broken lane at 0.42 items/s against a 2.0/s floor
  alerts on the first completed window, about fifteen minutes in; the healthy same lane at 9.8
  applied diffs/s is quiet at 4.9x the floor; the thinnest measured pair is NOT JUDGED at all.
  Delivery is by alert only — the slow set is kept out of `watchdog_tick`'s return value so it can
  never drive `--exit-on-silence`, because degrading a tape is bad and turning it into no tape is
  worse.
- **...and a whole recorded FAMILY going dark is now a page, reached by changing the subject rather
  than inventing a ceiling.** On 2026-08-05 a Polymarket `book` family wrote 117,724 rows in one
  minute, 90,540 in the next and 86,894 in the next — then 782 rows across the following eight
  minutes, five of them completely dark, while four to six members stayed subscribed throughout, and
  nothing said anything. Recency could not reach it: every member had produced rows inside the 300 s
  threshold, and the family ROTATED at least twice inside the dark span, so the tokens born into the
  collapse had nothing to be stale against. Cadence could not, and MUST NOT BE MADE TO — that lane is
  `Cadence::EventDriven`, whose `ceiling_per_s` returns `None` precisely so no consumer can invent a
  number, and `crates/vike-ops/tests/arch/family_collapse_independence_gate.rs` fails the build if the
  family rule so much as NAMES that table. The FAMILY key is the only subject with continuous
  existence across a rotation, so a dying member's tail and its successor's birth land in one total
  and the ~576 legitimate token deaths a day are invisible BY CONSTRUCTION — not by a tuned grace
  window and not by an exemption anybody maintains. The accumulator is a SUM OF PER-KEY DELTAS and
  never a delta of a sum, because differencing a summed counter across a changing key set reads
  zero-or-negative on a HEALTHY rotating family, every window, for ever. Each constant carries its
  measurement (`FAMILY_WINDOW_MS` = 30 s, derived from the shortest measured event — a 300 s tile at
  the wrong phase reads 0.175 and 0.157 of baseline on the motivating incident and MISSES IT
  ENTIRELY; `FAMILY_RING` = 20 windows; `MIN_BASELINE_ITEMS` = 5,000 as a RESOLUTION gate, not a
  venue classifier), and the 1/200 floor is stated as a judgement with its measured void underneath:
  across 23,154 judged healthy windows on 13 lane-days the worst healthy ratio is 0.0932, every
  incident onset window is 0.000000, and the fire count on eight clean days is ZERO at every
  threshold from 0.05 down to 0.001. A verdict is taken only under a LICENCE — some series outside
  the family produced items in the same window, and the witness must have reached its own learn
  ratio, so a host-wide outage cannot license itself on the stream-status markers a dead feed emits.
  It is a fourth rule id (`recorder-family-collapse`) with its own `AlertSignal`/`RuleTrigger` pair
  carrying an item COUNT and no per-second figure at all, because `expected_per_s` MEANS a declared
  cadence and putting a learned baseline there smuggles an invented number into the one place the
  rate rule exists to protect. ⚠ **It is the COMPLEMENT of the rate watchdog, never its substitute**:
  a fault already present when the baseline was learned is invisible to it — this ring would have
  learned the broken depth rate as normal within ten minutes and said nothing for all forty days —
  and its other declared blind spots (any shortfall shallower than ~200x, one member of a healthy
  family, a decay slower than the ring, the first ten minutes after a restart, any family under the
  resolution gate, and correctness as opposed to volume) are stated on the method, on the trigger and
  in `docs/ops/recorder-deploy.md`, because that honesty is the feature.

### Changed
- **⚠ BREAKING (CLI): `state` is gone from the `trade` REPL, `strategy-status` is gone from the top
  level, and the replacements are `status` / `halt` / `resume`.** The REPL's `state` was a READ and a
  WRITE on one word — `state` printed the trading mode, `state halted` HALTED A LIVE DAEMON, and the
  only thing between them was a second token — so a destructive action was reachable by adding a word
  to a read. It is removed outright: no alias, no deprecation shim (this workspace's no-shims rule),
  so an old runbook line now fails as an unknown command rather than doing something else. In its
  place, `halt` and `resume` each say in their own name that they change something, and `status`
  answers the whole "what is this node doing" question in ONE output — the trading mode, the daemon
  identity, its effective params and one row per mounted strategy — which is why it also subsumes the
  retired top-level `vike-cli strategy-status`. The same three words work at the prompt and as
  `vike-cli trade <verb> --node <host:port>`, so a halt can go in a runbook or a unit file. **Pass
  `--yes` in a script**: without it the one-shot reads one line from plain stdin, and a non-terminal
  stdin has three outcomes rather than one — an EOF is a no, a `y` on a pipe CONFIRMS, and a stdin
  that never delivers a line blocks. Typing the removed `state` at either surface refuses and names
  all three replacements, so a stale runbook line does not cost a search mid-incident.
  `Reducing` keeps no CLI verb — a halt admits position-covered reduces, so `market-exit`/`flatten`
  still work — and stays reachable through the `set_trading_state` MCP tool and Telegram's
  `/state reducing`. `trade status --json` nests the node's `WireStrategyStatus` verbatim under a
  `strategy_status` key beside the mode, so the wire half of the payload is unchanged for a script
  that reaches one level in; a half that could not be READ is a `trading_state_error` /
  `strategy_status_error` key and a stderr line, never an omission, and a node too old for the
  `strategy-verbs` capability still gets asked for — and still reports — its trading mode. Ruling 17
  of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`.
- **⚠ BREAKING (DEPLOY): the recorder and the data server are ONE daemon, and `vike-backend recorder`
  is retired.** Ruling 10: both are data-management processes and neither is whole — the recorder OWNS
  venue subscriptions and has no network surface at all, the data server has the wire and no feeds.
  The new spelling is `vike-backend datahub --record <profile>`; the profile format is UNCHANGED, so
  an operator's `settings/recorder.toml` parses exactly as it did and only the word in front of it
  moved (`--profile` → `--record`). ⚠ **The justification is RESPONSIBILITY, not socket economy**, and
  every file carrying the argument says so: measured before the merge, the trading daemon held bybit
  BTCUSDT while the recorder held polymarket plus binance BTCUSDT.P — three venues, overlap ZERO.
  A library edge was unavailable in EITHER direction (both crates declared `layer = 65`, and
  `layer_gate` fails when `to >= from`), so one file moves and the tier follows the ROLE: the
  recorder's daemon body becomes `crates/vike-datahub/src/recorder.rs`, its `[[bin]]` retires, and
  `vike-recorder` drops 65 → 55 as a library the data daemon composes. With no `--record` the process
  is byte-identical to before the merge; with `--record` the RECORDING keeps the main thread and
  `serve_authed` moves to a spawned one, deliberately that way round, because the serve loop has no
  teardown at all while the recorder's teardown is where the buffered tape is either flushed or lost.
  ONE STORE: the profile's `store` key and the root the server resolved must name the same directory
  or the daemon REFUSES to start, since a recording landing in a root the wire does not answer from is
  tape nobody can read. `vike-backend recorder` is now `Route::Usage(2)` — exit 2 with a tool list,
  never a 203/EXEC — so a stale unit fails loudly; `deploy/vike-recorder.service` and its project twin
  are gone, `deploy/vike-datahub.service` gains the stop budget a serve-only daemon deliberately did
  not have, and `deploy/vike-datahub-project.service` is the recorder's project unit renamed with
  `--record` on its `ExecStart`.
- **⚠ BREAKING (CLI/DEPLOY): the seven compute verbs leave the data daemon for `vike-backend backtest
  --addr`.** Ruling 7 splits one served surface into TWO daemons speaking ONE protocol:
  `RunBacktest`, `RunSlice`, `RunSweep`, `RunWalkforward`, `RunSweepProfile`, `RunWalkforwardProfile`
  and `ListStrategies` move; the store verbs stay. Measurably, co-hosting put the DOM and the tape
  inside a sweep's `MemoryMax=8G` kill radius, so a sweep an operator started could take the market
  data with it and then re-dial every venue socket under `RestartSec=5` — two processes now, two
  ceilings, and neither can reach the other's. THE ENGINE DID NOT MOVE and never lived in
  `vike-datahub`: that crate FORWARDED to `vike-backtest`, and what changed is which socket answers —
  with the consequence that the data server drops its `vike-studio-core`, `vike-script` and
  `vike-backtest` edges and no longer compiles a client-supplied strategy at all. The three Studio
  slice verbs are dispatched through an injected table whose type names only the `Wire*` DTOs (the
  `BackfillTable` seam this protocol already uses), mounted by the multicall dispatcher, because
  `vike-studio-core` (55) depends on `vike-backtest` (50) and the edge could never point the other
  way. Everything a second copy would eventually contradict moved DOWN into `vike-datahub-client` —
  `plane_of`, `required_scope`/`scope_admits`/`VerbScope`, `request_kind`, `wrong_plane_message` and
  the whole bind guard in the new `src/bind.rs` — with no `pub use` shim, and a verb sent to the wrong
  daemon is ANSWERED rather than dropped, by one helper that writes both refusals so they cannot
  drift. Daemon mode is `--addr` with an OPTIONAL value — the flag says become a daemon, the value
  resolves `--addr <v>` → `VIKE_BACKTEST_ADDR` → `config.backtest_addr` → `DEFAULT_BACKTEST_ADDR`,
  folded by `vike_config::Config::apply_env` so one loader decides precedence rather than a second
  ladder inside a binary.
- **⚠ BREAKING (CLI): `vike-cli sweep` is deleted, and the five data-management flags leave
  `backtest`.** Rulings 12 and 13. `--fetch`, `--fetch-starter`, `--seed-demo`, `--export` and
  `--rm-series` are none of them about backtesting — they fetch from venues, write the store, export
  from it and DELETE from it — so they become `vike-cli data <sub>` and, on a box with an engine and
  no `vike-cli`, `backtest data <sub>`. ⚠ It is a SURFACE move and could not have been anything else:
  all five open a `DataFusionHist` and `vike-cli` is DataFusion-FREE by construction (CI's
  `light-consumers` lane asserts it), so the implementation stays in `vike-backtest` and the CLI
  reaches it by SPAWNING. Each retired flag refuses BY NAME in BOTH spellings (`flag_given`, so
  `--fetch=…` is caught — the scripted form nobody is watching), and `--rm-series`' refusal says
  NOTHING WAS DELETED in as many words, because a cleanup script that starts exiting 2 must not read
  as "it may have partially run". `vike-cli data` gains the two subcommands that had no home —
  `export SPEC --out FILE` and `fetch-starter` — and `export`'s `--from`/`--to` are OPTIONAL and
  INDEPENDENT where `fetch`'s window is not, because a fetch with no window decides how much of
  somebody's rate limit to spend while an export bounds a slice already on disk. On the search side,
  `cmd/sweep.rs` is gone and `vike-cli backtest` absorbed `--rank-by` and grew `--optimizer`,
  `--euler-depth`, `--trials` and `--seed`; deleting the verb alone would have deleted the CAPABILITY,
  and growing it first surfaced a divergence that predates the merge — the server's `run_backtest`
  IGNORES a `[sweep]` table and reports ONE point while `--local` handed the same profile to an engine
  that ran the whole grid, same command line, two computations, nothing in the output saying which.
  Five knobs are `--local` only and each is refused BY NAME, because `Request::RunSweepProfile`
  carries no method selector and a silent downgrade to the grid is the precedence defect this release
  ended. The old spelling answers on the same `Exit::Usage` rung as an unknown verb but with the
  replacement in the message (`crate::RETIRED_COMMANDS`) — "unknown command 'sweep'" would tell a
  scripted caller that a verb which shipped for months never existed. A live defect closed on the way:
  `has_flag` is exact-token, so `--rm-series --yes --dry-run=1` IGNORED the `--dry-run` and RAN THE
  DELETE, on a command line written as a rehearsal.
- **binance, bybit and okx get the feeds/exec feature split, so a data daemon stops compiling the
  order plane.** Ruling 8: the module-level split between market data and the order plane already
  existed in all three CEX bridges, but the `[features]` tables did not — so a process that places no
  orders was linking `exec.rs`, the signed transport and the private user-data pump. One default-on
  `exec` feature gated at the `mod` line, nothing moved between files, every existing consumer's
  spelling unchanged and every default build byte-identical. `vike-aster` now takes `vike-binance`
  with `default-features = false` and forwards `vike-binance/exec` from its own `exec`: without that,
  features being additive, a signer-free aster build would have kept compiling the shared Binance
  family's signed rungs, which is the one crate best placed to exercise the new seam. ⚠ The
  `bridges-feeds` lane's `cargo tree` half for these three guards something NARROWER than
  hyperliquid's and aster's and the CI arm says so at length: they sign with HMAC-SHA256 through
  `vike_bridge_core::signer`, which rides `vike-bridge-core/full` that the FEEDS half needs anyway, so
  no crate leaves those trees at all and a crate-name grep would have passed in both configurations.
  The COMPILE half carries the weight instead, and genuinely does — a feeds module that reaches an
  exec module is now E0432. Two consts moved with the split and every call site was rewritten with
  nothing re-exported at the old path: `vike_binance::spot::VENUE` → `vike_binance::VENUE` and
  `vike_okx::transport::BROWSER_UA` → `vike_okx::BROWSER_UA`, both named by keyless kline code.

### Fixed
- **A subscriber that stops reading no longer parks a `vike-tradehub` thread forever.** The node's
  push writer had no write timeout, so a peer that kept its socket open and read nothing — the
  loopback peer is normally `sshd`, which does exactly that the moment its channel to a sleeping
  laptop stops draining — held its connection thread and a `MAX_CONNECTIONS` slot in `write_all`
  indefinitely; the heartbeat only ever caught a DEAD peer, whose socket eventually errors. The
  socket now carries `crates/vike-tradehub/src/server.rs`'s `PUSH_WRITE_TIMEOUT` (30 s, two
  heartbeats) from the moment a `Subscribe` turns the thread into a writer, a timed-out write
  closes the connection with a log line naming the bound, and the link-liveness suite proves it at
  150x scale. No wire change, no new setting.
- **The binance and aster perp depth lanes spoke SPOT's sequence rule, and reconnect-looped for forty
  days while every watchdog read green.** Measured on the recording box: `kind=depth/venue=binance/
  symbol=BTCUSDT.P` recorded 0.41–0.43 updates/s against the ~10/s a `@depth@100ms` stream carries —
  24x — with arrivals in PAIRS spaced 4.29–4.32 s at p50 across four windows at four times of day, and
  2,761 of every 3,600 seconds carrying no depth row at all while the binance TRADE tape on the same
  box in the same seconds was 84–99% busy. `crates/bridges/binance/src/family/depth.rs`'s
  `apply_depth_event` enforced binance's SPOT contiguity rule (`U > last_seq + 1` is a hole) on a
  USD-M FUTURES diff stream whose ids are non-contiguous by design, continuity being carried by `pu`,
  a field this workspace never read: the first post-seed diff applied by accident (it straddles
  `lastUpdateId`), the SECOND returned `DepthOutcome::Gap`, the session errored, and `run_depth_feed`
  slept `DEPTH_BACKOFF` and re-seeded — for ever, the two rows per cycle being the seed publish and
  the one diff that applied. The decoder now discriminates on the FRAME's own self-description (`pu`
  present means the futures grammar, absent means spot) rather than on a caller-supplied `is_perp`
  flag, so ONE edit fixes four venue×plane combinations — the same probe found aster speaks the
  futures grammar on BOTH its planes, so its SPOT depth lane had the identical defect. Replayed over
  the same live capture in the driver's own order, the old rule applies 1 frame and then gaps on
  binance perp, aster perp and aster spot alike, while the new one applies 295/267/194 with zero gaps
  and leaves the spot lane byte-identical. It was green for forty days because every fixture in the
  tree synthesised a contiguous diff as `U = last + 1` with no `pu` — the spot shape — including the
  shared conformance harness's binance `delta` builder, which now runs TWO rows whose futures half is
  RED against the old decoder. The disclosure half is why nobody knew: `FeedCtx::set_status` wrote a
  String into a mutex only the desktop's status bar reads and `depth_main` never called it at all,
  while `RecorderSink::stream_status` early-returned for every stream but `book`, so ~20,000 reconnect
  cycles a day reached neither the journal nor the store. Both are closed, `depth` markers now land in
  `kind=depth`, and the driver gains a rate-limited journal voice. ⚠ **Operationally this is a ~24x
  write amplification on a recording box** — depth goes from ~20 MB/day to ~500 MB/day and ~15.0M to
  ~373M rows/day — and `settings/recorder.toml`'s `max_merge_rows`/`target_mb` were sized against the
  BROKEN rate: re-check maintenance headroom after the first full day.
- **A market feed that recovered never said so, and that left one venue's live reconciliation
  suppressed for 42 hours.** Measured on a production box 2026-09-10: the live trading daemon
  suppressed bybit's reconcile leg once a minute for 42 hours — 2,516 occurrences, zero gaps — while
  the venue was perfectly healthy, with four ESTABLISHED sockets, ~2,100 core events a minute, the
  public API answering in 204 ms and zero faults logged in the entire window. The venue's
  authoritative wallet figure was frozen at one identical value across all 2,209 daemon summaries: the
  exchange's cash was not re-read once. Exposure was zero by LUCK from an unrelated subsystem — the
  Avellaneda maker was HOLDING because its break-even half-spread exceeded its configured maximum, so
  it posted no quote; had it been quoting, 42 hours of orders and fills would have gone unchecked
  against the venue. A deliberate restart cleared it and it re-latched twenty minutes later.
  `crates/vike-bridge-core/src/market_pump.rs`'s `run_market_feed_on` had exactly one status hook,
  `on_session_error`: a successful reconnect reset the backoff ladder — so the driver KNEW the session
  was healthy — and wrote nothing, while every CEX bridge wrote its healthy string ONCE at spawn, so
  one transient blip left `"… ws error (reconnecting): …"` standing for ever and
  `crates/vike-ops/src/reconcile_config.rs`'s `health_from_feed_status` reads that TEXT. Only a
  process restart cleared it, and that function's own doc had PREDICTED this in 2026-07 while its
  neighbouring claim — that `Error` is an unambiguous, currently-observed proof of a real fault — was
  false in its load-bearing word, because nothing kept it current. Patching bybit locally would have
  left eleven other bridges latchable, so this is the one coordinated change the shared-home rule
  asks for: `on_session_error: impl FnMut(&str)` becomes `on_session_status: impl
  FnMut(SessionStatus<'_>)`, WIDENED rather than added as an 8th argument so all ~26 call sites are a
  compile error and `match` exhaustiveness makes each venue's `Live` case a WRITTEN LINE in the diff.
  It fires on the first `FrameOutcome::Confirm` of a session rather than at connect-Ok: that covers
  both entry points (five production call sites drive `run_market_session` directly), reuses the one
  definition of "this session is working" the driver already trusts, is BOUNDED where the gate reads
  it, and manufactures no healthy reading during an accept-then-close storm. One healthy string per
  VENUE rather than per lane is a declared loss — `set_status` dedups on the TEXT, so per-lane
  spellings would turn every session boundary into a transition against the other lane's text, a
  journal line per reconnect into a file layer defaulting to `trace`.
- **Every DOM window after the first got an empty ladder, and the Connections tool called it live.**
  There are TWO doors into the market-data registry and only one of them attached. `MdSubscribe` goes
  through `crates/vike-datahub/src/server/market_data.rs`'s `run_market_writer`, which pushes
  `MdHub::attach_frames` per accepted key before its drain loop; `MdUpdate` — the door every window
  after the first takes, because `crates/vike-app-core/src/data/md_session/wire.rs`'s `push_update` opens a
  fresh short-lived connection — went through `crates/vike-datahub/src/md/hub.rs`'s `MdHub::update`,
  which bumped the refcount, folded the depth and pushed nothing. Nothing else covered for it:
  `publish_tick` skips any entry whose `dirty` bit is clear and all three writers of that bit are
  SINK-side, so the adding session received no status and no snapshot until that venue's NEXT update —
  seconds to minutes on a quiet polymarket instrument, and the whole of a binance depth reconnect on
  the other side. The symptom is worse than blank: `MdSession::gapped` is populated only by an
  arriving `Status`, so a key that receives NOTHING is not gapped and `refresh_statuses` counts it
  among the live streams — an empty ladder while the tool reports `2/2 stream(s) live`, which is the
  exact failure `gapped` exists to prevent, reached through a door its doc did not know about.
  `update` now pushes `attach_frames` into the ADDING session's mailbox and reuses that function
  VERBATIM, so status-first, book-only-when-`Live` and tape-never-replayed remain properties of its
  STRUCTURE rather than of a second copy; the tempting alternative (`acquire` calling `mark_dirty()`)
  is not merely dearer but wrong in four ways and would BROADCAST one dirty key to every subscriber of
  it on behalf of a session that did not ask. It attaches per CHANGE and not per ACCEPTANCE,
  deduplicated by key, and that is a BOUND rather than tidiness: three requests re-naming the same
  already-held 64-key set would otherwise push 192 frames onto a 128-deep control lane and answer the
  peer `MdBye::ControlLaneOverflow`, making
  `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`'s decision 2 — bounded by
  server constants, never by the request — false again on the one term it had to restore. Found by the
  wire CLIENT while its own half was being built, and reported against the server rather than fixed
  there.
- **The datahub trusted two wire fields, and one of them deleted series.** A client holding the
  Control key could send `DeleteSeries { selector, produced_by: Some(""), dry_run: false }` and the
  daemon would delete every series under that `kind=`/`venue=` subtree. The wire field is
  `Option<String>`, so a blank value is `Some("")` — not absent — and it stood down
  `delete_series_verb`'s `produced_by.is_none()` sweep gate; it then satisfied the provenance check
  VACUOUSLY, because `key_matches_prefix` is `starts_with` and every commit key starts with the empty
  string, so `RemovalPlan::verdict` found no foreign key, `execute_removal`'s re-check passed, and
  `delete_series_checked`'s re-assert under the series lock — the TOCTOU guard the whole verb turns
  on — passed for the same reason. Two guards and a re-check, one token.
  `vike_data::store_kind::resolve_produced_by` had refused exactly that spelling since it was written
  and had ONE caller in the tree: the ENGINE's local `data rm` arm. The cure is the WHOLE resolver
  rather than a re-implemented blank test — re-exported as
  `vike_datahub_client::proto::resolve_produced_by` beside the removal vocabulary that is there for
  exactly this reason, and imported from there by the server, so one grep shows both ends of the wire
  sharing one definition of a valid producer filter. It is called AT THE DOOR, before the plan and
  before one `series_commits` read, and unconditionally: refusing a DRY RUN matters because that arm
  returns the plan WITHOUT consulting `verdict()`, so a blank dry run used to render
  `provenance: SATISFIED` — and the MCP surface's `preview_token` binds to exactly that plan. Second
  hole, same shape: of a subscription spec's five fields, `symbol` was held by nothing but the
  post-auth 64 MiB frame ceiling, so an over-length one was ACCEPTED, cloned into the registry, and
  woke the reconciler into calling `subscribe_depth` on the REAL venue, after which `attach_frames`
  emitted a `Status` carrying the string verbatim far over `MD_CTRL_FRAME_CEILING_BYTES` — a TERM in
  `MD_MAILBOX_BYTES`' compile-time assertion, and therefore in the co-hosting budget that
  `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` states and rests on.
  `MD_MAX_SYMBOL_BYTES = 96` sits inside a derived range and the range is the argument: a
  FLOOR of 78, because a polymarket CLOB token id is a uint256 spelled in decimal and 2^256−1 is
  exactly 78 digits — a maximum by construction, not a measurement — and a CEILING of 109 from the
  widest framed empty-symbol `Status` the roster can produce. Authentication says WHO may send a verb;
  it says nothing about WHAT they may send, and nothing else was looking.
- **Four ways `backtest`'s search flags gave a DIFFERENT ANSWER instead of a refusal.** The
  hand-written ladder in `crates/vike-compute/src/backtest_cli.rs` had three arms, each parsing its
  own flags INSIDE the branch that used them, so a flag belonging to a branch not taken was never read
  and never validated — one cause with four faces, every one of them exiting 0: `--optimizer tpe
  --search bogus` ran tpe; `--search euler --trials 0` ignored a `0` that the tpe arm treats as fatal;
  `--search grid --euler-depth abc` was accepted and discarded; and `--optimizer=tpe` RAN THE GRID,
  because `crates/vike-analytics/src/binutil.rs`'s `arg` matched an exact token only, which is the
  worst of the four since it is not a refusal but a full ranked report with no diagnostic — and
  `--rank-by=return` ranked by Sharpe and `--store=DIR` read a different store on the same path. A
  fifth, found while reading: the whole ladder sat inside `if profile.is_sweep()`, so `backtest
  run.toml --optimizer tpe --trials 500` ran ONE ordinary backtest and exited 0. `METHOD_FLAGS` now
  says which method owns each knob and a knob handed to a method that does not own it is a REFUSAL, so
  `--trials 0` answers uniformly across methods instead of being fatal on one path and ignored on two;
  `--search` retires as an ERROR rather than an alias, because both spellings can be given with
  DIFFERENT values and every resolution of `--optimizer tpe --search euler` is a guess; a value-less
  trailing `--optimizer` is refused rather than read as absent (`required_value`), which is the
  spelling `--optimizer $METHOD` writes when `METHOD` is unset; and argv is triaged BEFORE any I/O,
  since `DataFusionHist::open` `create_dir_all`s its root and a mistyped flag used to mint an empty
  store on the way to its own error. The same widening audit closed a blank `--produced-by` in
  `resolve_produced_by`, where an empty prefix matched EVERYTHING and stood down both the provenance
  assertion and the rule requiring provenance before a wildcard delete.
- **The two shipped deploy templates disagreed about where the hist store lives.**
  `deploy/vike-backtest.service` shipped `VIKE_HIST_STORE=<root>/data/hist` while
  `deploy/vike-datahub.service` and `deploy/vike-datahub-record.service` shipped
  `VIKE_DATAHUB_STORE=<root>/market_data/hist`. `market_data/hist` is the authoritative spelling —
  `crates/vike-model/src/state_path.rs`'s `PROJECT_DATA_DIR` + `HIST_SUBDIR`, which
  `crates/vike-ops/tests/deploy/deploy_layout_gate/unit_fixtures.rs`'s `STORE_REL` pins — so an operator following both
  templates and substituting ONE real project root, exactly as the `sed` at the top of each file tells
  them to, got a compute daemon opening a directory the data daemon neither fills nor serves. The
  failure is the quiet kind, and the unit warned about it in its own voice six lines above the defect:
  `data/` holds no `kind=` partitions, so every run loads zero bars and reports a clean, empty, WRONG
  answer rather than refusing. No box in service was affected — only the TEMPLATE pair a NEW
  deployment is copied from, which is why nothing on a running box could reveal it. The gate that
  should have caught it was running over a roster that had silently stopped being complete:
  `every_shipped_store_root_is_one_spelling` iterated a hand-written list of exactly the three datahub
  units and read only `VIKE_DATAHUB_STORE`, so both backtest units were compared to nothing. It now
  iterates `DAEMON_UNITS` — already held exhaustive against the `deploy/` directory itself — and checks
  every unit that NAMES a store through either variable in the new `STORE_ENV_KEYS`, so a future
  store-naming unit joins the check by existing. A derived roster fails differently from a listed one,
  in that it can go silently EMPTY, so the rule additionally proves both spellings were exercised by a
  real unit and that no exemption row stands over a unit naming no store.
- **The backtest daemon's default port was the LIVE TRADING DAEMON's.** Ruling 7 picked
  `127.0.0.1:7879` for `backtest --addr`, and that port is taken:
  `crates/vike-config/src/config.rs`'s `node_addr` doc says the tradehub binds it on its own box, and
  a listening-socket check on a production box found the daemon that signs orders holding it. It was
  caught by a review of an unrelated PR whose `study` client had copied the same number and would
  therefore have dialled the trading socket by default. The numbers are now datahub 7878, tradehub
  7879, backtest daemon 7880 (`DEFAULT_BACKTEST_ADDR`), with the collision and its evidence written
  down at the constant — and pinned there by an `assert_ne!` against 7879 — so the next reader does
  not re-pick it.
- **Five shipped strings named the recording box, and only a tag would have found out.**
  `SERIES_CADENCE`'s `measured:` fields are `&'static str` DATA, not comments, so a box name in one of
  them was compiled into `vike-backend`'s rodata — and `scripts/refuse_box_paths.sh` greps the raw
  bytes of every published asset. That guard runs in `release.yml`'s `build` and `windows` jobs and
  NOWHERE ELSE, so the first tag cut after the recorder work would have built for ~15 minutes,
  refused, and published nothing; and because a tag-push run executes the sources AT THE TAG, a fix
  landing afterwards could not be reached by re-running it — the tag would have had to be re-cut.
  ⚠ **No PR could have caught this**, by construction: `scripts/publish_mirror.sh` REDACTS the token
  out of every text file, `.rs` included, BEFORE it runs the same token list as its FORBID scan, so
  the mirror gate sees a clean tree; `api-docs` documents that already-redacted copy; and
  `compile_time_path_gate` polices `env!("CARGO_MANIFEST_DIR")`-class embeds rather than hand-written
  literals. The binary is the only place the token survives, and only the release looks at a binary.
  Comments are deliberately left alone — the lexer discards them, so they reach no asset, and a
  sentence recording that a number was MEASURED rather than guessed is the thing the next reader
  needs. Only the five strings that SHIP changed, and every backticked path-and-symbol citation inside
  them is byte-identical.

## [0.1.23] - 2026-09-10

### Changed
- **The release hand-off no longer goes through GitHub.** `build`/`gui`/`windows` hand their assets
  to `publish` through a per-run directory on the CI box's nvme (the workflow's hand-off step
  names it) instead of `actions/upload-artifact`, which is what refused
  v0.1.22 at publish with every binary built. `packaging_gate` pins the four jobs to one `runs-on`
  label, and `publish` refuses a lane whose hand-off came from another host.
- **CI uploads nothing to GitHub's artifact storage any more.** The `api-docs` job attached its ~43 MB
  rustdoc bundle on most code PRs at seven days' retention — 140 copies / 6.03 GB on 2026-09-10, the
  whole of the org's quota, which is what refused v0.1.22's release uploads with every binary built —
  and nobody ever downloaded one. The job keeps its scan and its verdict; the built tree stays on the
  runner. `docs/ops/api-reference.md` carries the delivery question, unchanged.

### Note
- **v0.1.22 was tagged and never published.** Every asset built; GitHub refused the artifact uploads
  its release workflow still used ("Artifact storage quota has been hit"). This release supersedes it
  from the same tree plus the two changes above, and is the first to ship through the on-box
  hand-off. The v0.1.22 tag stays as a record; no Release and no deploy ever existed for it.

## [0.1.22] - 2026-09-10

### Changed
- **⚠ THE BACKEND IS `vike-backend`, THE DESKTOP IS `vike-desktop`, AND THE VERBS LOST THEIR
  `vike-` PREFIX** — the rename signed off in #1725 and landed as #1726, #1727 and #1729.
  - `vike` → `vike-backend` (the multicall; the bin follows the PACKAGE name, `crates/vike/Cargo.toml`
    declares no `[[bin]]`). Verbs: `vike-backend trade` (was `vike vike-tradehub`), `recorder`,
    `datahub`, `study`, `report` (was `tearsheet`); `backtest` and `vike-cli` are unchanged.
  - `vike-cli node …` → `vike-cli backend …`.
  - `vike-app` → `vike-desktop`, and the `fat` build is DELETED: the desktop signs nothing, mounts no
    venue and opens no venue socket — it works only against a running backend. The GUI release
    assets are `vike-desktop` / `vike-desktop.exe` (were `vike-app-fat` / `vike-app.exe`).
  - The container's links are the verbs (`trade`, `report`, …) and its entrypoint execs `trade`.
- **⚠ DEPLOY IS A THREE-STEP MIGRATION and this release is step two.** The helper accepts either
  backend name for one cycle, installs `bin/vike-backend` BESIDE `bin/vike`, and reports SKEW —
  "INSTALLED (NOT IN SERVICE)" — until each unit's `ExecStart=` is flipped to `bin/vike-backend
  <verb>`, `daemon-reload`ed and restarted, after which the deploy is re-run. `bin/vike` is removed
  by hand as the LAST step, and the next cycle retires the dual-name acceptance.
  `docs/ops/upgrading.md` carries the sequence.
- The multicall's feature parity gate has its second spelling back as a declared table
  (`SHIPPED_TOOL_FEATURES`) now that the per-package release build is gone; the image gate's
  cargo-side question narrows to the one binary every link points at.

### Removed
- The pmxt ClickHouse sink (`pmxt_backfill`) — a write path with no dedup key (#1724).

### Docs
- `docs/superpowers/specs/`: the rename design (#1725); the datahub market-data wire design carrying
  owner rulings 7–17 and the `Optimizer` trait design derived from grid/euler/TPE (#1728) — all
  awaiting sign-off, nothing built.

## [0.1.21] - 2026-09-09

### Changed
- **⚠ THE RELEASE SHIPS ONE BACKEND BINARY, NOT SEVEN AND THEIR SUM.** `vike` — the multicall —
  already contained every per-tool binary, so attaching both shipped the same program twice: **629 MB
  of per-tool binaries beside the multicall's 148**, measured on v0.1.20, uploaded to the release,
  downloaded again by every deploy so `sha256sum -c` could verify the manifest it came with, and
  re-published to the public mirror. `ASSETS` is now `(vike-cli vike)`.
  What is deliberately KEPT: the per-package `cargo auditable build` (it is `multicall_gate`'s
  comparison half — the only thing that can see a feature DROPPED from the multicall rather than one
  that fails to compile); `vike-cli` (9.6 MB, what `cargo binstall` fetches, and named on four units'
  `ExecStartPre=`).
  ⚠ **Every unit and every install path names the dispatcher now** — `ExecStart=<root>/bin/vike
  <tool>`, a SUBCOMMAND rather than a symlink, so there is no `203/EXEC` class to inherit. This is a
  reversal of #1702's reversal and the reasons differ: that one was right about LINKS, and the
  multicall never needed them. `deploy/sbin/vike-trader-ci-deploy` installs two files and REMOVES
  the three it used to install — its old loop reported an absent asset as "left unchanged", which
  from this release means a stale binary beside a new `vike` with nothing saying so.
  ⚠ Two guards were already lying before this touched them, both from #1716: `deploy.yml` required
  `vike-tradehub` while the helper had moved to `[ -f vike ]` (it would have failed EVERY deploy from
  this release, three steps before the helper it guards), and `run_mutations.sh`'s A22 anchor was
  dead for the same reason. Both fixed. `deploy/vike-tradehub.service` also promised "this DEFAULT
  build contains NO Telegram control channel" — the release builds `vike-tradehub/telegram`, so the
  path IS linked and what keeps it inert is the four runtime gates; the unit says that instead.
- **`vike-cli` finds the backtest engine inside the multicall**, so a release need not ship a
  standalone `backtest` for `--local` to work.

### Added
- **⚠ FXCM IS IN THE SHIPPED BINARY: the ForexConnect SDK is opened at RUNTIME.**
  `crates/bridges/fxcm/build.rs` emitted `cargo:rustc-link-lib=ForexConnect`, so any binary built
  with `--features fxcm` carried a hard `DT_NEEDED libForexConnect.so` and **did not reach `main` on
  a box without the libraries staged** — exit 127. Hence a SECOND 148 MB daemon asset,
  `vike-tradehub-fxcm`, for the boxes that trade FX.
  The C++ boundary is its own shared object now: `libfcshim.so` links the SDK the ordinary way and
  `crates/bridges/fxcm/src/loader.rs` opens it at runtime, resolving ten `extern "C"` entry points by
  name. **No Rust binary references a ForexConnect symbol.** So `crates/vike/Cargo.toml`'s `full`
  forwards `vike-tradehub/fxcm`, the one shipped `vike` carries the venue and starts exactly where it
  started before, the twin asset is GONE, and the release attaches the ~100 KB shim instead.
  ⚠ **The obvious smaller change — dlopen the SDK and dlsym `CO2GTransport::createSession` — was
  planned and is WRONG**, measured rather than reasoned: the shim needs FIVE SDK symbols, because it
  DERIVES from two SDK interfaces whose out-of-line constructors, virtual destructor and typeinfo
  live in the library. Defining them ourselves would put a second `typeinfo for IAddRef` in the
  process — an ODR violation that stays quiet until something compares type identity across the
  boundary. `loader.rs`'s header carries the `nm -u -C` output.
  ⚠ **The compile-time stub is gone, which is a coverage win**: every method in `sys.rs` existed
  twice behind `#[cfg(fcsdk)]`, so CI compiled the stub and never the FFI. The same code compiles on
  every box now, and a MISSING symbol is a diagnostic at load rather than a crash on the call that
  needed it. `sdk_linked()` → **`sdk_available()`** (it answers about THIS box, not the build box),
  with `sdk_unavailable_reason()` naming every path the loader tried.
  ⚠ **Operator step on a box that trades FXCM:** `just fxcm-package <root> <shim>` — the recipe takes
  a second argument, because the libraries alone are no longer sufficient. Omitting it packages them,
  says loudly that FXCM stays paper, and exits 0. `ldd <root>/bin/vike` now says nothing about FXCM
  either way; `ldd <root>/lib/libfcshim.so` is the check that means something.

### Fixed
- **`mirror-publish.yml` blamed the token for a missing tool, on its first real run.** The workflow
  shipped in 0.1.20 and had never executed; `gh` is not installed on the the CI box runners — a fact
  `release-image.yml` and `release.yml` had both MEASURED and written down — so `gh api` exited
  non-zero and the job reported `MIRROR_TOKEN cannot read … check its scope`. The token was correct.
  The prerequisite is its own assertion now, ahead of the question that depends on it. A
  `workflow_dispatch` also runs the workflow from `main` and `publish_mirror.sh` from the TAG, so the
  two can disagree; the header says so.

### Removed
- **The scoop bucket — added in 0.1.20, gone four days later. It was a third spelling of one
  install.** What it installed was `vike-cli.exe` from the release, which `cargo binstall` resolves
  through `[package.metadata.binstall.overrides]` and which a `curl.exe` of
  `releases/latest/download/vike-cli.exe` fetches directly; the bucket added a manifest pointing at
  that same asset and nothing else. Nobody asked for it — it was built because the Windows CLI asset
  had made it possible — and it carried a defect neither of the other two has: the mirror is one
  ORPHAN commit per publish and scoop refreshes a bucket with a plain `git pull`, which refuses
  unrelated histories, so `scoop update` could not advance it and `README.md` had to teach a
  remove-and-re-add procedure instead. The escape was a bucket repository of its own, whose CODE
  shipped in 0.1.20 and whose REPOSITORY nobody created. Deleted with it: the renderer, its
  template, the bucket publisher, `publish_mirror.sh`'s `--bucket` / `--bucket-in-mirror` /
  `--sums` flags, `mirror-publish.yml`'s second write target and the second scope on `MIRROR_TOKEN`,
  eight `packaging_gate` cases and two `publish_mirror_gate` cases. **No shipped artifact lost
  coverage and no install path was removed** — the two that remain are the two that always worked.
  ⚠ If you have it installed: `scoop uninstall vike-cli && scoop bucket rm vike`, then use either
  line in `README.md`; the binary is byte-identical. `docs/decisions/0037` carries the removal, what
  was learned building it (the binstall override pair, which is NOT scoop's and stays), and the one
  condition that would bring a bucket back — somebody who is not us asking for one.

## [0.1.20] - 2026-09-08

### Added
- **A gate over the multicall's feature parity, which three files asserted and nothing compared.**
  The release builds the seven tools twice — once per package with an explicit `--features` list,
  once as `vike --features full` — and `release.yml`, `crates/vike/Cargo.toml` and
  `scripts/ci_feature_suite.sh` each said those two spellings "must stay equal" while pointing at
  each other. ⚠ No lane could see a drift: the `multicall` lane compiles `-p vike --features full`,
  so a tool row that fails to COMPILE is caught, but a feature merely DROPPED compiles fine — a
  `vike-recorder` without `polymarket` records nothing for that venue and looks exactly like an
  unconfigured one, while every per-crate lane stays green because each crate is still built with its
  own features elsewhere. `multicall_gate.rs` now resolves `full` transitively and compares it to the
  workflow's per-package list. They agreed on all ten forwards, which is what made this a cheap
  moment to gate rather than a bug report; proven able to FAIL by dropping
  `vike-recorder/polymarket` on a throwaway branch and watching it name that feature.
- **The Windows CLI installs from the release: a `cargo binstall` override pair and a scoop bucket.**
  `release.yml` already cross-built `vike-cli.exe`; nothing installed it. Now
  `[package.metadata.binstall.overrides]` carries `x86_64-pc-windows-gnu` AND
  `x86_64-pc-windows-msvc`: binstall resolves EVERY triple `detect_targets` returns, and on an msvc
  host that list already carries the gnu fallback (`detect_alternative_targets`), so the msvc row is
  not what makes the install possible — it makes the FIRST probe the right one instead of ten
  default-template 404s. The public mirror is now also a scoop bucket:
  `scoop bucket add vike https://github.com/vike-io/vike-trader && scoop install vike-cli`.
  ⚠ The scoop manifest is RENDERED at publish time, never committed: it carries a concrete version,
  url and digest, and the DIGEST alone settles it — it belongs to a binary the release runner has
  not built yet. (This read "this tree has none of the three (every crate is `0.1.0`, the tag is the
  version…)". The version half stopped being true later in the same release — see *The version is
  the tag*, below — while the digest half is permanent.)
  `scripts/scoop/vike-cli.json.tmpl` is the committed
  template; `scripts/render_scoop_manifest.sh` fills it from the tag and the release's own
  `SHA256SUMS`, and `scripts/publish_mirror.sh` writes the result to `bucket/vike-cli.json` in the
  mirror tree BEFORE it pushes — which also moved every `gh`/slug/manifest refusal ahead of the
  source push, so they now fail while nothing is public.
  ⚠ **Both of those files were DELETED on 2026-09-09 with the bucket itself** — see *Removed* under
  *Unreleased*. This entry is left as it was written because it records what 0.1.20 shipped; the
  refusal-ordering half of the last sentence is the part that outlived it and is still true.
  ⚠ **`scoop update` cannot advance this bucket, and the install docs say so rather than promising
  otherwise.** The mirror is one ORPHAN commit per publish (`git init` + `git push -f`, so no
  earlier tree can hand back a file later withheld) and scoop updates a bucket with a plain
  `git pull -q`, which refuses unrelated histories; a later release is installed by removing the
  bucket and adding it again. `docs/decisions/0037` records the scoop deferral as LIFTED, that
  update gap as a deferral of its own (a bucket repository with real history), and Homebrew as
  still waiting on a macOS runner that does not exist.
- **⚠ BREAKING BY DEFAULT — a live `vike-tradehub` mount now has an automatic stop it was not
  asked for: the CONNECTION-state dead-man.** `policy.toml`'s new `link_deadman_grace_ms` is **ON
  when absent** (`vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS`); `0` is the only off, and the armed
  range is `MIN_LINK_DEADMAN_GRACE_MS`..=`MAX_LINK_DEADMAN_GRACE_MS`, refused at load by name and
  never clamped. When a venue's own bridge reports its market-data link `Disconnected` for longer
  than the grace, **that venue's** resting orders are cancelled (`OrderIntent::MassCancel` scoped to
  it, so a bybit socket death leaves a binance book alone) and — under the SHARED `deadman_action` —
  HALT engages: `Halted` on every engine plus the one cross-process sentinel file, which is
  process-wide because that file is.
  **It is not the silence switch with a new number.** `deadman_timeout_ms` counts INGEST, cannot
  tell a closed market from a dead socket, and stays OPT-IN for that reason; this one reads the
  `FeedStatus` the bridges DISCLOSE, and `Stale` — which is exactly what a weekend or an evening
  close looks like — never counts in either direction. That is what lets it have an armed default.
  ⚠ **It does not cover every venue, and cannot be made to from the file.** Where it arms is a
  per-venue table read from each bridge's emitter (`vike_model::link_deadman_default`): every FX and
  equities venue is OFF because its market has sessions, and most roster venues disclose no
  disconnect at all and are declared residuals. ⚠⚠ **And a second cut:** a venue arms only if this
  daemon subscribes the lane that carries the disconnect. As first shipped that cut was the binding
  one — binance/aster/bybit/okx disclosed a disconnect from their L2 DOM depth feed alone, which
  `vike-tradehub` does not subscribe, so a default (non-`polymarket`) build armed NOTHING and
  constructed no switch. **Closed before release** by the Changed entry below (the CEX tick pumps
  now disclose); the cut itself remains a wiring fact no `policy.toml` line can reach, in either
  direction. The mount logs ONE line per mounted venue at
  startup saying armed-or-why-not, and builds no switch at all when none is armed. `vike-app` and
  the headless IBKR mount stay unwired, as they were for the silence switch.
  `docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md` is the ruling;
  `docs/ops/kill-switches.md` carries the operator row, the residuals and the two gaps.

### Changed
- **`vike-cli` resolves the datahub node keys from the credential store, not only the environment.**
  It read `VIKE_DATAHUB_OBSERVE_KEY`/`VIKE_DATAHUB_CONTROL_KEY` from `std::env::vars()` and nowhere
  else, so a pair sitting in `<project>/settings/secrets.env` — where its own refusal text sends an
  operator — did nothing. ⚠ The same defect had already been found and fixed one key pair over: the
  module doc records that both TRADEHUB node keys "used to read `std::env::var` and nothing else, so
  a correctly-configured box answered 'nothing to do' and exited", and the datahub pair was then
  written in that shape with a comment claiming it worked "exactly as `node_keyring` does". The worst
  part was the refusal naming the wrong remedy. `datahub_keyring` now mirrors `node_keyring` — env
  first, store second, lazily, for the five arms that can dial a datahub — and `credential_store` is
  factored out so the read has ONE spelling. Measured against a keyed datahub: keys in the store
  alone gave "no node keys were supplied"; the same keys resolve now.
  ⚠ This WIDENS which arms open the venue-key file. The rule was "a provenance or backtest command
  has no business opening the file that holds every venue key on the box"; it is now "an arm that can
  dial a node may open the store", and `config`/`secrets`/`init`/`indicators` still never do. The
  narrower version — read the store only for a REMOTE invocation — was refused on measurement:
  `cmd::data`'s address resolves to a default whether or not `--addr` was passed, so "did this go
  remote" is not answerable from argv.
- **The deploy installs the tools a box could not previously run.** `deploy/sbin/vike-trader-ci-deploy`
  installed `vike-tradehub` and `vike-cli` and nothing else, so everything else on the box arrived by
  hand — which is how its `vike-recorder` came to be months old with nothing saying so. It now also
  installs `backtest`, `vike-study` and `tearsheet` as REAL per-tool binaries, selected by the rule
  *no systemd unit names it*, so installing them restarts nothing. `tearsheet` is the one worth
  naming: it reads the live daemon's journal and prints the same metrics a backtest over those fills
  would, on a box that could not previously render its own performance. Each goes down through a temp
  file and `rename(2)` — writing over a RUNNING executable fails with `ETXTBSY`, and a human's
  `backtest` can be mid-deploy. A missing asset is REPORTED per name, never fatal: the helper is
  installed out of band and will meet older releases. `status` now RUNS each tool's `--version`
  rather than listing names, so a tool that cannot execute is visible instead of implied.
  ⚠ Briefly this installed the `vike` multicall plus symlinks instead (631 MB of binaries against
  146 MB) and that was REVERSED on measurement: the deployment disk had 7 TB free, so the saving was
  0.009% of it, the deploy downloads every asset either way, and a dangling link is
  `status=203/EXEC` on a tag with no reviewer. The container image still ships the multicall, where
  the size genuinely matters because every user pulls it.
- **The version is the tag.** Nineteen releases shipped as `0.1.0`: all 64 member manifests spelled
  `version = "0.1.0"`, `[workspace.package]` carried no version, and `just release` cut a tag while
  bumping nothing. ⚠ The costs were user-facing, not cosmetic — `cargo binstall` had to resolve
  `releases/latest/download/` because `{ version }` rendered ONE string for every tag, so a user
  could install the **newest release and no other**, and a re-run reported "up to date" whatever the
  tag (`--force` was the way round it); `scoop list` said `0.1.19` while the binary it installed said
  `0.1.0`. The root `[workspace.package]` now carries the one `version`, every member takes
  `version.workspace = true` — the version is written **once** where it was written 64 times — and
  `just release X` rewrites and commits that line, refreshes `Cargo.lock`, and only then tags `vX`,
  so the tag cannot point at a tree that disagrees with it. `binstall` moved to
  `releases/download/v{ version }/`, which is exactly what `crates/vike-cli/Cargo.toml` had promised
  for "the day the release recipe bumps this manifest's version to the tag". A build BETWEEN releases
  reports the last released version; the identity that moves is the git commit
  `vike_buildinfo::version_line` already prints beside it. Three manifests keep their own versions
  because they are not workspace members (`crates/bridges/ctrader/protogen`, `fuzz`, the vendored
  `ibapi`), and `Cargo.lock` was updated by package NAME because five third-party crates genuinely
  are `0.1.0`.

- **The connection-state dead-man now REACHES the venues a default `vike-tradehub` daemon mounts.**
  It shipped ON by default and armed nothing on a non-`polymarket` build: binance/aster/bybit/okx
  disclosed a dead link from their L2 DOM depth feed alone, and the daemon's CEX arm subscribes the
  kline lane and the quote/trade/book tick pump — not that one. The pumps now disclose their own
  transport state (`crates/bridges/binance/src/family/depth.rs`'s `disclose_link` and its bybit/okx
  twins) straight onto the core tick lane they already write ticks to: one `Disconnected` per
  outage, one `Live` on the FIRST FRAME back rather than at connect, so a venue that accepts a
  socket and drops it in a loop reads as one outage instead of resetting the grace forever.
  Chosen over subscribing the DOM feed because that would open a SECOND socket to the same
  `@depth`-class stream on the same host, maintain a second copy of the same book and add a
  five-minutely weight-50 REST re-seed — for a signal the first socket already has, delivered into
  a sink verb that is a no-op in this daemon. Zero extra bytes on the wire. The per-venue table is
  still the authority (every FX and equities venue stays OFF — a close is not an outage), each CEX
  row now cites both emitters, and a gate panics if the mount ever stops subscribing a disclosing
  lane. `docs/decisions/0038-…` records the closure under its own "what would reopen this" clause;
  the residual is that no production link death has yet been watched tripping it.

### Removed
- **The `poly-l2-monitor` files left this repository for the one they describe.** Five files — a
  health check, its unit, its timer and two harnesses — watched a SEPARATE project's ClickHouse tape
  (`poly-l2-recorder`) while living here. ⚠ The tree had already NOTICED: two gates carried declared
  exemption rows saying so in as many words ("a `Type=oneshot` health check for the SEPARATE
  closed-repo poly-l2-recorder"; "it belongs to that repo's deployment, not this one's"). That is the
  shape worth remembering — an exemption whose REASON is "this belongs to another project" is a
  request to MOVE the file, not a reason to exempt it, and writing the exemption froze a correct
  diagnosis into infrastructure. They are in that repository now; both tables lost their rows and
  gained a note saying why. `vike-notify` deliberately stayed — it is shared alerting infrastructure
  three other units hang off. The three prose citations of the moved files are KEPT, as
  `DEAD_PATH_EXCEPTIONS` rather than deletions: they are the evidence for an incident write-up, and
  the citation gate cannot follow a path across a repository boundary.

### Fixed
- **The bybit and okx market-data pumps dialled with NO connect bound**, so a black-holed route
  pinned their feed thread for the OS's own SYN ladder (~127 s on Linux defaults) with the stop
  flag already raised behind it. The binance/aster twin of the same pump had this fixed on
  2026-08-08 and these two were missed; `crates/vike-ops/tests/venues/feed_stop_windows_gate.rs` found
  them the moment those files named a feed driver. Both now dial through
  `vike_bridge_core::ws_proxy::connect_ws` under `pump_spec::CONNECT_10S`, the same bounded arm
  every other feed takes. It matters more than it did: this is now the lane the connection-state
  dead-man hears a bybit/okx link die on, and 127 s of unstoppable dial is longer than that
  switch's own default grace.
- **The absent-`deadman_timeout_ms` warning no longer says "this live mount has NO automatic stop".**
  That sentence became false the moment the switch above shipped armed by default, and an operator
  acting on it would either disable a real protection or write a key that halts an FX mount every
  Friday. The message now names WHICH switch is off, says the other is on, and points at the
  per-venue lines beside it; a test asserts the retired sentence cannot come back.

## [0.1.19] - 2026-09-05

### Added
- **`vike-cli secrets set KEY` — the one credential writer the CLI is allowed to have.** The value
  comes from **stdin** or from a **named environment variable** (`--from-env NAME`) and NEVER from
  the command line: `secrets set KEY VALUE` is a usage error, because argv lands in shell history
  and in `ps` output for every user on the box, and the refusal deliberately echoes nothing the
  operator typed. It is a SECOND CALL SITE of `vike_secrets::save_credentials`, the workspace's one
  byte-preserving atomic upsert — never a second writer — so every other line, comment, blank and
  their order survive verbatim. The key is validated against `vike_model::credential_keys` and an
  unknown name is refused BY NAME with the nearest real ones; an ABSENT store is refused too,
  naming `vike-cli secrets template`, because creating the file stays the operator's decision. Each
  write appends one `change_journal` `credential_write` record (key NAMES only, never a value).
  ⚠ The MCP surface stays credential-free and none of this is reachable from it.
  `docs/decisions/0036` fixed this exact shape BEFORE the writer existed and now records that it
  exists; `crates/vike-ops/tests/credentials/credential_writer_gate.rs` pins the set of files in the whole tree
  that may call a credential writer, with the reason each may.
  Four narrowings landed in review, each now covered by a test: **no unrecognised token on `set` is
  quoted back at all** (a value starting with `-`, which base64url allows, used to be printed
  verbatim to stderr by the generic unknown-option arm); **`--file` is REFUSED on `set`** and stays
  an inspection flag on `list`/`path`/`template`, because it aimed a write at any path the operator
  named — `$VIKE_SETTINGS_DIR` is how a scripted run targets another project; **a multi-line value
  is refused** rather than written (it truncated the credential at the newline and turned the
  remainder into a second `KEY=VALUE` line for a key nobody named); and **a `KEY__LABEL` account
  name is refused without suggesting the unlabelled key**, which is a different account's live
  credential.
- **`vike-cli.exe` is now a Windows release asset.** The Windows job cross-built the GUI and nothing
  else, so a Windows user got `vike-app.exe` and no terminal surface at all — no `config check`, no
  `secrets path`, no `backtest`, and no way to run the agent-facing `mcp` server. ⚠ The standalone
  `backtest` engine is still a **Linux/container asset only**: `backtest --local`, `sweep --local`
  and the whole `data` verb therefore need an engine a Windows user supplies themselves, and the
  missing-engine message says so and names the command that builds one.
- **`vike-cli data fetch` and `vike-cli data seed-demo`** — market data into the hist store without
  knowing a second binary's name. Both drive the standalone `backtest` engine as a child process;
  the CLI itself stays DataFusion-free.
- **`vike-cli backtest --local` and `vike-cli sweep --local`** — the same run on this machine, with
  no `vike-datahub` to talk to, by driving that same engine. `--preset`/`--script` still apply
  client-side in both modes, so a local run is a rehearsal for a remote one.
- **`--json` on `secrets list` and `init --dry-run`**, the two machine-readable surfaces that were
  missing. `secrets list --json` prints key NAMES and their venue, never a value.
- **`cargo binstall --git https://github.com/vike-io/vike-trader vike-cli` installs the released
  CLI without a toolchain.** `crates/vike-cli/Cargo.toml` gained a `repository` field naming the
  PUBLIC mirror and a `[package.metadata.binstall]` section spelled against what `release.yml`
  ships — a bare Linux binary, no archive, no platform suffix — and read from cargo-binstall's own
  documentation rather than from memory. Linux only: there is no `vike-cli.exe` and no macOS
  build. It points at the mirror's LATEST release, because a release bumps no crate version and a
  `v{ version }` URL would never resolve. `crates/vike-ops/tests/release/packaging_gate.rs` renders the
  template itself and refuses one naming an asset no `SHA256SUMS.part-*` line carries.
- **The public mirror's release now carries the binaries.** `scripts/publish_mirror.sh` copied
  only the docs-data JSON from the private release, so a public `cargo binstall` would have
  404'd on its first run. It now downloads `SHA256SUMS`, then every asset it names, verifies each
  against it, and uploads them all plus the manifest — derived from the manifest, never a second
  list, the rule `release.yml`'s attach loop already follows. The docs-data handling is unchanged.
  Nothing becomes public until the owner runs the publish for a tag.

### Changed
- **⚠ `vike-cli`'s exit code is now a ladder rather than a boolean, which is a behaviour change for
  anything wrapping it.** `0` (success) and `1` (ran and failed) keep their exact meanings, so a
  script written against the old two-value behaviour still reads correctly — but failures that used
  to exit `1` now exit `2` when the command line was wrong (an unknown verb or flag, a missing
  required flag, no subcommand at all) and `3` when a service could not be reached (no datahub, no
  node, a refused or timed-out connection). The point is the distinction a wrapper needs: `2` is
  FIXED, `3` is WAITED on, `1` is the ordinary failure. `crates/vike-cli/src/exit.rs` is the
  authority; `4` and `5` appear in that enum and are RESERVED — nothing produces them yet, and a
  script may not branch on them.
- **The pinned Rust toolchain moved 1.96.0 → 1.97.1.** `rust-toolchain.toml` is the only thing
  making the four build environments agree — the Windows dev box, CI (the CI box), the latency runner
  (the latency box) and the CI box's test clone do not share a default toolchain — so the pin is the whole
  mechanism, and no box was touched individually.
- **The workspace moved to Rust edition 2024.** 64 manifests; the vendored `ibapi` copy and the
  workspace-excluded `ctrader/protogen` recipe deliberately stay on 2021 — the first is
  byte-compared by its drift gate. The migration was cheap for a reason worth knowing: the
  workspace's `unsafe_code = "forbid"` had already eliminated both of the edition's headline
  breakages (zero `unsafe fn`, zero `static mut`). What it DID force is the useful part: every
  `std::env::set_var` left the tests — 39 sites across 7 files — because the edition makes it an
  `unsafe fn` and the forbid cannot be lifted. Each test now drives a pure core over injected
  values; no settings-registry row moved, because every wrapper keeps its explicit
  `env::var(CONST)` read. `gen` became a reserved word (the two generated-code modules are
  `codegen` now), let-chains are stable (clippy's machine fix collapsed every collapsible `if` nest in the tree, 157 files), and the style-edition reformat
  is the bulk of the diff — with ONE of its hunks reverted after review: a `return` in `vike-app` that clippy, running under the `thin` feature, judged needless and that the default `fat` build depends on. `resolver` stays `"2"` on purpose — dependency resolution is a separate
  decision from the language edition.

### Fixed
- **The credential store's MODE, its SYMLINK and its DUPLICATE keys all survive a write now**
  — `vike_secrets::save_credentials`, so the GUI's Connections editor and cTrader's token rotation
  get these as well as the new CLI verb. The atomic temp+rename replaces the destination inode,
  which silently discarded two properties: a `chmod 600` store came back at the umask default
  (0644 — world-readable live signing keys, at the moment of a rotation that printed success), and
  a store kept as a SYMLINK into an encrypted or root-owned volume was replaced by a regular file
  full of plaintext inside the project directory, while the real file kept its pre-rotation
  contents for anything reading it by its own path. The mode is carried across now (a store this
  code CREATES starts at 0600 rather than inheriting the umask), and a symlinked store is resolved
  and written THROUGH. Separately, the upsert replaced only the FIRST line matching a key while
  `parse_dotenv` is LAST-wins — so rotating a key a store happened to hold twice (an ordinary
  hand-edit, or `secrets template >>`, the append typo of the documented `>`) reported "replaced",
  journalled `Applied`, and left every loader in the workspace reading the OLD credential; every
  matching line is replaced now. A failed write no longer leaves `.secrets.env.tmp-<pid>` beside
  the store holding the whole thing in plaintext. ⚠ The mode and symlink halves are Unix-shaped and
  are therefore covered by `#[cfg(unix)]` tests that the Windows dev box cannot run.
- **A shipped binary no longer carries the build box's paths, and a release refuses an asset that
  names the box.** rustc embeds every registry crate's source path — on the runners, a path under
  the runner account's home — in panic-location strings, and nothing remapped it; the same tokens
  `publish_mirror.sh` refuses in SOURCE went out in every binary. Each asset-building job of
  `release.yml` now takes `rust-ci-setup`'s `remap-paths`, which appends `--remap-path-prefix` for
  the checkout and `CARGO_HOME` to `RUSTFLAGS` (and `-ffile-prefix-map` to `CFLAGS` for the C in
  the tree), then runs `scripts/refuse_box_paths.sh` over every asset its manifest part names. The
  token set is ONE file, `scripts/forbidden_tokens.ere`, read by that guard and by
  `publish_mirror.sh`'s FORBID scan — which now also runs over the staged release assets before
  they reach the mirror. ⚠ The remap alone would have left the next tag RED on every asset lane,
  because the remap rewrites only what the compiler emits: measured on the real v0.1.18 assets,
  `vike-app-fat`/`-thin`/`.exe`, `backtest`, `vike-datahub` and the `vike` multicall each carried
  the runner's checkout path through `env!("CARGO_MANIFEST_DIR")` — the dev-checkout rung of the
  hist-store ladder, a string literal no flag touches — and everything linking `vike-ml` carried
  a box name in two error strings. Both producers are fixed: the three shipped ladder sites
  (`vike-app`'s `studio_store_root`, `vike-backtest`'s `binutil::repo_default`, `vike-datahub`'s
  `run`) pass that rung under `cfg(debug_assertions)` only, so a `--release` build resolves from
  the project walk down and carries no checkout path at all (`vike_model::store_path`'s
  `resolve_store_root` takes `Option<&Path>` for it, and its module doc argues the trade), and
  `vike-ml`'s error text names "the training box" rather than the box. `build_lightgbm.sh` also
  stops recording `hostname` in `lightgbm.PROVENANCE`; the copy cached on the release runner still
  carries one, but that hostname is not in the token set, so nothing refuses it and no rebuild is
  forced — this entry used to say the guards would, which was wrong.
- **The public mirror's release withholds what embeds somebody else's proprietary code, and the
  binstall URL cannot be stolen by the dataset release.** Two findings against the mirror step
  above, both from reading the REAL v0.1.18 manifest: the derived set would have re-published
  `vike-tradehub-fxcm` (the release runner DOES stage the ForexConnect SDK — the "no CI runner has
  it" assumption was stale) and the JForex shadow jar, which bundles Dukascopy's client libraries.
  `scripts/nonfree_release_assets` is the one spelling of what stays private; the asset step subtracts
  its rows from the manifest before it downloads, verifies or uploads anything, and the mirror's
  `SHA256SUMS` is the private one with those rows removed. And `releases/latest/` — the URL the
  template renders — is a property any release on the mirror could take: `publish_starter_data.sh`
  recreates the `starter-data` release at HEAD on every refresh, so a refresh after a snapshot made
  THAT the latest release and 404'd the install line. It now passes `--latest=false`, the version
  release passes `--latest`, and `packaging_gate.rs` holds both scripts and the withheld set.
  `docs/decisions/0037-…` records the withholding verdict and what would lift it.
- **The toolchain gate was blind to the copy every CI job actually uses.**
  `the_toolchain_version_has_exactly_one_value` scanned `.github/workflows/` and nothing else,
  while every Rust job installs its compiler through `./.github/actions/rust-ci-setup`, whose
  `toolchain:` line is a version literal like any other. `mutants.yml` carried a comment asserting
  that input was "counted once" by the gate; it was not. An unscanned copy contributes no entry,
  so a stale value there left the one-value assertion green while the shared setup step installed
  one compiler and `rust-toolchain.toml` overrode it to another. The scan now walks
  `.github/actions/**/action.yml`, and `the_shared_setup_action_is_scanned` fails if it ever stops
  reaching it — the one-value pin cannot catch that class on its own.

## [0.1.18] - 2026-09-04

### Fixed
- **`docker pull vikeio/vike-tradehub:latest` works.** It never had: `release_container_image.sh`
  pushed `:$version` and nothing else, so the floating tag was never created on either registry —
  while the README's FIRST command is that pull. The failure was invisible from the inside, because
  `pull vikeio/vike-tradehub:0.1.17` succeeds and the registry looks healthy to whoever published
  it; only a stranger following the README hits the 404. `push_latest` now moves the tag after each
  push, and REFUSES to move it for a pre-release — `release.yml` fires on `tags: ['v*']`, which
  matches `v0.2.0-rc1` as readily as `v0.2.0`, and `latest` means "the newest thing a stranger
  should run".
- **The published image's OCI labels pointed at a repository nobody can open.**
  `org.opencontainers.image.source` and `.documentation` both named the PRIVATE development
  repository, so both 404'd for everyone who pulled the public image. `source` is now the public
  mirror; `documentation` is the website rather than a repo path, because `docs/` is excluded from
  the mirror and a mirror blob URL would 404 just as reliably.
- **The desktop app showed an internal repository name to its own users.** The window title and the
  About menu read `vike_trader_rust`; they now read **Vike Trader**, the product name.

### Changed
- **The public mirror is `github.com/vike-io/vike-trader`** (was `.../vike`), and this repository is
  `vike-io/vike-trader-private` (was `vike_trader_rust`). Bare "vike" search results belong to
  vike.dev, the Vite meta-framework, so the brand is "Vike Trader", two words. `STARTER_BASE` —
  compiled into every shipped binary for the starter-dataset download — was REWRITTEN rather than
  left riding GitHub's rename redirect: a redirect is a trap rather than a safety net, working right
  up until somebody claims the freed name, and "vike" is a common word. `starter_dataset_gate` now
  detects both private-repo spellings for the same reason.

## [0.1.17] - 2026-09-03

### Added
- **The source is public** — `github.com/vike-io/vike-trader`, published by `scripts/publish_mirror.sh` and
  by nothing else. This repository stays private, and not as a preference: every workflow here runs
  on self-hosted runners and six fire on `pull_request`, for which GitHub runs the workflow file
  FROM THE FORK — so a public repository would mean anyone can execute code on the box that signs
  orders. The script selects by ALLOWLIST (a directory added next month does not ship until someone
  adds it), redacts the box names, paths, usernames and private IPs that ~300 files mention in
  passing, then scans the RESULT and **refuses to publish** if a forbidden token survived. It is a
  SNAPSHOT, one commit per release, because a mirror with history hands back every withheld file
  from an earlier commit. `docs/`, every `CLAUDE.md`, `.github/`, `justfile`, `scripts/` and the
  live `settings/*.toml` are never published, and `publish_mirror_gate.rs` holds that by RUNNING
  the script and inspecting the tree it produced.
- **A fresh install has data.** Three routes, because the reason a new box is empty differs: a
  deterministic **synthetic demo tape** (`vike-data`'s `demo` module, written under venue `demo` so
  it can never be mistaken for a real row) via `backtest --seed-demo`; a **keyless venue fetch**
  (`--fetch`, public klines, no credentials); and a **published starter dataset** (`--fetch-starter`,
  SHA256-verified) for a box that cannot reach a venue at all. Plus `--export` to Parquet.
- **The venue capability tables can now leave the repo.** `vike-ops`'s `docs_data` bin renders
  `venues.json` and `stats.json` as release assets, derived from the CI-gated tables themselves
  (`VENUES` through `caps_for`, `venue_margin_support`, `fee_schedule_for`, `amend_semantics`,
  attribution and `venue_tif`) — so a docs page or a venue matrix reads what the gates enforce
  instead of hand-copying it. Every hand copy of a roster in this tree has rotted.

### Fixed
- **The Connections picker never saved which backend it picked, and a bare launch signed with the
  wrong key.** Two defects on one promise — *configure a node once, launch bare afterwards* —
  found by installing the released Windows thin client and driving it against three live
  `vike-tradehub` daemons. Nothing ever SET `backends.json`'s `active` pointer, so a node added and
  connected in the GUI stayed `"active": null` and the next launch silently observed a DIFFERENT
  daemon with no error; and the startup connect discarded the record and kept only its address,
  signing with the fixed `VIKE_TRADEHUB_OBSERVE_KEY` instead of the record's own key name —
  `tradehub observe auth denied: bad mac`, on a retry loop, forever. `backend_conn::active_after`
  is now the one writer of the pointer, and `startup_backend` resolves a whole RECORD;
  `--observe ADDR` is unchanged.
- **The viewer no longer refuses to start.** A thin build used to `exit(2)` without
  `--observe <host>:<port>`, so a mistyped flag closed the window instead of opening one. The
  address now falls back to the registry's active backend and then to a local default, and the
  status bar says whether it connected — which is the thing a person can act on.
- **The containerised thin client showed a blank page and exposed its VNC port.** Whoever reached
  `6080` drove the same GUI, `x11vnc` listened on all interfaces without a password, and the noVNC
  page needed a manual click before it rendered anything. The port is now loopback-bound with an
  opt-in password, and the page auto-connects.
- **Linux GUI windows drew tofu boxes.** Every font path in `install_fonts` named a Windows
  location, so the window-control glyphs (`✕ ─ □`) resolved to nothing off Windows. Font
  candidates moved to `vike-app-core` with a test that every named family is BOUND on the platform
  the test runs on — the gate whose absence shipped this.

### Removed
- Six inert planning documents, and `docs/superpowers/`'s index is now gated as 1:1 with the tree
  rather than claiming to be.

## [0.1.16] - 2026-09-02

### Added
- **A licence.** The tree carried none at all — `licenseInfo: null`, with signed trading binaries
  already shipping to the end users `docs/decisions/0014` names. It is now **FSL-1.1-ALv2**
  (`LICENSE.md`, `docs/decisions/0034`): source-available today, each version irrevocably becoming
  Apache-2.0 on the second anniversary of its own release, with paid support and consulting
  explicitly permitted — the reason it was chosen over Commons Clause, which bans exactly that.
  Declared as `license-file` in the workspace manifest, never `license`: FSL has no SPDX id, and
  `deny.toml`'s licence gate stays green only through the `private` exemption, which its comment now
  explains instead of claiming the crates are proprietary.
- **`vike` — one executable carrying every headless tool** (`crates/vike`): `vike-cli`,
  `vike-tradehub`, `backtest`, `vike-datahub`, `vike-recorder`, `vike-study`, `tearsheet`,
  dispatched by argv[0] (an installed symlink) or `vike <tool>`. MEASURED on the CI box: the three
  DataFusion-linked binaries were 128 + 106 + 107 MB, each with its own static copy of the same
  closure; `vike --features full` is **128 MB** — the size of the largest tool it replaces. Every
  existing binary is kept as a thin shim, so every `ExecStart=`, `CARGO_BIN_EXE_*` and installed
  path still resolves. One tool per process is ENFORCED (a second dispatch panics), the dispatcher
  itself boots nothing — `multicall_gate` holds both, because `one_owner` structurally cannot see a
  dispatcher-plus-tool double boot. Thirteen settings-registry rows moved `Binary` → `Injected` on
  the way, which is that registry's declared target state, reached by threading parameters rather
  than by exempting ratchets.
- **The container image is the whole product, published where people can get it.**
  `docs/decisions/0035` lifts `0026`'s additive boundary: the image now carries every headless tool
  (as the one `vike` binary plus seven symlinks — 128 MB against ~500), the ForexConnect runtime so
  FXCM trades from a container (the author's distribution grant is recorded in `0035`; the `.so`
  set stays out of the release assets, where its 25 MB would tax every deploy), a portable Temurin
  JRE staged as a tool so Dukascopy's sidecar runs — no `apt-get`, the no-packages gate still holds
  — and it is pushed to **GHCR** (`ghcr.io/vike-io`, the job's own token, one `packages: write`
  scope, no stored secret) and **Docker Hub** (`vikeio/…`, the shopfront, a stored PAT taken
  deliberately). The privileged `vike-image-build` gained a `push` verb whose two destinations are
  CONSTANTS in the script — restoring push via `env_keep` would have handed the caller the
  destination — with the token on stdin and `docker logout` in a trap.
- **`vike-cli secrets template`** — the whole credential grid (every roster venue × SIM/DEMO/LIVE ×
  the credential suffixes, plus attribution keys), with EMPTY values, derived from `VENUES` so a
  new venue appears the day it joins. Prints to **stdout only**: a `--out` flag was deliberately not
  added, because the moment the command owns a path it can truncate a live store. The legacy
  `MAINNET` tier is read by the loader but not emitted — a first store should not be taught a
  spelling being retired. Composition lives in `vike_model::credential_keys::starter_keys`, because
  the registry gate reads a `credential_key` call site as "this crate READS these variables", and a
  command printing names reads none.
- **The the CI box daemon runs what users run**: the shipped `vike-tradehub` asset is now built with
  telegram, polymarket, record-feeds and materialize — decided so a production box can simulate its
  users. `fxcm` stays out of it (a hard `DT_NEEDED` dies at exec where the SDK is absent) and
  remains the separate `vike-tradehub-fxcm` asset; the two release gates that had forbidden ANY
  feature on the deployed pair were narrowed to their actual reason, native linkage.
- **The study runner that can FIT**, and panel provenance reproducing the engine bit-for-bit
  (#1584, prior session's work landing in this span).

### Fixed
- **Three of eight image binaries never built** (#1595): the roster named `vike-run` — a LIBRARY
  whose package builds no bin of its own — and `vike_recorder`, the file name where the manifest
  renames the target. Every text gate was green through it, because all three files AGREED on names
  that do not exist. `every_rostered_binary_is_a_real_cargo_bin_target` now asks `cargo metadata`
  rather than the filesystem.
- **`release-image.yml` could never fire** and the thin image carried seven drivers it cannot load
  (#1582, prior session); the image workflow's download list was also a hand-kept three while the
  image grew to eight — both the list and the expected checksum count now derive from the staging
  script's roster.
- **A venue outage no longer pages as our incident** (#1583, prior session).
- **`key_permissions.rs`'s module doc claimed the withdraw gate was not wired** while
  `vike_mount::make_engine` has called it for every credentialed binance MAINNET mount; corrected,
  with the survey of all fourteen venues recorded where the deferred per-venue table will need it —
  only bybit/okx/deribit can be probed at all, and for polymarket/hyperliquid/aster the capability
  belongs to the KEY ITSELF, where an all-`None` UNKNOWN that never refuses is the worst answer.

### Changed
- `docs/decisions/0026`'s "no registry credential exists on that box" corrected: a `github.token`
  is present in every workflow run, bounded by declared permissions — the distinction that made
  GHCR publishing one scope instead of a stored secret.
- Dependency bumps: the cargo minor-patch and major groups (#1580, #1581).

## [0.1.15] - 2026-08-31

### Added
- **The backend ships as a container image, and so does the client.** Two images, one per half of
  the split-plane architecture: `vike-tradehub` (the daemon) and `vike-thin` (the observe-only GUI).
  Both are built by `.github/workflows/release-image.yml` from the assets the Release already
  published, verified against its own `SHA256SUMS` before anything is copied in — so "built from the
  same bytes the release ships" is checked rather than inferred from sitting in the same directory.
  `docs/decisions/0026` is amended twice for this: the `"only at a real need"` deferral is lifted for
  `vike-tradehub`, and the GUI exclusion is narrowed to what it actually argued — GPU **passthrough**,
  not the application, which this repository's own `png-export` lane has been rendering on Mesa
  lavapipe with no GPU on every PR. The native path is untouched and remains the reference
  deployment; the image is additive or it does not happen.
- **Windows GUI assets — `vike-app.exe` and `vike-app-thin.exe`**, cross-built on the Linux release
  runner. The release published a Linux GUI only, so the largest desktop population had no native
  client and was pushed onto a container costing 1–3 CPU cores where an `.exe` costs about a tenth
  of one. That gap was never a decision; nobody had checked whether the cross-build worked. It does
  — and the binary was RUN on a Windows box, where it enumerated a real GPU through Vulkan,
  connected to a containerised daemon and rendered the full UI. ⚠ `x86_64-pc-windows-gnu`, not
  `-msvc`: the msvc target needs the MSVC linker and Windows SDK, which no Linux runner has. Worth
  knowing, because day-to-day development on a Windows box builds msvc.
- **`vike-app-thin`**, the Linux observe-only GUI asset — 51 MB against the fat build's 152 MB, and
  the input the client image is built from. `0.1.14` anticipated exactly this twin.
- **`vike-tradehub/ibkr`.** `vike-run` had carried the feature since the bridge landed and
  `vike-app` forwarded it, but this crate did not — so the GUI could mount IBKR live while the
  headless daemon fell through `make_engine`'s `("ibkr", _)` to paper **in silence**. The `ibkr` CI
  lane and its trigger set widen with it, so the arm is now compiled by something.
- **`VIKE_FRAME_LOG`** — an opt-in per-second frame-rate log. A measurement hook, not a feature.

### Fixed
- ⚠ **`vike-app` could not start on Linux at all.** `install_fonts` loaded every face from a
  `C:\Windows\Fonts\…` path, so off Windows the named families were never bound and epaint panicked
  on the title bar in frame one. **The release had been publishing a Linux GUI asset that had never
  been run** — `png-export` renders `vike-chart`/`vike-studio` EXAMPLES rather than this binary, and
  `app-check` only compiles it. Found by running it in a container. The regression test is
  platform-blind and mutation-tested both ways: it lays text out rather than checking membership,
  because a family bound to an EMPTY list passes a membership check.
- **An absent observe key signed with `""` and reported it as `bad mac`.** `unwrap_or_default()` on a
  missing key produces a well-formed HMAC the node rejects, so the operator saw "wrong key" when the
  truth was "no key". The daemon already got this right from its side; this is the client's half of
  the same sentence, and it names where the key is actually read from — the credential store, which
  an environment variable alone does not reach.
- **A warning that contradicted the chart in front of the operator.** `ensure_feed` ended "this chart
  will stay empty until one is", which is false in `--observe`: that build links no venue bridge, so
  every chart reaches that arm while the chart fills from `WireSnapshot::bars` over the node
  protocol. Measured while it logged on repeat against five hours of live candles.
- **The release-image workflow used `gh`, which is not installed on the release runner.** It would
  have failed on its first real run. `release.yml` already carried that rule beside its own publish
  step; the new workflow had not inherited it.

### Changed
- **`<project>/data` is now `<project>/market_data`.** It sat beside `<project>/user_data` and the
  pair read as one concept split in half — the question "which data?" had no answer in either name.
  Two doc claims went with it, both false beforehand: `hist` was never "the only thing in it"
  (`ticks` and a checkout's `bench_hist` are siblings), and the folder is wider than "market" — the
  hist store also holds this account's own `equity`, `exec_fill` and `exec_order`.

## [0.1.14] - 2026-08-29

### Added
- **The GUI ships as a release asset.** `vike-app-fat` — the full desktop app (the crate's default
  `fat` features: the local trading core, every venue bridge, the Studio's local store) for Linux
  x86-64 — is now built by `.github/workflows/release.yml` from the tagged commit, with its SBOM
  embedded by `cargo auditable` and its line in that release's `SHA256SUMS`, exactly like the two
  server binaries beside it. Obtaining the GUI used to mean compiling the workspace. The asset name
  carries the configuration on purpose: nothing is published as a bare `vike-app`, so a `thin`
  (`--observe`-only) twin could join later without re-pointing a single instruction that ever quoted
  the fat one. The tag path also grew the gate it was missing — `vike-app` is excluded from the
  derived crate roster a release re-validates, and `ci.yml`'s `app-check` runs on no tag, so that
  crate's unit tests now run on the release path instead of the artifact being published unrun.

## [0.1.13] - 2026-08-25

### Fixed
- **A reconcile pass that changed nothing logged anyway, once per pass.** `reconcile_reports` emitted
  `reconcile pass folded venue=… events=0 alerts=1` on every pass carrying alerts — ~1,440 lines a
  day per venue, and wholly redundant when nothing moved. Measured on the live box an hour after the
  0.1.11 deploy: 11:41:43 and 11:42:43, one minute apart, byte-identical. The line now reuses the
  held-divergence announcer's own "did anything happen this pass" answer rather than growing a
  second rate limiter beside the one 0.1.11 shipped, so the pass line and the backlog summary fire
  together by construction instead of because two cadences happen to agree. A pass that folds events
  still logs every time, and a venue with nothing held stays silent exactly as before — no heartbeat
  was invented where there had never been a line.
- **Deribit's post-reconnect replay stopped forever on the first socket close, in silence.** The A3
  resync transport connected once and nothing re-dialled it, so a venue-side close ended the replay
  of order/trade history across reconnect gaps for the life of the process — with every error
  swallowed, so nothing surfaced. It is the last of three never-heals sockets in that bridge; the
  other two were fixed in 0.1.11. Which cure applied was traced rather than assumed: that transport
  can carry exactly three frames ever — the auth, the order history read and the user-trades read —
  all non-matching-engine, so a re-dial plus a straight retry cannot double anything, and the licence
  is now stated on the type as what it MAY send. The heal deliberately did NOT go into the shared
  history helper, which the fill sentinel also calls on the EXEC ORDER socket — the one that carries
  `private/buy`, whose re-dial policy is decided elsewhere and must not gain a second, unreviewed
  entry point reachable from a timer. An outage is now bracketed by one warning naming the
  consequence and one recovery line, and the latch re-arms, because a latch that fires only once
  makes a broken lane look healthier the longer it stays broken.

## [0.1.12] - 2026-08-25

### Added
- **A venue can mount SEVERAL accounts.** `make_engine_accounts` returns one engine per ACTIVE
  account — its per-account ceiling permits it, its credentials load, the venue's arm can address an
  account at all, and the symbol-collision rule does not refuse it. Selection and the Data Manager's
  Effective column are ONE function, so the screen cannot disagree with the mount. A default-account
  box is identical, measured: one engine per roster venue, `route_key == venue`, empty
  `live_venues`, and a ledger record with no `account` key at all. ⚠ Only binance/bybit/okx/deribit
  consume the account-aware credential loader; a labelled account is REFUSED elsewhere rather than
  half-supported, because arming one would otherwise have built a live client on the first account's
  credentials.

### Fixed
- **The journal's p99.9 tail is gone rather than reduced** — 37 150 → 3 342 ns (11x), interleaved
  medians of 8 reps, bands that do not touch; the journal-less baseline is 3–7 µs, which is where it
  now sits. The warm window was ~20 ms of runway refreshed only AFTER a 36–141 ms blocking `msync`,
  and it was aimed at the watermark — behind the appender. ⚠ Two earlier claims recorded in this
  tree were wrong and are corrected at the code: the tail IS first-touch faults (the null result came
  from a harness appending 85 B records while the gate appends 275 B), and serialization is 61 % of
  the append's median but only 3 % of its p99.9 — the reused-buffer change was measured and NOT
  shipped. Production's 64 MiB configuration was already at 6.4 µs; this removes a large gate number
  and a smaller real one.
- **A HELD reconcile divergence names WHAT diverged**, and its dedup IDENTITY stops riding that text.
  `kind=PositionOnlyExternal detail=PositionOnlyExternal` was what the live box printed — a line
  raised to answer *which* answering *the same*. `Divergence::describe` is a total match, so a new
  variant is a compile error rather than one that inherits an empty description. ⚠ Because
  `HeldId::new` builds an un-keyed alert's identity from its `detail`, a detail carrying `qty=` would
  have made a drifting position a NEW alert every pass — the ~1,560-a-day bug `recon_held_tests.rs`
  exists to prevent, invisible to its own fixture, which holds qty constant. `identity_detail` is the
  churn-free half.

## [0.1.11] - 2026-08-25

### Fixed
- **The IG trade stream sent a made-up Lightstreamer client id, so the fill lane never worked — for
  any account, demo or live.** `LS_cid` is Lightstreamer's client-type LICENSING field; the trade
  lane sent `"vike-trader-rust"` and IG answered `CONERR,71,License not valid for this Client type`
  on every dial since the day it was written. The market-data lane always sent the TLCP-2.1.0
  mandated value from its own `LS_CID`, which is why feeds worked while fills did not — and why the
  split read as an account-permissions problem for weeks. Proven on the wire against the demo
  gateway with one login and two client ids, everything else held constant: the old value returns
  `CONERR,71` and closes; the mandated one returns `CONOK` and streams. The trade lane also carried
  its own copies of `LS_PROTOCOL`, `form` and `urlencode` — that duplication is what let the two
  spellings of one handshake drift — and `lightstreamer.rs`'s `form` doc already CLAIMED the sharing
  that did not exist. All four now come from one place. This settles the create_session handshake,
  not the fill lane end to end.
- **A permanently refused IG stream wrote an error line every second, forever.** Measured on the CI box:
  53,160 lines and 23 MB in one day from one loop, dead flat for 17 hours, 99.5% of the box's entire
  error volume — for a condition whose own message said it was permanent. The dial-failure path had
  the same defect unbounded and was worse (an unreachable endpoint warned ~78,500 times a day); it
  had been argued safe because "each drop costs a live session", which is true of exactly one of the
  six sites that produce it — the other five never establish a session at all. Retry and log policy
  now live in a pure state machine that takes the clock as an argument, so the log RATE is
  unit-tested rather than being a property of a running daemon. Both lanes now cost ~26 lines a day,
  and the driver reports the CONERR code as evidence instead of asserting a cause it cannot
  distinguish.
- **A Deribit socket closing mid-submit reported an order REJECTED that the venue may have
  accepted.** Only a response timeout was treated as ambiguous, so a mid-request close — the common
  first error on a dying socket — took the definite path and synthesized a terminal reject for a
  possibly-live order, stranding a phantom position. Every failure out of the read half is
  post-send by construction. The rule already existed and this transport was the one not following
  it: `vike_bridge_core::transport::read_body_ambiguous` states that a read failure is always
  ambiguous, never `code: 0`. Resolution is re-dial then RE-QUERY, never re-send — a re-dial
  rewrites no order frame. Verified against testnet that the label query returns terminal orders,
  cancelled and filled alike, so resolving on its answer cannot reject a fill that happened.
- **A Deribit reconcile lane died on the first socket close and never recovered.** The recon
  order-WS connected once at mount and nothing re-dialled it, so one venue-side close disabled
  reconciliation for the life of the process — 795 identical failures in a day, one problem that
  could not heal, while the daemon reported healthy. Every other socket in that crate already had a
  reconnect lifecycle; this one was given a dedicated transport and inherited none of it.
- **A held reconcile divergence re-announced itself every interval, and its alert store grew without
  bound.** 0.1.10 made the divergence visible and reasoned that the refresh path staying silent
  would bound it. It did not: an alert carrying no dedup key never takes the refresh path — it
  APPENDS — so every pass was a fresh raise. Measured on the CI box under `quarantine`: 396 warnings in a
  day for one bybit `PositionOnlyExternal`, with a genuinely new divergence indistinguishable among
  them, and ~1,560 store rows a day, each held in memory and projected into every published
  snapshot. A held divergence is a STATE, not an event: it is announced on entering and on clearing,
  with a slow backlog summary, and the store keeps one row per distinct identity. Confirming one now
  resolves what previously needed one confirm per pass.

### Security
- **A Rhai strategy — and a user indicator — could read any file on disk.** (#1520)

### Added
- **The venue sends a funding premium and we were discarding it** — recorded as `kind=perp_metrics`.
  (#1521)
- **A study can be RUN from the Studio GUI**, either tier, with one results surface. (#1523)


## [0.1.10] - 2026-08-24

### Fixed
- **The `ch_http` fake client survives `ETXTBSY`.** The v0.1.10 release re-validation refused to
  publish on `spawn: Text file busy (os error 26)` from a crate the tagged change never touched —
  which is exactly what that gate is for: a tag runs the FULL roster, a merge runs only the affected
  set. The test writes an executable and immediately spawns it; `std::fs::write` closes its handle,
  but a binary's tests run in parallel threads of one process and a `fork` in any other thread hands
  the child an inherited copy of the write descriptor. Retried in
  the TEST, matched on the errno alone, and BOTH call sites are wrapped: the `select` twin has the
  identical race and had simply not lost the coin toss yet.
  ⚠ **SUPERSEDED 2026-09-14, and how it went stale is the point.** This entry read *"CI cannot see
  it — the fast lane runs nextest, one process per test — so only the tag's plain `cargo test` pass
  reaches it"*. ⚠ A first correction called that FALSE. **It was not false — it was CRATE-SCOPED
  truth written as a CI-SCOPED claim**, and that distinction is the whole lesson, so the stronger
  wording is withdrawn here rather than left standing.
  Measured at the `v0.1.10` tag itself: `scripts/ci_feature_suite.sh`'s `light-consumers` arm
  ALREADY ran a plain `cargo test -p vike-cli` — one process, parallel threads, the exact shape this
  race needs — but `despite_text_file_busy` lived in `crates/vike-research/tests/ch_http.rs`, and
  `vike-research` appeared in NO plain-`cargo test` arm, only the nextest roster. There was also no
  `crates/vike-cli/tests/data_cli.rs` yet. So for the crate this entry was about, "CI cannot see it"
  was exactly right.
  What broke it was not a mistake in the reasoning but the pattern MOVING: the same
  write-chmod-spawn shape was later planted in `vike-cli`, which that lane does run, so the race
  became reachable on any PR whose affected set names it — measured twice on 2026-09-14, on two
  unrelated PRs. ⚠ And the cure itself is GONE from the tree: `vike-research` dissolved in #1524,
  taking `ch_http.rs` and `despite_text_file_busy` with it, so `main` carries no `os error 26` in
  any `.rs` at all. The diagnosis outlived its implementation, leaving the knowledge as prose here
  and no executable answer anywhere. See the `[Unreleased]` entry that cures the whole `vike-cli`
  roster.
  **The durable rule: a property of one CRATE recorded as a property of CI is a claim whose basis
  can be removed without touching the claim.** Say which lane runs which crate, not what "CI" can
  see.
- **A HELD reconcile divergence now reaches the LOG, not only the in-memory ring.** Found on the
  live the CI box box an hour after the 0.1.9 deploy: `reconcile pass folded venue=bybit events=0
  alerts=2` every 60 seconds for hours, zero lines at WARN or above, and no way for anyone reading
  logs to learn WHICH two divergences were being held. `note()` writes a ring reachable only
  through the control channel, and that channel is off on the shipped daemon — so the COUNT reached
  the operator and the CONTENT did not. The raise path now emits one `warn!` carrying the
  divergence kind, venue and detail; the REFRESH path stays silent on purpose, because a divergence
  held for a day re-raises 1440 times and logging each is how an operator learns to filter out the
  whole channel, raise included. `crates/vike-ops/tests/wiring/held_divergence_is_visible_gate.rs` anchors
  on the ORDER rather than on presence — a `warn!` anywhere in that 4000-line file would satisfy a
  presence check while the raise path stayed silent.

## [0.1.9] - 2026-08-24

### Added
- **A ForexConnect-linked `vike-tradehub` is obtainable from a release.** A release built on a
  runner with the proprietary ForexConnect SDK staged now attaches an ADDITIONAL
  `vike-tradehub-fxcm` asset — the same tagged commit with `--features fxcm` — covered by the same
  `SHA256SUMS` and installed by nothing. The ordinary `vike-tradehub`/`vike-cli` assets are
  byte-identical: a feature build gains a hard `DT_NEEDED` on `libForexConnect.so` and dies at exec
  where those libraries are absent, so flipping the feature on the name the deploy installs would
  have broken the next deploy of the live daemon. `scripts/release_fxcm_artifact.sh` builds into a
  scratch target directory, reads the produced ELF's dynamic section (a `--features fxcm` build
  without an SDK compiles a STUB and exits 0, so the exit code proves nothing), re-checksums the
  default artifacts before emitting anything, and declines silently where no SDK is staged.
  `docs/ops/fxcm-forexconnect.md` Step 5 carries what an operator must then do — the
  libraries are NOT in the release and `just fxcm-package <root>` is a required step, not an
  assumption.

### Fixed
- **The ForexConnect rpath reached `vike-fxcm`'s own test binaries and nothing downstream.** Found
  by building the artifact above, which is the first time anything downstream of that crate had
  ever been linked against the SDK: the binary came out with `NEEDED libForexConnect.so` and no
  rpath tag at all. `crates/bridges/fxcm/build.rs` was emitting all three link args correctly —
  cargo scopes `cargo:rustc-link-arg` to the targets of the package that emitted it, while
  `rustc-link-lib`/`rustc-link-search` propagate, which is why the `-lForexConnect` arrived alone.
  #1470's fix was therefore real and its reach was one package. The release build now sets the same
  args on the final link, pinned equal to `build.rs`'s own emissions by a gate and checked against
  the produced ELF before anything is published.

## [0.1.8] - 2026-08-22

### Added
- **The two runtime tools are OBTAINABLE.** A release now attaches the Dukascopy JForex sidecar jar
  and the pinned LightGBM binary (with the `PROVENANCE` file that `LightGbmCli::new` refuses to run
  without) alongside the two binaries, covered by the same `SHA256SUMS`. `just fetch-tools`
  downloads and verifies them into `<project>/bin/<tool>/`. Before this, `research run cohort`
  hard-failed on a fresh install because nothing shipped LightGBM, and Dukascopy only worked
  because a 43 MB jar was force-added past `.gitignore` into every clone's history.

### Fixed
- **The startup preflight's clock budget was sized against six reads, and the roster outgrew it.**
  The budget was a flat constant, sized against the measurement its own module doc
  records — six real reads, 1781 ms, the CI box 2026-08-09 — and never revisited as venues were added.
  By 2026-08-22 the wired roster's pinned healthy readings summed to 2973 ms, and one unreachable
  venue costs a full 3 s `CLOCK_READ_TIMEOUT`: 5973 ms needed against 5000 available. Observed on
  the dev box, where bybit's endpoint stopped answering and EIGHT venues reported their clock "not
  read" — `aster` among them, which runs against mainnet in practice and whose auth binds the clock
  into the order path. The budget is now DERIVED from how many venues are actually read
  (`clock_budget_for`), clamped between the old value as a floor and a ceiling, so a venue joining
  the roster buys the leg more time instead of squeezing the venues behind it. The sizing
  requirement — absorb one dead venue and still read the rest — is arithmetic over those same
  pinned measurements, so the roster outgrowing the allowance reddens a test instead of silently
  starving the tail. A per-venue deadline was considered and rejected;
  `docs/decisions/0027-clock-budget-derived-from-the-roster.md` records why.

### Changed
- **The JForex jar is no longer committed**, and `.gitignore` has no re-include chain left — the
  repository's one force-added path is gone. `crates/bridges/dukascopy/scripts/provision-jforex.sh`
  (and its `.ps1` sibling) fetch the jar instead of failing with "it is committed — bad checkout?".
  ⚠ **An offline clone therefore no longer gets a jar**; `JFOREX_BRIDGE_JAR` still names one
  outright and `--rebuild` still builds one from source. `README.md` carries both.
- **The jar's drift gate became a REPRODUCIBILITY gate.** With no committed copy there is nothing
  to drift from, but "the published bytes are re-derivable from the tag" is newly load-bearing, so
  `.github/workflows/jforex-bridge.yml` builds the jar twice from a cleaned build directory and
  requires the two byte-identical — the claim `build.gradle.kts` has always made and nothing ever
  checked. `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` gained the release workflow as a fourth
  JDK pin site, plus a test that the release still builds the artifact the gate proves.
- `.github/workflows/deploy.yml` follows the manifest instead of naming assets, so it downloads
  every file `SHA256SUMS` lists rather than only the two it installs — `sha256sum -c` verifies the
  whole manifest, and the same command runs again inside the root-owned deploy helper.

## [0.1.7] - 2026-08-22

### Added
- **A live smoke for the startup preflight's two legs.** It runs the real preflight twice — once
  with an empty credential map (clock reads only, the honest baseline) and once with the real store
  — and fails if any venue lost its clock check *only* when the credential leg ran beside it. The
  two-run control is the point: the budget legitimately starves venues behind a slow clock read, and
  only a baseline separates that from the defect below.
- **`just app-paper`** — runs the GUI with no credentials in scope, so every venue stays paper by
  construction. `vike-app` opens the live gate on the mere presence of credentials and a live mount
  refuses to start without an operator risk budget, so on a populated box the obvious "look at a
  chart" run could not start at all. That refusal is unchanged; this is the other way out, made one
  command. An optional path argument captures a framebuffer PNG.
- **A data-plane-only credential seam in the trading daemon** — a credentialed feed without an armed
  exec, so a venue's market data can be live while its order path is not.
- **One placement rule for third-party artifacts, plus a ratchet that keeps it.**
- **The GUI contact sheet gets a judge**, and the GUI's CI-excluded shell gets a coverage ratchet.

### Fixed
- **The startup preflight's clock budget was being spent by the credential leg.** `run_preflight`
  interleaves the two legs per venue, and a credential probe is a blocking authed read on that
  venue's own 30 s agent — so ONE unreachable venue spent the whole 5 s clock budget and every venue
  behind it reported its clock "not read", blaming "an earlier venue's clock read" and sending an
  operator to inspect rows that were all fast and healthy. Measured: a geo-blocked probe sat ~20 s on
  a TCP connect and cost alpaca, aster and hyperliquid their clock checks while the clock leg itself
  had spent 3.2 s of its 5 s. `aster` mattered most — it runs against mainnet in practice and its
  auth binds the clock into the order path. Each credential probe is now measured and the deadline
  pushed out by its cost, so the budget bounds the clock leg and only the clock leg.
- **OKX options were never in the catalog.** The listing was fetched as a bare `instType=OPTION`,
  which the venue refuses with `400`/`50015`; the module already documented that failure and made
  the loop tolerant so spot/perp/futures survived, but never fixed the call — so `asset_classes()`
  advertised `AssetClass::Option` while the picker received zero options and a warning fired on
  every start. Now fetched once per underlying, paired as `uly` (not `instFamily`, which serves only
  two of the four families). Measured: okx 2004 → 5162 instruments.
- **Alpaca reported a transport failure as `HTTP 401`** — an unreachable token mint read as "bad
  keys", answering a network condition with a credential rotation. `AuthError` now separates a mint
  that answered and refused from one that was never reached.
- **Two warnings that fired forever on a healthy box.** The network row WARNed with a developer's
  TODO on every credential-free start; it is now `NotApplicable` with its reason when no venue would
  mount live, and still WARNs whenever a venue is actually being checked. The Windows Vulkan loader's
  two per-start warnings about absent validation-layer manifests are pinned below WARN on the console
  via a new `NOISY_TARGETS` family in `vike-log`, which obeys the same rules as the credential pins
  (an explicit mention wins; a pin may only ever narrow).
- **The catalog defaults REFUSE** — "cannot enumerate" stopped answering as "empty store".
- **A pinned research price window slid an hour instead of extending.**
- **`App::new` stopped demanding an unconstructible type**, and a stale `Cargo.lock` stops reaching CI.
- **An empty `[Unreleased]` now REFUSES the cut** — v0.1.5 and v0.1.6 both shipped on a note nobody
  read, because an advisory line in a scrolling release log is invisible.

### Changed
- **CI economics**: co-firing small feature lanes share one matrix leg, and the `~/.cargo` GitHub-cache
  round-trip is gone from setup — it restored what was already on disk.
- **47 rotted re-export spellings** retired across nine audited crates.

## [0.1.6] - 2026-08-19

Written after the fact (2026-08-22) from the merge history: this release and 0.1.5 were both cut
while an empty `[Unreleased]` was a legal no-op, so neither wrote a section at the time. It is a
refusal now — `just release` will not cut a version that says nothing about itself.

### Added
- **More venues join the trading daemon's live-wired set** — alpaca, ctrader, oanda, deribit and
  ig. Each mounts a credentialed market-data lane in `vike-tradehub` the way binance and friends
  already did; deribit, oanda and ig also gained their first LIVE market-data adapters (oanda's
  chunked-HTTP pricing stream, ig's Lightstreamer TLCP spoken raw over the shared transport stack —
  and ig serves no depth, which the adapter says outright rather than reporting an empty book).
- **The datahub authenticates.** It served history reads, backtest compute over client-supplied
  Rhai, and — in a backfill-serve build — writes into the store, to anything that could reach the
  socket. It now speaks the tradehub node handshake with scoped reads and writes; keys ABSENT
  serves exactly as before, so an existing single-user install is unchanged. The verdict:
  [`docs/decisions/0025-datahub-remote-posture.md`](docs/decisions/0025-datahub-remote-posture.md).
- **One backend address really is one address.** A daemon can advertise the datahub it fronts
  (a new `datahub_advertise_addr` key in `config.toml`), so a client that configured the tradehub
  reaches the history plane too instead of being configured twice. Unset changes nothing.
- **Settings hot-reload for the classified-safe subset.** The console and file log levels apply on
  a running daemon without a restart; every other key is answered `restart_required`, and
  `policy.toml` is structurally excluded — no future key can make a risk ceiling hot-appliable. The
  periodic summary also stopped over-reporting what is mounted.
- **Runtime mounts survive a restart.** A strategy mounted over the wire is remembered as daemon
  state and comes back after `systemctl restart` or a crash; only an explicit unmount forgets one.
  The journal is deliberately untouched — topology is session state, not order state.
- **A detached Windows daemon can be stopped cleanly**, and CI now compiles the daemon for Windows
  so that path stops resting on one developer's box.
- **Data-Manager coverage over the wire**: the Partial column follows the store in remote mode
  instead of being marked local-only.
- `vike-cli strategy-status` — ask a running node what it is running (human table, or the wire
  payload verbatim under `--json`). Asking used to mean compiling a throwaway probe.
- `just creds-audit` — find credential files by CONTENT across every box, so a stray copy of live
  keys is discoverable rather than remembered.
- Polymarket has a feeds-only build: the market-data plane compiles without the order signer.
- Under `external-quarantine`, a reconcile `MissingFill` that carries no `client_order_id` is HELD
  rather than folded — a foreign order's fill inside the lookback now needs an operator claim,
  while coid-linked fills keep folding.

### Fixed
Places where a capability gap was answering as data — all the same defect:
- A history store that cannot serve depth now REFUSES instead of returning an empty read, so "this
  store has no depth lane" stopped being indistinguishable from "there are no rows".
- Studio: a series scan that FAILED stopped rendering as an empty store, and `depth` being
  unreplayable stopped being a silent omission.
- oanda: the live tier REFUSES instead of vanishing, and a rejected exec stream stops looping in
  silence.
- fxcm: a login that FAILED reddens the smokes instead of being reported as a green test.
- The venue smokes honour `VIKE_SETTINGS_DIR` — they had been skipping in silence wherever the
  credential store was not directly above them.

Also:
- cTrader's OAuth token parser survives the venue sending either field spelling.
- Studio's ChatPane takes its provider keys instead of opening the credential store itself.

## [0.1.5] - 2026-08-19

### Added
- **Aster joins the daemon's live-wired venues** — capability only, and the MAINNET warning travels
  with it: no aster testnet credentials exist, so an armed aster lane reaches the real account.
- **Backends are managed from the GUI** — add, edit and delete backend records in place, with
  key-name hygiene on the credential names each record points at.
- **Settings are writable over the wire.** A `config`/`preferences`/`flags` key can be edited on a
  running daemon behind the control scope and a typed confirmation: the would-be file is validated
  with the same loader that will read it back, edited comment-preservingly (untouched lines stay
  untouched bytes) and landed atomically. `policy.toml` is special-cased — a risk ceiling is not a
  remote edit.
- A core-free direct-bar path: third-mode klines go venue-direct instead of through a trading core
  that has nothing to do with them.

### Fixed
- The Data-Manager grid follows the store — remote inventory arrives over the `HistStore` trait, so
  the grid describes the store you are connected to rather than a local handle.
- The `just cockpit` SOCKS tunnel host is a parameter defaulting to the CI box; it hardwired the parked
  Dublin route, which no longer answers.

### Changed
- No `pub use` shims on a code move: when a symbol changes homes every call site updates, and a
  re-export kept so old spellings compile is a second name that rots. Aster's leftovers are gone.
- [`docs/decisions/0025-datahub-remote-posture.md`](docs/decisions/0025-datahub-remote-posture.md)
  opened (proposed), with the bind guard both routes need.

## [0.1.4] - 2026-08-18

### Added
- Split-plane Phase-1 safety trio: the observe snapshot names its daemon (`WireNodeIdentity` —
  profile name, mounted strategy, paper-vs-live, build identity), a Windows console-ctrl graceful
  stop (Ctrl-C / window-close now run the same bounded teardown as the unix SIGTERM arm), and
  `LiveLock` — one live process per venue account, so two live traders can no longer silently
  rewrite each other's positions through reconcile.
- Runtime backend switching in the GUI: `BackendConn` makes the observe connection switchable (one
  active backend, total state clear on switch), a persisted backend registry owns the records, key
  resolution and the control arming gate, and both status surfaces name the connected daemon —
  LIVE/paper plus its identity.
- Rhai strategies mount LIVE: `[strategy] rhai = "<path>"` in a daemon profile compiles the script
  through the same engine builder the backtest arm uses and mounts it on the same paper/live cores,
  with the script's audit hash logged at mount. The verdict and its fence:
  [`docs/decisions/0024-rhai-strategies-live.md`](docs/decisions/0024-rhai-strategies-live.md).
- Studio as a thin client: Studio operates on the `HistStore` trait (remote-store capable) and its
  default build is DataFusion-free, so it can browse a served store with no local query engine.
- Backfill-on-demand on the datahub wire: `Request::Backfill` has the backend fetch a bounded kline
  range once, write it through to the store, and reply only after the served handle reads it back —
  capability-negotiated, so a client refuses locally against a server that does not advertise it.
- Strategy verbs on the tradehub node wire: the read-only `Request::StrategyStatus` and the
  control-scoped `WireCommand::UpdateParams` — inspect and retune a mounted strategy without
  restarting the daemon.

## [0.1.1] - 2026-08-18

### ⚠ BREAKING — settings unification (2026-08-05 … 2026-08-06)

**If you are upgrading an existing install, read [`docs/ops/upgrading.md`](docs/ops/upgrading.md)
first.** It is the full table: what moved, from where, to where, what today's binary does about the
leftover (silent / warns / refuses / migrates itself), and the exact command to fix each one —
including the `/opt/vike` in-place-upgrade case under an unchanged systemd unit.

The short version: every setting, credential and program-written file now lives in ONE
project-owned directory.

```text
<project>/settings/policy.toml  config.toml  preferences.toml  flags.toml
<project>/settings/secrets.env                 every API key, every venue
<project>/settings/state/…                     program-written files
```

- **Credentials moved** from `<repo>/.env` to `<project>/settings/secrets.env`. Absent credentials
  ARE the live gate, so a leftover `.env` used to mean every venue silently on paper with nothing in
  any log. An absent store with a `.env` beside it now **warns** on every credential read and in both
  `vike-cli secrets` subcommands, naming both paths — a finding, never a refusal, because `.env` is
  also a legitimate systemd `EnvironmentFile`. Nothing reads, prints or moves its contents.
  The interim `~/.vike/secrets.env` and `~/.vike/secrets.enc` slots existed for about a day and are
  gone; they cannot be warned about, because the home-directory resolution that found them was
  deleted with them.
- **Settings TOMLs moved** from `~/.vike/*.toml` to `<project>/settings/*.toml`. Silent — an absent
  file means the code default.
- **`<project>` is resolved at RUNTIME** by walking up for a project marker: a checkout's WORKSPACE
  ROOT (the outermost `Cargo.toml` declaring `[workspace]`), else a deployment's own `settings/`
  directory. `VIKE_SETTINGS_DIR` names it outright and skips the walk — set it on a deployment.
- **Refused at startup, never ignored** (`vike-app`, `vike-cli`, `vike-tradehub`):
  `VIKE_MAX_ORDER_NOTIONAL` and `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` (→ `policy.toml`'s
  `max_notional_per_order`; a ceiling any exported variable can raise is not a ceiling),
  `VIKE_SECRETS_PASSPHRASE` (nothing consumes it), a leftover `<project>/vike.toml`, and the removed
  keys `policy.toml`'s `max_total_exposure` / `[rate] max_utilization` and `preferences.toml`'s
  `rate_utilization`.
- **State files** (`workspace.json`, `alerts.json`, `layouts/`, `studio_workspace.json`, `pace.json`,
  the Telegram ledger, the cTrader token, the REPL history, the rolling trace log) moved under
  `<project>/settings/state/`. Four migrate themselves on first write; the rest are silent and cost
  a layout or a cache, never trading.

### Added
- Security/CI hardening (2026-07-08 audit): `cargo-deny` supply-chain gate (`deny.toml` +
  `deny.yml`), SHA-pinned GitHub Actions with least-privilege tokens + job timeouts, pinned
  toolchain (`rust-toolchain.toml` 1.96.0) + MSRV, workspace `unsafe_code = "forbid"` lint,
  `rustfmt` gate, `gitleaks` pre-commit hook, checksum-verified JForex/Temurin/Gradle
  provisioning, `.gitattributes` EOL policy, a `justfile` task runner, and a weekly
  Windows compile check.
- Dependabot (alerts + cooldowned version updates) and this release pipeline.

<!-- Add the next release's user-facing changes under [Unreleased]. `just release-prep` cuts the
     section under a version heading via scripts/changelog_release.sh — no hand edit needed.

     That script REFUSES to cut an empty [Unreleased], and the refusal lands before the commit
     and the tag. It used to print a note and carry on, which is how 0.1.5 and 0.1.6
     shipped with nothing here. A release that genuinely has nothing to say is still possible, but
     it has to be said out loud: ALLOW_EMPTY_CHANGELOG=1. -->
