//! The DataSets destination: the DataSet tree, the editor form, its action strip and its headings.

use super::gaps::{backfillable, span_label};
use super::members::{
    MEMBER_COLUMNS, MEMBER_SEGMENTS, Member, MemberCut, member_cell, member_is_stale, member_keys,
    member_series, provider_names_a_venue,
};
use super::{action, legend};
use crate::tools;
use crate::ui::tool_views::ToolCtx;
use vike_data::datasets;
use vike_data_manager::SeriesKey;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::components::overlay;
use vike_ui_theme::components::rail::{RailItem, nav_rail};
use vike_ui_theme::components::segmented;
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::table;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;

/// The DataSets destination: the DataSet tree on the left and the editor form on the right. The
/// member cut's state is read and written back HERE, beside the `Ui` whose id keys it, and lent to
/// the editor column by `&mut`.
pub(super) fn data_datasets(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    t: &Tokens,
) {
    use egui::{Align, Layout, vec2};
    let dsets = ctx.dsets;
    // ===== DataSets — the DataSet tree + editor (the old "Symbols" tab) =====
    //
    // The member-symbol strip's parts sit at MODULE scope with their siblings —
    // `MemberCut`/`MEMBER_SEGMENTS` beside `GapMode`, and
    // `Member`/`provider_names_a_venue`/`member_series`/`member_is_stale`/`member_keys`
    // beside `stale_series_keys` — so this arm holds RENDERING and no derivation.
    //
    // ⚠ They were declared block-local INSIDE this arm first, and what the move bought is
    // worth recording, because it is exactly what the locality cost: the `#[cfg(test)]`
    // module at the bottom of this file can now REACH them. The roster gate
    // (`every_segment_roster_offers_each_value_once_and_leads_with_the_default`) covers
    // `MemberCut`'s segments, and every derivation has tests where it previously had a
    // doc comment asserting the same thing to nobody. The move was pure — nothing here
    // ever closed over an arm local — so the only thing that changed is what is CHECKED.

    // The member cut rides egui temp memory keyed off the BODY `Ui`'s id, like every other
    // strip's state on this screen — the module doc argues why, and `tools.rs` is not this
    // arm's to extend. Read HERE, beside the `Ui` whose id keys it, because the control
    // itself is rendered two closures deep inside the editor column, where `ui.scope_id()` is a
    // different id entirely and would key a second, unrelated entry.
    let member_cut_id = ui.scope_id().with("dm_ds_member_cut");
    let mut member_cut: MemberCut =
        ui.data_mut(|d| d.get_temp::<MemberCut>(member_cut_id)).unwrap_or_default();

    if tv.ds_name.is_empty()
        && tv.ds_sel.is_none()
        && let Some(d) = dsets.sets.first()
    {
        ds_load_form(tv, d);
    }
    let groups = dataset_groups(&dsets.sets, ctx.directory);
    let full = ui.available_width();
    let tree_w = (full * data::DS_TREE_FRAC).clamp(data::DS_TREE_MIN_W, data::DS_TREE_MAX_W);
    ui.horizontal_top(|ui| {
        // ----- LEFT: + New DataSet + grouped tree -----
        ui.allocate_ui_with_layout(
            vec2(tree_w, ui.available_height()),
            Layout::top_down(Align::Min),
            |ui| {
                datasets_tree(ui, tv, dsets, &groups, tree_w, t);
            },
        );
        ui.add_space(space::XL);
        // ----- RIGHT: editor form -----
        ui.allocate_ui_with_layout(
            vec2(full - tree_w - space::XL2, ui.available_height()),
            Layout::top_down(Align::Min),
            |ui| {
                datasets_editor(ui, ctx, tv, t, &mut member_cut);
            },
        );
    });
    // Written back ONCE, after every closure that could have moved it has returned — the
    // shape `GapMode`'s strip uses, and the reason its read sits at the top of this arm.
    // The module doc states what egui temp memory costs: the cut falls back to
    // `all members` if egui evicts the entry, which is the right trade for a FILTER and
    // would be the wrong one for anything an operator had typed.
    ui.data_mut(|d| d.insert_temp(member_cut_id, member_cut));
}

/// DataSets, left column: `+ New DataSet` and the grouped tree of saved sets.
fn datasets_tree(
    ui: &mut egui::Ui,
    tv: &mut tools::ToolView,
    dsets: &datasets::Store,
    groups: &[DsGroup],
    tree_w: f32,
    t: &Tokens,
) {
    if ui
        .add_sized(
            [tree_w - space::MD, t.metrics.control_h],
            ActionButton::secondary("+ New DataSet"),
        )
        .clicked()
    {
        tv.ds_sel = None;
        tv.ds_name = dsets.fresh_name();
        tv.ds_provider = "Auto".into();
        tv.ds_interval = "1m".into();
        tv.ds_benchmark.clear();
        tv.ds_symbols_text.clear();
    }
    ui.add_space(space::MD);
    egui::ScrollArea::vertical().id_salt("ds_tree").show(ui, |ui| {
        // The kit rail (owner decision 2b): one heading per group, a row per
        // set, and the open set reported as a Label. A set listed under two
        // groups is one value, so both of its rows light together, as they
        // always did.
        let mut items: Vec<RailItem<'_, Option<usize>>> = Vec::new();
        for g in groups {
            for (i, d) in dsets.sets.iter().enumerate() {
                if g.has(d) {
                    items.push(RailItem {
                        value: Some(i),
                        group: g.label.as_str(),
                        icon: icons::DATASETS,
                        label: d.name.as_str(),
                        count: None,
                    });
                }
            }
        }
        let mut open = dsets.sets.iter().position(|d| d.name == tv.ds_name);
        if nav_rail(ui, &mut open, &items)
            && let Some(i) = open
        {
            ds_load_form(tv, &dsets.sets[i]);
        }
    });
}

/// DataSets, editor column — the form rows: Name, Provider, Interval, Benchmark.
fn datasets_form_rows(ui: &mut egui::Ui, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::RichText;
    use egui::{Align, Layout, vec2};
    let lbl_w = data::DS_LABEL_W;
    let row = |ui: &mut egui::Ui, label: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                vec2(lbl_w, t.metrics.control_h),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(
                        RichText::new(label).font(t.font(TextRole::Body)).color(t.theme.text2),
                    );
                },
            );
            body(ui);
        });
    };
    row(ui, "Name", &mut |ui| {
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data::DS_NAME_FIELD_W;
            input::text(ui, &mut tv.ds_name, Field::default());
        });
    });
    // A dropdown is a kit button carrying a caret; its menu marks the current
    // value with a check.
    row(ui, "Provider", &mut |ui| {
        let resp = ui.add_sized(
            [data::DS_PROVIDER_W, t.metrics.control_h],
            ActionButton::secondary((tv.ds_provider.as_str(), icons::DISCLOSE_OPEN)),
        );
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(data::DS_PROVIDER_MENU_MIN_W);
            for p in ["Auto", "binance", "bybit", "okx", "coinbase", "kraken", "yahoo", "dukascopy"]
            {
                let current = (tv.ds_provider == p).then_some(icons::CHECK);
                if overlay::menu_item(ui, current, p, None).clicked() {
                    tv.ds_provider = p.to_string();
                }
            }
        });
    });
    row(ui, "Interval", &mut |ui| {
        let resp = ui.add_sized(
            [data::DS_INTERVAL_W, t.metrics.control_h],
            ActionButton::secondary((tv.ds_interval.as_str(), icons::DISCLOSE_OPEN)),
        );
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(data::DS_INTERVAL_MENU_MIN_W);
            for iv in ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "1d", "1w"] {
                let current = (tv.ds_interval == iv).then_some(icons::CHECK);
                if overlay::menu_item(ui, current, iv, None).clicked() {
                    tv.ds_interval = iv.to_string();
                }
            }
        });
    });
    row(ui, "Benchmark", &mut |ui| {
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data::DS_BENCHMARK_FIELD_W;
            input::text(
                ui,
                &mut tv.ds_benchmark,
                Field {
                    hint: "optional, e.g. SPY / BTCUSDT (else equal-weight)",
                    ..Field::default()
                },
            );
        });
    });
}

/// DataSets, right column: the form rows, the member strip and list, the symbols editor and the
/// action strip, in that order. `member_cut` is this destination's own state, read once by
/// [`data_datasets`] and lent here.
fn datasets_editor(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    t: &Tokens,
    member_cut: &mut MemberCut,
) {
    use egui::RichText;
    use egui::vec2;
    datasets_form_rows(ui, tv, t);
    ui.add_space(space::LG);
    // "Symbols in this DataSet — select one to Test" (read-only list)
    ui.label(
        RichText::new("Symbols in this DataSet — select one to Test")
            .font(t.font(TextRole::Body))
            .color(t.theme.text2),
    );
    // ⚠ Recomputed from the TEXT every frame rather than cached: the
    // multiline editor further down is the writer, and an operator can retype
    // the whole membership between two frames.
    let syms = datasets::parse_symbols(&tv.ds_symbols_text);
    // The set's own declaration, captured ONCE. Both the Provider and the
    // Interval popup are rendered ABOVE, so these are this frame's values, and
    // nothing below writes either — so the member resolution here and the bulk
    // button at the foot of this column cannot be reading different ones.
    let ds_provider = tv.ds_provider.clone();
    let ds_interval = tv.ds_interval.clone();
    // Staleness is DERIVED from what this screen already holds — the tree, and
    // the same tree-wide window the Stale destination judges against. No new
    // I/O path and no second fetch: `vike_data_manager::global_span` and
    // `is_stale` are the two functions `stale_series_keys` calls.
    let (gfirst, glast) = vike_data_manager::global_span(ctx.stored.tree);
    // ONE resolution per member, reused by the filter, the counts, the row
    // markers and the bulk button — so the number on the button and the rows in
    // the list cannot disagree. That is the property `stale_series_keys`'s and
    // `gapped_series_keys`'s docs argue for at length on the sibling strips,
    // and it is the reason this is a `Vec` computed once here rather than a
    // predicate re-evaluated at each site that asks.
    let members: Vec<Member> =
        syms.iter().map(|s| (s.clone(), member_series(ctx.stored.tree, &ds_provider, s))).collect();
    let stale_n = members.iter().filter(|(_, st)| member_is_stale(st, gfirst, glast)).count();
    let unstored_n = members.iter().filter(|(_, st)| st.is_empty()).count();
    // ----- the design's member strip: `all members` / `stale only` -----
    //
    // ⚠ Rendered BEFORE the list is cut, so a click takes effect on the frame
    // it happens on rather than the next one — the ordering `GapMode`'s strip
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
        ui.label(RichText::new("tree window").font(t.font(TextRole::Caption)).color(t.theme.text3));
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(TextRole::Caption))
                .color(t.theme.text2),
        );
    });
    let shown: Vec<&Member> = match *member_cut {
        MemberCut::AllMembers => members.iter().collect(),
        MemberCut::StaleOnly => {
            members.iter().filter(|(_, st)| member_is_stale(st, gfirst, glast)).collect()
        }
    };
    // The kit table (owner decision 3): header, zebra, the selected row's
    // accent edge. Capped at the header and five rows, as the old 92 px box was.
    let selected = shown.iter().position(|(s, _)| tv.ds_sym_sel.as_deref() == Some(s.as_str()));
    let table_h = data::DS_MEMBER_ROWS as f32 * t.metrics.row_h;
    let seen = ui
        .allocate_ui(egui::vec2(ui.available_width(), table_h), |ui| {
            table::data_table(ui, "ds_syms", &MEMBER_COLUMNS, shown.len(), selected, |r, c| {
                member_cell(shown[r], c, gfirst, glast)
            })
        })
        .inner;
    if let Some(r) = seen.clicked {
        tv.ds_sym_sel = Some(shown[r].0.clone());
    }
    if shown.is_empty() {
        // ⚠ TWO different emptinesses, said apart. "No symbols" and "none of
        // them is stale" send an operator to different places, and one
        // message serving both would report an empty set when the filter is
        // what emptied it.
        state::view(
            ui,
            Load::Empty(if members.is_empty() {
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
    datasets_symbols_editor(ui, tv, t);
    datasets_action_strip(ui, tv, t, &shown, &ds_provider, &ds_interval);
}

/// DataSets, editor column — the multiline symbols editor and the Ask-the-AI box.
fn datasets_symbols_editor(ui: &mut egui::Ui, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::RichText;
    ui.add_space(space::LG);
    ui.label(
        RichText::new("Symbols (comma or newline separated)")
            .font(t.font(TextRole::Body))
            .color(t.theme.text2),
    );
    // Still egui's own multiline editor: the kit has no multiline input (a
    // knob on this PR's report). Symbols are read character by character: mono.
    ui.add(
        egui::TextEdit::multiline(&mut tv.ds_symbols_text)
            .font(t.mono(TextRole::Body))
            .hint_text(RichText::new("BTCUSDT, ETHUSDT, SOLUSDT…").color(t.theme.text3))
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(space::LG);
    // Ask the AI
    egui::Frame::new()
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(egui::CornerRadius::same(RADIUS))
        .inner_margin(t.metrics.pad)
        .show(ui, |ui| {
            ui.label(RichText::new("Ask the AI").font(t.font(TextRole::Body)).color(t.theme.text2));
            ui.scope(|ui| {
                let w = ui.available_width();
                ui.spacing_mut().text_edit_width = w;
                input::text(
                    ui,
                    &mut tv.ds_ai_prompt,
                    Field { hint: "e.g. top 10 liquid crypto majors", ..Field::default() },
                );
            });
            if ui.add(ActionButton::secondary("Suggest")).clicked() {
                let s = datasets::suggest_symbols(&tv.ds_ai_prompt);
                if !s.is_empty() {
                    tv.ds_symbols_text = s.join(", ");
                }
            }
        });
}

/// DataSets, editor column — the strip of Backfill N / Save / Test symbol / Test DataSet / Delete,
/// over the cut the member list shows (`shown`) and the set's own provider and interval, which the
/// caller captured once so the list and this strip cannot read different ones.
fn datasets_action_strip(
    ui: &mut egui::Ui,
    tv: &mut tools::ToolView,
    t: &Tokens,
    shown: &[&Member],
    ds_provider: &str,
    ds_interval: &str,
) {
    use egui::{Align, Layout};
    ui.add_space(space::XL);
    // Backfill N symbols / Save / Test symbol / Test DataSet / Delete
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
        // `tools::ToolView::stored_backfill`, drained by
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
        let per_member: Vec<Vec<SeriesKey>> = shown
            .iter()
            .map(|(s, stored)| member_keys(stored, ds_provider, ds_interval, s))
            .collect();
        let keys: Vec<SeriesKey> = per_member.iter().flatten().cloned().collect();
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
                                     `{ds_provider}` at interval `{ds_interval}` — a key needs a \
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
        if ui.add(ActionButton::primary((icons::SAVE, "Save"))).clicked() {
            tv.ds_save = true;
        }
        // the list pick if any, else the first symbol in the editor
        let test_sym = tv
            .ds_sym_sel
            .clone()
            .or_else(|| datasets::parse_symbols(&tv.ds_symbols_text).into_iter().next());
        let test = ActionButton::secondary((icons::RUN, "Test symbol"));
        let test = if test_sym.is_some() { test } else { test.disabled_because(NO_SYMBOL_TO_TEST) };
        if ui.add(test).clicked() {
            tv.ds_test = test_sym;
        }
        ui.add(
            ActionButton::secondary((icons::RUN, "Test DataSet")).disabled_because(NO_DATASET_TEST),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Danger: this deletes at once — there is no confirm behind it.
            let delete = ActionButton::danger((icons::DELETE, "Delete"));
            let delete = if tv.ds_sel.is_some() {
                delete
            } else {
                delete.disabled_because(NOTHING_SAVED_TO_DELETE)
            };
            if ui.add(delete).clicked() {
                tv.ds_delete = tv.ds_sel.take();
                tv.ds_name.clear();
            }
        });
    });
}

/// Why Test symbol is dark: nothing names a symbol to open.
const NO_SYMBOL_TO_TEST: &str =
    "This DataSet names no symbol yet — type one under Symbols, or pick one in the list above.";
/// Why Test DataSet is ALWAYS dark: the window's one test slot (`tools::ToolView::ds_test`) opens a
/// chart for ONE symbol, and nothing opens a whole set.
const NO_DATASET_TEST: &str = "Nothing opens a whole DataSet — the one test this window can run opens \
                               a chart for a single symbol. Use Test symbol.";
/// Why Delete is dark: a set that was never saved has nothing on disk to delete.
const NOTHING_SAVED_TO_DELETE: &str =
    "This DataSet is not saved yet — there is nothing on disk to delete.";
/// Why Backfill is dark with no member shown.
const NO_MEMBERS_TO_QUEUE: &str = "This cut shows no member symbols, so there is nothing to queue.";

/// One heading of the DataSet tree: every set, the sets of one provider, or the user's own.
#[derive(Debug)]
pub(super) struct DsGroup {
    /// The heading: the provider's name as the settings database spells it, else its key.
    pub(super) label: String,
    /// `None` for "All" and "My DataSets"; the provider KEY a set names otherwise.
    provider: Option<String>,
    /// "My DataSets": the user's own sets.
    mine: bool,
}

impl DsGroup {
    /// Whether set `d` is listed under this heading.
    pub(super) fn has(&self, d: &datasets::DataSet) -> bool {
        match &self.provider {
            Some(p) => d.provider == *p,
            None => !self.mine || d.user,
        }
    }
}

/// The DataSet tree's headings (I15(a) of the Trade window plan): "All", then one per provider the
/// sets actually name, by key, each spelled through [`super::venue_label`] — so the settings
/// database spells it, or the key does, and no venue list lives in this file — then "My DataSets".
/// `Auto` is the form's sentinel rather than a venue ([`provider_names_a_venue`]), so it heads no
/// group; its sets are under "All".
pub(super) fn dataset_groups(
    sets: &[datasets::DataSet],
    directory: Option<&vike_tradehub_client::wire::WireDirectory>,
) -> Vec<DsGroup> {
    let mut keys: Vec<&str> =
        sets.iter().map(|d| d.provider.as_str()).filter(|p| provider_names_a_venue(p)).collect();
    keys.sort_unstable();
    keys.dedup();
    let fixed =
        |label: &str, mine: bool| DsGroup { label: label.to_string(), provider: None, mine };
    let providers = keys.into_iter().map(|k| DsGroup {
        label: super::venue_label(directory, k).to_string(),
        provider: Some(k.to_string()),
        mine: false,
    });
    std::iter::once(fixed("All", false))
        .chain(providers)
        .chain([fixed("My DataSets", true)])
        .collect()
}

/// Load a DataSet into the Symbols-tab editor working copy.
pub(super) fn ds_load_form(tv: &mut tools::ToolView, d: &datasets::DataSet) {
    tv.ds_sel = Some(d.name.clone());
    tv.ds_name = d.name.clone();
    tv.ds_provider = d.provider.clone();
    tv.ds_interval = d.interval.clone();
    tv.ds_benchmark = d.benchmark.clone();
    tv.ds_symbols_text = d.symbols.join(", ");
    tv.ds_sym_sel = None;
}
