//! `desktop_reads_the_live_node`, READ-ONLY: what it shows and how to run it are in the parent's
//! module doc.

use eframe::egui::accesskit::Role;
use egui_kittest::kittest::NodeT;

use super::{Desk, FX_TICK, boot, engine_symbols, one_per_process, staged};

#[test]
#[ignore = "LIVE: READ-ONLY, sends nothing; needs the node"]
fn desktop_reads_the_live_node() {
    one_per_process();
    let Some(boot) = boot() else { return };
    let Some((ig, dukascopy, ctrader)) = engine_symbols(boot) else { return };
    // A fresh desktop fetches no catalog by itself (the Data Manager's Refresh does), so the picker
    // and the ticket's lot know nothing of these venues: the rows the order tests stage are staged
    // here too, into the scratch cache only, to show what their Trade windows then say.
    let rows = [
        staged("ig", &ig, 1.0, FX_TICK),
        staged("dukascopy", &dukascopy, 1000.0, FX_TICK),
        staged("ctrader", &ctrader, 1000.0, FX_TICK),
    ];
    let Some(mut d) = Desk::launch(boot) else { return };
    d.stage(&rows);
    let snapshot = d.snapshot();
    for b in &snapshot.portfolio.venues {
        eprintln!(
            "  engine {} {:?} symbol {} extra {:?} tier {:?} {:?}",
            b.venue, b.account, b.symbol, b.extra_symbols, b.tier, b.trading_state
        );
    }
    d.frames(250);
    eprintln!("READ before any window: {}", d.dump());
    eprintln!("READ market-data feeds the datahub mounted: {}", d.feeds());
    let targets = [("ig", ig), ("dukascopy", dukascopy), ("ctrader", ctrader)]
        .map(|(v, s)| (v.to_string(), s));
    eprintln!("READ catalog: {} rows", d.app().symbols_catalog.len());
    for (venue, symbol) in &targets {
        let filter = vike_catalog::CatalogSearchFilter { tab: None, venue: Some(venue.clone()) };
        let rows: Vec<String> = d
            .app()
            .symbols_catalog
            .search(symbol, &filter, 5)
            .iter()
            .map(|i| {
                format!(
                    "{} tick {} lot {} min {}",
                    i.raw_symbol,
                    i.properties.tick_size,
                    i.properties.step_size,
                    i.properties.min_qty
                )
            })
            .collect();
        eprintln!("READ {venue} {symbol}: catalog rows {rows:?}");
        eprintln!(
            "READ {venue} {symbol}: stored 1m price (close, age) {:?}",
            d.stored_price(venue, symbol)
        );
        let opened =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.open_trade(venue, symbol)));
        let Ok(w) = opened else {
            eprintln!(
                "READ {venue} {symbol}: no Trade window reaches it (the panic above says why)"
            );
            continue;
        };
        d.frames(100);
        let rect = d.win_rect(w);
        // The ticket as a trader sets up a limit order (clicks only, nothing is sent): where the
        // Price field is, against the window's own rect.
        d.click(Some(rect), "Limit", |l| l == "Limit");
        eprintln!("READ {venue} {symbol}: window {rect:?}; {}", d.ui_dump());
        let book = d
            .app()
            .books
            .get(venue, symbol)
            .map(|(b, _)| (b.best_bid().map(|l| l.price), b.best_ask().map(|l| l.price)));
        let buttons: Vec<String> = d
            .find(Some(Role::Button), Some(rect), |l| {
                l.starts_with("Buy ") || l == "TP/SL" || l == "Bid"
            })
            .iter()
            .map(|n| {
                format!(
                    "{} (disabled {})",
                    n.accesskit_node().label().unwrap_or_default(),
                    n.accesskit_node().is_disabled()
                )
            })
            .collect();
        eprintln!("READ {venue} {symbol}: book {book:?}; strip `{}`; {buttons:?}", d.strip(w));
    }
    eprintln!("READ market-data feeds after the windows: {}", d.feeds());
    d.shut();
}
