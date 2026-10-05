//! The chart window's symbol picker (design: `docs/superpowers/specs/2026-10-04-symbol-search-design.md`):
//! a search field, a switch (All · Spot · Perp), a chip per VENUE that doubles as the source filter,
//! the node's own series, and ONE LINE per underlying with a chip per venue that lists it.
//!
//! It lives here rather than in `vike-desktop` for the reason [`crate::ui::symbol_row`] gives: the
//! desktop binary is excluded from the merge gate's test lane, and everything in this file is pure
//! `egui` painting over [`vike_catalog`]'s model and the kit, so it builds and is TESTED on CI.
//!
//! # Which venues the chips show, and why they are not a list in this file
//!
//! A LOADED chip is a venue the catalog holds spot or perpetual instruments for
//! ([`vike_catalog::venue_counts`]). A COLD chip is a roster venue whose list is public
//! (`catalog_availability`), whose data path can address spot or perpetual (`addressing_for`) and
//! which the catalog holds nothing for — dashed, and a click is [`PickerOut::load_venue`]. The
//! picker never fetches: the shell hands the venue to `CatalogRefresh::request`, the same entry the
//! Data Manager's Refresh button uses, so its cooldown and refusals apply and a start never opens a
//! dial the owner did not ask for (decisions 0062 and 0066).

use egui::{Id, Ui};
use vike_catalog::{
    Catalog, Kind, PickerQuery, Underlying, catalog_availability, kind_of, picker_results,
    venue_counts,
};
use vike_model::AssetClass;
use vike_tradehub_client::wire::WireDirectory;
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::venue_chip::{
    Mark, RowPick, UnderlyingRow, VenueChip, source_chip, underlying_row,
};
use vike_ui_theme::components::{Tokens, input};
use vike_ui_theme::type_scale::TextRole;

use crate::ui::series_follow::PublishedSeries;
use crate::ui::tool_views::venue_label;

/// The picker's width: its search field and its lines take all of it.
pub const PICKER_W: f32 = 470.0;
/// How many lines the picker draws.
const LINES_SHOWN: usize = 60;
/// How tall the list grows before it scrolls.
const LIST_H: f32 = 300.0;

const SCOPES: [Segment<'static, Option<Kind>>; 6] = [
    Segment { value: None, label: "All", why: "Every kind of instrument the picker offers" },
    Segment { value: Some(Kind::Spot), label: "Spot", why: "Crypto spot markets only" },
    Segment { value: Some(Kind::Perp), label: "Perp", why: "Perpetual contracts only" },
    Segment { value: Some(Kind::Forex), label: "Forex", why: "Currency pairs only" },
    Segment { value: Some(Kind::Cfd), label: "CFD", why: "Contracts for difference only" },
    Segment { value: Some(Kind::Stock), label: "Stocks", why: "Stocks and ETFs only" },
];

/// What the picker reads each frame.
#[derive(Clone, Copy)]
pub struct PickerSources<'a> {
    pub catalog: &'a Catalog,
    /// The node's directory, for each venue's own spelling (`venue.title`); `None` before its reply.
    pub directory: Option<&'a WireDirectory>,
    /// The series the connected node publishes, with their intervals.
    pub backend: &'a [PublishedSeries],
    /// The window's own venue, symbol and interval.
    pub venue: &'a str,
    pub symbol: &'a str,
    pub interval: &'a str,
}

/// What a pick sets on the window: the same four fields a catalog hit set before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickedSymbol {
    pub symbol: String,
    pub venue: String,
    pub asset_class: Option<AssetClass>,
    /// Set only by a pick from the node's own series, which carry one.
    pub interval: Option<String>,
}

/// What one frame of the picker answers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PickerOut {
    pub pick: Option<PickedSymbol>,
    /// A cold venue's chip was clicked: the shell asks `CatalogRefresh` for its list.
    pub load_venue: Option<String>,
}

/// Cross-frame state, in egui's temp memory under the window's id.
#[derive(Clone, Default)]
struct PickerState {
    query: String,
    kind: Option<Kind>,
    venues: Vec<String>,
}

/// The per-venue counts, kept with the catalog length they were counted for.
#[derive(Clone, Default)]
struct CountsMemo {
    len: usize,
    counts: Vec<(String, usize)>,
}

/// Roster venues that are worth a COLD chip: public, addressable as a kind the picker offers, and
/// absent from `loaded`.
fn cold_venues(loaded: &[(String, usize)]) -> Vec<&'static str> {
    vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| catalog_availability(v).is_public())
        .filter(|v| {
            let a = vike_catalog::addressing_for(v);
            AssetClass::ALL.iter().any(|c| a.addresses(*c) && kind_of(*c).is_some())
        })
        .filter(|v| !loaded.iter().any(|(l, _)| l.eq_ignore_ascii_case(v)))
        .collect()
}

/// The chip a click on the LINE opens: the window's own venue when the line lists it, else the first.
fn default_listing(line: &Underlying<'_>, window_venue: &str) -> usize {
    line.listings.iter().position(|l| l.instrument.venue == window_venue).unwrap_or(0)
}

/// Draw the picker into `ui` (the body of the symbol popup) and say what was picked or asked.
pub fn symbol_picker(ui: &mut Ui, id: Id, src: &PickerSources<'_>) -> PickerOut {
    let t = Tokens::of(ui.ctx());
    let mut out = PickerOut::default();
    ui.set_min_width(PICKER_W);
    ui.spacing_mut().text_edit_width = PICKER_W;
    let state_id = id.with("symbol_picker_state");
    let mut st = ui.data_mut(|d| d.get_temp::<PickerState>(state_id).unwrap_or_default());

    let entry = input::text(
        ui,
        &mut st.query,
        input::Field { hint: "Search symbol", ..Default::default() },
    );
    if ui.memory(|m| m.focused().is_none()) {
        entry.request_focus();
    }
    ui.add_space(t.metrics.gap);
    segmented::segmented(ui, &mut st.kind, &SCOPES);
    ui.add_space(t.metrics.gap);

    // The source chips: every venue the catalog holds, then the venues it could load.
    let memo_id = id.with("symbol_picker_counts");
    let counts = match ui.data_mut(|d| d.get_temp::<CountsMemo>(memo_id)) {
        Some(m) if m.len == src.catalog.len() => m.counts,
        _ => {
            let counts = venue_counts(src.catalog);
            let memo = CountsMemo { len: src.catalog.len(), counts: counts.clone() };
            ui.data_mut(|d| d.insert_temp(memo_id, memo));
            counts
        }
    };
    let cold = cold_venues(&counts);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        for (venue, n) in &counts {
            let label = venue_label(src.directory, venue);
            let on = st.venues.iter().any(|v| v.eq_ignore_ascii_case(venue));
            let why = format!("{label}: {n} spot and perpetual listed");
            if source_chip(ui, label, on, false, &why).clicked() {
                if on {
                    st.venues.retain(|v| !v.eq_ignore_ascii_case(venue));
                } else {
                    st.venues.push(venue.clone());
                }
            }
        }
        for venue in &cold {
            let label = venue_label(src.directory, venue);
            let why = format!("{label}: not loaded — click to load its list");
            if source_chip(ui, label, false, true, &why).clicked() {
                out.load_venue = Some((*venue).to_string());
            }
        }
    });
    ui.add_space(t.metrics.gap);

    egui::ScrollArea::vertical().id_salt(id.with("symbol_picker_list")).max_height(LIST_H).show(
        ui,
        |ui| {
            // The node's own series first, unfiltered by the search box: the only place a node's
            // venue is reachable at all, short by construction, and it carries the interval.
            if !src.backend.is_empty() {
                ui.label(
                    egui::RichText::new("On this backend")
                        .font(t.font(TextRole::Caption))
                        .color(t.theme.text3),
                );
                for s in src.backend {
                    let cur = s.venue == src.venue
                        && s.symbol == src.symbol
                        && s.interval == src.interval;
                    if ui.selectable_label(cur, s.label()).clicked() {
                        out.pick = Some(PickedSymbol {
                            symbol: s.symbol.clone(),
                            venue: s.venue.clone(),
                            asset_class: None,
                            interval: Some(s.interval.clone()),
                        });
                    }
                }
                ui.separator();
            }
            let query = PickerQuery { text: &st.query, kind: st.kind, venues: &st.venues };
            let lines = picker_results(src.catalog, &query, LINES_SHOWN);
            if lines.is_empty() {
                ui.label(
                    egui::RichText::new(no_match_words(&st, &counts, &cold))
                        .font(t.font(TextRole::Body))
                        .color(t.theme.text2),
                );
            }
            for line in &lines {
                let whys: Vec<String> = line
                    .listings
                    .iter()
                    .map(|l| {
                        let label = venue_label(src.directory, &l.instrument.venue);
                        match l.others {
                            0 => label.to_string(),
                            n => format!("{label} · {n} more match"),
                        }
                    })
                    .collect();
                let chips: Vec<VenueChip<'_>> = line
                    .listings
                    .iter()
                    .zip(&whys)
                    .map(|(l, why)| VenueChip {
                        label: venue_label(src.directory, &l.instrument.venue),
                        mark: Mark::Hidden,
                        current: l.instrument.venue == src.venue
                            && l.instrument.raw_symbol.eq_ignore_ascii_case(src.symbol),
                        cold: false,
                        why,
                    })
                    .collect();
                let heading = line.heading();
                let row = UnderlyingRow {
                    heading: &heading,
                    kind: line.kind.label(),
                    chips: &chips,
                    hot: false,
                    current: chips.iter().any(|c| c.current),
                };
                let picked = match underlying_row(ui, (id, "line", &heading, line.kind), &row) {
                    Some(RowPick::Chip(i)) => Some(i),
                    Some(RowPick::Row) => Some(default_listing(line, src.venue)),
                    None => None,
                };
                if let Some(l) = picked.and_then(|i| line.listings.get(i)) {
                    out.pick = Some(PickedSymbol {
                        symbol: l.instrument.raw_symbol.clone(),
                        venue: l.instrument.venue.clone(),
                        asset_class: Some(l.instrument.asset_class),
                        interval: None,
                    });
                }
            }
        },
    );
    if out.pick.is_some() {
        st.query.clear();
    }
    ui.data_mut(|d| d.insert_temp(state_id, st));
    out
}

/// What the list says when nothing matches: never a bare "nothing" while venues are cold — a fresh
/// install holds one venue, and the trader is told the others exist and how to load them.
fn no_match_words(st: &PickerState, loaded: &[(String, usize)], cold: &[&str]) -> String {
    let q = st.query.trim();
    let what = st.kind.map_or("symbol", |k| k.label());
    let mut words = if q.is_empty() {
        format!("No {what} to list.")
    } else {
        format!("No {what} matches \u{201C}{q}\u{201D}.")
    };
    if !st.venues.is_empty() {
        words.push_str(" The venue filter is on.");
    }
    if loaded.is_empty() {
        words.push_str(" No venue list is loaded yet.");
    }
    if !cold.is_empty() {
        words.push_str(&format!(
            " {} venue{} not loaded: click a dashed chip to load it.",
            cold.len(),
            if cold.len() == 1 { " is" } else { "s are" }
        ));
    }
    words
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use vike_catalog::Instrument;
    use vike_ui_theme::appearance::Appearance;

    use super::*;

    fn inst(venue: &str, sym: &str, base: &str, quote: &str, class: AssetClass) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: sym.into(),
            asset_class: class,
            base: base.into(),
            quote: quote.into(),
            description: String::new(),
            properties: Default::default(),
            contract_type: None,
            settle_asset: None,
        }
    }

    fn catalog(items: Vec<Instrument>) -> Catalog {
        Catalog::from_instruments(items)
    }

    /// Everything one harness frame reads, and the answers it collected.
    struct Fixture {
        catalog: Catalog,
        backend: Vec<PublishedSeries>,
        venue: &'static str,
        symbol: &'static str,
        out: Rc<RefCell<Vec<PickerOut>>>,
    }

    fn harness(f: Fixture) -> Harness<'static, Fixture> {
        Harness::builder().with_size(egui::vec2(560.0, 520.0)).build_ui_state(
            |ui, f: &mut Fixture| {
                if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &Appearance::default()) {
                    return;
                }
                ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
                let src = PickerSources {
                    catalog: &f.catalog,
                    directory: None,
                    backend: &f.backend,
                    venue: f.venue,
                    symbol: f.symbol,
                    interval: "1m",
                };
                let out = symbol_picker(ui, Id::new("picker"), &src);
                if out != PickerOut::default() {
                    f.out.borrow_mut().push(out);
                }
            },
            f,
        )
    }

    fn fixture(items: Vec<Instrument>) -> Fixture {
        Fixture {
            catalog: catalog(items),
            backend: Vec::new(),
            venue: "okx",
            symbol: "BTC-USDT-SWAP",
            out: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn btc() -> Vec<Instrument> {
        vec![
            inst("bybit", "BTCUSDT.P", "BTC", "USDT", AssetClass::CryptoPerp),
            inst("binance", "BTCUSDT.P", "BTC", "USDT", AssetClass::CryptoPerp),
            inst("binance", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot),
        ]
    }

    /// Every loaded venue is a chip, and a chip filters: with Binance on, Bybit's listing is gone
    /// from the line.
    #[test]
    fn the_picker_lists_every_loaded_venue_as_a_chip_and_filters_by_them() {
        let mut h = harness(fixture(btc()));
        h.run();
        assert!(h.query_by_label("bybit: 1 spot and perpetual listed").is_some());
        assert!(h.query_by_label("binance: 2 spot and perpetual listed").is_some());
        assert!(h.query_by_label("BTC perpetual").is_some());
        assert_eq!(h.query_all_by_label("bybit").count(), 1, "Bybit's chip on the perpetual line");
        h.get_by_label("binance: 2 spot and perpetual listed").click();
        h.run();
        assert_eq!(h.query_all_by_label("bybit").count(), 0, "the filter hid Bybit's listing");
        assert_eq!(h.query_all_by_label("binance").count(), 2, "Binance on both lines");
    }

    /// A cold public venue is a dashed chip, and a click asks to load it — the picker fetches
    /// nothing itself.
    #[test]
    fn a_cold_public_venue_is_a_chip_and_a_click_asks_to_load_it() {
        let mut h = harness(fixture(btc()));
        h.run();
        let why = "aster: not loaded — click to load its list";
        assert!(h.query_by_label(why).is_some(), "aster is a public venue the catalog lacks");
        h.get_by_label(why).click();
        h.run();
        let asked: Vec<String> =
            h.state().out.borrow().iter().filter_map(|o| o.load_venue.clone()).collect();
        assert_eq!(asked, ["aster"]);
    }

    /// Review Focus 3: a fresh install holds ONE venue. A search that finds nothing there says what
    /// is loaded and how to load the rest — never a bare "nothing".
    #[test]
    fn a_fresh_install_says_the_other_venues_are_not_loaded() {
        let only_deribit =
            vec![inst("deribit", "BTC-PERPETUAL", "BTC", "USD", AssetClass::CryptoPerp)];
        let mut h = harness(fixture(only_deribit));
        h.run();
        h.event(egui::Event::Text("zzz".to_string()));
        h.run();
        assert!(h.query_by_label_contains("No symbol matches").is_some());
        assert!(h.query_by_label_contains("not loaded: click a dashed chip").is_some());
    }

    /// The node's own series stay above the results and carry their interval.
    #[test]
    fn on_this_backend_stays_above_the_results_with_its_interval() {
        let mut f = fixture(btc());
        f.backend = vec![PublishedSeries::new("bybit", "BTCUSDT", "5m")];
        let out = f.out.clone();
        let mut h = harness(f);
        h.run();
        assert!(h.query_by_label("On this backend").is_some());
        h.get_by_label_contains("5m").click();
        h.run();
        let picks: Vec<PickedSymbol> = out.borrow().iter().filter_map(|o| o.pick.clone()).collect();
        assert_eq!(
            picks,
            [PickedSymbol {
                symbol: "BTCUSDT".into(),
                venue: "bybit".into(),
                asset_class: None,
                interval: Some("5m".into()),
            }]
        );
    }

    /// A pick carries the symbol, the venue and the asset class of the instrument picked — the
    /// fields the window routes its feed by.
    #[test]
    fn a_pick_carries_the_symbol_venue_and_asset_class() {
        let f = fixture(btc());
        let out = f.out.clone();
        let mut h = harness(f);
        h.run();
        h.get_by_label("bybit").click();
        h.run();
        let picks: Vec<PickedSymbol> = out.borrow().iter().filter_map(|o| o.pick.clone()).collect();
        assert_eq!(
            picks,
            [PickedSymbol {
                symbol: "BTCUSDT.P".into(),
                venue: "bybit".into(),
                asset_class: Some(AssetClass::CryptoPerp),
                interval: None,
            }]
        );
    }
}
