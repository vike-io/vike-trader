//! Building a snapshot from the engines: the account-set digest and `build`.
//!
//! The snapshot TYPES are vike-exec's read model (`vike_exec::CoreSnapshot` and its rows); this
//! file is the one producer that fills them from the core's engines, as free functions because an
//! inherent `impl` cannot live outside the crate that owns the type.

use vike_exec::{
    CoreSnapshot, ExecutionClient, ExecutionEngine, HeldOrderView, MountView, OrderView, Portfolio,
    PositionView, PriceCfg, ReconBlock, ResolvedEquity, VenueBlock,
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
// runtime-internal ctor: counters come from the loop
#[allow(clippy::too_many_arguments, clippy::allow_attributes)]
pub fn build<C: ExecutionClient>(
    seq: u64,
    engine: &ExecutionEngine<C>,
    extra_engines: &[(f64, ExecutionEngine<C>)],
    seed_cash: f64,
    price_cfg: PriceCfg,
    // The ONE maintenance rate (the scope-parameterized liquidation law's rate source):
    // the operator's `MarginCallConfig::mm_requirement` when the watchdog is configured,
    // else that config's default — resolved by the caller (see `publish`). Feeds the
    // per-position liq-price badge; the retired `im * 0.5` hardcode is dead.
    mm_rate: f64,
    // The ring arrives RENDERED (`Arc<str>` lines): the fold formats each note at push and this
    // build clones one refcount per entry. Perf audit finding #2 (#887) had deferred the render
    // to this publish on the premise that it is coalesced (>=16ms); it is not — it also runs on
    // every idle transition, i.e. per event on a sporadic feed — so #896 moved the render back
    // (`crates/vike-core/src/runtime/journaling.rs`'s `note_event` carries the measurement).
    recent_events: &std::collections::VecDeque<std::sync::Arc<str>>,
    bars: &indexmap::IndexMap<vike_exec::SeriesKey, vike_exec::BarSeries>,
    // An `Arc` list, taken by value: the caller (`CoreThread::published_mount_rows`) either
    // reuses the list the previous publish carried (a refcount bump) or builds one fresh, and
    // `build` only stores it. It used to take a slice and `to_vec()` it, which cloned every row
    // and its three `String`s a second time on every publish.
    mounts: std::sync::Arc<[MountView]>,
    fault: &Option<String>,
    conflated_market_drops: u64,
    rejected_commands: u64,
    // Task 17: the GUI-facing reconcile block (held alerts + last pass ts), built by the
    // caller from its held-alert store (`CoreThread::recon_block`) — an owned value (not a
    // `&`) since the caller already builds a fresh one per publish.
    recon: ReconBlock,
    // Live-runtime OTO/OCO: pending bracket exits held off the venue, built by the caller from
    // `CoreThread::held_orders` (an empty `Vec` — no allocation — on every non-bracket run).
    // By value for the same reason as `mounts`: moved into the snapshot, not copied again.
    held_exits: Vec<HeldOrderView>,
) -> CoreSnapshot {
    let acc = &engine.account;
    // Publish path: coalesced (≥16ms) while the core is busy, but ALSO run on every idle
    // transition, so on a sporadic feed this fn runs once per event, inside the gated hop.
    // `price_cfg` is caller-tunable (`CoreConfig::price_cfg`, threaded through from PR-3's
    // equity sampler, which resolves prices through the SAME cfg); permissive default
    // (`PriceCfg::default()`: no freshness windows, mark enabled) unless the caller opts in.
    let cfg = price_cfg;
    // Compute the primary block first so the top-level scalar `equity` and `positions`
    // can be reused from it verbatim (they are documented mirrors of the primary venue —
    // see the struct docs above `equity`/`positions`).
    let primary = venue_block(engine, seed_cash, &cfg, mm_rate);
    let top_equity = primary.equity;
    let top_positions = primary.positions.clone();
    let mut venues = Vec::with_capacity(1 + extra_engines.len());
    venues.push(primary);
    for (seed, e) in extra_engines {
        venues.push(venue_block(e, *seed, &cfg, mm_rate));
    }
    // CrossVenueDriver::aggregate_equity law: py_sum in venue registration order
    let equity_total = vike_model::py_sum(venues.iter().map(|v| v.equity));
    // The drawdown latch's frozen capital base — SAME fold law, SAME order (primary first,
    // then extras in registration order) as `CoreThread::sweep_drawdown_latch` computes it
    // from `seed_of`, so the published `Portfolio::drawdown_curve` is bit-identical to the
    // number the latch acted on. See `Portfolio::capital_base`.
    let capital_base =
        vike_model::py_sum(std::iter::once(seed_cash).chain(extra_engines.iter().map(|(s, _)| *s)));
    let missing_prices_total: u32 = venues.iter().map(|v| v.missing_prices).sum();
    let margin_used_total: f64 = venues.iter().map(|v| v.margin_used).sum();
    let orders = order_views(engine, extra_engines, &venues);
    CoreSnapshot {
        seq,
        venue: engine.venue.clone(),
        symbol: engine.symbol.clone(),
        trading_state: engine.trading_state,
        balance: acc.balance,
        balance_mode: acc.balance_mode,
        portfolio: Portfolio {
            equity: top_equity,
            equity_total,
            realized_pnl: acc.realized_pnl,
            fees_paid: acc.fees_paid,
            funding_paid: acc.funding_paid,
            margin_used_total,
            capital_base,
            missing_prices_total,
            balances_by_asset: acc.balances_by_asset.iter().map(|(a, q)| (a.clone(), *q)).collect(),
            venues,
        },
        positions: top_positions,
        marks: acc.marks_iter().map(|((v, s), px)| (v.to_string(), s.to_string(), *px)).collect(),
        orders,
        held_exits,
        bars: bars.clone(), // Arc clones for the closed series — cheap by design
        mounts,
        recent_events: recent_events.iter().cloned().collect(),
        fault: fault.clone(),
        conflated_market_drops,
        rejected_commands,
        recon,
        // The reconcile-path coin deltas are a side map the caller overwrites at the publish
        // site (`runtime::publish`) from `CoreThread::recon_coin_deltas` — kept out of this
        // ctor's arg list (already `too_many_arguments`) since it is retained fold-thread state,
        // not derived from the engine here. Empty on the test/direct-build path.
        recon_coin_deltas: indexmap::IndexMap::new(),
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
    }
}

/// One engine's `VenueBlock`: its margin figures, its position views and the block itself, all
/// from ONE `resolve_equity` call. Its venue is always `e.venue`, so that is not a parameter.
fn venue_block<C: ExecutionClient>(
    e: &ExecutionEngine<C>,
    seed: f64,
    cfg: &PriceCfg,
    mm_rate: f64,
) -> VenueBlock {
    // One resolve per engine, parallel to `account.positions` insertion order. Builds a
    // whole VenueBlock from a `ResolvedEquity` so `equity`/`unrealized`/`missing_prices`
    // and each PositionView's `unrealized`/`mark_source` all come from the SAME resolve
    // call — never a second, possibly-stale board read.
    let re: ResolvedEquity = e.resolve_equity(seed, cfg);
    let margin = margin_figures(e, cfg, &re);
    let positions = position_views(e, &re, &margin, mm_rate);
    let MarginFigures { margin_used, free_bp, margin_ratio, .. } = margin;
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
        // What the engine trades and what stands behind it (the Trade window design, §4.3).
        // One small `String` clone, the same kind of work as `route_key` above; the `Vec`
        // clone allocates nothing while it is empty, which is every venue mount.
        symbol: e.symbol.clone(),
        extra_symbols: e.extra_symbols.clone(),
        mode: Some(e.mode),
        balance: e.account.balance,
        realized_pnl: e.account.realized_pnl,
        fees_paid: e.account.fees_paid,
        funding_paid: e.account.funding_paid,
        balance_mode: e.account.balance_mode,
        equity: re.equity,
        unrealized: re.unrealized_total,
        missing_prices: re.missing,
        margin_used,
        free_bp,
        margin_ratio,
        fee_schedule: e.fee_schedule,
        trading_state: e.trading_state,
        // Pointer-copy of the engine's immutable multiplier grid — one `Arc` refcount
        // bump per venue per publish, no allocation (same cost profile as `bars`).
        multipliers: e.account.multiplier_grid(),
        multiplier_default: e.account.multiplier_default(),
        positions,
    }
}

/// The margin fields and the liq-badge pool inputs of one engine's block, priced under the
/// SAME `cfg` as that block's `resolve_equity`.
fn margin_figures<C: ExecutionClient>(
    e: &ExecutionEngine<C>,
    cfg: &PriceCfg,
    re: &ResolvedEquity,
) -> MarginFigures {
    // Margin fields are computed ONLY when the gate is armed — this keeps the snapshot
    // publish (which is on the measured core-hop) zero-cost on the default/off path (the
    // latency gate runs with the gate off), and byte-identical to the pre-margin builder.
    let margin_on =
        e.gate.limits.im_requirement.is_some() || !e.gate.limits.im_by_symbol.is_empty();
    let mut margin_used = 0.0;
    if margin_on {
        // THE shared margin-in-use fold (`Account::margin_in_use`) — the SAME authority
        // the pre-trade gate uses, so the published number counts every position the gate
        // counts. DIVERGENCE FIX: the old snapshot fold used `im_for(s)` with NO fallback,
        // so a position whose symbol had no per-symbol override (`Command::SetMargin` writes
        // ONLY `im_by_symbol`, leaving the global `im_requirement` unset) was SKIPPED here
        // while the gate still counted it at the order symbol's rate — the published
        // margin_used UNDERSTATED what the gate enforced. (These snapshot fields are
        // GUI-only — the auto-liquidation watchdog computes its own margin from
        // `mm_requirement` via `check_margin_call` and never reads this number — so the
        // bug was a rosy GUI display, not a liquidation-safety issue.) RATE POLICY now
        // mirrors the gate: each position uses its own IM, falling back — for a
        // no-override position — to the account's MOST CONSERVATIVE armed initial-margin
        // rate (the max of `im_by_symbol` and any global `im_requirement`). That guarantees
        // such a position is VISIBLE and priced no lower than any single gate check would
        // price it, so the published free_bp is never rosier than the gate's admission
        // basis. When `im_requirement` IS set (global default), `im_for` never returns None
        // and this fallback is never consulted → byte-identical to the pre-fix number.
        //
        // KNOWN GUI ARTIFACT (cosmetic, deliberate): because the fallback is the max over
        // ALL armed rates, a no-override position's displayed margin shifts when an
        // UNRELATED symbol's rate is armed/disarmed via `Command::SetMargin`. It settles
        // once every held symbol has its own override or a global `im_requirement` is set.
        // Never dangerous (overstates, never understates, GUI-only).
        let fallback = e
            .gate
            .limits
            .im_by_symbol
            .values()
            .copied()
            .chain(e.gate.limits.im_requirement)
            .fold(0.0_f64, f64::max);
        // POOL POLICY (the liquidation law's partition, mirroring the gate and
        // `check_margin_call`): only CROSS positions price into this shared number.
        // An Isolated position is backed by its own walled-off wallet (surfaced
        // per-position via `PositionView::isolated_margin`) and a Cash position is
        // fully funded — folding either into `margin_used` would double-charge a
        // mixed account's published free_bp against equity that never backs them.
        // All-cross accounts (every position today): the filter is a no-op →
        // byte-identical to the pre-filter number (the dedup-A1 pins still hold).
        // PRICE BASIS (risk-lane completion): the fold is resolver-priced
        // (`resolved_margin_in_use_by` under the SAME `cfg` as `resolve_equity` above),
        // so the published `margin_ratio`'s numerator and denominator — and the free_bp
        // crossing — share one price basis; a stale `Account.marks` scalar can no longer
        // overstate margin against a fresh-quote equity. Rate/pool policy unchanged.
        margin_used = e.resolved_margin_in_use_by(cfg, |(_v, s, _side), p| {
            p.margin_mode.is_cross().then(|| e.gate.limits.im_for(s).unwrap_or(fallback))
        });
    }
    // gate off ⇒ free_bp == equity floored at 0.0 (nothing locked; a negative equity publishes 0),
    // ratio 0.
    let free_bp = if margin_on {
        vike_model::free_buying_power(
            re.equity,
            margin_used,
            0.0,
            e.gate.limits.required_free_bp_pct,
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
                && let Some(mark) = e.account.mark_of(v, s)
            {
                cross_notional_total +=
                    vike_model::gross_notional(p.size, mark, e.account.multiplier_of(s));
            }
        }
    }
    MarginFigures {
        margin_on,
        margin_used,
        free_bp,
        margin_ratio,
        cross_notional_total,
        pool_equity,
    }
}

/// One engine's `PositionView`s, parallel to `account.positions` and to `re.per_position`.
fn position_views<C: ExecutionClient>(
    e: &ExecutionEngine<C>,
    re: &ResolvedEquity,
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
            // `im * 0.5` hardcode is dead). 0.0 when the gate is off or the leg is flat;
            // skips the per-position `im_for` lookup entirely on the off path. By mode:
            // - Isolated → the closed-form `liquidation_price` at the REAL maint rate
            //   (its own wallet is its pool, so a per-position price is exact in shape);
            // - Cross → `cross_liquidation_price_est`: the mark at which the SHARED
            //   pool first hits the law's line, others frozen (a per-position cross liq
            //   price is inherently an estimate — the venue-UI shape, advisory only;
            //   the watchdog acts on the account-level law, never on this number);
            // - Cash → no badge (structurally cannot breach).
            let (leverage, liq_price) = match margin_on.then(|| e.gate.limits.im_for(s)) {
                Some(Some(im)) if im > 0.0 && p.size != 0.0 => {
                    let liq = if p.margin_mode.is_isolated() {
                        vike_model::liquidation_price(p.avg_px, p.size.signum() as i32, im, mm_rate)
                    } else if p.margin_mode.is_cross() {
                        match e.account.mark_of(v, s).as_ref() {
                            Some(mark) => {
                                let mult = e.account.multiplier_of(s);
                                let own = vike_model::gross_notional(p.size, *mark, mult);
                                vike_model::cross_liquidation_price_est(
                                    pool_equity,
                                    p.size,
                                    *mark,
                                    mult,
                                    cross_notional_total - own,
                                    mm_rate,
                                )
                            }
                            None => 0.0, // unmarked → the pool can't be judged here
                        }
                    } else {
                        0.0 // Cash: never liquidates
                    };
                    (1.0 / im, liq)
                }
                _ => (0.0, 0.0),
            };
            PositionView {
                venue: v.to_string(),
                symbol: s.to_string(),
                position_side: ps.to_string(),
                size: p.size,
                avg_px: p.avg_px,
                unrealized: rp.unrealized,
                mark_source: rp.mark_source,
                leverage,
                liq_price,
                // Carried straight from the account's PositionEntry (Cross/None by
                // default → byte-identical to the pre-field view).
                margin_mode: p.margin_mode,
                isolated_margin: p.isolated_margin,
            }
        })
        .collect()
}

/// Every registry order of every engine, primary first, as an `OrderView` that reads its
/// account label from the block `venues` already holds for its engine.
fn order_views<C: ExecutionClient>(
    engine: &ExecutionEngine<C>,
    extra_engines: &[(f64, ExecutionEngine<C>)],
    venues: &[VenueBlock],
) -> Vec<OrderView> {
    let mut orders: Vec<OrderView> = Vec::new();
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
    for (e, block) in
        std::iter::once(engine).chain(extra_engines.iter().map(|(_, e)| e)).zip(venues)
    {
        for (coid, mo) in e.registry.iter() {
            orders.push(OrderView {
                client_order_id: coid.clone(),
                venue: e.venue.clone(),
                // `None` for the default account (every node today), which clones to nothing; a
                // labelled account pays one small `String` per order.
                account: block.account.clone(),
                symbol: mo.request.symbol.clone(),
                side: mo.request.side,
                qty: mo.request.qty,
                order_type: mo.request.order_type.clone(),
                price: mo.request.price,
                trigger_price: mo.request.trigger_price,
                status: mo.status,
                venue_order_id: mo.venue_order_id.clone(),
                filled_qty: mo.filled_qty,
                avg_fill_px: mo.avg_fill_px,
            });
        }
    }
    orders
}

/// The margin half of one engine's `VenueBlock`, handed from `margin_figures` to the
/// position views and the block: Copy scalars, so the hand-off allocates nothing.
#[derive(Clone, Copy)]
struct MarginFigures {
    margin_on: bool,
    margin_used: f64,
    free_bp: f64,
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
    // FNV-1a, 64-bit, spelled out: `std::hash::DefaultHasher` is documented as not stable
    // across Rust releases, and this value is compared across a node RESTART — which is also a
    // node UPGRADE. A toolchain bump must not read as an account-set change.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for key in keys.iter() {
        for b in key.as_bytes().iter().copied().chain(std::iter::once(0u8)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    if h == 0 { 1 } else { h }
}
