//! The Polymarket up/down scalp-cockpit tool-window GLUE — `vike-app`'s wiring AROUND the
//! [`vike_cockpit`] widgets (chain rail / Price-to-Beat header / one-click ticket / probability
//! ladder), moved down out of the CI-excluded `main.rs` (tool-view extraction batch 3, audit F1).
//! The widgets already lived in vike-cockpit; what moves here is the binary's adaptation layer —
//! the window-chain card build, the YES/NO quote derivation, the book→cent bucketing, the
//! resting-order projection, and the action drain into the `tv.cockpit_*_actions` OUT slots.
//!
//! Also moved: the four cockpit tuning constants (re-imported by `vike-app`, which still owns the
//! window bookkeeping that reads them) and [`CockpitCmd`], the resolved order intent the window
//! loop folds onto the command lane.
//!
//! What deliberately did NOT move: `poly_cockpit_seed_token` (an `env::var` read — libraries take
//! configuration as parameters, only binaries read the process env) and `pick_updown_token` (it
//! names a `vike_polymarket::GammaMarket`, a type behind the heavy `polymarket` feature this crate
//! must not pull in). Both stay in `vike-app`.

use super::ToolCtx;
use crate::poly_labels::poly_short_label;
use crate::tools;

/// Polymarket up/down window length (seconds) — the 5-minute rolling market the cockpit trades.
pub const POLY_WINDOW_SECS: i64 = 300;
/// Cockpit book staleness threshold (ms). A Polymarket book ticks far less often than a crypto DOM
/// (whose `DOM_STALE_MS` is 2 s), so a perfectly valid up/down snapshot would flash STALE under the
/// DOM threshold. ~7.5× the DOM window keeps a live-but-quiet market reading fresh while still
/// catching a genuinely dead feed.
pub const POLY_STALE_MS: i64 = 15_000;
/// How many FUTURE windows the chain rail shows past the current one.
pub const POLY_CHAIN_LOOKAHEAD: usize = 4;
/// The cockpit's placeholder token-id before a real one is resolved (env seed or background Gamma):
/// it is NOT subscribed (`ensure_poly_book` skips it) and its book stays empty/STALE.
pub const POLY_PLACEHOLDER_TOKEN: &str = "POLY-DEMO";

/// One resolved Polymarket cockpit order intent, translated from a `vike_cockpit` action at the
/// window-loop drain (where the ticket stake + live book are in scope) and folded onto the command
/// lane after the loop — the cockpit twin of the DOM's `(venue, inst, DomAction)` drain.
#[derive(Debug, Clone, PartialEq)]
pub enum CockpitCmd {
    /// Submit an order for `token` (venue `"polymarket"`): a market taker (`price == None`) or a
    /// resting limit (`price == Some`). `side` is +1 buy YES / −1 sell YES.
    Submit { token: String, side: i32, price: Option<f64>, qty: f64 },
    /// Cancel a resting order by client-order-id (the ladder's inline ✕).
    Cancel(String),
}

/// A 0..1 Polymarket probability rounded to its integer-cent ladder rung.
pub fn price_to_cents(price: f64) -> i64 {
    (price * 100.0).round() as i64
}

/// Bucket a book's `(price, qty)` levels onto the 1..=99 cent ladder, summing every level that
/// rounds to the same cent. Out-of-range rungs (`0` and `100`, i.e. a price that rounds to a
/// resolved outcome) are DROPPED — the ladder only paints live probability rungs. Ascending by
/// cent (`BTreeMap` order), which is the order the ladder widget expects.
pub fn bucket_prob_levels(
    bids: &[vike_model::Level],
    asks: &[vike_model::Level],
) -> Vec<vike_cockpit::ProbLevel> {
    let mut by_cent: std::collections::BTreeMap<i64, (f64, f64)> =
        std::collections::BTreeMap::new();
    for &(px, qty) in bids {
        let c = price_to_cents(px);
        if (1..=99).contains(&c) {
            by_cent.entry(c).or_default().0 += qty;
        }
    }
    for &(px, qty) in asks {
        let c = price_to_cents(px);
        if (1..=99).contains(&c) {
            by_cent.entry(c).or_default().1 += qty;
        }
    }
    by_cent
        .into_iter()
        .map(|(price_cents, (bid_size, ask_size))| vike_cockpit::ProbLevel {
            price_cents,
            bid_size,
            ask_size,
        })
        .collect()
}

/// The Polymarket up/down scalp cockpit body (`WinKind::Polymarket`). Stacks the four `vike-cockpit`
/// widgets over ONE token's live L2 book: the window-chain rail, the Price-to-Beat header, the
/// one-click ticket, and the probability ladder (fills the remainder). [`ToolCtx::book`] carries the
/// per-window transport: `symbol` is the YES-outcome token-id, `venue` is `"polymarket"`, `book` is
/// that token's live book from the shared `data_sink::BookStore` (`None` until the first snapshot
/// lands ⇒ `stale`). Ladder and ticket order intents leave as `vike_cockpit` actions (into
/// `tv.cockpit_*_actions`) the window loop maps to `vike_exec::Command`s — the same seam as
/// [`super::dom_tool_content`].
///
/// Inputs the app cannot yet source are fed NEUTRAL (and flagged in the crate report): the CURRENT
/// window's rail card carries the live book's up/dn odds, but FUTURE windows' odds and all per-window
/// volume stay `None` (their markets aren't resolved yet — full per-window Gamma resolution is a
/// follow-up), and the header's reference/spot underlying is `0.0` (no BTC/ETH spot feed is threaded
/// to the cockpit yet).
pub fn cockpit_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    let snap = ctx.snap;
    let token = ctx.book.symbol;
    let venue = ctx.book.venue;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let real = ctx.book.book.filter(|b| b.bid_levels() + b.ask_levels() > 0);

    // Header label for the rail/ladder: the resolved market NAME when the Gamma resolver has cached
    // one for this token (`poly_names`, keyed by token-id), else the elided token-id — the raw
    // 78-digit id is unreadable. The seeded `VIKE_POLY_COCKPIT_TOKEN` case (no Gamma resolve ran)
    // has no entry and keeps the elided token. The full id stays the routing key everywhere below
    // (order match, subscribe seam).
    let asset_label = ctx.poly_names.get(token).cloned().unwrap_or_else(|| poly_short_label(token));

    // YES/NO quote from the single YES-token book: buying YES pays the best ask; buying NO ≈ 1 − best
    // bid (selling YES). `None` while the relevant side is empty.
    let up_price = real.and_then(|b| b.best_ask()).map(|(p, _)| p);
    let dn_price = real.and_then(|b| b.best_bid()).map(|(p, _)| 1.0 - p);

    // --- window-chain rail: the deterministic 5-minute up/down chain around `now`. Only the CURRENT
    // window (index 0) maps to the token whose live book is in scope here, so it carries live up/dn
    // odds; every FUTURE window resolves to a market not yet created, so it stays `None` (correct —
    // full per-window Gamma resolution is a follow-up, deliberately NOT a fetch loop in the render
    // path). Per-window volume is likewise unsourced → `None`. ---
    let chain = vike_cockpit::chain_windows(now_ms, POLY_WINDOW_SECS, POLY_CHAIN_LOOKAHEAD);
    let cards: Vec<vike_cockpit::WindowCard> = chain
        .iter()
        .enumerate()
        .map(|(i, w)| vike_cockpit::WindowCard {
            open_ms: w.open_ms,
            up_price: if i == 0 { up_price } else { None },
            dn_price: if i == 0 { dn_price } else { None },
            volume: None,
        })
        .collect();
    let rail_inputs = vike_cockpit::ChainRailInputs {
        asset: &asset_label,
        cards: &cards,
        window_secs: POLY_WINDOW_SECS,
        now_ms,
    };
    // The selection is latched into `tv.cockpit_chain`; a window pick emits no core action (it only
    // re-routes which window the cockpit shows — a follow-up once per-window markets are wired).
    let _ = vike_cockpit::draw_chain_rail(ui, &mut tv.cockpit_chain, &rail_inputs);

    // --- Price-to-Beat header. reference_price/spot need the UNDERLYING (BTC/ETH) spot, not the
    // Polymarket book — no such feed is threaded here yet, so both are fed 0.0 (neutral). ---
    let resolution_ts = vike_cockpit::window_close_ms(
        vike_cockpit::window_open_ms(now_ms, POLY_WINDOW_SECS),
        POLY_WINDOW_SECS,
    );
    let ptb = vike_cockpit::PtbInputs {
        reference_price: 0.0,
        spot: 0.0,
        resolution_ts,
        now_ms,
        up_price,
        dn_price,
    };
    vike_cockpit::draw_ptb_header(ui, &ptb);

    // --- one-click ticket (drawn above the ladder so the ladder fills the remaining height). ---
    let ticket_inputs =
        vike_cockpit::TicketInputs { up_price, dn_price, up_win_payout: None, dn_win_payout: None };
    let ticket_acts = vike_cockpit::draw_ticket(ui, &mut tv.cockpit_ticket, &ticket_inputs);
    tv.cockpit_ticket_actions.extend(ticket_acts);

    // --- probability ladder over the token book: bucket each 0..1 level to its integer cent. ---
    let (bids, asks) = real.map(|b| b.top_n(64)).unwrap_or_default();
    let levels = bucket_prob_levels(&bids, &asks);
    let inside_bid = real.and_then(|b| b.best_bid()).map(|(p, _)| price_to_cents(p));
    let inside_ask = real.and_then(|b| b.best_ask()).map(|(p, _)| price_to_cents(p));
    let orders: Vec<vike_cockpit::ProbOrder> = snap
        .orders
        .iter()
        .filter(|o| o.venue == venue && o.symbol == token)
        .map(|o| vike_cockpit::ProbOrder {
            client_order_id: o.client_order_id.clone(),
            side: o.side,
            price_cents: price_to_cents(o.price.or(o.trigger_price).unwrap_or(0.0)),
            qty: o.qty,
            marker: vike_cockpit::ProbMarker::Resting,
        })
        .collect();
    let ladder_inputs = vike_cockpit::ProbLadderInputs {
        asset: &asset_label,
        levels: &levels,
        orders: &orders,
        inside_bid,
        inside_ask,
        stale: ctx.book.stale,
    };
    let ladder_acts = vike_cockpit::draw_ladder(ui, &mut tv.cockpit_ladder, &ladder_inputs);
    tv.cockpit_ladder_actions.extend(ladder_acts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_to_cents_rounds_to_the_nearest_rung() {
        assert_eq!(price_to_cents(0.0), 0);
        assert_eq!(price_to_cents(0.014), 1);
        assert_eq!(price_to_cents(0.5), 50);
        assert_eq!(price_to_cents(0.996), 100);
    }

    #[test]
    fn bucket_prob_levels_sums_levels_that_share_a_cent() {
        // 0.501 and 0.5049 both round to rung 50 → one summed row.
        let bids = [(0.501, 10.0), (0.5049, 5.0), (0.49, 3.0)];
        let levels = bucket_prob_levels(&bids, &[]);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].price_cents, 49);
        assert_eq!(levels[0].bid_size, 3.0);
        assert_eq!(levels[1].price_cents, 50);
        assert_eq!(levels[1].bid_size, 15.0);
        assert_eq!(levels[1].ask_size, 0.0);
    }

    #[test]
    fn bucket_prob_levels_merges_both_sides_onto_one_rung() {
        let levels = bucket_prob_levels(&[(0.42, 7.0)], &[(0.42, 2.0), (0.43, 4.0)]);
        assert_eq!(levels.len(), 2);
        assert_eq!((levels[0].price_cents, levels[0].bid_size, levels[0].ask_size), (42, 7.0, 2.0));
        assert_eq!((levels[1].price_cents, levels[1].bid_size, levels[1].ask_size), (43, 0.0, 4.0));
    }

    #[test]
    fn bucket_prob_levels_drops_the_resolved_ends() {
        // rung 0 and rung 100 are not tradable probability rungs — the ladder never paints them.
        let levels = bucket_prob_levels(&[(0.001, 9.0)], &[(0.9999, 9.0)]);
        assert!(levels.is_empty());
    }

    #[test]
    fn bucket_prob_levels_of_an_empty_book_is_empty() {
        assert!(bucket_prob_levels(&[], &[]).is_empty());
    }

    #[test]
    fn levels_come_back_ascending_by_cent() {
        let levels = bucket_prob_levels(&[(0.90, 1.0), (0.10, 1.0), (0.50, 1.0)], &[]);
        let cents: Vec<i64> = levels.iter().map(|l| l.price_cents).collect();
        assert_eq!(cents, vec![10, 50, 90]);
    }
}
