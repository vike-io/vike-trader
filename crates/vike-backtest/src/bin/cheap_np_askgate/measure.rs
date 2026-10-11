//! Scoring ONE entry at ONE anchor in every lane, and the per-entry CSV row each outcome appends.

use vike_data::cheap_np_book::ask_ladder_at;
use vike_model::book_taker_price;
use vike_model::fair::UPDOWN_WINDOW_SECS;
use vike_model::{BookLevel, BookUpdate, QuoteTick};
use vike_strategy::strategies::cheap_np_ask::{edge_at, price_at_edge, prob_wc};

use super::lane::{l1_ask_at, live_law_price, rested_order_crossed};
use super::{Anchor, Entry, Lane, as_book};

/// Score ONE entry at ONE anchor, in all three lanes.
#[expect(clippy::too_many_arguments)]
pub(super) fn measure(
    agg: &mut Anchor,
    books: &[BookUpdate],
    l1: &[QuoteTick],
    cutoff: i64,
    strict: bool,
    e: &Entry,
    theta: f64,
    size: f64,
    delay_ms: i64,
    label: &str,
    live_ms: &[i64],
    per_entry: &mut Vec<String>,
) {
    // Lane 1 — the PRINT gate: today's published behaviour, and the denominator everything else is
    // compared against. It needs no book at all, which is precisely the problem.
    let pw = prob_wc(e.ask, e.edge);
    agg.print.take(e.ask, size, e, pw);

    // Lane 4..n — the LIVE bot's own law (see `live_law_price`). Scored FIRST and independently of
    // the θ lanes: the live bot never consults θ at execution time, so its lanes must not inherit
    // any of the ask gate's early returns.
    if agg.live.is_empty() {
        agg.live = live_ms.iter().map(|&ms| (ms, Lane::default())).collect();
        agg.live_l1 = live_ms.iter().map(|&ms| (ms, Lane::default())).collect();
    }
    let mut live_px: Vec<f64> = Vec::with_capacity(live_ms.len() * 2);
    let mut live_l1_px: Vec<f64> = Vec::with_capacity(live_ms.len());
    for (i, &ms) in live_ms.iter().enumerate() {
        match l1_ask_at(l1, cutoff + ms) {
            Some(px) => {
                agg.live_l1[i].1.take(px, size, e, pw);
                live_l1_px.push(px);
            }
            None => {
                agg.live_l1[i].1.no_book += 1;
                live_l1_px.push(f64::NAN);
            }
        }
        match live_law_price(books, l1, cutoff + ms, size) {
            None => {
                agg.live[i].1.no_book += 1;
                live_px.push(f64::NAN);
            }
            // An empty ask side is the live bot's 'empty' status — a real miss, not a data gap.
            Some(None) => {
                agg.live[i].1.no_edge += 1;
                live_px.push(f64::NAN);
            }
            Some(Some(px)) => {
                agg.live[i].1.take(px, size, e, pw);
                live_px.push(px);
            }
        }
    }

    live_px.extend_from_slice(&live_l1_px);

    let Some(decision) = ask_ladder_at(books, l1, &[], cutoff, strict) else {
        agg.ask.no_book += 1;
        agg.delayed.no_book += 1;
        push_row(
            per_entry,
            label,
            e,
            f64::NAN,
            f64::NAN,
            "no_book",
            f64::NAN,
            "no_book",
            f64::NAN,
            false,
            size,
            &live_px,
        );
        return;
    };
    let best_ask = decision
        .iter()
        .find(|&&BookLevel { qty: q, .. }| q > 0.0)
        .map(|&BookLevel { price: p, .. }| p)
        .unwrap_or(f64::NAN);
    if best_ask.is_finite() {
        agg.ask_minus_print.push(best_ask - e.ask);
    }
    // Was there anything at all resting at the price the strategy claims it paid?
    let at_print = decision
        .iter()
        .filter(|&&BookLevel { price: p, .. }| p <= e.ask)
        .map(|&BookLevel { qty: q, .. }| q.max(0.0))
        .sum::<f64>();
    if at_print <= 0.0 {
        agg.nothing_at_print_px += 1;
    }

    // The θ-clearing limit — the order the strategy would really send.
    let Some(limit) = price_at_edge(pw, theta) else {
        agg.ask.no_edge += 1;
        agg.delayed.no_edge += 1;
        push_row(
            per_entry,
            label,
            e,
            best_ask,
            f64::NAN,
            "no_edge",
            f64::NAN,
            "no_edge",
            f64::NAN,
            false,
            size,
            &live_px,
        );
        return;
    };

    // Lane 2 — the ASK gate: the ENGINE's own taker law, at the decision book.
    let ask_px = book_taker_price(&as_book(&decision), 1, size, Some(limit));
    match ask_px {
        Some(px) if edge_at(pw, px) > theta => agg.ask.take(px, size, e, pw),
        _ => agg.ask.no_edge += 1,
    }

    // Lane 3 — the VENUE HOLD: same decision, same frozen limit, matched against the book as it
    // stands `delay_ms` later. An order that no longer crosses is BOOKED — it rests.
    let (delay_outcome, delay_px, rested_crossed) = if ask_px.is_none() {
        // Nothing to submit: the ask gate already refused, so the hold never happens.
        agg.delayed.no_edge += 1;
        ("no_edge", f64::NAN, false)
    } else {
        match ask_ladder_at(books, l1, &[], cutoff + delay_ms, false) {
            None => {
                agg.delayed.no_book += 1;
                ("no_book", f64::NAN, false)
            }
            Some(later) => match book_taker_price(&as_book(&later), 1, size, Some(limit)) {
                Some(px) => {
                    agg.delayed.take(px, size, e, pw);
                    ("filled", px, false)
                }
                None => {
                    agg.delayed.rested += 1;
                    // A resting BUY at `limit` is hit when the market comes back to it. Watched to
                    // the window close; can only under-count (see the helper's doc).
                    let crossed = rested_order_crossed(
                        books,
                        l1,
                        cutoff + delay_ms,
                        (e.sts + UPDOWN_WINDOW_SECS) * 1000,
                        limit,
                    );
                    if crossed {
                        agg.delayed.rested_crossed += 1;
                    }
                    ("rested", f64::NAN, crossed)
                }
            },
        }
    };

    push_row(
        per_entry,
        label,
        e,
        best_ask,
        limit,
        if ask_px.is_some() { "filled" } else { "no_edge" },
        ask_px.unwrap_or(f64::NAN),
        delay_outcome,
        delay_px,
        rested_crossed,
        size,
        &live_px,
    );
}

/// Append ONE per-entry row. Every early return in [`measure`] goes through here too, so the CSV
/// carries a row for EVERY (anchor, entry) pair — which is what lets a sharded run (one store per
/// slice of the archive) be aggregated back into exact totals: the reader can trust that a missing
/// row means a missing entry, never a silently-skipped outcome.
#[expect(clippy::too_many_arguments)]
fn push_row(
    per_entry: &mut Vec<String>,
    label: &str,
    e: &Entry,
    best_ask: f64,
    limit: f64,
    ask_outcome: &str,
    ask_px: f64,
    delay_outcome: &str,
    delay_px: f64,
    rested_crossed: bool,
    size: f64,
    live_px: &[f64],
) {
    let mut row = format!(
        "{label},{sts},{ts},{oidx},{print_px:.6},{edge:.6},{won},{tok},{best_ask:.6},{limit:.6},\
         {ask_outcome},{ask_px:.6},{ask_q:.4},{delay_outcome},{delay_px:.6},{delay_q:.4},{crossed}",
        sts = e.sts,
        ts = e.ts_ms,
        oidx = e.oidx,
        print_px = e.ask,
        edge = e.edge,
        won = e.won,
        tok = e.token_id,
        ask_q = if ask_outcome == "filled" { size } else { 0.0 },
        delay_q = if delay_outcome == "filled" { size } else { 0.0 },
        crossed = rested_crossed,
    );
    for px in live_px {
        row.push_str(&format!(",{px:.6}"));
    }
    per_entry.push(row);
}
