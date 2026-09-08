//! `capture_seed` — the SYNTHETIC-STATE capture hooks: the two decisions a marketing/QA capture
//! needs that no other knob in the family can reach, because both are about *content* rather than
//! about which window is open.
//!
//! The rest of the capture vocabulary ([`crate::startup`]'s `VIKE_SHOT_WIN`, [`crate::
//! initial_arrange`]'s `VIKE_TOOL`/`VIKE_TOOLS`) decides WHICH surface renders. That is enough for
//! a surface whose content is a live feed or a local store, and it is NOT enough for the two
//! surfaces whose content is a TRADING SESSION — the Trade panel and the chart's drawing layer.
//! `.trader/shots/manifest.json` records both as `capture_gap`s, and the failure they name is the
//! one the whole rig exists to design out: an empty Trade panel is indistinguishable from the
//! bug class where orders silently vanish, so it must never be the frame a marketing page ships.
//!
//! **Data in, decisions out** — the shape [`crate::startup`] established and [`crate::
//! initial_arrange`] repeated. Every function here is PURE: the already-read knob value and the
//! already-loaded market data in, an `OrderRequest` list or a polyline out. `vike-app`'s
//! `app_ui.rs` performs the I/O (the `Dispatch::send`, the `ChartState` write) and owns the
//! process-environment read, so no `Layer::Library` row joins `vike_ops::settings`' `LIBRARY_PIN`
//! ratchet — the same reason the ten `initial_arrange` knobs are read in the binary.
//!
//! ## Why the orders are REAL and the drawing is not
//!
//! The two hooks sit on opposite sides of a line worth stating, because the asymmetry looks like
//! an inconsistency until you know which risk each is answering.
//!
//! [`plan_trade_seed`] mints ordinary [`OrderRequest`]s and hands them to the caller to submit
//! down the SAME `vike_exec::Command::Order` path every UI button uses — the boundary
//! `VIKE_DOM_TESTORDER` established (`crates/vike-app/src/app_ui.rs`, the `dom_test_pending`
//! block) and the reason that hook's caller carries a paper-only guard and a
//! remote-control guard. Nothing here is faked into the snapshot: the resting order rests in a
//! `vike_paper::PaperExecutionClient`'s book and the position is booked by a REAL fill of a REAL
//! order. A screenshot of a fabricated `CoreSnapshot` would be a picture of a code path that does
//! not exist, which for the panel that shows a human their money is the worst thing to publish.
//!
//! [`trendline_overlay`] writes straight into `vike_chart::model::ChartState::overlays` because
//! there is nothing else it could do: that field is the chart's user-drawing layer, it is READ by
//! `crates/vike-chart/src/chart/price_render.rs`'s `paint_price_overlays` and folded into the
//! autofit extent by `chart::draw`, and **nothing in this workspace writes it**. ⚠ That is worth
//! reading twice, because `.trader/shots/manifest.json` and `scripts/marketing_shots.sh` both
//! describe the gap as "the drawing tools ... are click-only". They are not click-only; they do
//! not exist. `overlays` is a rendering surface with no producer, so a capture hook is not
//! reaching past a UI it could have driven — it is the FIRST producer.
//!
//! ## ⚠ The trade seed SUBMITS NOTHING TODAY, and the reason is not in this file
//!
//! Measured on the GPU box 2026-09-06: a `VIKE_TRADE_SEED=1` capture rendered a Trade window with
//! `Accounts: no venues`, `Orders (0)` and a zero equity hero — not even the RESTING order, which
//! this file's own doc says arrives synchronously. It does; it is never asked for.
//!
//! `vike-app` has had **no local trading core** since #1610 (`e482a9de`, 2026-09-03). That commit
//! gave the viewer a default address — `found.filter(..)` became
//! `found.filter(..).or_else(registry).or_else(|| Some(DEFAULT_OBSERVE_ADDR))` — which is right for
//! the THIN viewer it was written for and, in a FAT build, also flips the app out of local-core
//! mode, because `App::new` reads "an address exists" as "observe". #1611 then refactored the
//! ladder into [`crate::backend_conn::startup_backend_from`] and preserved it exactly; that
//! function's doc argues at length that there is no `None` rung, which is correct for the question
//! IT answers (what address should the Connections UI show) and is not the question `App::new` is
//! asking. So `crates/vike-app/src/main.rs` builds `observe_backend` as
//! `Some(backend_conn::startup_backend(..))` UNCONDITIONALLY, `App::new` branches
//! `if let Some(observe_backend)` straight into the OBSERVER arm, which binds
//! `let core: Option<CoreHandle> = None`. `app_ui.rs` then gates every order write
//! on `match (&app.core, remote_ctrl)`, which is `(None, None)` unless `VIKE_TRADEHUB_CONTROL=1`,
//! so the whole block is skipped — `VIKE_DOM_TESTORDER` with it, which is the corroboration worth
//! having: the pre-existing injection this hook was modelled on is equally dead, so the defect is
//! upstream of both rather than in either.
//!
//! **Nothing here is the fix.** [`plan_trade_seed_commands`] is correct and tested and will submit
//! on the first frame a local core exists; restoring one is a product change to the mode selection
//! of the binary that signs orders, and it belongs in its own PR with its own sign-off. Until then
//! `.trader/shots/manifest.json` marks the two poses that depend on this hook `blocked: true`, so
//! the rig SKIPS them loudly instead of publishing a frame that reads `Orders (0)`.
//!
//! ## Both are OFF unless asked for
//!
//! Neither function is reachable without its knob, and neither knob has a default: an unset
//! environment leaves `vike-app` byte-identical, with no seeded order, no overlay and no extra
//! window. See `crates/vike-app/src/main.rs`'s `TRADE_SEED_ENV` / `CHART_DRAW_ENV` reads.

use vike_chart::model::Bar;
use vike_model::OrderRequest;

/// The quantity every seeded order carries. Deliberately the same `0.01` the `VIKE_DOM_TESTORDER`
/// injection uses (`crates/vike-app/src/app_ui.rs`'s `dom_test_pending` block): small enough that
/// a permissive default `vike_app_core::order_entry::OrderLimits` admits it, and large enough that
/// the Trade panel's quantity column renders a number rather than a rounding artifact.
pub const SEED_QTY: f64 = 0.01;

/// Where the RESTING order sits, as a fraction of the last close.
///
/// It is far below the market on purpose and the distance is the whole point: a `vike_paper`
/// buy limit fills when a bar's LOW reaches it (`vike_fills::fill_resolution::
/// resolve_intrabar_fills`), so a resting order that is meant to still be resting when the shutter
/// opens has to be somewhere no bar of the capture's lifetime will trade through. 20% below the
/// last close is several orders of magnitude outside a minute's range on any instrument this app
/// charts, while still rendering as a plausible bid rather than as an obviously fake number.
pub const RESTING_DISCOUNT: f64 = 0.80;

/// `side` for a BUY, in `vike_model::OrderRequest`'s integer encoding.
const SIDE_BUY: i32 = 1;

/// The instrument the Trade-panel seed trades, on [`crate::workspace::DEFAULT_VENUE`].
///
/// ⚠ It is a CONSTANT SHARED BY BOTH HALVES of the hook rather than a per-call-site literal, and
/// the sharing is the point: `crate::startup::plan`'s `trade_seed` arm opens the bar feed whose
/// closes clock the fill, and `vike-app`'s frame loop mints the orders. Those two spelling the
/// symbol separately is precisely the one-character disagreement `crate::initial_arrange`'s module
/// doc records for the QA DOM (its `"BTCUSDT"` against `live_window_keys`' `"{symbol}@1m"`), where
/// the consequence was a feed reaped on the frame after it was opened. Here the consequence would
/// be quieter and worse: the feed would be alive, the orders would be accepted, and the market
/// order would simply never fill, because its `(venue, symbol)` engine is not the one the bars
/// reach.
pub const SEED_SYMBOL: &str = "BTCUSDT";

/// The two orders a Trade-panel capture needs, in the order they must be submitted.
///
/// SEPARATE fields rather than a `Vec`, because the two are not interchangeable and a caller that
/// treated them as a list could silently submit only one: [`Self::resting`] is what makes the
/// working-orders table non-empty and [`Self::market`] is what makes the positions table
/// non-empty. The manifest's `state` clause for `pipeline-3-golive.png` asks for BOTH by name.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeSeed {
    /// A far-from-market buy limit ([`RESTING_DISCOUNT`]) that rests for the whole session. Shows
    /// up the instant it is accepted — `vike_paper::PaperExecutionClient::submit` emits
    /// `OrderSubmitted` + `OrderAccepted` synchronously — so this half of the pose needs no bar.
    pub resting: OrderRequest,
    /// A market buy that opens the position.
    ///
    /// ⚠ **This half is NOT synchronous, and the delay is a property of the paper exchange rather
    /// than of this hook.** A market order rests in the paper book's `pending` list until
    /// `ExecutionClient::on_bar` — the fill clock `crates/vike-core/src/runtime/mod.rs` drives
    /// from `Ingest::BarClose` — hands it a CLOSED bar. So the position appears one bar-close
    /// after submit, not one frame, and a capture whose shutter opens before that close sees the
    /// resting order alone. What the caller must therefore guarantee is a live bar feed on this
    /// `(venue, symbol)`; see [`FILL_CLOCK_INTERVAL`] and `crate::startup::plan`'s `trade_seed`
    /// arm for how that feed is kept alive with no chart window on screen.
    pub market: OrderRequest,
}

/// The bar interval whose closes clock the paper fill for a seeded market order.
///
/// ⚠ It is a CONSTANT rather than a knob because the choice is not the operator's to get wrong:
/// this interval decides how long the capture must run, and the two plausible answers differ by a
/// factor of sixty. `"1m"` is the interval every venue in the roster serves and the one the
/// default chart already uses, so the feed this arm opens is the app's most-exercised path — at
/// the cost that the first close is up to a minute away.
///
/// A capture that wants the POSITION half of the pose therefore has to outlive that close:
/// `VIKE_SHOT_FRAME` must be pushed past it (the self-shot loop in `crates/vike-app/src/main.rs`
/// times out at `start + 900` frames, so raising `start` raises the deadline with it). A capture
/// that only wants the resting order needs nothing.
///
/// ⚠ **`"1s"` is the obvious improvement and is deliberately NOT taken yet — it is UNMEASURED, and
/// its failure mode is worse than the cost it would save.** It would cut the wait from up to sixty
/// seconds to about one, and the groundwork is all there: `"1s"` is a first-class interval in this
/// app (`crates/vike-app/src/main.rs`'s `IVLS` offers it, `crate::tickvol`'s `KLINE_SET` knows it,
/// `vike_model::time::interval_ms` resolves it) and a plain `BTCUSDT` on binance is SPOT here —
/// `crates/bridges/binance/src/market_feed.rs` marks a perp with a trailing `.P` — which is the
/// side of that venue that serves 1s klines at all. What is missing is a RUN: no CI runner has a
/// GPU, so nothing in the merge gate can execute this path, and if a 1s subscription is refused
/// anywhere along that chain the feed simply never delivers and the market order never fills — a
/// capture that silently loses half its pose, which is strictly worse than one that takes a
/// minute. Flip it once somebody has watched a 1s seeded capture fill on a real box.
pub const FILL_CLOCK_INTERVAL: &str = "1m";

/// Mint the two orders for a Trade-panel capture on `(venue, symbol)`, priced off `last_close`.
///
/// `nonce` is stirred into both client order ids so a second call in the same session cannot
/// collide with the first — the same reason the DOM injection spells `dom-test-{shot_n}`.
///
/// Returns `None` — mints NOTHING — when `last_close` is not a usable price. That arm is
/// load-bearing rather than defensive: an empty or still-loading chart series yields `0.0`, and a
/// resting limit at `0.0 * 0.80` is an order at zero, which
/// `vike_app_core::order_entry::validate_with_multiplier` would pass (it caps notional, it has no
/// floor) and a venue would reject. `None` means "not yet" — the caller re-tries on a later frame
/// rather than burning its one-shot flag on a price it did not have.
#[must_use]
pub fn plan_trade_seed(
    venue: &str,
    symbol: &str,
    last_close: f64,
    nonce: u32,
) -> Option<TradeSeed> {
    if !last_close.is_finite() || last_close <= 0.0 {
        return None;
    }
    let base = OrderRequest {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: SIDE_BUY,
        qty: SEED_QTY,
        ..Default::default()
    };
    Some(TradeSeed {
        resting: OrderRequest {
            client_order_id: format!("capture-rest-{nonce}"),
            order_type: "limit".to_string(),
            price: Some(last_close * RESTING_DISCOUNT),
            ..base.clone()
        },
        market: OrderRequest {
            client_order_id: format!("capture-fill-{nonce}"),
            order_type: "market".to_string(),
            price: None,
            ..base
        },
    })
}

/// The `ChartState::overlays` key the trendline is written under.
///
/// A NAMED key, not an index, because `overlays` is a `BTreeMap`: re-writing under the same name
/// replaces rather than accumulates, so a hook that fired twice cannot stack sixty trendlines on
/// one chart. The name also reaches the render — `paint_price_overlays` passes it to
/// `egui_plot::Line::new` — so it says what it is rather than something like `"0"`.
pub const TREND_OVERLAY: &str = "capture · trend";

/// ...and the horizontal one. Two drawings rather than one because the manifest's `state` clause
/// for `grid-charting.png` asks for "an overlay/drawing visible" on a tile whose CLAIM is
/// multi-pane analysis: a lone diagonal reads as an indicator, a diagonal plus a level reads as
/// somebody having marked up a chart, which is what the claim is about.
pub const SUPPORT_OVERLAY: &str = "capture · support";

/// The frame on which the seed fires — late, so the price it reads comes off a settled series
/// rather than off the first bar to arrive. The same `shot_n` threshold the `VIKE_DOM_TESTORDER`
/// injection waits for, and named here rather than spelled at the call site so the two capture
/// injections cannot drift apart about what "late in the run" means.
pub const SEED_FRAME: u32 = 320;

/// The last close of the fill clock's series, read out of the app's chart map — the price the
/// resting order is quoted off. `None` while the feed has not delivered a bar yet, which is the
/// "not yet, re-try next frame" input [`plan_trade_seed_commands`] must not spend its flag on.
///
/// The KEY and the LOOKUP are ONE concept and live together here rather than split across the call
/// site: the key is composed from the same three constants `crate::startup::plan`'s `trade_seed`
/// arm builds its window from, through the same `crate::workspace::series_key` the app's own feed
/// routing uses, and a key spelled by hand beside the lookup is exactly how it comes to name a
/// series nothing feeds.
#[must_use]
pub fn fill_clock_close(
    charts: &std::collections::HashMap<String, vike_chart::model::ChartState>,
) -> Option<f64> {
    let key = crate::workspace::series_key(
        crate::workspace::DEFAULT_VENUE,
        SEED_SYMBOL,
        FILL_CLOCK_INTERVAL,
    );
    charts.get(&key).and_then(|c| c.bars.last()).map(|b| b.c)
}

/// Everything the frame loop knows that the Trade-panel seed's decision depends on.
///
/// A bundle rather than seven parameters, for the reason [`crate::startup::StartupEnv`] and
/// [`crate::initial_arrange::ArrangeEnv`] are bundles: the fields are ALREADY-READ values, so the
/// decision below is a pure function of them and its tests need neither an `App` nor a process
/// environment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeedInputs<'a> {
    /// `App::trade_seed_pending` — the one-shot flag `VIKE_TRADE_SEED` armed.
    pub armed: bool,
    /// `App::shot_n`, the self-shot frame counter, against [`SEED_FRAME`].
    pub frame: u32,
    /// A remote `Scope::Control` channel is mounted.
    ///
    /// ⚠ **This is the guard that is NOT redundant with [`Self::venue_is_live`], and the DOM
    /// injection's own comment is where the reason is recorded**: a control-enabled `--observe`
    /// client's `App::live_venues` is ALWAYS EMPTY, because it mounts no local exec client at all.
    /// So the paper check below would pass while the order travelled down the control channel to a
    /// remote daemon holding REAL venues. A capture must never be able to do that, which is why
    /// this arm refuses outright rather than falling through to the venue test.
    pub remote_control: bool,
    /// The seed venue is in `App::live_venues` — a credential-gated LIVE exec client.
    pub venue_is_live: bool,
    /// The last close of the fill clock's series, or `None` while the feed has not delivered.
    pub last_close: Option<f64>,
    /// The venue the seed trades on — `crate::workspace::DEFAULT_VENUE` at the only call site,
    /// carried as a field so the refusal message and the minted orders name the same one.
    pub venue: &'a str,
}

/// What the frame loop must DO about the seed — commands out, plus whether the one-shot flag is
/// spent. The shape [`crate::order_dispatch::DispatchPlan`] established, for the same reason: the
/// decision (including every guard) belongs in a crate CI compiles, and the shell keeps the I/O.
#[derive(Debug, Default)]
pub struct SeedPlan {
    /// Fire these in order — the caller hands each to its `Dispatch::send`. Already through the
    /// local preview, so a command here is one the UI's own order paths would also have sent.
    pub commands: Vec<vike_exec::Command>,
    /// One line the caller should `tracing::warn!` — a refusal, or an order the local preview
    /// rejected. Never a reason to panic: a capture that cannot seed must still render.
    pub warnings: Vec<String>,
    /// Whether the caller clears its one-shot flag.
    ///
    /// FALSE means "not yet, try again next frame" — the feed has not delivered a price. TRUE
    /// means the decision is FINAL, and it is true on BOTH the submitted arm and every refused
    /// arm: a refused seed that left the flag armed would re-refuse (and re-warn) on every frame
    /// for the rest of the session.
    pub spend: bool,
}

/// Decide the whole Trade-panel seed for one frame. Pure.
///
/// The guard ladder is here rather than at the call site DELIBERATELY, and it is the main reason
/// this function exists: `vike-app` is named in `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI`, so a
/// guard written there is a guard no test in this workspace can execute — and these two guards are
/// the whole of what stops a marketing screenshot reaching a real account. Down here they are
/// covered by the tests below.
///
/// Order of the ladder is load-bearing: `armed` and `frame` first (cheap, and the common case is
/// "not this frame"), then `remote_control`, then `venue_is_live`, then the price. The two
/// refusals come BEFORE the price check so that a live-venue capture refuses immediately rather
/// than sitting armed until a feed happens to deliver.
#[must_use]
pub fn plan_trade_seed_commands(
    inputs: &SeedInputs<'_>,
    limits: &crate::order_entry::OrderLimits,
    snap: &vike_core::CoreSnapshot,
) -> SeedPlan {
    let mut plan = SeedPlan::default();
    if !inputs.armed || inputs.frame < SEED_FRAME {
        return plan;
    }
    if inputs.remote_control {
        plan.warnings.push(
            "VIKE_TRADE_SEED refused: a remote control channel is mounted, so this order could \
             reach a live venue"
                .to_string(),
        );
        plan.spend = true;
        return plan;
    }
    if inputs.venue_is_live {
        plan.warnings.push(format!("VIKE_TRADE_SEED refused: {} is armed LIVE", inputs.venue));
        plan.spend = true;
        return plan;
    }
    // No price yet ⇒ NOT a refusal: leave the flag armed and re-try. See `plan_trade_seed`'s doc
    // for why a missing price must not mint an order rather than minting a cheap one.
    let Some(px) = inputs.last_close else { return plan };
    let Some(seed) = plan_trade_seed(inputs.venue, SEED_SYMBOL, px, inputs.frame) else {
        return plan;
    };
    // Both orders take the SAME local preview every other submit path takes — the DOM injection's
    // rule, and for its reason: this price comes from the chart series, which
    // `crate::order_dispatch::plan_dispatch` does not see.
    for req in [seed.resting, seed.market] {
        let mult = snap.multiplier_of(&req.venue, &req.symbol);
        match crate::order_entry::validate_with_multiplier(&req, limits, mult) {
            Ok(()) => plan
                .commands
                .push(vike_exec::Command::Order(vike_exec::OrderIntent::Submit(Box::new(req)))),
            Err(reject) => plan.warnings.push(format!(
                "capture trade seed refused by local preview ({} {}): {reject}",
                req.venue, req.symbol
            )),
        }
    }
    // Spent whatever the preview said: a rejected seed must not re-submit every frame.
    plan.spend = true;
    plan
}

/// One capture drawing: the `ChartState::overlays` key it is written under, and its polyline in
/// the chart's own `[x, price]` plot space.
///
/// A named alias rather than the tuple spelled at the signature — `clippy::type_complexity` is a
/// merge gate here and refused the inline form, which is the right call: the pair is the unit
/// `vike-app`'s writer loops over, so it deserves a name that says what it is.
pub type CaptureDrawing = (&'static str, Vec<[f64; 2]>);

/// The minimum number of bars a drawing is derived from. Below this the extremes are noise — two
/// bars produce a "trendline" between the only two points there are, which is a line through the
/// data rather than a reading of it.
const MIN_DRAW_BARS: usize = 8;

/// Derive the two capture drawings from `bars`, or `None` when there is not enough loaded series
/// to draw anything honest.
///
/// The geometry is deliberately the simplest reading a human would make and is a pure function of
/// the loaded bars, so the same series always yields the same picture:
///
///   * `TREND_OVERLAY` — the line joining the LOWEST low to the HIGHEST high, ordered by x so the
///     polyline runs left-to-right whichever extreme came first.
///   * `SUPPORT_OVERLAY` — a horizontal level at that lowest low, spanning the whole loaded range.
///
/// Points are `[x, price]` in the chart's own plot space: `x` is `Bar::t`, the BAR INDEX the
/// price pane plots against, taken from the bar rather than from the slice position — the two
/// agree today and only the bar's own field is guaranteed to keep agreeing, and
/// `paint_price_overlays` filters these points against the visible INDEX bounds.
///
/// `None` (fewer than [`MIN_DRAW_BARS`] bars, or a series carrying no finite extreme) writes
/// nothing at all, which leaves the chart byte-identical — the caller must not substitute an
/// empty polyline, because `paint_price_overlays` skips a `pts.len() < 2` entry and a
/// zero-length entry would sit in the map looking like a drawing that failed to render.
#[must_use]
pub fn trendline_overlay(bars: &[Bar]) -> Option<Vec<CaptureDrawing>> {
    if bars.len() < MIN_DRAW_BARS {
        return None;
    }
    let finite = |b: &&Bar| b.l.is_finite() && b.h.is_finite() && b.t.is_finite();
    let lo = bars.iter().filter(finite).min_by(|a, b| a.l.total_cmp(&b.l)).map(|b| (b.t, b.l))?;
    let hi = bars.iter().filter(finite).max_by(|a, b| a.h.total_cmp(&b.h)).map(|b| (b.t, b.h))?;
    // Degenerate series (every bar the same price) — the "trendline" would be a point, and the
    // support level says nothing. Draw nothing rather than a dot.
    if lo.1 >= hi.1 {
        return None;
    }
    let mut trend = vec![[lo.0, lo.1], [hi.0, hi.1]];
    trend.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let (x0, x1) = (bars.first()?.t, bars.last()?.t);
    Some(vec![(TREND_OVERLAY, trend), (SUPPORT_OVERLAY, vec![[x0, lo.1], [x1, lo.1]])])
}

/// Write the capture drawings onto one chart's state, reporting whether anything was drawn.
///
/// The caller's whole job is the loop over its chart map and the one-shot flag; this owns the
/// "derive, then insert under the stable names" half so the insert cannot drift from the derive.
/// `false` — the series is too short, or too flat, to draw honestly ([`trendline_overlay`]) —
/// leaves `overlays` untouched rather than inserting an empty polyline, which the render would
/// silently skip and a reader would mistake for a drawing that failed.
pub fn apply_drawings(state: &mut vike_chart::model::ChartState) -> bool {
    let Some(drawings) = trendline_overlay(&state.bars) else { return false };
    for (name, pts) in drawings {
        state.overlays.insert(name.to_string(), pts);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(t: f64, l: f64, h: f64) -> Bar {
        Bar { t, ot: 0, o: l, h, l, c: h, v: 1.0 }
    }

    /// A ramp of `n` bars whose low/high both climb, so the lowest low is bar 0 and the highest
    /// high is the last — the ordinary shape, and the one whose extremes are unambiguous.
    fn ramp(n: usize) -> Vec<Bar> {
        (0..n).map(|i| bar(i as f64, 100.0 + i as f64, 110.0 + i as f64)).collect()
    }

    #[test]
    fn a_seed_prices_the_resting_order_below_the_market_and_leaves_the_market_order_unpriced() {
        let s = plan_trade_seed("binance", "BTCUSDT", 50_000.0, 7).expect("a usable close");
        assert_eq!(s.resting.order_type, "limit");
        assert_eq!(s.resting.price, Some(50_000.0 * RESTING_DISCOUNT));
        assert!(
            s.resting.price.expect("a limit price") < 50_000.0,
            "the resting order must sit BELOW the market or it is not resting"
        );
        assert_eq!(s.market.order_type, "market");
        assert_eq!(s.market.price, None, "a market order carries no price");
        assert_eq!((s.resting.qty, s.market.qty), (SEED_QTY, SEED_QTY));
        assert_eq!((s.resting.side, s.market.side), (SIDE_BUY, SIDE_BUY));
    }

    #[test]
    fn both_seeded_orders_carry_the_asked_for_venue_and_symbol() {
        let s = plan_trade_seed("okx", "ETHUSDT", 2_000.0, 0).expect("a usable close");
        for r in [&s.resting, &s.market] {
            assert_eq!(r.venue, "okx");
            assert_eq!(r.symbol, "ETHUSDT");
        }
    }

    /// The ids must differ from each other AND across nonces — one core-wide id space, and a
    /// collision is an order the engine refuses rather than a second order.
    #[test]
    fn seeded_client_order_ids_are_unique_within_and_across_calls() {
        let a = plan_trade_seed("binance", "BTCUSDT", 100.0, 1).expect("a usable close");
        let b = plan_trade_seed("binance", "BTCUSDT", 100.0, 2).expect("a usable close");
        let ids = [
            a.resting.client_order_id,
            a.market.client_order_id,
            b.resting.client_order_id,
            b.market.client_order_id,
        ];
        let uniq: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(uniq.len(), ids.len(), "seeded ids collided: {ids:?}");
    }

    /// The "not yet" arm — see [`plan_trade_seed`]'s doc for why this is not merely defensive.
    #[test]
    fn an_unusable_last_close_mints_no_order_at_all() {
        for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                plan_trade_seed("binance", "BTCUSDT", px, 0).is_none(),
                "a last close of {px} must mint nothing"
            );
        }
    }

    /// The "everything is fine" inputs, which every guard test below then spoils in exactly one
    /// way — so a test that goes green proves the guard it names, not some other field.
    fn ok_inputs() -> SeedInputs<'static> {
        SeedInputs {
            armed: true,
            frame: SEED_FRAME,
            remote_control: false,
            venue_is_live: false,
            last_close: Some(50_000.0),
            venue: "binance",
        }
    }

    fn plan_with(i: &SeedInputs<'_>) -> SeedPlan {
        // `CoreSnapshot::empty` is `crate::order_dispatch`'s own test idiom. The snapshot is
        // consulted for exactly one thing here — `multiplier_of`, which an empty grid answers with
        // the 1.0 fallback — so an empty one over the seed's own (venue, symbol) is the honest
        // fixture rather than a stub that happens to compile.
        plan_trade_seed_commands(
            i,
            &crate::order_entry::OrderLimits::default(),
            &vike_core::CoreSnapshot::empty(i.venue, SEED_SYMBOL),
        )
    }

    #[test]
    fn the_happy_path_submits_both_orders_and_spends_the_flag() {
        let p = plan_with(&ok_inputs());
        assert_eq!(p.commands.len(), 2, "the resting order AND the market order");
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(p.spend);
    }

    /// ⚠ THE SAFETY TEST. Both refusals must emit NO command — and both must SPEND the flag, or a
    /// refused seed re-refuses on every frame for the rest of the session.
    #[test]
    fn neither_guard_can_emit_an_order_and_both_are_final() {
        for (name, i) in [
            ("remote control", SeedInputs { remote_control: true, ..ok_inputs() }),
            ("live venue", SeedInputs { venue_is_live: true, ..ok_inputs() }),
            // Both at once: still refused, still silent — the ladder must not fall through.
            ("both", SeedInputs { remote_control: true, venue_is_live: true, ..ok_inputs() }),
        ] {
            let p = plan_with(&i);
            assert!(p.commands.is_empty(), "{name}: a guarded seed emitted {:?}", p.commands.len());
            assert!(p.spend, "{name}: a refusal that re-arms warns every frame forever");
            assert_eq!(p.warnings.len(), 1, "{name}: a refusal must say so exactly once");
        }
    }

    /// A control-enabled observer is the case the venue check alone CANNOT catch — its
    /// `live_venues` is empty, so `venue_is_live` is false and only the remote guard stands
    /// between a capture knob and a real account. Spelled as its own test because the combination
    /// is the whole hazard.
    #[test]
    fn a_control_enabled_observer_is_refused_even_though_no_venue_reads_live() {
        let i = SeedInputs { remote_control: true, venue_is_live: false, ..ok_inputs() };
        assert!(plan_with(&i).commands.is_empty());
    }

    /// The three "not yet" arms, which must NOT spend the flag — each is a state a later frame
    /// can leave.
    #[test]
    fn a_seed_that_cannot_act_yet_stays_armed_and_says_nothing() {
        for (name, i) in [
            ("disarmed", SeedInputs { armed: false, ..ok_inputs() }),
            ("too early", SeedInputs { frame: SEED_FRAME - 1, ..ok_inputs() }),
            ("no price yet", SeedInputs { last_close: None, ..ok_inputs() }),
            ("unusable price", SeedInputs { last_close: Some(0.0), ..ok_inputs() }),
        ] {
            let p = plan_with(&i);
            assert!(p.commands.is_empty(), "{name}");
            assert!(!p.spend, "{name}: must stay armed so a later frame can re-try");
            assert!(p.warnings.is_empty(), "{name}: a not-yet is not a complaint");
        }
    }

    /// A disarmed knob is the DEFAULT, and it must reach the same do-nothing answer whatever else
    /// is true — including on a frame where every guard would have refused.
    #[test]
    fn an_unarmed_seed_is_inert_whatever_the_rest_of_the_world_looks_like() {
        let i =
            SeedInputs { armed: false, remote_control: true, venue_is_live: true, ..ok_inputs() };
        let p = plan_with(&i);
        assert!(p.commands.is_empty() && p.warnings.is_empty() && !p.spend);
    }

    #[test]
    fn apply_drawings_writes_both_overlays_and_reports_whether_it_drew() {
        // `Default` + `extend`, NOT a struct literal and NOT a field assignment: `ChartState`
        // carries private cache fields (`cache_key`, `vol_cache`, …) so a literal is illegal from
        // outside `vike-chart`, and a plain `st.bars = ..` right after a `default()` trips
        // `clippy::field_reassign_with_default`, which is a merge gate here. Same shape as
        // `crate::core_sync`'s own `chart_with_ots` test builder.
        let mut st = vike_chart::model::ChartState::default();
        st.bars.extend(ramp(20));
        assert!(apply_drawings(&mut st));
        assert_eq!(st.overlays.len(), 2);
        assert!(
            st.overlays.contains_key(TREND_OVERLAY) && st.overlays.contains_key(SUPPORT_OVERLAY)
        );

        // Idempotent: the map is keyed by NAME, so a second pass replaces rather than accumulates.
        assert!(apply_drawings(&mut st));
        assert_eq!(st.overlays.len(), 2, "re-applying must not stack drawings");

        // Nothing to draw ⇒ nothing written, and the caller is told so.
        let mut empty = vike_chart::model::ChartState::default();
        assert!(!apply_drawings(&mut empty));
        assert!(empty.overlays.is_empty(), "a series it cannot read must stay untouched");
    }

    #[test]
    fn a_ramp_draws_a_trendline_from_the_lowest_low_to_the_highest_high() {
        let v = trendline_overlay(&ramp(20)).expect("20 bars is enough to draw");
        let trend = &v.iter().find(|(n, _)| *n == TREND_OVERLAY).expect("a trend line").1;
        assert_eq!(trend, &vec![[0.0, 100.0], [19.0, 129.0]]);
    }

    /// The x-ordering rule: on a DOWN series the highest high comes first, and the polyline must
    /// still run left-to-right.
    #[test]
    fn a_falling_series_still_draws_its_trendline_left_to_right() {
        let bars: Vec<Bar> =
            (0..12).map(|i| bar(i as f64, 100.0 - i as f64, 110.0 - i as f64)).collect();
        let v = trendline_overlay(&bars).expect("12 bars is enough to draw");
        let trend = &v.iter().find(|(n, _)| *n == TREND_OVERLAY).expect("a trend line").1;
        assert!(
            trend[0][0] < trend[1][0],
            "the polyline must be x-ascending whichever extreme came first: {trend:?}"
        );
        assert_eq!(trend, &vec![[0.0, 110.0], [11.0, 89.0]]);
    }

    #[test]
    fn the_support_level_is_horizontal_at_the_lowest_low_across_the_whole_range() {
        let v = trendline_overlay(&ramp(20)).expect("20 bars is enough to draw");
        let sup = &v.iter().find(|(n, _)| *n == SUPPORT_OVERLAY).expect("a support line").1;
        assert_eq!(sup, &vec![[0.0, 100.0], [19.0, 100.0]]);
    }

    /// Every drawing must satisfy the render's own admission rule — `paint_price_overlays` skips
    /// anything shorter than two points, and an entry it skips is a drawing nobody sees.
    #[test]
    fn every_drawing_carries_at_least_the_two_points_the_render_requires() {
        let v = trendline_overlay(&ramp(20)).expect("20 bars is enough to draw");
        assert_eq!(v.len(), 2, "both drawings must be produced together");
        for (name, pts) in &v {
            assert!(pts.len() >= 2, "{name} has {} point(s), the render skips it", pts.len());
            assert!(
                pts.iter().all(|p| p[0].is_finite() && p[1].is_finite()),
                "{name} carries a non-finite point"
            );
        }
    }

    #[test]
    fn too_few_bars_draws_nothing() {
        for n in 0..MIN_DRAW_BARS {
            assert!(trendline_overlay(&ramp(n)).is_none(), "{n} bars must draw nothing");
        }
        assert!(trendline_overlay(&ramp(MIN_DRAW_BARS)).is_some(), "the floor itself draws");
    }

    /// A flat series has no extreme worth drawing — see the degenerate arm's comment.
    #[test]
    fn a_flat_series_draws_nothing() {
        let flat: Vec<Bar> = (0..20).map(|i| bar(i as f64, 100.0, 100.0)).collect();
        assert!(trendline_overlay(&flat).is_none());
    }

    /// A non-finite bar must not become the extreme it would otherwise win by `total_cmp`
    /// (`NaN` sorts above every finite value, so an unfiltered `max_by` would pick it).
    #[test]
    fn a_non_finite_bar_is_not_allowed_to_become_an_extreme() {
        let mut bars = ramp(20);
        bars[5] = bar(5.0, f64::NAN, f64::NAN);
        let v = trendline_overlay(&bars).expect("the finite bars still draw");
        for (name, pts) in &v {
            assert!(
                pts.iter().all(|p| p[0].is_finite() && p[1].is_finite()),
                "{name} took a non-finite bar as an extreme: {pts:?}"
            );
        }
    }
}
