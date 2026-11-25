//! The DataSets destination: the DataSet tree, the editor form, its action strip and its headings.

pub(super) mod members;

use super::gaps::{backfillable, span_label};
use super::{action, legend};
use crate::ui::windows::WindowToolContext;
use crate::ui::windows::WindowView;
use members::{
    DataManagerSymbolSetMember, DataManagerSymbolSetMemberCut, MEMBER_COLUMNS, MEMBER_SEGMENTS,
    member_cell, member_is_stale, member_keys, member_series,
};
use vike_catalog::{Catalog, CatalogSearchFilter, PickerTab};
use vike_data::datasets::{self, provider_names_a_venue};
use vike_data_manager::DataManagerInventorySeriesKey;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::input::{self, ThemeInputField};
use vike_ui_theme::components::overlay;
use vike_ui_theme::components::rail::{ThemeRailItem, navigation_rail};
use vike_ui_theme::components::segmented;
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::table;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::data;

/// The DataSets destination: the DataSet tree on the left and the editor form on the right. The
/// member cut's state is read and written back HERE, beside the `Ui` whose id keys it, and lent to
/// the editor column by `&mut`.
pub(super) fn data_symbol_sets(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    t: &ThemeTokens,
) {
    use egui::{Align, Layout, vec2};
    let symbol_set_store = context.symbol_set_store;
    // ===== DataSets — the DataSet tree + editor (the old "Symbols" tab) =====
    //
    // The member-symbol strip's parts sit at MODULE scope with their siblings —
    // `DataManagerSymbolSetMemberCut`/`MEMBER_SEGMENTS` beside `DataManagerGapCut`, and
    // `DataManagerSymbolSetMember`/`provider_names_a_venue`/`member_series`/`member_is_stale`/`member_keys`
    // beside `stale_series_keys` — so this arm holds RENDERING and no derivation.
    //
    // ⚠ They were declared block-local INSIDE this arm first, and what the move bought is
    // worth recording, because it is exactly what the locality cost: the `#[cfg(test)]`
    // module at the bottom of this file can now REACH them. The roster gate
    // (`every_segment_roster_offers_each_value_once_and_leads_with_the_default`) covers
    // `DataManagerSymbolSetMemberCut`'s segments, and every derivation has tests where it previously had a
    // doc comment asserting the same thing to nobody. The move was pure — nothing here
    // ever closed over an arm local — so the only thing that changed is what is CHECKED.

    // The member cut rides egui temp memory keyed off the BODY `Ui`'s id, like every other
    // strip's state on this screen — the module doc argues why, and `ui/windows/view.rs` is not this
    // arm's to extend. Read HERE, beside the `Ui` whose id keys it, because the control
    // itself is rendered two closures deep inside the editor column, where `ui.scope_id()` is a
    // different id entirely and would key a second, unrelated entry.
    let member_cut_id = ui.scope_id().with("data_manager_symbol_set_member_cut");
    let mut member_cut: DataManagerSymbolSetMemberCut = ui
        .data_mut(|d| d.get_temp::<DataManagerSymbolSetMemberCut>(member_cut_id))
        .unwrap_or_default();

    if tv.symbol_set_name.is_empty()
        && tv.symbol_set_selection.is_none()
        && let Some(d) = symbol_set_store.sets.first()
    {
        symbol_set_load_form(tv, d);
    }
    let groups = symbol_set_groups(&symbol_set_store.sets, context.directory);
    let full = ui.available_width();
    let tree_w = (full * data::SYMBOL_SET_TREE_FRACTION)
        .clamp(data::SYMBOL_SET_TREE_MIN_WIDTH, data::SYMBOL_SET_TREE_MAX_WIDTH);
    ui.horizontal_top(|ui| {
        // ----- LEFT: + New DataSet + grouped tree -----
        ui.allocate_ui_with_layout(
            vec2(tree_w, ui.available_height()),
            Layout::top_down(Align::Min),
            |ui| {
                symbol_sets_tree(ui, tv, symbol_set_store, &groups, tree_w, t);
            },
        );
        ui.add_space(space::XL);
        // ----- RIGHT: editor form -----
        ui.allocate_ui_with_layout(
            vec2(full - tree_w - space::XL2, ui.available_height()),
            Layout::top_down(Align::Min),
            |ui| {
                symbol_sets_editor(ui, context, tv, t, &mut member_cut);
            },
        );
    });
    // Written back ONCE, after every closure that could have moved it has returned — the
    // shape `DataManagerGapCut`'s strip uses, and the reason its read sits at the top of this arm.
    // The module doc states what egui temp memory costs: the cut falls back to
    // `all members` if egui evicts the entry, which is the right trade for a FILTER and
    // would be the wrong one for anything an operator had typed.
    ui.data_mut(|d| d.insert_temp(member_cut_id, member_cut));
}

/// DataSets, left column: `+ New symbol set` and the grouped tree of saved sets.
fn symbol_sets_tree(
    ui: &mut egui::Ui,
    tv: &mut WindowView,
    symbol_set_store: &datasets::SymbolSetStore,
    groups: &[SymbolSetGroup],
    tree_w: f32,
    t: &ThemeTokens,
) {
    if ui
        .add_sized(
            [tree_w - space::MD, t.metrics.control_height],
            ThemeButtonAction::secondary("+ New symbol set"),
        )
        .clicked()
    {
        tv.symbol_set_selection = None;
        tv.symbol_set_name = symbol_set_store.fresh_name();
        tv.symbol_set_provider = "Auto".into();
        tv.symbol_set_interval = "1m".into();
        tv.symbol_set_benchmark.clear();
        tv.symbol_set_symbols_text.clear();
    }
    ui.add_space(space::MD);
    if symbol_set_store.sets.is_empty() {
        state::view(ui, ThemeEmptyPane::Empty(NO_SYMBOL_SETS));
    }
    egui::ScrollArea::vertical().id_salt("symbol_set_tree").show(ui, |ui| {
        // The kit rail (owner decision 2b): one heading per group, a row per
        // set, and the open set reported as a Label. A set listed under two
        // groups is one value, so both of its rows light together, as they
        // always did.
        let mut items: Vec<ThemeRailItem<'_, Option<usize>>> = Vec::new();
        for g in groups {
            for (i, d) in symbol_set_store.sets.iter().enumerate() {
                if g.has(d) {
                    items.push(ThemeRailItem {
                        value: Some(i),
                        group: g.label.as_str(),
                        icon: icons::DATASETS,
                        label: d.name.as_str(),
                        count: None,
                    });
                }
            }
        }
        let mut open = symbol_set_store.sets.iter().position(|d| d.name == tv.symbol_set_name);
        if navigation_rail(ui, &mut open, &items)
            && let Some(i) = open
        {
            symbol_set_load_form(tv, &symbol_set_store.sets[i]);
        }
    });
}

/// DataSets, editor column — the form rows: Name, Provider, Interval, Benchmark.
fn symbol_sets_form_rows(ui: &mut egui::Ui, tv: &mut WindowView, t: &ThemeTokens) {
    use egui::RichText;
    use egui::{Align, Layout, vec2};
    let label_w = data::SYMBOL_SET_LABEL_WIDTH;
    // `body` gets the row label's id, the name its field is read by.
    let row = |ui: &mut egui::Ui, label: &str, body: &mut dyn FnMut(&mut egui::Ui, egui::Id)| {
        ui.horizontal(|ui| {
            let name = ui
                .allocate_ui_with_layout(
                    vec2(label_w, t.metrics.control_height),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(label)
                                .font(t.font(ThemeTextRole::Body))
                                .color(t.theme.text_secondary),
                        )
                        .id
                    },
                )
                .inner;
            body(ui, name);
        });
    };
    row(ui, "Name", &mut |ui, name| {
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data::SYMBOL_SET_NAME_FIELD_WIDTH;
            input::text(ui, &mut tv.symbol_set_name, ThemeInputField::default()).labelled_by(name);
        });
    });
    // A dropdown is a kit button carrying a caret; its menu marks the current
    // value with a check.
    row(ui, "Provider", &mut |ui, _| {
        let response = ui.add_sized(
            [data::SYMBOL_SET_PROVIDER_WIDTH, t.metrics.control_height],
            ThemeButtonAction::secondary((tv.symbol_set_provider.as_str(), icons::DISCLOSE_OPEN)),
        );
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(data::SYMBOL_SET_PROVIDER_MENU_MIN_WIDTH);
            for p in ["Auto", "binance", "bybit", "okx", "coinbase", "kraken", "yahoo", "dukascopy"]
            {
                let current = (tv.symbol_set_provider == p).then_some(icons::CHECK);
                if overlay::menu_item(ui, current, p, None).clicked() {
                    tv.symbol_set_provider = p.to_string();
                }
            }
        });
    });
    row(ui, "Interval", &mut |ui, _| {
        let response = ui.add_sized(
            [data::SYMBOL_SET_INTERVAL_WIDTH, t.metrics.control_height],
            ThemeButtonAction::secondary((tv.symbol_set_interval.as_str(), icons::DISCLOSE_OPEN)),
        );
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(data::SYMBOL_SET_INTERVAL_MENU_MIN_WIDTH);
            for iv in ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "1d", "1w"] {
                let current = (tv.symbol_set_interval == iv).then_some(icons::CHECK);
                if overlay::menu_item(ui, current, iv, None).clicked() {
                    tv.symbol_set_interval = iv.to_string();
                }
            }
        });
    });
    row(ui, "Benchmark", &mut |ui, _| {
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data::SYMBOL_SET_BENCHMARK_FIELD_WIDTH;
            input::text(
                ui,
                &mut tv.symbol_set_benchmark,
                ThemeInputField {
                    hint: "optional, e.g. SPY / BTCUSDT (else equal-weight)",
                    ..ThemeInputField::default()
                },
            );
        });
    });
}

/// DataSets, right column: the form rows, the member strip and list, the symbols editor and the
/// action strip, in that order. `member_cut` is this destination's own state, read once by
/// [`data_symbol_sets`] and lent here.
fn symbol_sets_editor(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    t: &ThemeTokens,
    member_cut: &mut DataManagerSymbolSetMemberCut,
) {
    use egui::RichText;
    use egui::vec2;
    symbol_sets_form_rows(ui, tv, t);
    ui.add_space(space::LG);
    // "Symbols in this symbol set — select one to Test" (read-only list)
    ui.label(
        RichText::new("Symbols in this symbol set — select one to Test")
            .font(t.font(ThemeTextRole::Body))
            .color(t.theme.text_secondary),
    );
    // ⚠ Recomputed from the TEXT every frame rather than cached: the
    // multiline editor further down is the writer, and an operator can retype
    // the whole membership between two frames.
    let symbols = datasets::parse_symbols(&tv.symbol_set_symbols_text);
    // The set's own declaration, captured ONCE. Both the Provider and the
    // Interval popup are rendered ABOVE, so these are this frame's values, and
    // nothing below writes either — so the member resolution here and the bulk
    // button at the foot of this column cannot be reading different ones.
    let symbol_set_provider = tv.symbol_set_provider.clone();
    let symbol_set_interval = tv.symbol_set_interval.clone();
    // Staleness is DERIVED from what this screen already holds — the tree, and
    // the same tree-wide window the Stale destination judges against. No new
    // I/O path and no second fetch: `vike_data_manager::global_span` and
    // `is_stale` are the two functions `stale_series_keys` calls.
    let (gfirst, glast) = vike_data_manager::global_span(context.stored.tree);
    // ONE resolution per member, reused by the filter, the counts, the row
    // markers and the bulk button — so the number on the button and the rows in
    // the list cannot disagree. That is the property `stale_series_keys`'s and
    // `gapped_series_keys`'s docs argue for at length on the sibling strips,
    // and it is the reason this is a `Vec` computed once here rather than a
    // predicate re-evaluated at each site that asks.
    let members: Vec<DataManagerSymbolSetMember> = symbols
        .iter()
        .map(|s| (s.clone(), member_series(context.stored.tree, &symbol_set_provider, s)))
        .collect();
    let stale_n = members.iter().filter(|(_, st)| member_is_stale(st, gfirst, glast)).count();
    let unstored_n = members.iter().filter(|(_, st)| st.is_empty()).count();
    // ----- the design's member strip: `all members` / `stale only` -----
    //
    // ⚠ Rendered BEFORE the list is cut, so a click takes effect on the frame
    // it happens on rather than the next one — the ordering `DataManagerGapCut`'s strip
    // uses, and the whole reason `shown` is computed below this block instead
    // of above it. Reversed, the control lights up a segment while the rows
    // below it still answer the other one, which reads as a stuck filter.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, member_cut, &MEMBER_SEGMENTS);
        // The window is on the strip for the reason the Stale screen states
        // it: the cut-off is a property of the TREE, so it moves every time
        // the newest row anywhere in the store does. A `stale only` list with
        // no window beside it reads as a fixed calendar date.
        ui.label(
            RichText::new("tree window")
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(ThemeTextRole::Caption))
                .color(t.theme.text_secondary),
        );
    });
    let shown: Vec<&DataManagerSymbolSetMember> = match *member_cut {
        DataManagerSymbolSetMemberCut::AllMembers => members.iter().collect(),
        DataManagerSymbolSetMemberCut::StaleOnly => {
            members.iter().filter(|(_, st)| member_is_stale(st, gfirst, glast)).collect()
        }
    };
    // The kit table (owner decision 3): header, zebra, the selected row's
    // accent edge. Capped at the header and five rows, as the old 92 px box was.
    let selected = shown
        .iter()
        .position(|(s, _)| tv.symbol_set_symbol_selection.as_deref() == Some(s.as_str()));
    let table_h = data::SYMBOL_SET_MEMBER_ROWS as f32 * t.metrics.row_height;
    let seen = ui
        .allocate_ui(egui::vec2(ui.available_width(), table_h), |ui| {
            table::data_table(
                ui,
                "symbol_set_symbols",
                &MEMBER_COLUMNS,
                shown.len(),
                selected,
                |r, c| member_cell(shown[r], c, gfirst, glast),
            )
        })
        .inner;
    if let Some(r) = seen.clicked {
        tv.symbol_set_symbol_selection = Some(shown[r].0.clone());
    }
    if shown.is_empty() {
        // ⚠ TWO different emptinesses, said apart. "No symbols" and "none of
        // them is stale" send an operator to different places, and one
        // message serving both would report an empty set when the filter is
        // what emptied it.
        state::view(
            ui,
            ThemeEmptyPane::Empty(if members.is_empty() {
                "No symbols yet."
            } else {
                "No member is stale against this window."
            }),
        );
    }
    legend(
        ui,
        &format!(
            "{} of {} member symbols shown · {stale_n} stale · {unstored_n} \
                                 not in this store. Stale is the tree-wide 35% cut the Stale \
                                 destination applies, asked per member: a symbol this store holds \
                                 nothing for is NOT stale — it has nothing to be behind — so \
                                 `stale only` hides it rather than leading with it.",
            shown.len(),
            members.len(),
        ),
    );
    symbol_sets_symbols_editor(ui, tv, t, context.catalog);
    symbol_sets_action_strip(
        ui,
        tv,
        t,
        &shown,
        &symbol_set_provider,
        &symbol_set_interval,
        context.symbol_set_store.writable,
    );
}

/// DataSets, editor column — the multiline symbols editor and the Ask-the-AI box, which suggests
/// from the kept instrument lists (`catalog`).
fn symbol_sets_symbols_editor(
    ui: &mut egui::Ui,
    tv: &mut WindowView,
    t: &ThemeTokens,
    catalog: &Catalog,
) {
    use egui::RichText;
    ui.add_space(space::LG);
    ui.label(
        RichText::new("Symbols (comma or newline separated)")
            .font(t.font(ThemeTextRole::Body))
            .color(t.theme.text_secondary),
    );
    // Still egui's own multiline editor: the kit has no multiline input (a
    // knob on this PR's report). Symbols are read character by character: mono.
    ui.add(
        egui::TextEdit::multiline(&mut tv.symbol_set_symbols_text)
            .font(t.mono(ThemeTextRole::Body))
            .hint_text(RichText::new("BTCUSDT, ETHUSDT, SOLUSDT…").color(t.theme.text_tertiary))
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(space::LG);
    // Ask the AI
    egui::Frame::new()
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(egui::CornerRadius::same(RADIUS))
        .inner_margin(t.metrics.padding)
        .show(ui, |ui| {
            ui.label(
                RichText::new("Ask the AI")
                    .font(t.font(ThemeTextRole::Body))
                    .color(t.theme.text_secondary),
            );
            ui.scope(|ui| {
                let w = ui.available_width();
                ui.spacing_mut().text_edit_width = w;
                input::text(
                    ui,
                    &mut tv.symbol_set_ai_prompt,
                    ThemeInputField {
                        hint: "e.g. top 10 liquid crypto majors",
                        ..ThemeInputField::default()
                    },
                );
            });
            let suggest = ThemeButtonAction::secondary("Suggest");
            let suggest = if catalog.is_empty() {
                suggest.disabled_because(NO_LIST_TO_SUGGEST)
            } else {
                suggest
            };
            if ui.add(suggest).clicked() {
                let s = suggest_symbols(&tv.symbol_set_ai_prompt, catalog, &tv.symbol_set_provider);
                if !s.is_empty() {
                    tv.symbol_set_symbols_text = s.join(", ");
                }
            }
        });
}

/// DataSets, editor column — the strip of Backfill N / Save / Test symbol / Test symbol set / Delete,
/// over the cut the member list shows (`shown`) and the set's own provider and interval, which the
/// caller captured once so the list and this strip cannot read different ones.
fn symbol_sets_action_strip(
    ui: &mut egui::Ui,
    tv: &mut WindowView,
    t: &ThemeTokens,
    shown: &[&DataManagerSymbolSetMember],
    symbol_set_provider: &str,
    symbol_set_interval: &str,
    writable: bool,
) {
    use egui::{Align, Layout};
    ui.add_space(space::XL);
    // Backfill N symbols / Save / Test symbol / Test symbol set / Delete
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        // ----- the design's `Backfill 12 symbols` — LIVE -----
        //
        // It leads the strip because the design's does (`Backfill …`, then
        // `Test DataSet`, then `Save`). The remaining buttons keep the order
        // they had: the design's strip is a SUBSET of this one, so reordering
        // the rest would be churn a reviewer cannot check against anything.
        //
        // It fills the SAME out-slot the Has-gaps and Stale strips fill —
        // `WindowView::stored_backfill`, drained by
        // `crates/vike-desktop/src/app_methods.rs`'s
        // `maybe_spawn_stored_backfill` — so nothing new is wired to make it
        // real. N is the count the CURRENT member cut shows, which is what
        // joins the two controls into one gesture: filter to `stale only`,
        // then backfill exactly those.
        //
        // Per MEMBER first, then flattened — the two-step is what lets the
        // hover distinguish a symbol that produced NO key at all from a series
        // the planner will skip. Flattening straight to keys loses the former,
        // and it is the one an operator can act on: it means the form above is
        // incomplete, not that the venue is unsupported.
        let per_member: Vec<Vec<DataManagerInventorySeriesKey>> = shown
            .iter()
            .map(|(s, stored)| member_keys(stored, symbol_set_provider, symbol_set_interval, s))
            .collect();
        let keys: Vec<DataManagerInventorySeriesKey> = per_member.iter().flatten().cloned().collect();
        // The SAME client-side gate the two sibling strips print, through the
        // same function: `kind == "bar"` with an interval, and no venue term.
        // `backfillable`'s own doc carries why the design mock's "binance,
        // bybit and okx only" line is stale and must not be restated — here
        // included, and a tooltip counts.
        let fillable = backfillable(&keys);
        let no_key_n = per_member.iter().filter(|k| k.is_empty()).count();
        let noun = if shown.len() == 1 { "symbol" } else { "symbols" };
        let queueable = !keys.is_empty();
        let label = format!("Backfill {} {noun}", shown.len());
        let refusal = if shown.is_empty() {
            NO_MEMBERS_TO_QUEUE.to_string()
        } else {
            // ⚠ Names the two values rather than a verdict: the same refusal
            // covers `Auto` and a blank interval, and an operator told only
            // "cannot backfill" has to guess which of the two fields above to
            // go and fix.
            format!(
                "None of these {} {noun} resolves to a series to fetch. This \
                                     store holds nothing for them, and the set declares Provider \
                                     `{symbol_set_provider}` at interval `{symbol_set_interval}` — a key needs a \
                                     real venue AND an interval, and `Auto` names no venue. Set \
                                     both on the form above, or Refresh the store.",
                shown.len(),
            )
        };
        let hover = format!(
            "Queue the {} {noun} this cut shows — {} series in all. \
                                 {fillable} will plan a job (kind=bar with an interval); the other \
                                 {} are counted as skips, never silently dropped. {no_key_n} of \
                                 the {noun} shown resolve to no series at all and are not queued. \
                                 Progress lands in the status line beside Refresh.",
            shown.len(),
            keys.len(),
            keys.len().saturating_sub(fillable),
        );
        let backfill = action(Some(icons::BACKFILL), &label);
        let backfill = if queueable { backfill } else { backfill.disabled_because(&refusal) };
        if ui.add(backfill).on_hover_text(hover).clicked() {
            tv.stored_backfill.extend(keys.iter().cloned());
        }
        // Save is the view's primary action (spec §4.3).
        let save = ThemeButtonAction::primary((icons::SAVE, "Save"));
        let save = if writable { save } else { save.disabled_because(NO_SERVER_TO_SAVE_TO) };
        if ui.add(save).clicked() {
            tv.symbol_set_save = true;
        }
        // the list pick if any, else the first symbol in the editor
        let test_symbol = tv
            .symbol_set_symbol_selection
            .clone()
            .or_else(|| datasets::parse_symbols(&tv.symbol_set_symbols_text).into_iter().next());
        let test = ThemeButtonAction::secondary((icons::RUN, "Test symbol"));
        let test = if test_symbol.is_some() { test } else { test.disabled_because(NO_SYMBOL_TO_TEST) };
        if ui.add(test).clicked() {
            // On the set's own provider and interval; `Auto` names no venue, so none is invented.
            tv.symbol_set_test = test_symbol.map(|symbol| crate::ui::startup::SeriesSpec {
                venue: if provider_names_a_venue(symbol_set_provider) {
                    symbol_set_provider.trim().to_string()
                } else {
                    String::new()
                },
                symbol,
                interval: symbol_set_interval.to_string(),
                asset_class: None,
            });
        }
        ui.add(
            ThemeButtonAction::secondary((icons::RUN, "Test symbol set")).disabled_because(NO_SYMBOL_SET_TEST),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Danger: this deletes at once — there is no confirm behind it.
            let delete = ThemeButtonAction::danger((icons::DELETE, "Delete"));
            let delete = if !writable {
                delete.disabled_because(NO_SERVER_TO_SAVE_TO)
            } else if tv.symbol_set_selection.is_some() {
                delete
            } else {
                delete.disabled_because(NOTHING_SAVED_TO_DELETE)
            };
            if ui.add(delete).clicked() {
                tv.symbol_set_delete = tv.symbol_set_selection.take();
                tv.symbol_set_name.clear();
            }
        });
    });
}

/// Why Test symbol is dark: nothing names a symbol to open.
const NO_SYMBOL_TO_TEST: &str =
    "This symbol set names no symbol yet — type one under Symbols, or pick one in the list above.";
/// Why Test DataSet is ALWAYS dark: the window's one test slot (`WindowView::symbol_set_test`) opens a
/// chart for ONE symbol, and nothing opens a whole set.
const NO_SYMBOL_SET_TEST: &str = "Nothing opens a whole symbol set — the one test this window can run opens \
                               a chart for a single symbol. Use Test symbol.";
/// Why Delete is dark: a set that was never saved has nothing to delete.
const NOTHING_SAVED_TO_DELETE: &str =
    "This symbol set is not saved yet — there is nothing on the server to delete.";
/// Why Save and Delete are dark: no datahub has answered, so there is nowhere to save to (the lists
/// live in the server's settings database).
const NO_SERVER_TO_SAVE_TO: &str = "No backend has answered, so there is nowhere to save a symbol set — connect a backend to save or delete one.";
/// What the tree says when the server holds no DataSet: nothing is compiled in (the owner's ruling of
/// 2026-11-13), so a fresh install starts here.
const NO_SYMBOL_SETS: &str = "No symbol sets yet. + New symbol set starts one: name it, pick its provider and interval, list its symbols, Save.";
/// Why Suggest is dark: it suggests from the venues' kept instrument lists, and none has loaded.
const NO_LIST_TO_SUGGEST: &str = "No instrument list has loaded yet, so there is nothing to suggest from — connect a backend, or \
     refresh a venue's list in Data Manager → Instruments.";

/// **The Ask-the-AI box's suggestions, read from the kept instrument lists** — never a compiled list
/// (the owner's ruling of 2026-11-13). The prompt's first number is how many (1 to 30, else 10), and a
/// word picks the drawer: `forex`/`fx`/`currenc` the FX one, `stock`/`equit`/`nasdaq`/`s&p` the
/// stocks, anything else crypto spot. The rows are the catalog's own, in its order, on the form's
/// provider when that names a venue, each symbol once. A deterministic stand-in for a model call:
/// nothing here ranks by liquidity.
pub(super) fn suggest_symbols(prompt: &str, catalog: &Catalog, provider: &str) -> Vec<String> {
    let p = prompt.to_lowercase();
    let n = p
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|t| t.parse::<usize>().ok())
        .next()
        .unwrap_or(10)
        .clamp(1, 30);
    let tab = if p.contains("forex") || p.contains("fx") || p.contains("currenc") {
        PickerTab::Forex
    } else if p.contains("stock")
        || p.contains("equit")
        || p.contains("nasdaq")
        || p.contains("s&p")
    {
        PickerTab::Stocks
    } else {
        PickerTab::Crypto
    };
    let venue = provider_names_a_venue(provider).then(|| provider.trim().to_string());
    let mut out: Vec<String> = Vec::new();
    for i in catalog.search("", &CatalogSearchFilter { tab: Some(tab), venue }, usize::MAX) {
        if out.len() == n {
            break;
        }
        if !out.contains(&i.raw_symbol) {
            out.push(i.raw_symbol.clone());
        }
    }
    out
}
/// Why Backfill is dark with no member shown.
const NO_MEMBERS_TO_QUEUE: &str = "This cut shows no member symbols, so there is nothing to queue.";

/// One heading of the DataSet tree: every set, the sets of one provider, or the user's own.
#[derive(Debug)]
pub(super) struct SymbolSetGroup {
    /// The heading: the provider's name as the settings database spells it, else its key.
    pub(super) label: String,
    /// `None` for "All" and "My symbol sets"; the provider KEY a set names otherwise.
    provider: Option<String>,
    /// "My symbol sets": the user's own sets.
    mine: bool,
}

impl SymbolSetGroup {
    /// Whether set `d` is listed under this heading.
    pub(super) fn has(&self, d: &datasets::SymbolSet) -> bool {
        match &self.provider {
            Some(p) => d.provider == *p,
            None => !self.mine || d.user,
        }
    }
}

/// The DataSet tree's headings (I15(a) of the Trade window plan): "All", then one per provider the
/// sets actually name, by key, each spelled through [`super::venue_label`] — so the settings
/// database spells it, or the key does, and no venue list lives in this file — then "My symbol sets".
/// `Auto` is the form's sentinel rather than a venue ([`provider_names_a_venue`]), so it heads no
/// group; its sets are under "All".
pub(super) fn symbol_set_groups(
    sets: &[datasets::SymbolSet],
    directory: Option<&vike_tradehub_client::wire::AccountDirectoryWire>,
) -> Vec<SymbolSetGroup> {
    let mut keys: Vec<&str> =
        sets.iter().map(|d| d.provider.as_str()).filter(|p| provider_names_a_venue(p)).collect();
    keys.sort_unstable();
    keys.dedup();
    let fixed =
        |label: &str, mine: bool| SymbolSetGroup { label: label.to_string(), provider: None, mine };
    let providers = keys.into_iter().map(|k| SymbolSetGroup {
        label: super::venue_label(directory, k).to_string(),
        provider: Some(k.to_string()),
        mine: false,
    });
    std::iter::once(fixed("All", false))
        .chain(providers)
        .chain([fixed("My symbol sets", true)])
        .collect()
}

/// Load a DataSet into the Symbols-tab editor working copy.
pub(super) fn symbol_set_load_form(tv: &mut WindowView, d: &datasets::SymbolSet) {
    tv.symbol_set_selection = Some(d.name.clone());
    tv.symbol_set_name = d.name.clone();
    tv.symbol_set_provider = d.provider.clone();
    tv.symbol_set_interval = d.interval.clone();
    tv.symbol_set_benchmark = d.benchmark.clone();
    tv.symbol_set_symbols_text = d.symbols.join(", ");
    tv.symbol_set_symbol_selection = None;
}
