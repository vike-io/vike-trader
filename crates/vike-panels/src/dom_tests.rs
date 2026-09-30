use super::*;

#[test]
fn row_key_price_roundtrip_group1() {
    let tick = 0.5;
    // group 1: each key spans one tick; price snaps to the tick grid
    let k = row_key(62_804.0, tick, 1);
    assert_eq!(key_price(k, tick, 1), 62_804.0);
    // a price mid-tick rounds to the nearest tick key
    assert_eq!(row_key(62_804.2, tick, 1), row_key(62_804.0, tick, 1));
}

#[test]
fn row_key_groups_ticks_into_buckets() {
    let tick = 1.0;
    // group 10: prices 100..109 all fall in the same bucket; 110 is the next
    let base = row_key(100.0, tick, 10);
    for p in 100..110 {
        assert_eq!(row_key(p as f64, tick, 10), base, "price {p} should share the bucket");
    }
    assert_eq!(row_key(110.0, tick, 10), base + 1);
    // the bucket's aligned edge price
    assert_eq!(key_price(base, tick, 10), 100.0);
}

#[test]
fn group_book_sums_qty_into_rows() {
    let tick = 1.0;
    let levels: Vec<BookLevel> = vec![
        BookLevel::new(100.0, 1.0),
        BookLevel::new(101.0, 2.0),
        BookLevel::new(105.0, 4.0),
        BookLevel::new(109.0, 8.0),
    ];
    // group 10 → all four collapse into one row summing to 15
    let m = group_book(&levels, tick, 10);
    assert_eq!(m.len(), 1);
    let key = row_key(100.0, tick, 10);
    assert_eq!(m.get(&key).copied(), Some(15.0));
    // group 5 → {100,101} and {105,109} split into two rows
    let m5 = group_book(&levels, tick, 5);
    assert_eq!(m5.len(), 2);
    assert_eq!(m5.get(&row_key(100.0, tick, 5)).copied(), Some(3.0));
    assert_eq!(m5.get(&row_key(105.0, tick, 5)).copied(), Some(12.0));
}

#[test]
fn resolve_is_stop_side_bound() {
    let (bid, ask) = (100.0, 101.0);
    // buy below/at the ask = limit; buy above the ask = stop
    assert!(!resolve_is_stop(1, 100.0, bid, ask, false));
    assert!(!resolve_is_stop(1, 101.0, bid, ask, false));
    assert!(resolve_is_stop(1, 102.0, bid, ask, false));
    // sell above/at the bid = limit; sell below the bid = stop
    assert!(!resolve_is_stop(-1, 101.0, bid, ask, false));
    assert!(!resolve_is_stop(-1, 100.0, bid, ask, false));
    assert!(resolve_is_stop(-1, 99.0, bid, ask, false));
}

#[test]
fn resolve_is_stop_shift_forces_stop() {
    let (bid, ask) = (100.0, 101.0);
    // Shift forces a stop even where the auto rule says limit
    assert!(resolve_is_stop(1, 100.0, bid, ask, true));
    assert!(resolve_is_stop(-1, 101.0, bid, ask, true));
}

#[test]
fn col_at_thirds() {
    // left=0, width=300 → [0,102)=Bid, [102,198)=Price, [198,300)=Ask
    assert_eq!(col_at(10.0, 0.0, 300.0), Col::Bid);
    assert_eq!(col_at(150.0, 0.0, 300.0), Col::Price);
    assert_eq!(col_at(280.0, 0.0, 300.0), Col::Ask);
    // clamped at the edges
    assert_eq!(col_at(-50.0, 0.0, 300.0), Col::Bid);
    assert_eq!(col_at(9999.0, 0.0, 300.0), Col::Ask);
}

#[test]
fn heatmap_ring_bounds_and_peak() {
    let mut h = DomHeatmap { max_cols: 4, ..Default::default() };
    for i in 0..10 {
        h.push(vec![i as f32, (i * 2) as f32]);
    }
    // only the last 4 columns are retained
    assert_eq!(h.len(), 4);
    // peak ratchets to the global max seen (9*2 = 18)
    assert_eq!(h.peak, 18.0);
    // oldest surviving column is index 6 → [6, 12]
    assert_eq!(h.cols.front().unwrap(), &vec![6.0, 12.0]);
}

#[test]
fn group_steps_navigation() {
    assert_eq!(next_group(1), 2);
    assert_eq!(next_group(5), 10);
    assert_eq!(next_group(50), 50); // clamps at the top
    assert_eq!(prev_group(10), 5);
    assert_eq!(prev_group(1), 1); // clamps at the bottom
}

/// Pins the rendered strings, INCLUDING the divergences that keep this formatter local
/// instead of swapping to `vike_ui_theme::fmt::fmt_compact` (GUI audit F7 — a swap would
/// change rendered text).
#[test]
fn fmt_qty_tiers_decimals_by_size() {
    // sub-unit qtys keep four decimals (the toolbar presets) — fmt_compact would print "0"
    assert_eq!(fmt_qty(0.001), "0.0010");
    assert_eq!(fmt_qty(0.05), "0.0500");
    assert_eq!(fmt_qty(0.1), "0.1000");
    // unit-scale: two decimals; signed for position sizes
    assert_eq!(fmt_qty(2.5), "2.50");
    assert_eq!(fmt_qty(-2.5), "-2.50");
    // large: plain digits, never a K/M compaction — fmt_compact would print "1.23K"
    assert_eq!(fmt_qty(1000.0), "1000");
    assert_eq!(fmt_qty(1234.0), "1234");
}

/// The tick-aware price formatter has no shared-fmt twin (GUI audit F7's named KEEP) — its
/// decimal count follows the venue tick, pinned per tier.
#[test]
fn fmt_px_follows_tick_decimals() {
    assert_eq!(fmt_px(100.0, 1.0), "100"); // tick ≥ 1 → integer prices
    assert_eq!(fmt_px(62_804.5, 0.5), "62804.5"); // tick ≥ 0.1 → one decimal
    assert_eq!(fmt_px(62_804.25, 0.01), "62804.25"); // tick ≥ 0.01 → two decimals
    assert_eq!(fmt_px(0.62, 0.01), "0.62");
    assert_eq!(fmt_px(1.0625, 0.0001), "1.0625"); // finer → four decimals
}

/// The DOM's drag-to-reprice gate (audit br6): the widget offers the control ONLY when the
/// venue's declared caps wire a native modify. A modify-less venue (or the unknown/default
/// caps) blocks it — the exact condition that stops an unsupported `Command::Modify`.
#[test]
fn drag_to_reprice_gate_follows_caps() {
    // venues whose adapters wire a native amend → the drag control is offered
    assert!(drag_to_reprice_allowed(&vike_model::venue_caps::BINANCE));
    assert!(drag_to_reprice_allowed(&vike_model::venue_caps::BYBIT));
    assert!(drag_to_reprice_allowed(&vike_model::venue_caps::OKX));
    // no native modify → blocked, so the marker is greyed and no Modify is emitted
    assert!(!drag_to_reprice_allowed(&vike_model::venue_caps::OANDA));
    assert!(!drag_to_reprice_allowed(&vike_model::venue_caps::POLYMARKET));
    assert!(!drag_to_reprice_allowed(&VenueCaps::UNSUPPORTED));
}

// ---- cost-to-fill (opt-in footer readout) ----

/// asks 100@1, 101@2, 102@3 ; bids 99@1, 98@2, 97@3 ⇒ mid 99.5, tick 1.0
fn c2f_book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    b
}

#[test]
fn cost_to_fill_walks_both_sides() {
    let c = cost_to_fill(&c2f_book(), 3.0);
    assert_eq!(c.qty, 3.0);
    // BUY 3 = 100×1 + 101×2 = 302 / 3
    let b = c.buy.expect("asks can fill 3");
    assert!((b.avg_px - 302.0 / 3.0).abs() < 1e-12, "avg {}", b.avg_px);
    assert_eq!((b.worst_px, b.filled, b.complete, b.levels), (101.0, 3.0, true, 2));
    // slippage vs mid 99.5, positive = worse for the taker
    let bs = b.slippage_bps.unwrap();
    assert!((bs - (302.0 / 3.0 - 99.5) / 99.5 * 10_000.0).abs() < 1e-9, "bps {bs}");
    // SELL 3 = 99×1 + 98×2 = 295 / 3, also positive (below mid)
    let s = c.sell.expect("bids can fill 3");
    assert!((s.avg_px - 295.0 / 3.0).abs() < 1e-12, "avg {}", s.avg_px);
    assert_eq!((s.worst_px, s.complete, s.levels), (98.0, true, 2));
    assert!(s.slippage_bps.unwrap() > 0.0, "a sell below mid costs the taker");
}

#[test]
fn cost_to_fill_flags_partial_and_empty() {
    let book = c2f_book();
    // 6 is exactly the displayed depth per side; 9 overruns it
    assert!(cost_to_fill(&book, 6.0).buy.unwrap().complete);
    let over = cost_to_fill(&book, 9.0);
    let b = over.buy.unwrap();
    assert!(!b.complete && b.filled == 6.0 && b.levels == 3, "{b:?}");
    assert!(!over.sell.unwrap().complete);
    // empty book / non-positive qty ⇒ nothing to show on either side
    let empty = cost_to_fill(&L2Book::new(1.0), 1.0);
    assert!(empty.buy.is_none() && empty.sell.is_none());
    let zero = cost_to_fill(&book, 0.0);
    assert!(zero.buy.is_none() && zero.sell.is_none());
}

#[test]
fn cost_to_fill_one_sided_book_has_no_slippage() {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(1, &[], &[BookLevel::new(100.0, 5.0)]);
    let c = cost_to_fill(&b, 2.0);
    let buy = c.buy.expect("asks fill");
    assert_eq!(buy.avg_px, 100.0);
    assert_eq!(buy.slippage_bps, None, "no mid ⇒ no slippage number, never a fabricated 0");
    assert!(c.sell.is_none(), "no bids ⇒ nothing to sell into");
}

/// REGRESSION (review minor): a partial walk must not read as a finite, affordable cost —
/// the gate denies that same order as not-fillable.
#[test]
fn cost_label_flags_a_partial_walk_as_unbounded() {
    let book = c2f_book();
    let over = cost_to_fill(&book, 9.0);
    let (txt, hover) = cost_label("B", 9.0, over.buy);
    assert!(txt.contains('∞'), "partial walk must not show a finite bp figure: {txt}");
    assert!(!txt.contains("bp"), "no bps at all for an unbounded cost: {txt}");
    assert!(hover.contains("not-fillable"), "hover must name the gate verdict: {hover}");
    // a COMPLETE walk still shows the number, and says it is the gate's number
    let (txt, hover) = cost_label("B", 3.0, cost_to_fill(&book, 3.0).buy);
    assert!(txt.contains("bp") && !txt.contains('∞'), "{txt}");
    assert!(hover.contains("max_slippage_bps"), "{hover}");
    // empty side
    let (txt, _) = cost_label("S", 1.0, None);
    assert_eq!(txt, "S —");
}

/// The readout and `vike_exec::impact_veto` must judge the same number. vike-chart cannot
/// depend on vike-exec, so this pins the shared primitive both go through.
#[test]
fn cost_to_fill_matches_the_gates_simulate_fill() {
    let book = c2f_book();
    for qty in [1.0, 3.0, 6.0, 9.0] {
        for side in [1, -1] {
            let sim = book.simulate_fill(side, qty);
            let c = cost_to_fill(&book, qty);
            let got = if side > 0 { c.buy } else { c.sell };
            match (got, sim.avg_px) {
                (Some(g), Some(avg)) => {
                    assert_eq!(g.avg_px, avg, "qty {qty} side {side}");
                    assert_eq!(g.slippage_bps, sim.slippage_bps_vs_mid);
                    // this is the exact predicate `impact_veto` denies NotFillable on
                    assert_eq!(g.complete, sim.remaining == 0.0 && sim.total_filled > 0.0);
                }
                (None, None) => {}
                other => panic!("readout/gate disagree at qty {qty} side {side}: {other:?}"),
            }
        }
    }
}

/// One row per strip: the DOM the design system inherited, and what a wide window still gets.
const ONE_ROW_EACH: Rows =
    Rows { header: 1, toolbar: 1, footer: FooterRows { readouts: 1, buttons: 0 } };

/// Each strip is the density's control height plus an inset (the source strip and the ladder row
/// are its row height), so a kit control fills a strip exactly. At Normal the four strips are the
/// heights the DOM had before the design system; only the ladder's row moves, 20 → 18.
#[test]
fn the_strips_follow_the_density_and_normal_keeps_todays_heights() {
    let s = |d: Density| {
        let s = strips(&d.metrics(), ONE_ROW_EACH);
        [s.header, s.source, s.toolbar, s.footer, s.row]
    };
    assert_eq!(s(Density::Normal), [26.0, 18.0, 26.0, 28.0, 18.0]);
    assert_eq!(s(Density::Compact), [22.0, 16.0, 22.0, 24.0, 16.0]);
    assert_eq!(s(Density::Comfortable), [30.0, 22.0, 30.0, 32.0, 22.0]);
}

/// A strip of one row is the height it always had, and each row it adds is one control height and
/// one gap — allocated up front, so the ladder region that follows starts below all of them.
#[test]
fn a_strip_grows_by_a_control_and_a_gap_for_each_row_it_adds() {
    let m = Density::Normal.metrics();
    assert_eq!(stack_height(&m, 1, STRIP_INSET), 26.0);
    assert_eq!(stack_height(&m, 1, FOOTER_INSET), 28.0);
    assert_eq!(stack_height(&m, 2, STRIP_INSET), 26.0 + m.control_h + ROW_GAP);
    assert_eq!(stack_height(&m, 3, FOOTER_INSET), 28.0 + 2.0 * (m.control_h + ROW_GAP));
    let s =
        strips(&m, Rows { header: 2, toolbar: 3, footer: FooterRows { readouts: 1, buttons: 2 } });
    assert_eq!([s.source, s.row], [m.row_h, m.row_h], "only the strips that stack grow");
    assert_eq!(
        s.footer,
        stack_height(&m, 3, FOOTER_INSET),
        "the footer's rows are its readouts' and its buttons'"
    );
}

/// The rows each strip uses at the widths that matter, under the default look (a strip is the
/// window's width less the harness's 8 pt margins, so the launcher's 320 pt window is a 304 pt strip).
/// A wide window keeps one row each. The launcher's stacks the header on two rows, the toolbar on
/// three and splits the footer's buttons over two. A footer wide enough for its five buttons keeps
/// them together on a row of their own; the cost readout, when C2F is on, goes on a second readout
/// row where the first cannot hold it.
#[test]
fn the_rows_at_the_widths_that_matter() {
    let m = Density::Normal.metrics();
    let at = |w: f32, cost: bool| rows_for(w, &m, TextSize::Standard, cost);
    let rows = |header, toolbar, readouts, buttons| Rows {
        header,
        toolbar,
        footer: FooterRows { readouts, buttons },
    };
    assert_eq!(at(884.0, false), ONE_ROW_EACH);
    assert_eq!(at(884.0, true), ONE_ROW_EACH);
    assert_eq!(at(640.0, false), ONE_ROW_EACH);
    assert_eq!(at(640.0, true), rows(1, 1, 1, 1), "the cost readout wants the buttons a row apart");
    assert_eq!(at(560.0, false), rows(1, 2, 1, 0));
    assert_eq!(at(400.0, false), rows(1, 2, 1, 1));
    assert_eq!(at(400.0, true), rows(1, 2, 2, 1), "and, narrower, the readouts a second row");
    assert_eq!(at(320.0, false), rows(2, 3, 1, 1));
    assert_eq!(at(304.0, false), rows(2, 3, 1, 2));
    assert_eq!(at(304.0, true), rows(2, 3, 2, 2));
    assert_eq!(
        at(280.0, false),
        rows(2, 3, 1, 2),
        "no narrower than the launcher's stacks further"
    );
}

/// The five sizes are the one toolbar row that cannot be split across rows, so they alone decide
/// whether the toolbar needs a fourth: at the launcher's width they fit a row in every look but the
/// widest, Large text on Comfortable density, where the padding and the text add up to 298 pt against
/// the 284 the row has.
#[test]
fn only_the_widest_look_needs_a_fourth_toolbar_row_at_the_launchers_width() {
    let toolbar = |d: Density, text: TextSize| rows_for(304.0, &d.metrics(), text, false).toolbar;
    assert_eq!(toolbar(Density::Normal, TextSize::Standard), 3);
    assert_eq!(toolbar(Density::Normal, TextSize::Large), 3);
    assert_eq!(toolbar(Density::Comfortable, TextSize::Standard), 3);
    assert_eq!(toolbar(Density::Compact, TextSize::Large), 3);
    assert_eq!(toolbar(Density::Comfortable, TextSize::Large), 4);
}

/// A wider window never needs MORE rows, in any look, with the cost readout on or off: a strip that
/// gained a row as the window widened would flicker on a drag.
#[test]
fn a_wider_window_never_needs_more_rows() {
    for density in Density::ALL {
        for text in TextSize::ALL {
            for cost in [false, true] {
                let m = density.metrics();
                let mut before = rows_for(200.0, &m, text, cost);
                for i in 1..=2_000 {
                    let w = 200.0 + i as f32 * 0.5;
                    let now = rows_for(w, &m, text, cost);
                    assert!(
                        now.header <= before.header
                            && now.toolbar <= before.toolbar
                            && now.footer.readouts <= before.footer.readouts
                            && now.footer.buttons <= before.footer.buttons,
                        "{density:?} / {text:?} / cost {cost}: {before:?} -> {now:?} at {w}"
                    );
                    before = now;
                }
            }
        }
    }
}

/// A looser or larger look has wider controls, so it never keeps a row the default look gives up: the
/// constants are measured at Normal / Standard and scaled for the rest.
#[test]
fn a_looser_or_larger_look_stacks_no_later_than_the_default() {
    let base = Density::Normal.metrics();
    for (density, text) in [
        (Density::Comfortable, TextSize::Standard),
        (Density::Normal, TextSize::Large),
        (Density::Comfortable, TextSize::Large),
    ] {
        let m = density.metrics();
        for w in (200..=900).step_by(2) {
            let w = w as f32;
            let (a, b) =
                (rows_for(w, &base, TextSize::Standard, false), rows_for(w, &m, text, false));
            assert!(
                b.header >= a.header
                    && b.toolbar >= a.toolbar
                    && b.footer.total() >= a.footer.total(),
                "{density:?} / {text:?} stacks later than the default at {w}: {b:?} vs {a:?}"
            );
        }
    }
}

/// The cost-to-fill readout adds width to the footer, so it never gives the footer fewer rows.
#[test]
fn the_cost_readout_never_gives_the_footer_fewer_rows() {
    let m = Density::Normal.metrics();
    for w in (200..=1_000).step_by(2) {
        let w = w as f32;
        let (off, on) =
            (rows_for(w, &m, TextSize::Standard, false), rows_for(w, &m, TextSize::Standard, true));
        assert!(on.footer.total() >= off.footer.total(), "{off:?} vs {on:?} at {w}");
        assert_eq!((on.header, on.toolbar), (off.header, off.toolbar), "only the footer reads it");
    }
}

/// A LIMIT marker is filled with its side's colour and carries the on-fill letter; a STOP is hollow —
/// the background inside, its side's colour around it and on its letter (owner decision 2). A venue
/// that cannot reprice greys the outline of both kinds (audit br6), and dragging rings either kind
/// in the accent (spec §2: the focus ring).
#[test]
fn a_limit_marker_is_filled_and_a_stop_marker_is_hollow() {
    use vike_ui_theme::appearance::Appearance;
    let t = Tokens::from_appearance(&Appearance::default());
    let (fill, edge, ink) = marker_colours(1, false, true, false, &t);
    assert_eq!((fill, edge.color, ink), (t.market.up, t.theme.text, ON_FILL));
    let (fill, edge, ink) = marker_colours(-1, true, true, false, &t);
    assert_eq!((fill, edge.color, ink), (t.theme.bg, t.market.down, t.market.down_text));
    for stop in [false, true] {
        assert_eq!(marker_colours(1, stop, false, false, &t).1.color, t.theme.text3, "stop={stop}");
        assert_eq!(marker_colours(1, stop, true, true, &t).1.color, t.theme.accent, "stop={stop}");
    }
}

/// The feed word's status (owner decision 4): the DOM's OWN depth link being down is the error
/// red, as it has always been here — unlike the status bar's dots, which have no words and so paint
/// "down" and "unknown" alike.
#[test]
fn feed_down_is_the_error_red_and_an_unknown_link_is_muted() {
    use vike_model::feed_status::ConnectionState as C;
    assert_eq!(source_status(C::Connected), Status::Ok);
    assert_eq!(source_status(C::Connecting), Status::Warning);
    assert_eq!(source_status(C::Disconnected), Status::Error);
    assert_eq!(source_status(C::Error), Status::Error);
    assert_eq!(source_status(C::Unknown), Status::Muted);
}

/// The venue button's hover names every venue the button cycles through. It named four of the five
/// until this PR.
#[test]
fn the_venue_hint_names_every_venue_the_button_cycles() {
    let mut v = DomVenue::Binance;
    loop {
        assert!(
            VENUE_CYCLE_HINT.contains(v.label()),
            "{} missing: {VENUE_CYCLE_HINT:?}",
            v.label()
        );
        v = v.next();
        if v == DomVenue::Binance {
            break;
        }
    }
}

/// The three bookless renderings (owner decision 5, spec §4.2): nothing while connecting — a
/// bookless DOM does not move, so no spinner — the tray when the link is up or not known, the
/// unreachable cloud in the warning colour when it is down.
#[test]
fn a_bookless_dom_shows_no_icon_while_connecting_the_tray_when_up_and_the_cloud_when_down() {
    use vike_model::feed_status::ConnectionState as C;
    use vike_ui_theme::appearance::Appearance;
    let t = Tokens::from_appearance(&Appearance::default());
    assert_eq!(absence_icon(C::Connecting, &t), None);
    for up in [C::Connected, C::Unknown] {
        assert_eq!(
            absence_icon(up, &t).map(|(i, c, _)| (i, c)),
            Some((icons::EMPTY, t.theme.text3))
        );
    }
    for down in [C::Disconnected, C::Error] {
        assert_eq!(
            absence_icon(down, &t).map(|(i, c, _)| (i, c)),
            Some((icons::UNREACHABLE, Status::Warning.color()))
        );
    }
}

#[test]
fn cost_to_fill_defaults_off() {
    let s = DomState::default();
    assert!(!s.cost_to_fill, "the readout must default off (render path unchanged)");
    assert_eq!(s.cost_qty, None, "size follows the toolbar order qty by default");
}
