//! The Polymarket up/down scalp-cockpit tool-window GLUE — the shell's wiring AROUND the
//! [`vike_order_entry::cockpit`] widgets (chain rail / Price-to-Beat header / one-click ticket / probability
//! ladder), moved down out of the CI-excluded `main.rs` (tool-view extraction batch 3, audit F1).
//! The widgets already lived in vike-order-entry's `cockpit` module; what moves here is the binary's adaptation layer —
//! the window-chain card build, the YES/NO quote derivation, the book→rung bucketing, the
//! resting-order projection, and the action drain into the `tv.cockpit_*_actions` OUT slots.
//!
//! Also moved: the four cockpit tuning constants (re-imported by the shell, `vike-desktop`, which
//! still owns the window bookkeeping that reads them). The resolved order intent the window loop
//! folds onto the command lane is `vike_order_entry::cockpit::CockpitCommand`, beside the dispatcher.
//!
//! What deliberately did NOT move: `polymarket_cockpit_seed_token`'s `env::var` read (libraries take
//! configuration as parameters, only binaries read the process env — it stays in
//! `crates/vike-desktop/src/main.rs`) and `pick_updown_token` (it named a
//! `vike_polymarket::GammaMarket`, a type behind the heavy `polymarket` feature this crate must not
//! pull in). Both stayed in the shell; `pick_updown_token` then went with the `fat` build, and
//! `main.rs` carries its tombstone. (This said "Both stay in `vike-app`" until 2026-09-28.)

use super::WindowToolContext;

pub(super) mod follow;
mod labels;
use crate::ui::windows::WindowView;
use labels::polymarket_short_label;
use vike_marketdata::L2BookLevel;
use vike_model::fair::UPDOWN_WINDOW_SECS;

/// Cockpit book staleness threshold (ms). A Polymarket book ticks far less often than a crypto
/// ladder (the shell's `TRADE_STALE_MS` for a Trade window is 2 s, as the DOM's was), so a
/// perfectly valid up/down snapshot would flash STALE under the ladder's threshold. ~7.5× that
/// window keeps a live-but-quiet market reading fresh while still catching a genuinely dead feed.
pub const POLYMARKET_STALE_MS: i64 = 15_000;
/// How many FUTURE windows the chain rail shows past the current one.
pub(crate) const POLYMARKET_CHAIN_LOOKAHEAD: usize = 4;
/// The cockpit's placeholder token-id before a real one is known (the env seed, or the live window
/// [`super::follow_cockpit`] picks): it is NOT subscribed (`ensure_polymarket_book` skips it) and its
/// book stays empty/STALE.
pub const POLYMARKET_PLACEHOLDER_TOKEN: &str = "POLY-DEMO";

/// The Polymarket up/down scalp cockpit body (`WindowKind::Polymarket`). Stacks the four `vike-order-entry` cockpit
/// widgets over ONE token's live L2 book: the window-chain rail, the Price-to-Beat header, the
/// one-click ticket, and the probability ladder (fills the remainder). [`WindowToolContext::market`] carries the
/// per-window transport: `symbol` is the YES-outcome token-id, `venue` is `"polymarket"`, `book` is
/// that token's live book from the shared `data_sink::L2BookStore` (`None` until the first snapshot
/// lands ⇒ `stale`). Ladder and ticket order intents leave as `vike_order_entry::cockpit` actions (into
/// `tv.cockpit_*_actions`) the window loop maps to `vike_execution::Command`s — the same seam as
/// [`super::trade_tool_content`].
///
/// Inputs the app cannot yet source are fed NEUTRAL (and flagged in the crate report): the CURRENT
/// window's rail card carries the live book's up/dn odds, but FUTURE windows' odds and all per-window
/// volume stay `None` (their markets aren't resolved yet — full per-window Gamma resolution is a
/// follow-up), and the header's reference/spot underlying is `0.0` (no BTC/ETH spot feed is threaded
/// to the cockpit yet).
pub fn cockpit_tool_content(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
) {
    let snapshot = context.snapshot;
    let token = context.market.symbol;
    let venue = context.market.venue;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let real = context.market.book.filter(|b| b.bid_levels() + b.ask_levels() > 0);

    // Header label for the rail/ladder: the elided token-id — the raw 78-digit id is unreadable,
    // and nothing resolves a token to its market NAME (the desktop makes no venue call).
    // The full id stays the routing key everywhere below (order match, subscribe seam).
    let asset_label = polymarket_short_label(token);
    // Why no live market is shown, in words (`super::follow_cockpit`: no datahub, an older one, no recorder, the first
    // ask still out), and a repaint while an ask is out so its answer is taken as it lands.
    if let Some(why) = tv.cockpit_follow.status() {
        ui.weak(why);
    }
    if tv.cockpit_follow.asking() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }

    // YES/NO quote from the single YES-token book: buying YES pays the best ask; buying NO ≈ 1 − best
    // bid (selling YES). `None` while the relevant side is empty.
    let up_price = real.and_then(|b| b.best_ask()).map(|L2BookLevel { price: p, .. }| p);
    let down_price = real.and_then(|b| b.best_bid()).map(|L2BookLevel { price: p, .. }| 1.0 - p);

    // --- window-chain rail: the deterministic 5-minute up/down chain around `now`. Only the CURRENT
    // window (index 0) maps to the token whose live book is in scope here, so it carries live up/dn
    // odds; every FUTURE window resolves to a market not yet created, so it stays `None` (correct —
    // full per-window Gamma resolution is a follow-up, deliberately NOT a fetch loop in the render
    // path). Per-window volume is likewise unsourced → `None`. ---
    let chain = vike_order_entry::cockpit::chain_windows(
        now_ms,
        UPDOWN_WINDOW_SECS,
        POLYMARKET_CHAIN_LOOKAHEAD,
    );
    let cards: Vec<vike_order_entry::cockpit::CockpitUpDownWindowCard> = chain
        .iter()
        .enumerate()
        .map(|(i, w)| vike_order_entry::cockpit::CockpitUpDownWindowCard {
            open_ms: w.open_ms,
            up_price: if i == 0 { up_price } else { None },
            down_price: if i == 0 { down_price } else { None },
            volume: None,
        })
        .collect();
    let rail_inputs = vike_order_entry::cockpit::CockpitChainRailInputs {
        asset: &asset_label,
        cards: &cards,
        window_secs: UPDOWN_WINDOW_SECS,
        now_ms,
    };
    // The selection is latched into `tv.cockpit_chain`; a window pick emits no core action (it only
    // re-routes which window the cockpit shows — a follow-up once per-window markets are wired).
    let _ = vike_order_entry::cockpit::draw_chain_rail(ui, &mut tv.cockpit_chain, &rail_inputs);

    // --- Price-to-Beat header. reference_price/spot need the UNDERLYING (BTC/ETH) spot, not the
    // Polymarket book — no such feed is threaded here yet, so both are fed 0.0 (neutral). ---
    // `UPDOWN_WINDOW_SECS` is a positive length, so the window is always there; without one,
    // nothing would remain of it.
    let resolution_timestamp =
        vike_order_entry::cockpit::window_open_ms(now_ms, UPDOWN_WINDOW_SECS)
            .and_then(|open| vike_order_entry::cockpit::window_close_ms(open, UPDOWN_WINDOW_SECS))
            .unwrap_or(now_ms);
    let ptb = vike_order_entry::cockpit::CockpitPriceToBeatInputs {
        reference_price: 0.0,
        spot: 0.0,
        resolution_timestamp,
        now_ms,
        up_price,
        down_price,
    };
    vike_order_entry::cockpit::draw_price_to_beat(ui, &ptb);

    // --- one-click ticket (drawn above the ladder so the ladder fills the remaining height). ---
    let ticket_inputs = vike_order_entry::cockpit::CockpitTicketInputs { up_price, down_price };
    let ticket_acts =
        vike_order_entry::cockpit::draw_cockpit_ticket(ui, &mut tv.cockpit_ticket, &ticket_inputs);
    tv.cockpit_ticket_actions.extend(ticket_acts);

    // --- probability ladder over the token book: bucket each 0..1 level to its rung on the
    // market's own tick, which the book carries (a `tick_size_change` re-grids it); before the first
    // book, Polymarket's whole cent. ---
    let tick = context.market.book.map_or(0.01, |b| b.tick_size);
    let rung = |p: f64| vike_order_entry::cockpit::rung_of(p, tick);
    let (bids, asks) = real.map(|b| b.top_n(64)).unwrap_or_default();
    let levels = vike_order_entry::cockpit::bucket_probability_levels(&bids, &asks, tick);
    let inside_bid = real.and_then(|b| b.best_bid()).map(|L2BookLevel { price: p, .. }| rung(p));
    let inside_ask = real.and_then(|b| b.best_ask()).map(|L2BookLevel { price: p, .. }| rung(p));
    let orders: Vec<vike_order_entry::cockpit::CockpitProbabilityLadderOrder> = snapshot
        .orders
        .iter()
        .filter(|o| o.venue == venue && o.symbol == token)
        .map(|o| vike_order_entry::cockpit::CockpitProbabilityLadderOrder {
            order_client_id: o.order_client_id.clone(),
            side: o.side,
            price_ticks: rung(o.price.or(o.trigger_price).unwrap_or(0.0)),
            quantity: o.qty,
            marker: vike_order_entry::cockpit::CockpitProbabilityLadderMarker::Resting,
        })
        .collect();
    let ladder_inputs = vike_order_entry::cockpit::CockpitProbabilityLadderInputs {
        asset: &asset_label,
        tick,
        levels: &levels,
        orders: &orders,
        inside_bid,
        inside_ask,
        stale: context.market.stale,
    };
    let ladder_acts = vike_order_entry::cockpit::draw_probability_ladder(
        ui,
        &mut tv.cockpit_ladder,
        &ladder_inputs,
    );
    tv.cockpit_ladder_actions.extend(ladder_acts);
}
