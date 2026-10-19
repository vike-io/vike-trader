//! **The stdio summary line, and its MOUNTED-SET scoping** — `summary_line` (the one-line JSON
//! summary the daemon prints on its snapshot tick) and the pure half it calls,
//! [`mounted_book_equity`]. The scoping is public because it must be callable from an integration
//! test over a REAL multi-mount core (`tests/daemon/multi_mount_profile.rs`); a bin crate's private
//! function is reachable from nothing. `summary_line` itself lived in `crate::tradehub_cli` until
//! the `run` phase split moved it here, body unchanged, beside the function it calls.
//!
//! # The observation this exists for (I10 rehearsal, `docs/ops/i10-rehearsal-2026-08-19.md`)
//!
//! A two-venue live rehearsal's summary line printed `equity_book: 100000.0` while the operator
//! had seeded exactly two mounts at 10k each. Nothing was wrong with the number: `assemble_runtime`
//! seeds EVERY default-build venue engine with the primary mount's `seed_cash`, so ten paper
//! engines exist and `Portfolio::equity_book_total` sums all ten `Delta` blocks — the daemon's
//! whole book, honestly reported. What was wrong was the READER's inference, and the rehearsal
//! note says so: "an operator eyeballing the summary should know the number is not 'the two
//! mounts' seeds'".
//!
//! # Why a NEW field rather than a narrowed `equity_book`
//!
//! `equity_book` is one half of a documented PARTITION: `equity_book + equity_wallet` covers
//! exactly the sum `Portfolio::equity_total` takes, which is what makes the "a wallet and a book
//! are not addable" split in `summary_line`'s own doc checkable rather than merely asserted.
//! Scoping that field to the mounted subset would silently break the partition, silently change a
//! number an existing `jq`/alerting consumer already reads, and leave the un-mounted seed capital
//! reported by NOTHING.
//!
//! So this follows the precedent that fixed the ORIGINAL conflation — report both, conflate
//! neither, name the provenance in the key. `equity_wallet` gained `wallet_venues`; the daemon's
//! book half now gains `equity_book_mounted` + `mounted_venues`. Every existing key keeps its
//! meaning and its value, and the operator gets the figure they were reaching for, labelled.
//!
//! ⚠ The wire snapshot (`crate::publish`'s `WireSnapshot`) is deliberately untouched: it carries
//! the per-venue blocks AND the mount rows already, so any wire consumer can compute this scoping
//! itself. This is the stdio line's problem, and it is fixed on the stdio line.

use vike_exec::{CoreSnapshot, MountRowKind};

/// The mounted-set book equity plus the venues it was scoped to.
#[derive(Debug, Clone, PartialEq)]
pub struct MountedBookEquity {
    /// `Σ` the `BalanceMode::Delta` venue blocks whose venue is MOUNTED, py_sum in venue
    /// REGISTRATION order — the same fold law and the same order `Portfolio::equity_book_total`
    /// uses, so the scoped figure and the unscoped one can be compared without an ULP argument.
    pub total: f64,
    /// The distinct venues of the snapshot's `MountRowKind::Mount` rows, in MOUNT order — "what
    /// this daemon actually runs", which is the question the operator was asking. The trailing
    /// `MountRowKind::Residual` row (whose `venue` is the empty string by construction) is
    /// excluded: it is a ledger row, not a mount.
    pub venues: Vec<String>,
}

/// Scope the book-kept equity to the MOUNTED venues (module doc for why this is a new figure and
/// not a narrowed one).
///
/// Two edge shapes, both deliberate and both tested:
///
/// - **No mount rows** (a core that has mounted nothing, or a hand-built `CoreSnapshot::empty`):
///   the mounted set is EMPTY, so the total is `0.0` and `venues` is empty. A reader can tell the
///   difference between "scoped to nothing" and "nothing to scope" from the empty name list —
///   which is exactly why the names ship beside the number.
/// - **A mounted venue that has flipped `Authoritative`** (reconcile adopted its wallet):
///   it is NAMED in `venues` but contributes NOTHING to `total`, because it is no longer part of
///   the book half at all — its equity is in `equity_wallet`, under `wallet_venues`. That is the
///   same partition rule `equity_book` obeys, merely scoped; a mounted-venue figure that quietly
///   pulled an adopted wallet back into a "book" number would re-commit the exact conflation the
///   split exists to prevent.
pub fn mounted_book_equity(snap: &CoreSnapshot) -> MountedBookEquity {
    let mut venues: Vec<String> = Vec::new();
    for m in snap.mounts.iter().filter(|m| m.kind == MountRowKind::Mount) {
        if !venues.iter().any(|v| v == &m.venue) {
            venues.push(m.venue.clone());
        }
    }
    let total = vike_model::py_sum(
        crate::publish::current_venue_blocks(&snap.portfolio.venues)
            .filter(|v| v.balance_mode == vike_exec::BalanceMode::Delta)
            .filter(|v| venues.iter().any(|m| m == &v.venue))
            .map(|v| v.equity),
    );
    MountedBookEquity { total, venues }
}

/// `Σ` the equity of the CURRENT blocks ([`crate::publish::current_venue_blocks`]: the newest of each route key)
/// that are on `mode`, py_sum in venue registration order: `Portfolio::equity_book_total` /
/// `equity_wallet_total` over the engines that still exist.
///
/// ⚠ The core's own two halves sum EVERY engine it ever held, so an account that stopped and started again counts
/// its stopped engine's equity beside the new one's, and the venue wallet both adopted is added twice
/// (`crate::publish::current_equity_total` carries the drill that measured it). With no block superseded this is
/// bit-identical to the core's half (same filter, same order, same fold), so a node that never restarted an
/// account prints the numbers it always printed.
pub(crate) fn current_equity(snap: &CoreSnapshot, mode: vike_exec::BalanceMode) -> f64 {
    vike_model::py_sum(
        crate::publish::current_venue_blocks(&snap.portfolio.venues)
            .filter(|v| v.balance_mode == mode)
            .map(|v| v.equity),
    )
}

/// `Σ field` over the CURRENT blocks, py_sum in registration order: `Portfolio::realized_pnl_total` /
/// `fees_paid_total` / `funding_paid_total` over the engines that still exist, which is what the sums beside
/// [`current_equity`] must be (one `/equity` message printed current-only equity next to all-block sums). With no
/// block superseded the sequence is the core's, so the figure is bit-identical.
pub(crate) fn current_total(
    snap: &CoreSnapshot,
    field: impl Fn(&vike_exec::VenueBlock) -> f64,
) -> f64 {
    vike_model::py_sum(crate::publish::current_venue_blocks(&snap.portfolio.venues).map(field))
}

/// `Portfolio::position_count` over the CURRENT blocks (rows, as the core counts them).
pub(crate) fn current_position_count(snap: &CoreSnapshot) -> usize {
    crate::publish::current_venue_blocks(&snap.portfolio.venues).map(|v| v.positions.len()).sum()
}

/// `Portfolio::margin_used_total` over the CURRENT blocks. A plain left fold from `+0.0`, as the core's own sum
/// and the observe bridge's, so an empty list reads `0.00` and not the `-0.00` of an empty `Iterator::sum`.
/// (Only the Telegram `/equity` reads it, so a build without that feature does not compile it.)
#[cfg(feature = "telegram")]
pub(crate) fn current_margin_used(snap: &CoreSnapshot) -> f64 {
    crate::publish::current_venue_blocks(&snap.portfolio.venues)
        .fold(0.0, |acc, v| acc + v.margin_used)
}

/// `Portfolio::missing_prices_total` over the CURRENT blocks (Telegram `/equity` only, like the margin sum).
#[cfg(feature = "telegram")]
pub(crate) fn current_missing_prices(snap: &CoreSnapshot) -> u32 {
    crate::publish::current_venue_blocks(&snap.portfolio.venues).map(|v| v.missing_prices).sum()
}

/// The venues behind [`current_equity`]'s `Authoritative` half, in registration order, each current block once:
/// `Portfolio::wallet_venues` over the engines that still exist.
pub(crate) fn current_wallet_venues(snap: &CoreSnapshot) -> Vec<&str> {
    crate::publish::current_venue_blocks(&snap.portfolio.venues)
        .filter(|v| v.balance_mode == vike_exec::BalanceMode::Authoritative)
        .map(|v| v.venue.as_str())
        .collect()
}

/// The engines (route key, else venue id) whose OWN trading state is `state`, newest block per key.
///
/// ⚠ `snap.trading_state` is the PRIMARY engine's, so a venue-scoped link dead-man halt
/// (decision 0180) on another venue is invisible in it, and a dead primary venue reads `Halted` for
/// the whole node. These per-block lists are what the summary line adds beside it.
///
/// ⚠ `stopped` is the node's NOT RUNNING list (`PublisherHandle::not_running`; the core publishes
/// `CoreSnapshot::not_running` empty, so it cannot be read off `snap`). A stopped account that was not
/// started again keeps its last block (its position stays valued), and a `Halted` one would read as a halt
/// for ever: an engine named in `stopped` is skipped.
pub fn engines_in_state(
    snap: &CoreSnapshot,
    state: vike_model::TradingState,
    stopped: &[vike_exec::NotRunningAccount],
) -> Vec<String> {
    crate::publish::current_venue_blocks(&snap.portfolio.venues)
        .filter(|v| v.trading_state == state)
        .map(|v| if v.route_key.is_empty() { v.venue.clone() } else { v.route_key.clone() })
        .filter(|key| !stopped.iter().any(|n| n.route_key == *key))
        .collect()
}

/// One-line JSON summary of the live snapshot for the STDOUT protocol surface. Pure over the
/// snapshot — a LOSSY read, never a fold. `token` selects the mount symbol's net position.
///
/// ⚠ **Every number here is CROSS-VENUE — the whole account, not one engine.** That is a
/// correctness property, not a preference: four of the seven used to read the PRIMARY engine
/// (`vike_core::snapshot::build` binds `let acc = &engine.account`, and `vike_tradehub_core::wired_markets` lists
/// binance first, so the primary on a the CI box CEX node is the binance PAPER engine, which has traded
/// nothing) while `orders`/`working` iterated every engine and `equity` was
/// `Portfolio::equity_total`. Measured on the CI box: the bybit mount took ten maker fills and moved the
/// balance, `equity` tracked them to eight decimal places, and the same line printed
/// `fees: 0.0, realized_pnl: 0.0, net_pos: 0.0, positions: 0` — a line that reads like an account
/// summary while four of its fields describe a different venue than the two that move. So the
/// scope rule is: **a field on this line may not be narrower than the equity fields'.**
///
/// Deliberately still one FLAT object rather than gaining a per-mount breakdown: this is a
/// fixed-arity metric row a `jq`/alerting consumer reads positionally, and a nested
/// variable-length array would make every parser's shape depend on how many mounts happen to be
/// up. Per-mount truth is not lost — it rides in `CoreSnapshot::mounts`, which the alerting engine
/// on this same thread already reads.
///
/// ## ⚠ There is no `equity` key, on purpose — a wallet and a book are not addable
///
/// This line used to carry one `equity`, and it was `Portfolio::equity_total`: the `py_sum` of
/// every venue block's equity. A block's equity means two different things depending on its
/// `BalanceMode` (`ExecutionEngine::mode_equity`) — a `Delta` block is `seed + own cash flow +
/// realized + unrealized`, this daemon's own book-keeping, while an `Authoritative` block is
/// `venue wallet + unrealized`, the venue's number for the WHOLE ACCOUNT the credentials open.
/// Summing the two produces a figure that is neither.
///
/// Measured on the CI box 2026-08-17: `VIKE_RECONCILE=1` made bybit authoritative, and this line's
/// `equity` jumped from ~10000 to **62647.10600813** — the shared bybit demo account's 53647 USDT
/// `walletBalance` (settlements observed on AUCTIONUSDT/ONDOUSDT/ETHUSDT/WLDUSDT, none of them
/// traded by this daemon) plus nine paper mounts' 1000 seed each. Nothing had gained 52 thousand
/// dollars; the report had added a wallet it does not own to seed cash that does not exist.
///
/// So the field is SPLIT, and each half names its own provenance in its own key —
/// [`vike_exec::Portfolio::equity_book_total`] / [`vike_exec::Portfolio::equity_wallet_total`],
/// which partition exactly the sum `equity_total` takes:
///
/// - **`equity_book`** — the mount's own accounting, summed over the `Delta` blocks. Seeds this
///   daemon chose, moved by fills this daemon booked.
/// - **`equity_wallet`** — the venue-attested observation, summed over the `Authoritative` blocks.
///   Whole-account wallets, adopted verbatim from `ReconClient::fetch_balance` / a live
///   `AccountState` push.
/// - **`wallet_venues`** — comma-joined venue ids behind `equity_wallet` (empty string when
///   none), so the reader can see WHOSE wallet was quoted without opening a log. Still fixed
///   arity: always present, always a string.
///
/// ## ⚠ `equity_book` is the WHOLE book, not the mounted set — hence `equity_book_mounted`
///
/// `assemble_runtime` seeds EVERY default-build venue engine with the primary mount's `seed_cash`, so a
/// daemon running two mounts still carries ten `Delta` blocks and `equity_book` sums all ten. The
/// I10 live rehearsal (`docs/ops/i10-rehearsal-2026-08-19.md`) measured exactly that: two mounts
/// seeded at 10k each, `equity_book` printing **100000.0**, and the note observing that "an
/// operator eyeballing the summary should know the number is not 'the two mounts' seeds'".
///
/// The figure is CORRECT and stays. What it lacked was the scoped companion an operator is
/// actually reading for, so the same treatment the `equity` split got is applied again — report
/// both, conflate neither, name the provenance in the key:
///
/// - **`equity_book_mounted`** — `equity_book` restricted to the venues this daemon has MOUNTED
///   (`CoreSnapshot::mounts`' `MountRowKind::Mount` rows), same `py_sum` law, same registration
///   order. On the rehearsal's profile this reads 20000.0 beside `equity_book`'s 100000.0.
/// - **`mounted_venues`** — comma-joined venues it was scoped to (empty when the core has mounted
///   nothing, which is also when the figure is `0.0` — the name list is how the reader tells
///   "scoped to nothing" from "nothing to scope"). Fixed arity, like `wallet_venues`.
///
/// A mounted venue that has flipped `Authoritative` is NAMED but contributes nothing to the book
/// figure — its equity is in `equity_wallet`, exactly as `equity_book`'s own partition demands.
/// `crate::summary::mounted_book_equity` is the implementation and carries the full
/// argument for why this is a NEW pair of keys rather than a narrowed `equity_book` (the
/// partition property, and the rule against silently changing a number a consumer already reads).
///
/// **Renaming rather than keeping `equity` as the sum is the deliberate half of this change.** A
/// consumer keyed on `.equity` now reads `null` and breaks loudly instead of silently reading a
/// number that means nothing; the only two consumers in this repo are this file's own tests and
/// `tests/sigterm_stop.rs`'s `"kind":"summary"` substring match, and `vike_tradehub_client` reads
/// the SEPARATE TCP observe wire (`WireSnapshot`), not stdout. On a pure-paper node
/// `equity_wallet` is `0.0` with an empty `wallet_venues`; on a fully-live node `equity_book` is
/// `0.0`. Either way a reader can tell WHICH quantity is in front of them from the key alone.
///
/// ⚠ **The daemon cannot decide this for the operator.** Adopting the venue balance is exactly
/// right on a DEDICATED account and is why `CoreThread::reconcile_reports` does it; it is wrong
/// on a SHARED one, and no venue API field distinguishes the two — the same
/// `crates/bridges/bybit/src/recon_client.rs` scopes `fetch_position_status_reports`/
/// `fetch_fill_reports` to the mount's symbol and `fetch_balance` to the whole account.
/// Reporting both and conflating neither is the honest
/// answer available to code; a dedicated sub-account is the operator's lever.
///
/// `net_pos` is `Portfolio::net_position` — the SIGNED sum of `token`'s legs across every venue,
/// not the primary's leg. Cross-venue netting is the right meaning here because that is what
/// `equity` beside it already does, and because a hedged basis pair genuinely IS flat; ⚠ it nets
/// on the symbol STRING, so it only nets legs that are the same instrument — two venues spelling
/// one underlying differently stay two rows, and a shared spelling across venues with different
/// contract multipliers sums CONTRACTS, not coins.
///
/// ## ⚠ `trading_state` is the PRIMARY engine's — `halted_venues` / `reducing_venues` are the rest
///
/// A venue-scoped link dead-man halt (decision 0180) halts one venue's engines while the primary's
/// venue trades on (and a dead primary venue reads `Halted` for the whole node), so two more fixed
/// arity strings name the engines whose OWN block is `Halted` / `Reducing` ([`engines_in_state`]:
/// comma-joined route keys, empty when none; an account on `stopped`, the node's NOT RUNNING list, is
/// skipped, its last block being history). Additive: no existing key moved.
pub(crate) fn summary_line(
    snap: &CoreSnapshot,
    token: &str,
    stopped: &[vike_exec::NotRunningAccount],
) -> String {
    let working = snap.orders.iter().filter(|o| !o.status.is_terminal()).count();
    // `Portfolio::net_position` over the CURRENT blocks (the same plain fold), so it agrees with `positions` beside it.
    let net_pos: f64 = crate::publish::current_venue_blocks(&snap.portfolio.venues)
        .flat_map(|v| v.positions.iter())
        .filter(|p| p.symbol == token)
        .map(|p| p.size)
        .sum();
    // The MOUNTED-SET scoping (I10 rehearsal follow-up) — `crate::summary`, which owns
    // the argument for why this is a NEW pair of keys rather than a narrowed `equity_book`.
    let mounted = crate::summary::mounted_book_equity(snap);
    // `fault` is `Option<String>` — render it as an explicit JSON null / string.
    let fault = match &snap.fault {
        Some(f) => serde_json::Value::String(f.clone()),
        None => serde_json::Value::Null,
    };
    serde_json::json!({
        "kind": "summary",
        "seq": snap.seq,
        "trading_state": format!("{:?}", snap.trading_state),
        "orders": snap.orders.len(),
        "working": working,
        "positions": current_position_count(snap),
        "net_pos": net_pos,
        "realized_pnl": current_total(snap, |v| v.realized_pnl),
        "fees": current_total(snap, |v| v.fees_paid),
        "equity_book": current_equity(snap, vike_exec::BalanceMode::Delta),
        "equity_book_mounted": mounted.total,
        "mounted_venues": mounted.venues.join(","),
        "equity_wallet": current_equity(snap, vike_exec::BalanceMode::Authoritative),
        "wallet_venues": current_wallet_venues(snap).join(","),
        "halted_venues": engines_in_state(snap, vike_model::TradingState::Halted, stopped).join(","),
        "reducing_venues":
            engines_in_state(snap, vike_model::TradingState::Reducing, stopped).join(","),
        "fault": fault
    })
    .to_string()
}
