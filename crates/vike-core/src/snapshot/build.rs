//! Building a snapshot from the engines: the account-set digest and `build`.
//!
//! The snapshot TYPES are vike-execution's read model (`vike_execution::CoreSnapshot` and its rows); this
//! file is the one producer that fills them from the core's engines, as free functions because an
//! inherent `impl` cannot live outside the crate that owns the type.

use vike_execution::{
    CoreSnapshot, Engine, ExecutionClient, Interned, MountView, OrderHeldView, OrderView,
    Portfolio, PositionView, PriceBoardConfig, PriceBoardEquity, ReconcileBlock, VenueBlock,
};

/// [`CoreSnapshot::accounts_epoch`]'s derivation: an FNV-1a digest over this process's route
/// keys, SORTED so the same set of accounts answers the same value whatever order the mount
/// fan-out registered them in.
///
/// Never `0` for a non-empty set — a digest that landed there is mapped to `1`, so `0` keeps
/// meaning exactly "no fold yet" ([`CoreSnapshot::empty`]).
///
/// Allocation-free up to `ACCOUNTS_EPOCH_STACK_KEYS` keys: they are sorted in a stack array,
/// and only a longer set is collected into a heap `Vec` first. Both paths hand the same keys to
/// the same in-place slice sort and the same fold, so which one ran cannot show in the value —
/// and `crates/vike-core/src/snapshot/account_fields_tests.rs`'s `reference` (the heap-only
/// derivation this replaced, kept verbatim) is what the tests beside it hold it equal to.
#[must_use]
pub fn accounts_epoch_of<'a>(route_keys: impl IntoIterator<Item = &'a str>) -> u64 {
    let mut route_keys = route_keys.into_iter();
    let mut stack: [&str; ACCOUNTS_EPOCH_STACK_KEYS] = [""; ACCOUNTS_EPOCH_STACK_KEYS];
    let mut len = 0;
    while len < ACCOUNTS_EPOCH_STACK_KEYS {
        match route_keys.next() {
            Some(key) => {
                stack[len] = key;
                len += 1;
            }
            None => return epoch_digest(&mut stack[..len]),
        }
    }
    // The stack is full and the iterator has not yet said `None`. One more key decides the
    // path; `next` is never called again after a `None`, so either path digests exactly the
    // keys a plain `collect` would have taken.
    match route_keys.next() {
        None => epoch_digest(&mut stack),
        Some(key) => {
            let mut keys: Vec<&str> =
                stack.into_iter().chain(std::iter::once(key)).chain(route_keys).collect();
            epoch_digest(&mut keys)
        }
    }
}

/// Build a [`CoreSnapshot`] from the engines — the one producer of a published snapshot. Called
/// by the core loop's publish, which is coalesced to
/// `snapshot_interval` while the core is busy but ALSO runs on every idle transition — so on a
/// sporadic feed it runs once per event, and its cost sits inside the gated core hop
/// (`crates/vike-core/tests/runtime_latency.rs`'s `run_snapshot_build` measures it alone).
// runtime-internal ctor: the counters are overlaid by the loop's publish
#[allow(clippy::too_many_arguments, clippy::allow_attributes)]
pub fn build<C: ExecutionClient>(
    seq: u64,
    engine: &Engine<C>,
    extra_engines: &[(f64, Engine<C>)],
    seed_cash: f64,
    price_config: PriceBoardConfig,
    // The ONE maintenance rate (the scope-parameterized liquidation law's rate source):
    // the operator's `MarginCallConfig::maintenance_margin_requirement` when the watchdog is configured,
    // else that config's default — resolved by the caller (see `publish`). Feeds the
    // per-position liq-price badge; the retired `initial_margin * 0.5` hardcode is dead.
    mm_rate: f64,
    // The ring arrives RENDERED (`Arc<str>` lines): the fold formats each note at push and this
    // build clones one refcount per entry. Perf audit finding #2 (#887) had deferred the render
    // to this publish on the premise that it is coalesced (>=16ms); it is not — it also runs on
    // every idle transition, i.e. per event on a sporadic feed — so #896 moved the render back
    // (`crates/vike-core/src/runtime/journaling.rs`'s `note_event` carries the measurement).
    recent_events: &std::collections::VecDeque<std::sync::Arc<str>>,
    bars: &indexmap::IndexMap<vike_execution::SeriesKey, vike_execution::BarSeries>,
    // An `Arc` list, taken by value: the caller (`CoreThread::published_mount_rows`) either
    // reuses the list the previous publish carried (a refcount bump) or builds one fresh, and
    // `build` only stores it. It used to take a slice and `to_vec()` it, which cloned every row
    // and its three `String`s a second time on every publish.
    mounts: std::sync::Arc<[MountView]>,
    fault: &Option<String>,
    // Task 17: the GUI-facing reconcile block (held alerts + last pass ts), built by the
    // caller from its held-alert store (`CoreThread::reconcile_block`) — an owned value (not a
    // `&`) since the caller already builds a fresh one per publish.
    recon: ReconcileBlock,
    // Live-runtime OTO/OCO: pending bracket exits held off the venue, built by the caller from
    // `CoreThread::held_orders` (an empty `Vec` — no allocation — on every non-bracket run).
    // By value for the same reason as `mounts`: moved into the snapshot, not copied again.
    held_exits: Vec<OrderHeldView>,
    // Which registry orders this publish carries (`super::OrderWindows`): the open ones and those that ended within
    // the hour. The core keeps one across publishes, so the walk never reaches the orders that ended long ago.
    windows: &mut super::OrderWindows,
) -> CoreSnapshot {
    let account = &engine.account;
    // Publish path: coalesced (≥16ms) while the core is busy, but ALSO run on every idle
    // transition, so on a sporadic feed this fn runs once per event, inside the gated hop.
    // `price_config` is caller-tunable (`CoreConfig::price_config`, threaded through from PR-3's
    // equity sampler, which resolves prices through the SAME cfg); permissive default
    // (`PriceBoardConfig::default()`: no freshness windows, mark enabled) unless the caller opts in.
    let config = price_config;
    // Compute the primary block first so the top-level scalar `equity` and `positions`
    // can be reused from it verbatim (they are documented mirrors of the primary venue —
    // see the struct docs above `equity`/`positions`).
    let primary = venue_block(engine, seed_cash, &config, mm_rate);
    let top_equity = primary.equity;
    // The primary block's money, as published (a coin ledger's in the account's currency, decision 0258).
    let (top_balance, top_realized, top_fees, top_funding) =
        (primary.balance, primary.realized_pnl, primary.fees_paid, primary.funding_paid);
    // The currency GROUP the cross-engine totals below fold: the first engine's. A total adds one
    // currency's figures only, so a block of an engine in another currency is left out of it
    // (`Portfolio::ledger_totals` has a row per group). With every engine in one group (every node
    // today) the totals are the plain folds they were. The publish site restates them for the first LIVE
    // engine's group when the first engine is a tombstone in another one (`CoreThread::state_groups`).
    let primary_currency = primary.ledger_currency;
    let top_positions = primary.positions.clone();
    let mut venues = Vec::with_capacity(1 + extra_engines.len());
    venues.push(primary);
    // Whether any engine is in another currency group than the first's, learned in the loop that
    // already builds the blocks (a pointer compare each). A node of one currency (every node today) never
    // takes the grouped folds below.
    let mut mixed = false;
    for (seed, e) in extra_engines {
        let block = venue_block(e, *seed, &config, mm_rate);
        mixed |= block.ledger_currency != primary_currency;
        venues.push(block);
    }
    let missing_prices_total: u32 = venues.iter().map(|v| v.missing_prices).sum();
    let (equity_total, capital_base, margin_used_total) = if mixed {
        grouped_totals(&venues, seed_cash, extra_engines.iter().map(|(s, _)| *s), primary_currency)
    } else {
        // The three folds a node of ONE currency always ran, verbatim: the single-group path pays
        // nothing for the grouping but the `mixed` test above.
        //
        // CrossVenueDriver::aggregate_equity law: compensated_sum in venue registration order
        let equity_total = vike_math::summation::compensated_sum(venues.iter().map(|v| v.equity));
        // The drawdown latch's frozen capital base — SAME fold law, SAME order (primary first,
        // then extras in registration order) as `CoreThread::sweep_drawdown_latch` computes it
        // from `seed_of`, so the published `Portfolio::drawdown_curve` is bit-identical to the
        // number the latch acted on. See `Portfolio::capital_base`.
        let capital_base = vike_math::summation::compensated_sum(
            std::iter::once(seed_cash).chain(extra_engines.iter().map(|(s, _)| *s)),
        );
        let margin_used_total: f64 = venues.iter().map(|v| v.margin_used).sum();
        (equity_total, capital_base, margin_used_total)
    };
    let orders = order_views(engine, extra_engines, &venues, windows);
    CoreSnapshot {
        sequence: seq,
        venue: engine.venue.clone(),
        symbol: engine.symbol.clone(),
        trading_state: engine.trading_state,
        balance: top_balance,
        balance_mode: account.balance_mode,
        portfolio: Portfolio {
            equity: top_equity,
            equity_total,
            realized_pnl: top_realized,
            fees_paid: top_fees,
            funding_paid: top_funding,
            margin_used_total,
            capital_base,
            missing_prices_total,
            balances_by_asset: account
                .balances_by_asset
                .iter()
                .map(|(a, q)| (a.clone(), *q))
                .collect(),
            venues,
            // The core's own list keeps the primary engine's block first, stopped or not.
            primary_currency_stated: None,
            // Filled at the publish site, and only for a node with several currency groups.
            group_curves: Vec::new(),
        },
        positions: top_positions,
        // The interned names copied, not formatted into `String`s.
        marks: account
            .marks_iter()
            .map(|((v, s), px)| (Interned::from(*v), Interned::from(*s), *px))
            .collect(),
        orders,
        held_exits,
        // Overlaid at the publish site (`runtime::publish`) from the core-held stops, like
        // `reconcile_coin_deltas` below: an empty `Vec` here allocates nothing.
        core_held_stops: Vec::new(),
        emulated_orders: Vec::new(),
        bars: bars.clone(), // Arc clones for the closed series — cheap by design
        mounts,
        recent_events: recent_events.iter().cloned().collect(),
        // Overlaid at the publish site (`runtime::publish`) once the core denied an order.
        denied_orders: None,
        fault: fault.clone(),
        // Overlaid at the publish site (`runtime::publish`) once the journal fence halted the core.
        journal_fence_halt: false,
        // Overlaid at the publish site (`runtime::publish`'s `collect_counters`), like
        // `reconcile_coin_deltas` below: zero on the test/direct-build path.
        counters: vike_execution::CoreCounters::default(),
        reconcile: recon,
        // The reconcile-path coin deltas are a side map the caller overwrites at the publish
        // site (`runtime::publish`) from `CoreThread::reconcile_coin_deltas` — kept out of this
        // ctor's arg list (already `too_many_arguments`) since it is retained fold-thread state,
        // not derived from the engine here. Empty on the test/direct-build path.
        reconcile_coin_deltas: indexmap::IndexMap::new(),
        // The account-set digest (§6.3), over the SAME engines whose blocks are published
        // above. Recomputed each build rather than cached because the set cannot change within
        // a process — so the value is constant per run. The cost is NOT off the gated path: a
        // sort of one `&str` per engine in a stack array (a heap `Vec` only past
        // `ACCOUNTS_EPOCH_STACK_KEYS` engines) + a few dozen bytes of FNV on EVERY publish, and
        // a publish also runs on every idle transition, i.e. per event on a sporadic feed.
        accounts_epoch: accounts_epoch_of(
            std::iter::once(engine.route_key.as_str())
                .chain(extra_engines.iter().map(|(_, e)| e.route_key.as_str())),
        ),
        // EMPTY from the core, on purpose (decision 0156): the accounts that do not run are fixed
        // at assembly, the core keeps them for its refusals (`CoreThread::not_running`), and the
        // node's publisher appends them to the wire frame off this hop. An empty `Vec` allocates
        // nothing — no work added to the gated publish.
        not_running: Vec::new(),
    }
}

/// The three cross-engine totals of a node whose engines are NOT all in one currency (decision 0183,
/// item 13): the folds of `build`, over the blocks of the `primary_currency` group only, in the same order and
/// with the same laws. `seeds` is the extras' seeds in registration order; the primary's is `seed_cash`,
/// and they are zipped with the blocks (one per engine, same order). Out of line and cold: only a node with
/// several currencies calls it.
#[inline(never)]
#[cold]
fn grouped_totals(
    venues: &[VenueBlock],
    seed_cash: f64,
    seeds: impl Iterator<Item = f64>,
    primary_currency: Interned,
) -> (f64, f64, f64) {
    let in_group = |v: &&VenueBlock| v.ledger_currency == primary_currency;
    let equity_total =
        vike_math::summation::compensated_sum(venues.iter().filter(in_group).map(|v| v.equity));
    let capital_base = vike_math::summation::compensated_sum(
        std::iter::once(seed_cash)
            .chain(seeds)
            .zip(venues.iter())
            .filter(|(_, v)| v.ledger_currency == primary_currency)
            .map(|(s, _)| s),
    );
    let margin_used_total: f64 = venues.iter().filter(in_group).map(|v| v.margin_used).sum();
    (equity_total, capital_base, margin_used_total)
}

/// One engine's `VenueBlock`: its margin figures, its position views and the block itself, all
/// from ONE `resolve_equity` call. Its venue is always `e.venue`, so that is not a parameter.
fn venue_block<C: ExecutionClient>(
    e: &Engine<C>,
    seed: f64,
    config: &PriceBoardConfig,
    mm_rate: f64,
) -> VenueBlock {
    // One resolve per engine, parallel to `account.positions` insertion order. Builds a
    // whole VenueBlock from a `PriceBoardEquity` so `equity`/`unrealized`/`missing_prices`
    // and each PositionView's `unrealized`/`mark_source` all come from the SAME resolve
    // call — never a second, possibly-stale board read.
    let re: PriceBoardEquity = e.resolve_equity(seed, config);
    let margin = margin_figures(e, seed, config, &re);
    let positions = position_views(e, config, &re, &margin, mm_rate);
    let MarginFigures { margin_used, free_buying_power: free_bp, margin_ratio, .. } = margin;
    // The conversion rates behind a foreign-quoted symbol's valuation and their age at THIS publish.
    // An account with no such symbol answers `("", [])` after one `Option` test: no allocation, no
    // clock read (the engine's own `now_ms` is the clock the staleness verdict is judged on).
    let (quote_rates_in, quote_rates) = e.account.quote_rate_table(e.now_ms);
    // What the foreign-currency ledger left out or booked at a stale rate (`(0, 0)` after one `Option`
    // test for an account with no foreign-quoted symbol).
    let (fees_unconverted, fees_at_stale_rate) = e.account.fx_fee_counts();
    // A COIN LEDGER's money is published in the account's currency, times the coin's price (decision 0258), so
    // every screen and sum reads dollars as it always did; any other engine's figure as it is (one `Option` test).
    let usd = e.account_money(config);
    VenueBlock {
        venue: e.venue.to_string(),
        // The ROUTING half of the canonical/routing split, published so a consumer can tell
        // two accounts of one exchange apart — `venue` above cannot, and on a two-account
        // node it is the same string in both blocks. The label is the INVERSE of the same
        // renderer the mount stamped the engine with, so the two cannot drift.
        // ⚠ `AccountLabel::Default` renders as an ABSENT field, never as `Some(Default)`:
        // that is the convention `AccountLabel`'s own doc states for every carrier of the
        // type (*"an account-less mount emits no key and a pre-change file still parses"*),
        // and it is what keeps a single-account node's published shape indistinguishable
        // from the one it published before this field existed.
        account: vike_model::accounts::account_keys::label_of_route_key(&e.venue, &e.route_key)
            .filter(|l| !l.is_default()),
        route_key: e.route_key.clone(),
        // What the engine trades and the tier of the account behind it (the Trade window design,
        // §4.3; decision 0156).
        // One small `String` clone, the same kind of work as `route_key` above; the extra symbols
        // are shared with the engine's last list (an `Arc` bump), and an empty list holds no
        // allocation, which is every venue mount.
        symbol: e.symbol.clone(),
        extra_symbols: e.published_extra_symbols(),
        tier: Some(e.tier),
        balance: usd(e.account.balance),
        realized_pnl: usd(e.account.realized_pnl),
        fees_paid: usd(e.account.fees_paid),
        funding_paid: usd(e.account.funding_paid),
        balance_mode: e.account.balance_mode,
        equity: re.equity,
        unrealized: re.unrealized_total,
        missing_prices: re.missing,
        margin_used,
        free_buying_power: free_bp,
        margin_ratio,
        fee_schedule: e.fee_schedule,
        trading_state: e.trading_state,
        // Pointer-copy of the engine's immutable contract-value grid — one `Arc` refcount
        // bump per venue per publish, no allocation (same cost profile as `bars`).
        contract_values: e.account.contract_value_grid(),
        contract_value_default: e.account.contract_value_default(),
        // The capability, read off the flag the mount set from the node door's table: a bool copy.
        admits_symbols: e.gate.limits.deny_unadmitted_symbols,
        quote_rates,
        quote_rates_in,
        // The currency group of every figure above: EMPTY for the dollar group. A copy of the key the
        // engine cached when it was told its currency, so the publish interns nothing.
        ledger_currency: e.ledger_currency_key(),
        // Whether that currency is only the owner's assumed USD (presentation): a bool copy.
        currency_assumed: e.account_currency_assumed(),
        colliding_fills_refused: e.account.colliding_fills_refused,
        fees_unconverted,
        fees_at_stale_rate,
        // The account code a client stamps into an id for this account (decision 0270), once the root told
        // the engine one: a copy.
        account_code: (e.account_code
            != vike_model::orders::client_id::OrderClientIdAccountCode::NONE)
            .then_some(e.account_code),
        // ...and the shape a CLIENT mints for it, said once the root told the engine either input of its own shape:
        // the venue's fit for the broker code with no instance tag (a client never carries one). `None` (an engine
        // nobody told, every test core) is the full shape, and the block is the one it always was.
        order_client_id_shape: (e.account_code
            != vike_model::orders::client_id::OrderClientIdAccountCode::NONE
            || e.broker_code_len > 0)
            .then(|| vike_model::orders::client_id::OrderClientIdShape {
                tag: false,
                ..vike_venues::venue_order_client_id_budget::order_client_id_shape_for(
                    &e.venue,
                    usize::from(e.broker_code_len),
                    0,
                )
            }),
        positions,
    }
}

/// The margin fields and the liq-badge pool inputs of one engine's block, priced under the
/// SAME `config` as that block's `resolve_equity`.
fn margin_figures<C: ExecutionClient>(
    e: &Engine<C>,
    seed: f64,
    config: &PriceBoardConfig,
    re: &PriceBoardEquity,
) -> MarginFigures {
    // Margin fields are computed ONLY when the gate is armed — this keeps the snapshot
    // publish (which is on the measured core-hop) zero-cost on the default/off path (the
    // latency gate runs with the gate off), and byte-identical to the pre-margin builder.
    let margin_on = e.gate.limits.initial_margin_requirement.is_some()
        || !e.gate.limits.initial_margin_by_symbol.is_empty();
    let mut margin_used = 0.0;
    let mut fallback = 0.0;
    if margin_on {
        // THE gate's position fold (`Engine::cross_margin_in_use`), so the published
        // number counts every position the gate counts. (These snapshot fields are GUI-only:
        // the auto-liquidation watchdog computes its own margin from `maintenance_margin_requirement` via
        // `check_margin_call_priced` and never reads this number.) RATE POLICY: each position
        // uses its own initial margin, falling back — for a no-override position — to the account's MOST
        // CONSERVATIVE armed initial-margin rate (the max of `initial_margin_by_symbol` and any global
        // `initial_margin_requirement`): there is no order symbol here, and the max prices such a position
        // no lower than any single gate check would, so the published free_buying_power is never rosier
        // than the gate's admission basis. When `initial_margin_requirement` IS set (global default),
        // `initial_margin_for` never returns None and this fallback is never consulted.
        //
        // KNOWN GUI ARTIFACT (cosmetic, deliberate): because the fallback is the max over
        // ALL armed rates, a no-override position's displayed margin shifts when an
        // UNRELATED symbol's rate is armed/disarmed via `Command::SetMargin`. It settles
        // once every held symbol has its own override or a global `initial_margin_requirement` is set.
        // Never dangerous (overstates, never understates, GUI-only).
        fallback = e
            .gate
            .limits
            .initial_margin_by_symbol
            .values()
            .copied()
            .chain(e.gate.limits.initial_margin_requirement)
            .fold(0.0_f64, f64::max);
        // POOL POLICY (the liquidation law's partition, mirroring the gate and
        // `check_margin_call_priced`): only CROSS positions price into this shared number.
        // An Isolated position is backed by its own walled-off wallet (surfaced
        // per-position via `PositionView::isolated_margin`) and a Cash position is
        // fully funded — folding either into `margin_used` would double-charge a
        // mixed account's published free_buying_power against equity that never backs them.
        // All-cross accounts (every position today): the filter is a no-op →
        // byte-identical to the pre-filter number (the dedup-A1 pins still hold).
        // PRICE BASIS (risk-lane completion): the fold is resolver-priced under the SAME
        // `config` as `resolve_equity` above,
        // so the published `margin_ratio`'s numerator and denominator — and the free_buying_power
        // crossing — share one price basis; a stale `Account.marks` scalar can no longer
        // overstate margin against a fresh-quote equity. Rate/pool policy unchanged.
        margin_used = e.cross_margin_in_use(config, fallback);
    }
    // gate off ⇒ free_buying_power == equity floored at 0.0 (nothing locked; a negative equity publishes 0),
    // ratio 0. Gate on ⇒ what the gate judges (`Engine::gate_margin_used` on the capped equity):
    // `margin_used` stays the position margin and the live un-filled orders count here alone. The walk is
    // per publish, armed only, over the live orders alone (never the ended ones the registry keeps); with no ceiling the cap returns the
    // figure verbatim, so a book with nothing resting publishes what it always did.
    let free_bp = if margin_on {
        vike_model::free_buying_power(
            e.cap_sizing_equity(e.cap_at_coin_seed(re.equity, seed, e.coin_rate(config))),
            margin_used + e.live_order_margin(config, fallback, ""),
            0.0,
            e.gate.limits.required_free_buying_power_percent,
        )
    } else {
        re.equity.max(0.0)
    };
    let margin_ratio = if margin_on && re.equity > 0.0 { margin_used / re.equity } else { 0.0 };
    // Liq-badge pool inputs (GUI-only; on the publish path, which runs per event on an idle
    // core, so they are computed only when the gate is armed) — the scope-parameterized
    // law's cross pool over THIS venue's account: total marked CROSS notional, and the
    // pool equity = resolved equity minus every isolated position's walled-off pool
    // (wallet + its own uPnL). With no isolated positions (today's default) the pool
    // equity IS `re.equity`.
    let mut cross_notional_total = 0.0;
    let mut pool_equity = re.equity;
    if margin_on {
        for (((v, s, _ps), p), rp) in e.account.positions.iter().zip(re.per_position.iter()) {
            if p.size == 0.0 {
                continue;
            }
            if p.margin_mode.is_isolated() {
                pool_equity -= p.isolated_margin.unwrap_or(0.0) + rp.unrealized;
            } else if p.margin_mode.is_cross()
                && let Some(mark) = e.resolved_position_price(v, s, p.size, config)
            {
                cross_notional_total += e.account.contract_value_of(s).gross_notional(p.size, mark);
            }
        }
    }
    MarginFigures {
        margin_on,
        margin_used,
        free_buying_power: free_bp,
        margin_ratio,
        cross_notional_total,
        pool_equity,
    }
}

/// One engine's `PositionView`s, parallel to `account.positions` and to `re.per_position`.
fn position_views<C: ExecutionClient>(
    e: &Engine<C>,
    config: &PriceBoardConfig,
    re: &PriceBoardEquity,
    margin: &MarginFigures,
    mm_rate: f64,
) -> Vec<PositionView> {
    let MarginFigures { margin_on, cross_notional_total, pool_equity, .. } = *margin;
    e.account
        .positions
        .iter()
        .zip(re.per_position.iter())
        .map(|(((v, s, ps), p), rp)| {
            // Per-symbol leverage + the liq-price badge, from the ONE liquidation law
            // (`vike_model::money::liquidation`) at the ONE maintenance rate (`mm_rate` — the
            // `initial_margin * 0.5` hardcode is dead). 0.0 when the gate is off or the leg is flat;
            // skips the per-position `initial_margin_for` lookup entirely on the off path. By mode:
            // - Isolated → `ContractValue::isolated_liquidation_price`, the closed form at the REAL
            //   maint rate (its own wallet is its pool, so a per-position price is exact in shape); it
            //   is the LINEAR form, so an INVERSE contract withholds (0.0), as in the cross arm below;
            // - Cross → `ContractValue::cross_liquidation_price`: the mark at which the SHARED
            //   pool first hits the law's line, others frozen (a per-position cross liq
            //   price is inherently an estimate — the venue-UI shape, advisory only;
            //   the watchdog acts on the account-level law, never on this number); an INVERSE
            //   contract's notional does not contain the price (decision 0170), so the badge is
            //   withheld (0.0, the unmarked answer) rather than shown wrong;
            // - Cash → no badge (structurally cannot breach).
            let (leverage, liq_price) = match margin_on.then(|| e.gate.limits.initial_margin_for(s))
            {
                Some(Some(initial_margin)) if initial_margin > 0.0 && p.size != 0.0 => {
                    let liquidation = if p.margin_mode.is_isolated() {
                        e.account.contract_value_of(s).isolated_liquidation_price(
                            p.average_price,
                            p.size.signum() as i32,
                            initial_margin,
                            mm_rate,
                        )
                    } else if p.margin_mode.is_cross() {
                        // The price the position can be closed at, as its equity reads it (owner,
                        // 2026-11-15: a long at the bid, a short at the ask), never the slot's mid.
                        match e.resolved_position_price(v, s, p.size, config) {
                            Some(mark) => {
                                let value = e.account.contract_value_of(s);
                                let own = value.gross_notional(p.size, mark);
                                value.cross_liquidation_price(
                                    pool_equity,
                                    p.size,
                                    mark,
                                    cross_notional_total - own,
                                    mm_rate,
                                )
                            }
                            None => 0.0, // unmarked → the pool can't be judged here
                        }
                    } else {
                        0.0 // Cash: never liquidates
                    };
                    (1.0 / initial_margin, liquidation)
                }
                _ => (0.0, 0.0),
            };
            PositionView {
                // The engine's own interned names, copied: no allocation per row.
                venue: Interned::from(*v),
                symbol: Interned::from(*s),
                position_side: *ps,
                size: p.size,
                average_price: p.average_price,
                unrealized: rp.unrealized,
                mark_source: rp.mark_source,
                leverage,
                liquidation_price: liq_price,
                // Carried straight from the account's PositionEntry (Cross/None by
                // default → byte-identical to the pre-field view).
                margin_mode: p.margin_mode,
                isolated_margin: p.isolated_margin,
            }
        })
        .collect()
}

/// The registry orders `windows` publishes of every engine (the open ones and those that ended within
/// `super::ENDED_ORDER_PUBLISH_MS`), primary first and in registry order, each as an `OrderView` that reads its
/// account label from the block `venues` already holds for its engine. A row whose order did not move is the `Arc`
/// the last publish carried (`super::OrderWindows::publish`).
fn order_views<C: ExecutionClient>(
    engine: &Engine<C>,
    extra_engines: &[(f64, Engine<C>)],
    venues: &[VenueBlock],
    windows: &mut super::OrderWindows,
) -> Vec<std::sync::Arc<OrderView>> {
    let mut orders = Vec::new();
    // One (engine, block) pair per engine: `venues` was filled above from exactly these engines
    // in exactly this order, primary first. The block already carries the label
    // `label_of_route_key` derived from the engine's route key, so an order reads THAT instead of
    // parsing the key a second time per engine per publish — and `OrderView::account` cannot
    // disagree with `VenueBlock::account` about which account it is.
    //
    // The zip is only right while `venues` holds exactly one block per engine, so the invariant
    // is checked here. `debug_assert_eq!` is compiled out of release builds: nothing is logged
    // and nothing allocates on this per-event path (the formatting runs only on a failure).
    debug_assert_eq!(
        venues.len(),
        1 + extra_engines.len(),
        "one venue block per engine, primary first: the order loop below zips them"
    );
    for (at, (e, block)) in
        std::iter::once(engine).chain(extra_engines.iter().map(|(_, e)| e)).zip(venues).enumerate()
    {
        windows.publish(at, e, &block.account, &mut orders);
    }
    orders
}

/// The registry order at `index` of `e` as published, its account `account` (its engine's block's), built anew;
/// `None` past the end. Only [`super::OrderWindows::publish`] calls it, for an order written since its last view.
pub(super) fn order_view<C: ExecutionClient>(
    e: &Engine<C>,
    account: &Option<vike_model::accounts::account_keys::AccountLabel>,
    index: usize,
) -> Option<OrderView> {
    let (order_client_id, mo) = e.registry.get_index(index)?;
    let rejection = order_rejection(e, order_client_id, mo.status);
    Some(OrderView {
        order_client_id: order_client_id.clone(),
        venue: e.venue.clone(),
        // `None` for the default account (every node today), which clones to nothing; a
        // labelled account pays one small `String` per order.
        account: account.clone(),
        symbol: mo.request.symbol.clone(),
        side: mo.request.side,
        qty: mo.request.qty,
        order_type: mo.request.order_type.clone(),
        price: mo.request.price,
        trigger_price: mo.request.trigger_price,
        status: mo.status,
        venue_order_id: mo.venue_order_id.clone(),
        filled_qty: mo.filled_qty,
        average_fill_price: mo.average_fill_price,
        // One `Copy` of an `Option` per order (decision 0140), no allocation.
        data_delay: mo.request.data_delay,
        reason: rejection.map(|(reason, _)| reason.to_owned()),
        refusal_id: rejection.and_then(|(_, refusal_id)| refusal_id).map(str::to_owned),
        pending_price: order_pending_price(e, order_client_id, mo.status),
        // Two `Copy`s of an `Option` per order, no allocation.
        trail: mo.request.trail,
        limit_offset: mo.request.limit_offset,
        // The attributes wave's four: `Copy`s of an `Option` each (no `Box`, no `String`), no allocation.
        attributes: mo.request.attributes,
        trail_percent: mo.request.trail_percent,
        adjust: mo.request.adjust,
        release: mo.request.release,
        // A `Copy` of an `Option` too: zero is a HIDDEN order.
        display_quantity: mo.request.display_quantity,
    })
}

/// `OrderView::reason` and `OrderView::refusal_id` of an order in `status`, as `(reason, refusal_id)`: read off the
/// engine's side table in one lookup, so they move without a registry write (a newer refusal evicts them). Cold: only a
/// refused order (or a live one whose last modify was refused) has an entry; the lookup is a no-op while the table is
/// empty.
pub(super) fn order_rejection<'e, C: ExecutionClient>(
    e: &'e Engine<C>,
    order_client_id: &str,
    status: vike_model::OrderStatus,
) -> Option<(&'e str, Option<&'e str>)> {
    (matches!(status, vike_model::OrderStatus::Rejected | vike_model::OrderStatus::Denied)
        || status.is_modifiable())
    .then(|| e.rejection(order_client_id))
    .flatten()
}

/// `OrderView::pending_price` of an order in `status`: read off the engine's side table and judged on its clock, so it
/// moves without a registry write. Cold: a modify in flight; one `is_empty` test per order while none is, which is
/// nearly always, and no allocation either way.
pub(super) fn order_pending_price<C: ExecutionClient>(
    e: &Engine<C>,
    order_client_id: &str,
    status: vike_model::OrderStatus,
) -> Option<f64> {
    if status.is_modifiable() { e.pending_modify_price(order_client_id) } else { None }
}

/// The margin half of one engine's `VenueBlock`, handed from `margin_figures` to the
/// position views and the block: Copy scalars, so the hand-off allocates nothing.
#[derive(Clone, Copy)]
struct MarginFigures {
    margin_on: bool,
    margin_used: f64,
    free_buying_power: f64,
    margin_ratio: f64,
    cross_notional_total: f64,
    pool_equity: f64,
}

/// How many route keys [`accounts_epoch_of`] sorts in a STACK array before it falls
/// back to a heap `Vec`, the only path it had before. `build` hands it one key per engine on every
/// publish — per event on a sporadic feed, inside the gated core hop — so this is sized for the
/// shapes that exist: one engine per armed venue, up to the whole of `vike_model::VENUES` armed at
/// once (fourteen venues when this was sized). A core holding more engines than this (the
/// account-routing design's many-accounts-per-venue scale) takes the pre-buffer path, one `Vec` per
/// publish, unchanged rather than slower.
///
/// ⚠ Raising it is not free on the common path: the array is initialized WHOLE on every call, a
/// one-engine core included, so the cost grows with this number and not with the engine count.
pub(super) const ACCOUNTS_EPOCH_STACK_KEYS: usize = 16;

/// [`accounts_epoch_of`]'s digest over keys it SORTS IN PLACE: `0` for an empty set,
/// otherwise FNV-1a over the NUL-terminated keys in sorted order, mapped off `0`. Equal keys are
/// equal bytes, so the unstable sort cannot make the order of two of them show in the value.
fn epoch_digest(keys: &mut [&str]) -> u64 {
    if keys.is_empty() {
        return 0;
    }
    keys.sort_unstable();
    // FNV-1a, 64-bit, from `vike_utility::fnv`, whose algorithm is frozen: never
    // `std::hash::DefaultHasher`, which is documented as not stable across Rust releases, while
    // this value is compared across a node RESTART — which is also a node UPGRADE. A toolchain
    // bump must not read as an account-set change. Each key is followed by its NUL terminator.
    let mut digest = vike_utility::fnv::Fnv1a64::new();
    for key in keys.iter() {
        digest.write(key.as_bytes());
        digest.write(&[0]);
    }
    let h = digest.finish();
    if h == 0 { 1 } else { h }
}
