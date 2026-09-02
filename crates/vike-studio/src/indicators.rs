//! The Indicators pane: browse the vike-indicators registry (171 single-series indicators),
//! configure a selection's params, and preview the computed series over the currently loaded
//! bars. Pure browse/compute helpers here (`grouped_search`, `default_params`,
//! `compute_indicator`) are unit-tested; `IndicatorsPane::ui` (SP2 discipline) is not — it's
//! thin, calling only into the tested helpers below and `vike_indicators`/`vike_model` types.
use vike_indicators::{coerce, registry, Category, IndicatorMeta};
use vike_model::Bar;

/// Fixed display order for the pick-list groups. `Category` has no `Hash`/`Ord` (it's a plain
/// `Copy` discriminant enum — see `vike-indicators/src/registry.rs`), so grouping walks this
/// fixed order rather than building a map.
const CATEGORY_ORDER: [Category; 8] = [
    Category::Overlap,
    Category::Momentum,
    Category::Volatility,
    Category::Volume,
    Category::Statistics,
    Category::Pattern,
    Category::Price,
    Category::Structure,
];

/// Registry entries matching a case-insensitive substring of `name`/`pretty` (empty query
/// matches everything), grouped by [`Category`] in [`CATEGORY_ORDER`], skipping empty groups.
/// The one list-building function the pane's search box + grouped list both read.
pub fn grouped_search(query: &str) -> Vec<(Category, Vec<&'static IndicatorMeta>)> {
    let q = query.trim().to_lowercase();
    let matches: Vec<&'static IndicatorMeta> = registry()
        .iter()
        .filter(|m| {
            q.is_empty()
                || m.name.to_lowercase().contains(&q)
                || m.pretty.to_lowercase().contains(&q)
        })
        .collect();
    CATEGORY_ORDER
        .iter()
        .map(|&cat| {
            (cat, matches.iter().copied().filter(|m| m.category == cat).collect::<Vec<_>>())
        })
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

/// This indicator's default parameter values, in `make_with`'s slice order (empty for paramless
/// indicators like `vwap`/`obv`).
pub fn default_params(meta: &IndicatorMeta) -> Vec<f64> {
    meta.params.iter().map(|p| p.default).collect()
}

/// Coerce `raw_params` against `meta.params`, build a fresh instance, and batch-compute it over
/// `bars` — one output `Vec<f64>` per `meta.outputs` line. `bars` empty yields empty output
/// vecs (vectorize's own contract; no special-casing needed here). `coerce` is called explicitly
/// even though every registry `make_with` already coerces internally (registry.rs's own doc:
/// "external callers should call coerce(meta.params, raw) first too") — cheap and idempotent.
pub fn compute_indicator(meta: &IndicatorMeta, raw_params: &[f64], bars: &[Bar]) -> Vec<Vec<f64>> {
    let params = coerce(meta.params, raw_params);
    let ind = meta.build_with(&params);
    ind.vectorize(bars)
}

/// Studio pane state: search text, the selected registry entry (by name — stable across
/// re-filtering the search box), its current param values, and the last computed preview (or a
/// human-readable error). Owns no store/picker access; `StudioState::compute_indicator_preview`
/// (studio.rs) is what loads bars and calls [`IndicatorsPane::compute`].
#[derive(Default)]
pub struct IndicatorsPane {
    pub search: String,
    pub selected_name: Option<&'static str>,
    pub params: Vec<f64>,
    pub preview: Option<Vec<Vec<f64>>>,
    pub error: Option<String>,
}

impl IndicatorsPane {
    /// Select a registry entry by name, resetting its params to defaults and clearing any stale
    /// preview/error. No-op if `name` isn't in the registry.
    pub fn select(&mut self, name: &'static str) {
        let Some(meta) = vike_indicators::get(name) else { return };
        self.selected_name = Some(name);
        self.params = default_params(meta);
        self.preview = None;
        self.error = None;
    }

    /// Compute the selected indicator over `bars`, caching the result (or an error) for the
    /// preview. No-op if nothing is selected.
    pub fn compute(&mut self, bars: &[Bar]) {
        let Some(name) = self.selected_name else { return };
        let Some(meta) = vike_indicators::get(name) else { return };
        if bars.is_empty() {
            self.preview = None;
            self.error = Some("no bars loaded for the selected data slice".to_string());
            return;
        }
        self.preview = Some(compute_indicator(meta, &self.params, bars));
        self.error = None;
    }

    /// Render the browse list + param sliders + preview. Returns `true` the frame the user
    /// clicks "Compute" (the caller, `StudioState::ui`, then loads bars from the store and calls
    /// [`IndicatorsPane::compute`] — this method never touches the store itself).
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut compute_clicked = false;
        crate::theme::section_header(ui, "📈", "Indicators");
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .desired_width(f32::INFINITY)
                .hint_text("Search 171 indicators…"),
        );
        ui.add_space(2.0);
        egui::ScrollArea::vertical().id_salt("indicator-list").max_height(220.0).show(ui, |ui| {
            let groups = grouped_search(&self.search);
            if groups.is_empty() {
                ui.weak("No indicator matches that search.");
            }
            for (cat, metas) in groups {
                ui.label(egui::RichText::new(cat.label()).strong().color(crate::theme::ACCENT));
                for meta in metas {
                    let selected = self.selected_name == Some(meta.name);
                    if ui.selectable_label(selected, meta.pretty).clicked() && !selected {
                        self.select(meta.name);
                    }
                }
                ui.add_space(2.0);
            }
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        let Some(name) = self.selected_name else {
            ui.weak("Pick an indicator above to configure and preview it.");
            return compute_clicked;
        };
        let Some(meta) = vike_indicators::get(name) else { return compute_clicked };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(meta.pretty).strong());
            ui.label(egui::RichText::new(format!("({})", meta.name)).weak());
        });
        // Fit slider + trailing param label inside the fixed tools panel: the default slider
        // width plus a long param name is exactly the kind of over-wide row the old layout let
        // push the icon rail off-screen.
        ui.spacing_mut().slider_width = (ui.available_width() - 150.0).max(80.0);
        for (i, spec) in meta.params.iter().enumerate() {
            if let Some(v) = self.params.get_mut(i) {
                ui.add(
                    egui::Slider::new(v, spec.min..=spec.max).step_by(spec.step).text(spec.name),
                );
            }
        }
        ui.add_space(4.0);
        if ui
            .add(crate::theme::primary("Compute"))
            .on_hover_text("Preview over the selected data slice's bars")
            .clicked()
        {
            compute_clicked = true;
        }
        if let Some(err) = &self.error {
            ui.colored_label(crate::theme::ERR, err);
        } else if let Some(preview) = &self.preview {
            use egui_plot::{Line, Plot, PlotPoints};
            Plot::new("indicator-preview").height(160.0).show(ui, |p| {
                for (i, series) in preview.iter().enumerate() {
                    let pts: PlotPoints = series
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| !v.is_nan())
                        .map(|(idx, &v)| [idx as f64, v])
                        .collect();
                    let out_name =
                        meta.outputs.get(i).map(|o| o.name).unwrap_or(meta.name).to_string();
                    p.line(Line::new(out_name, pts));
                }
            });
        } else {
            ui.weak("Press Compute to preview.");
        }
        compute_clicked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closes(vals: &[f64]) -> Vec<Bar> {
        vals.iter()
            .enumerate()
            .map(|(i, &c)| Bar {
                ts: 60_000 * (i as i64 + 1),
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            })
            .collect()
    }

    #[test]
    fn compute_indicator_matches_a_hand_computed_sma() {
        let meta = vike_indicators::get("sma").expect("sma is in the registry");
        // closes 1..=20, explicit SMA length 5 -> at index 9 (bar #10, 1-based), the window is
        // closes[5..=9] = [6,7,8,9,10], mean = 8.0. (The registry default length is 20, which
        // would be all-NaN warm-up over only 20 bars up to index 9 — so we pass 5 explicitly.)
        let bars = closes(&(1..=20).map(|i| i as f64).collect::<Vec<_>>());
        let out = compute_indicator(meta, &[5.0], &bars);
        assert_eq!(out.len(), 1, "sma has exactly one output series");
        let sma = &out[0];
        assert_eq!(sma.len(), bars.len());
        let window: Vec<f64> = (6..=10).map(|v| v as f64).collect();
        let naive_mean = window.iter().sum::<f64>() / window.len() as f64;
        assert_eq!(sma[9], naive_mean, "sma[9] should equal the naive mean of closes[5..=9]");
        assert_eq!(naive_mean, 8.0);
    }

    #[test]
    fn compute_indicator_respects_overridden_params() {
        let meta = vike_indicators::get("sma").unwrap();
        let bars = closes(&(1..=10).map(|i| i as f64).collect::<Vec<_>>());
        // length 3 instead of the default 20 -> sma[2] = mean(closes[0..=2]) = 2.0
        let out = compute_indicator(meta, &[3.0], &bars);
        assert_eq!(out[0][2], 2.0);
    }

    #[test]
    fn compute_indicator_on_empty_bars_is_empty_not_panicking() {
        let meta = vike_indicators::get("sma").unwrap();
        let out = compute_indicator(meta, &default_params(meta), &[]);
        assert_eq!(out, vec![Vec::<f64>::new()]);
    }

    #[test]
    fn default_params_matches_registry_param_count_and_values() {
        let meta = vike_indicators::get("macd").unwrap();
        let defaults = default_params(meta);
        assert_eq!(defaults.len(), 3, "macd has fast/slow/signal");
        assert_eq!(defaults, vec![12.0, 26.0, 9.0]);
    }

    #[test]
    fn default_params_empty_for_paramless_indicator() {
        let meta = vike_indicators::get("vwap").unwrap();
        assert!(default_params(meta).is_empty());
    }

    #[test]
    fn grouped_search_empty_query_covers_the_whole_registry_exactly_once() {
        let groups = grouped_search("");
        let total: usize = groups.iter().map(|(_, v)| v.len()).sum();
        assert_eq!(total, registry().len(), "every registry entry appears in exactly one group");
        // no duplicates across groups
        let mut names: Vec<&str> =
            groups.iter().flat_map(|(_, v)| v.iter().map(|m| m.name)).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), registry().len());
    }

    #[test]
    fn grouped_search_filters_by_name_and_pretty_case_insensitively() {
        // "rsi" matches by name/pretty; the match set is identical regardless of query case.
        let lower: Vec<&str> =
            grouped_search("rsi").iter().flat_map(|(_, v)| v.iter().map(|m| m.name)).collect();
        let upper: Vec<&str> =
            grouped_search("RSI").iter().flat_map(|(_, v)| v.iter().map(|m| m.name)).collect();
        assert!(lower.contains(&"rsi"), "the rsi entry must match the query 'rsi'");
        assert_eq!(lower, upper, "search must be case-insensitive");
        // a query matching a `pretty` substring but not a `name` still hits (Connors RSI's
        // pretty is "Connors RSI" — proves pretty is searched, not just name).
        assert!(lower.contains(&"connors_rsi"));
    }

    #[test]
    fn grouped_search_no_match_returns_no_groups() {
        assert!(grouped_search("zzz_not_a_real_indicator_zzz").is_empty());
    }

    #[test]
    fn pane_select_resets_params_to_defaults_and_clears_preview() {
        let mut pane = IndicatorsPane::default();
        pane.select("sma");
        assert_eq!(pane.selected_name, Some("sma"));
        assert_eq!(pane.params, default_params(vike_indicators::get("sma").unwrap()));
        assert!(pane.preview.is_none());
    }

    #[test]
    fn pane_compute_populates_preview_for_the_selected_indicator() {
        let mut pane = IndicatorsPane::default();
        pane.select("sma");
        let bars = closes(&(1..=20).map(|i| i as f64).collect::<Vec<_>>());
        pane.compute(&bars);
        assert!(pane.error.is_none());
        let preview = pane.preview.as_ref().expect("compute should populate a preview");
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].len(), bars.len());
    }

    #[test]
    fn pane_compute_with_no_bars_sets_an_error_not_a_panic() {
        let mut pane = IndicatorsPane::default();
        pane.select("sma");
        pane.compute(&[]);
        assert!(pane.preview.is_none());
        assert!(pane.error.is_some());
    }

    #[test]
    fn pane_compute_without_a_selection_is_a_no_op() {
        let mut pane = IndicatorsPane::default();
        let bars = closes(&[1.0, 2.0, 3.0]);
        pane.compute(&bars);
        assert!(pane.preview.is_none());
        assert!(pane.error.is_none());
    }
}
