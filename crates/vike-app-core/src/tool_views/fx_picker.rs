//! The chart window's ƒx indicator picker popup — search + category tabs + the ★ Favourites
//! section + the "✎ My indicators" section (the USER studies a binary installed, if any) + the
//! "On this chart" active-study list (per-study settings ⚙ and foreign source-symbol menu) + the
//! "Add to" pane target selector. Moved verbatim from `vike-app`'s `main.rs` (tool-view extraction
//! batch 2).
//!
//! NOT a tool body — it is chart-window chrome — but it lives in `tool_views` for the same reason
//! the tool bodies do: it is pure `egui` over `vike_app_core::workspace` + `vike_chart` state, so
//! down here it finally compiles (and its pure helpers unit-test) in CI. Two adaptations, both
//! mechanical: `chart_window::search_result_row` is now the shared
//! [`crate::symbol_row::search_result_row`] (it moved with this file), and the thrice-repeated
//! search-match expression became the named, unit-tested [`indicator_matches`].

use crate::symbol_row::search_result_row;
use crate::workspace::{self, DEFAULT_VENUE};
use vike_chart::{chart, indicators};
use vike_ui_theme::palette::ACCENT;

/// The ƒx indicator picker popup (searchable, category tabs). Returns the chosen
/// indicator name; the caller adds it to the window (routed to `w.picker_target` via
/// `add_indicator_to`). `favs` is the global favourites set (part b): the picker reads
/// it to render the ★ Favourites section + per-row star state, and mutates it on a
/// star/unstar click. `catalog` is the cross-venue instrument universe (#269), reused
/// for the per-study foreign "source symbol" picker.
pub fn fx_picker_popup(
    ctx: &egui::Context,
    w: &mut workspace::WinState,
    favs: &mut Vec<String>,
    // Foreign-source study (TradingView "symbol" input): the cross-venue instrument
    // universe (#269), reused verbatim for the per-study "source symbol" picker.
    catalog: &vike_catalog::Catalog,
) -> Option<&'static str> {
    if !w.picker_open {
        return None;
    }
    let mut chosen: Option<&'static str> = None;
    let mut close = false;
    // Volume-as-indicator: clicking the synthetic "Volume" picker entry turns the
    // volume pane back on (applied after the window closure, like `chosen`).
    let mut add_volume = false;
    // T8: the picker doubles as the "active indicators" list — the ONLY settings
    // entry point for overlays (they have no pane header, unlike oscillators). A ⚙
    // per active row opens its settings dialog. Snapshot (uid, label, source) up front
    // so the render closure below doesn't hold a borrow of `w.indicators` while it
    // also mutates other `w` fields. `source` is this study's foreign-source pair (if
    // any) — displayed on, and edited via, the per-row source button.
    let actives: Vec<(u64, &'static str, Option<indicators::SourceSymbol>)> =
        w.indicators.iter().map(|a| (a.uid, a.spec.pretty, a.source_symbol.clone())).collect();
    let mut open_settings: Option<u64> = None;
    // Part (a): the existing study panes this add can target (ordinal-numbered "Pane N"
    // in the "Add to" selector). Snapshotted before the closure since it borrows `w`
    // immutably while the closure mutates other `w` fields.
    let study_panes: Vec<chart::PaneKey> = w.present_study_panes();
    // Foreign-source study: the (uid → new source) change picked this frame, applied
    // after the window closure (the closure borrows `w`, so it can't mutate
    // `w.indicators` directly — same collect-now/apply-after shape as `open_settings`).
    // `Some((uid, None))` clears back to the primary; `Some((uid, Some(pair)))` sets it.
    let mut set_source: Option<(u64, Option<indicators::SourceSymbol>)> = None;
    egui::Window::new("ƒx · Indicators")
        .id(w.id.with("fx_popup"))
        .collapsible(false)
        .resizable(false)
        .default_pos(w.pos + egui::vec2(50.0, 64.0))
        .show(ctx, |ui| {
            ui.set_width(300.0);
            ui.horizontal(|ui| {
                ui.label("🔍");
                ui.add(
                    egui::TextEdit::singleline(&mut w.picker_query)
                        .hint_text("search…")
                        .desired_width(210.0),
                );
                if ui.button("✕").clicked() {
                    close = true;
                }
            });
            if !actives.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new("On this chart").weak());
                for (uid, pretty, source) in &actives {
                    ui.horizontal(|ui| {
                        if ui.small_button("⚙").on_hover_text("Settings").clicked() {
                            open_settings = Some(*uid);
                        }
                        ui.label(egui::RichText::new(*pretty).size(13.0));
                        // Foreign-source study (TradingView "symbol" input): a compact
                        // per-study source button on the right. Label = the current
                        // source ("chart" for the primary, else the source symbol,
                        // venue-tagged when non-Binance); ACCENT-tinted when foreign so
                        // it reads as "active". The menu reuses the #269 symbol search.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (label, tint) = match source {
                                None => {
                                    ("src ▾".to_string(), egui::Color32::from_rgb(120, 128, 140))
                                }
                                Some(s) if s.venue == DEFAULT_VENUE => (s.symbol.clone(), ACCENT),
                                Some(s) => {
                                    (format!("{}:{}", s.venue.to_uppercase(), s.symbol), ACCENT)
                                }
                            };
                            let btn = ui
                                .button(egui::RichText::new(label).size(11.0).color(tint))
                                .on_hover_text(
                                    "Source symbol (compute this study off another symbol)",
                                );
                            egui::Popup::menu(&btn).id(w.id.with(("ind_src", *uid))).show(|ui| {
                                ui.set_min_width(240.0);
                                // "Chart (primary)" — reset to the window's own symbol.
                                if ui
                                    .selectable_label(source.is_none(), "Chart (primary symbol)")
                                    .clicked()
                                {
                                    set_source = Some((*uid, None));
                                    ui.close();
                                }
                                ui.separator();
                                // Live cross-venue search (falls back to nothing until
                                // the catalog lands — the primary picker has the same
                                // fallback shape; a study source is niche enough to
                                // simply wait for the one-shot catalog fetch).
                                let q_id = w.id.with(("ind_src_q", *uid));
                                let mut q =
                                    ui.data_mut(|d| d.get_temp::<String>(q_id).unwrap_or_default());
                                let entry = ui.add(
                                    egui::TextEdit::singleline(&mut q)
                                        .hint_text("Search symbol")
                                        .desired_width(224.0),
                                );
                                if ui.memory(|m| m.focused().is_none()) {
                                    entry.request_focus();
                                }
                                ui.separator();
                                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                                    if catalog.is_empty() {
                                        ui.weak("  Loading symbols…");
                                    }
                                    let cur_sel = |m: &vike_catalog::Instrument| {
                                        source.as_ref().is_some_and(|s| {
                                            s.venue == m.venue
                                                && s.symbol.eq_ignore_ascii_case(&m.raw_symbol)
                                        })
                                    };
                                    for m in catalog.search(
                                        &q,
                                        &vike_catalog::SearchFilter::default(),
                                        40,
                                    ) {
                                        if search_result_row(ui, cur_sel(m), m) {
                                            set_source = Some((
                                                *uid,
                                                Some(indicators::SourceSymbol {
                                                    venue: m.venue.to_string(),
                                                    symbol: m.raw_symbol.clone(),
                                                }),
                                            ));
                                            q.clear();
                                            ui.close();
                                        }
                                    }
                                });
                                ui.data_mut(|d| d.insert_temp(q_id, q));
                            });
                        });
                    });
                }
            }
            // Part (a): "Add to" target selector. `Auto` (the default) routes by the
            // indicator's RenderKind — byte-identical to the pre-feature behavior. The
            // other choices reuse `move_study` at add-time (see `add_indicator_to`):
            // `Price`/`New pane` mirror the two RenderKind defaults; each existing study
            // pane offers a one-step MERGE (only honored for oscillators — an overlay
            // stays on the price pane regardless; see `resolve_add_target`).
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Add to:").weak());
                let t = &mut w.picker_target;
                if ui
                    .selectable_label(*t == chart::PaneTarget::Auto, "Auto")
                    .on_hover_text("By indicator type (default)")
                    .clicked()
                {
                    *t = chart::PaneTarget::Auto;
                }
                if ui
                    .selectable_label(*t == chart::PaneTarget::Price, "Price")
                    .on_hover_text("Overlay on the price pane")
                    .clicked()
                {
                    *t = chart::PaneTarget::Price;
                }
                if ui
                    .selectable_label(*t == chart::PaneTarget::NewPane, "New pane")
                    .on_hover_text("A fresh study pane (oscillators)")
                    .clicked()
                {
                    *t = chart::PaneTarget::NewPane;
                }
                for (i, &pane) in study_panes.iter().enumerate() {
                    let sel = *t == chart::PaneTarget::Existing(pane);
                    if ui
                        .selectable_label(sel, format!("Pane {}", i + 1))
                        .on_hover_text("Merge into this study pane (oscillators)")
                        .clicked()
                    {
                        *t = chart::PaneTarget::Existing(pane);
                    }
                }
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(w.picker_tab.is_none(), "All").clicked() {
                    w.picker_tab = None;
                }
                for cat in [
                    indicators::Category::Overlap,
                    indicators::Category::Momentum,
                    indicators::Category::Volatility,
                    indicators::Category::Volume,
                ] {
                    if ui.selectable_label(w.picker_tab == Some(cat), cat.label()).clicked() {
                        w.picker_tab = Some(cat);
                    }
                }
                // A "User" tab, and ONLY when this machine actually has user indicators: an
                // always-present tab that can only ever be empty advertises a feature as broken
                // to everyone who has not used it.
                if !indicators::user_registry().is_empty() {
                    let cat = indicators::Category::User;
                    if ui.selectable_label(w.picker_tab == Some(cat), cat.label()).clicked() {
                        w.picker_tab = Some(cat);
                    }
                }
            });
            ui.separator();
            let q = w.picker_query.to_lowercase();
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                // Part (b): Favourites section at the TOP of the picker — one-click add
                // of any ⭐-starred indicator, filtered by the same search query (but NOT
                // the category tab: favourites cut across categories). An empty favourites
                // set renders nothing here — byte-identical to the pre-feature picker.
                // `get_any`: a user study can be favourited like any other. A fav naming an
                // indicator that no longer resolves (an uninstalled user file, a renamed
                // built-in) is skipped here and by the row loop below — the same silent
                // degradation `add_indicator` gives that name.
                let has_favs = favs.iter().any(|name| {
                    indicators::get_any(name).is_some_and(|s| indicator_matches(s, &q))
                });
                if has_favs {
                    ui.label(egui::RichText::new("★ Favourites").weak());
                    // Clone the fav ids so the row loop can mutate `favs` (unstar) without
                    // aliasing the iteration.
                    let fav_ids: Vec<String> = favs.clone();
                    for name in &fav_ids {
                        let Some(s) = indicators::get_any(name) else { continue };
                        if !indicator_matches(s, &q) {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            if ui.small_button("★").on_hover_text("Unfavourite").clicked() {
                                toggle_fav(favs, s.name);
                            }
                            if ui.add(egui::Button::new(row_label(s)).frame(false)).clicked() {
                                chosen = Some(s.name);
                            }
                        });
                    }
                    ui.separator();
                }
                // USER indicators (`user_data/indicators/*.rhai`) get their OWN section, above
                // the built-in list and below Favourites, rather than rows mixed into the
                // catalog. They are the only entries whose set depends on the MACHINE — a file
                // that failed to compile is simply an absent row — so "did mine load?" has to be
                // answerable at a glance, which it is not when two of them sit somewhere inside
                // the whole built-in catalogue. It respects the category tab like every other
                // section: they carry `Category::User`, so a built-in tab hides them and the
                // User tab (added above, only when there are any) isolates them. An empty
                // user registry renders
                // nothing here — byte-identical to the pre-feature picker for anyone who has
                // never written one.
                let user_rows: Vec<&'static indicators::IndicatorMeta> =
                    if w.picker_tab.is_none_or(|c| c == indicators::Category::User) {
                        indicators::user_registry()
                            .iter()
                            .copied()
                            .filter(|s| indicator_matches(s, &q))
                            .collect()
                    } else {
                        Vec::new()
                    };
                if !user_rows.is_empty() {
                    ui.label(egui::RichText::new("✎ My indicators").weak());
                    for s in user_rows {
                        ui.horizontal(|ui| {
                            let starred = favs.iter().any(|f| f == s.name);
                            let star = if starred { "★" } else { "☆" };
                            if ui
                                .small_button(star)
                                .on_hover_text(if starred { "Unfavourite" } else { "Favourite" })
                                .clicked()
                            {
                                toggle_fav(favs, s.name);
                            }
                            if ui
                                .add(egui::Button::new(row_label(s)).frame(false))
                                .on_hover_text("Your own indicator (user_data/indicators)")
                                .clicked()
                            {
                                chosen = Some(s.name);
                            }
                        });
                    }
                    ui.separator();
                }
                // Volume-as-indicator: a synthetic "Volume ·pane" entry so volume
                // is added like any other indicator. Shown only when it's OFF (an
                // ADD action — removal is the volume pane's own ✕); respects the
                // search query and the Volume category tab.
                let vol_in_tab =
                    w.picker_tab.is_none() || w.picker_tab == Some(indicators::Category::Volume);
                if !w.options.show_volume
                    && vol_in_tab
                    && (q.is_empty() || "volume".contains(&q))
                    && ui.add(egui::Button::new("Volume   ·pane").frame(false)).clicked()
                {
                    add_volume = true;
                }
                for s in indicators::registry() {
                    let in_tab = w.picker_tab.is_none_or(|c| s.category == c);
                    if in_tab && indicator_matches(s, &q) {
                        // Part (b): a per-row star toggle (filled ★ when favourited).
                        ui.horizontal(|ui| {
                            let starred = favs.iter().any(|f| f == s.name);
                            let star = if starred { "★" } else { "☆" };
                            if ui
                                .small_button(star)
                                .on_hover_text(if starred { "Unfavourite" } else { "Favourite" })
                                .clicked()
                            {
                                toggle_fav(favs, s.name);
                            }
                            if ui.add(egui::Button::new(row_label(s)).frame(false)).clicked() {
                                chosen = Some(s.name);
                            }
                        });
                    }
                }
            });
        });
    if let Some(uid) = open_settings {
        // Fresh open: only set the target — chart::draw reseeds the working +
        // snapshot edit copies from the live `Active` next frame.
        w.indicator_dialog.open_uid = Some(uid);
        w.indicator_dialog.working = None;
        w.indicator_dialog.snapshot = None;
        w.picker_open = false; // hand off to the settings dialog
    }
    // Foreign-source study (TradingView "symbol" input): apply the source change onto
    // the live `Active`. The per-frame window loop then folds the study over that
    // symbol's bars (subscribing its feed) — a source change is a structural change,
    // so `Active::update` refolds next frame with no explicit reset needed here.
    if let Some((uid, src)) = set_source {
        if let Some(a) = w.indicators.iter_mut().find(|a| a.uid == uid) {
            a.source_symbol = src;
        }
    }
    // Volume-as-indicator: apply the synthetic "Volume" add outside the closure.
    if add_volume {
        w.options.show_volume = true;
    }
    if close || chosen.is_some() || add_volume {
        w.picker_open = false;
    }
    chosen
}

/// Toggle indicator registry key `name` in the global favourites list (part b): remove
/// it if present, else append it (append keeps star-order == display-order). Deduped by
/// construction — a name is present at most once.
fn toggle_fav(favs: &mut Vec<String>, name: &str) {
    if let Some(i) = favs.iter().position(|f| f == name) {
        favs.remove(i);
    } else {
        favs.push(name.to_string());
    }
}

/// Does this indicator match the picker's search box? The haystack is the registry key AND the
/// pretty label joined (`"rsi Relative Strength Index"`), lowercased, so typing either the short id
/// or a word from the display name finds a study. `q` is expected ALREADY lowercased (the caller
/// lowercases the query once per frame); an empty `q` matches everything, which is what makes the
/// unfiltered picker list the whole registry.
fn indicator_matches(meta: &indicators::IndicatorMeta, q: &str) -> bool {
    let hay = format!("{} {}", meta.name, meta.pretty).to_lowercase();
    q.is_empty() || hay.contains(q)
}

/// One picker row's button label: the pretty name, then the dot-tags.
///
/// `·overlay`/`·pane` is where it draws (unchanged). `·user` marks a USER-written indicator, and
/// is derived from [`indicators::IndicatorMeta::is_user`] — i.e. from the same field that decides
/// which constructor actually runs — rather than from which list the row was iterated out of. A
/// tag computed at the loop instead would be a second, independently-wrong copy of the answer the
/// moment a favourited user study is rendered from the Favourites section.
fn row_label(meta: &indicators::IndicatorMeta) -> String {
    let where_ = if meta.kind == indicators::RenderKind::Overlay { "overlay" } else { "pane" };
    let user = if meta.is_user() { " ·user" } else { "" };
    format!("{}   ·{where_}{user}", meta.pretty)
}

#[cfg(test)]
mod tests {
    use super::{indicator_matches, row_label, toggle_fav};
    use vike_chart::indicators;

    #[test]
    fn toggle_adds_then_removes_keeping_star_order() {
        let mut favs: Vec<String> = Vec::new();
        toggle_fav(&mut favs, "rsi");
        toggle_fav(&mut favs, "ema");
        // Append order == the ★ Favourites section's display order.
        assert_eq!(favs, ["rsi", "ema"]);
        // A second toggle of the same key unstars it, leaving the rest in order.
        toggle_fav(&mut favs, "rsi");
        assert_eq!(favs, ["ema"]);
        // Re-starring appends at the END, not back at its old slot.
        toggle_fav(&mut favs, "rsi");
        assert_eq!(favs, ["ema", "rsi"]);
    }

    #[test]
    fn toggle_never_duplicates_a_name() {
        let mut favs = vec!["rsi".to_string()];
        toggle_fav(&mut favs, "rsi");
        toggle_fav(&mut favs, "rsi");
        assert_eq!(favs, ["rsi"]);
    }

    /// The registry is the real one — pick a study every build has and search it both ways.
    ///
    /// NOTE the pretty labels are SHORT: `rsi`'s is the acronym `"RSI"`, not "Relative Strength
    /// Index", so only a study whose label is genuinely multi-word exercises the pretty half.
    /// `bollinger`/"Bollinger Bands" is that study — searching "bands" can only match through the
    /// label, never through the registry key.
    #[test]
    fn search_matches_on_registry_key_and_on_pretty_label() {
        let bb =
            vike_chart::indicators::get("bollinger").expect("bollinger is a registry indicator");
        // Empty query = the unfiltered list.
        assert!(indicator_matches(bb, ""));
        // The short registry key.
        assert!(indicator_matches(bb, "bollinger"));
        // A word that appears ONLY in the pretty label ("Bollinger Bands").
        assert!(indicator_matches(bb, "bands"));
        // Nothing in either half.
        assert!(!indicator_matches(bb, "ichimoku"));
    }

    /// A do-nothing [`indicators::Indicator`] — the picker never runs one, it only labels the row.
    #[derive(Clone)]
    struct Inert;

    impl indicators::Indicator for Inert {
        fn on_bar(&mut self, _b: &vike_model::Bar) -> Vec<f64> {
            vec![f64::NAN]
        }
        fn vectorize(&self, bars: &[vike_model::Bar]) -> Vec<Vec<f64>> {
            vec![bars.iter().map(|_| f64::NAN).collect()]
        }
        fn value(&self) -> Vec<f64> {
            vec![f64::NAN]
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "inert"
        }
    }

    fn user_meta(name: &str, kind: indicators::RenderKind) -> &'static indicators::IndicatorMeta {
        indicators::IndicatorMeta::user(name, name, kind, Vec::new(), &|_: &[f64]| Box::new(Inert))
    }

    /// A USER study's row is visibly a user study, and a built-in's is unchanged.
    ///
    /// Non-vacuous: the pre-feature label was `format!("{}   ·{tag}", pretty)` with `tag` computed
    /// at the loop, which produces the built-in assertion below verbatim and drops `·user`
    /// entirely. It is asserted on the LABEL rather than on which loop rendered it because a
    /// favourited user study is drawn from the ★ Favourites section, where no loop knows.
    #[test]
    fn a_user_studys_row_is_tagged_user_and_a_builtins_is_not() {
        let sma = indicators::get("sma").expect("sma is a built-in");
        assert_eq!(row_label(sma), "Simple MA   ·overlay");
        assert!(!row_label(sma).contains("·user"));

        let mine = user_meta("my_thing", indicators::RenderKind::Oscillator);
        assert_eq!(row_label(mine), "my_thing   ·pane ·user");

        // ...and the placement half still tracks `kind`, so a price-pane user study says so.
        let overlay = user_meta("my_overlay", indicators::RenderKind::Overlay);
        assert_eq!(row_label(overlay), "my_overlay   ·overlay ·user");
    }

    /// The picker's search box finds a user study by the only name it has — the file stem.
    ///
    /// Non-vacuous only in combination with the section being rendered at all; what it pins is
    /// that `indicator_matches` needs no user-specific arm, which is why the section can reuse it.
    #[test]
    fn search_finds_a_user_study_by_its_file_stem() {
        let mine = user_meta("zscore_of_close", indicators::RenderKind::Oscillator);
        assert!(indicator_matches(mine, ""));
        assert!(indicator_matches(mine, "zscore"));
        assert!(!indicator_matches(mine, "bollinger"));
    }
}
