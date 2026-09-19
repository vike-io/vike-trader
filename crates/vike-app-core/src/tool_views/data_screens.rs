//! The Data Manager's three NEW destinations — Overview, By venue and Store.
//!
//! The rail's other five re-house content that already existed. These three did not exist in any
//! form, and they are Phase 2 of the approved design.
//!
//! # ⚠ Every number here is DERIVED from what the window already loaded
//!
//! Not one of these screens fetches anything. `StoredCtx` already carries the venue tree, the
//! per-series gap map and the cross-kind partial-day map, because the stored grid needs all three;
//! these screens are folds over the same data. That is deliberate — a landing screen that issues
//! its own I/O is a landing screen that can be slow or wrong in a way the screen behind it is not.
//!
//! The one number the design asks for and this module does NOT render is **free disk space**. It is
//! the only tile that needs a syscall nothing else in this window makes, and an invented figure on
//! the screen whose job is to tell you whether you can afford a backfill is worse than no figure.
//!
//! # ⚠ What Overview deliberately does not show
//!
//! The design's fourth tile is *venues armed*. Producing it means reading the credential store, and
//! `crates/vike-app-core/src/tool_views/data_rail.rs`'s `DataDest::reads_arming` gates that read to
//! the venue-arming screen alone — a per-frame credential read behind the window's LANDING screen
//! is a cost every operator pays on every open, to render a number they came for once. The tile is
//! a link to that screen instead.

use super::ToolCtx;
use super::data_rail::{self, DataDest};
use crate::inventory::VenueNode;
use vike_ui_theme::fmt::{fmt_bytes, fmt_count_compact};
use vike_ui_theme::palette as theme;

/// How many series in `tree` are stale, and the tree-wide window they were judged against.
///
/// ⚠ This pays one `filter_tree` clone of the whole tree, which is why the RAIL deliberately carries
/// no stale badge (`data_rail::RailCounts`' doc argues that). Paying it on the ONE screen that shows
/// the number is the trade; paying it on every frame of every screen to render a badge is not.
fn stale_count(tree: &[VenueNode], gaps: &vike_data_manager::GapMap) -> (usize, (i64, i64)) {
    let span = vike_data_manager::global_span(tree);
    let filtered =
        vike_data_manager::filter_tree(tree, &vike_data_manager::ViewFilter::Stale, gaps);
    (filtered.iter().map(|v| v.total.series).sum(), span)
}

/// Format an epoch-ms bound as a UTC date, or `—` for the zero-width window an empty tree produces.
fn day(ts: i64) -> String {
    if ts <= 0 { "—".to_string() } else { vike_model::time::epoch_ms_to_utc_date(ts) }
}

/// One big-number tile.
fn tile(ui: &mut egui::Ui, n: &str, label: &str, sub: &str, warn: bool) {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .inner_margin(egui::Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_min_width(148.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(n).size(24.0).color(if warn {
                    ui.visuals().warn_fg_color
                } else {
                    theme::TEXT
                }));
                ui.label(egui::RichText::new(label).size(9.5).color(theme::TEXT3));
                if !sub.is_empty() {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(sub).size(10.5).color(theme::TEXT3));
                }
            });
        });
}

/// One attention row: a glyph, an identity, a ONE-SENTENCE reason, and the action.
fn task(
    ui: &mut egui::Ui,
    glyph: &str,
    title: &str,
    why: &str,
    action: Option<(&str, DataDest)>,
) -> Option<DataDest> {
    let mut go = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(glyph).size(12.0).color(ui.visuals().warn_fg_color));
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(title).size(12.5).color(theme::TEXT));
            ui.label(egui::RichText::new(why).size(11.0).color(theme::TEXT3));
        });
        if let Some((label, dest)) = action {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new(label).size(10.5).color(theme::TEXT))
                            .fill(theme::CARD)
                            .stroke(egui::Stroke::new(1.0, theme::BORDER)),
                    )
                    .clicked()
                {
                    go = Some(dest);
                }
            });
        }
    });
    ui.add_space(3.0);
    go
}

/// **Overview** — the window's landing screen: what needs attention, before the filing cabinet.
///
/// Returns a destination when the operator clicked through to one.
pub fn overview(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    counts: &data_rail::RailCounts,
) -> Option<DataDest> {
    let tree = ctx.stored.tree;
    let (stale, span) = stale_count(tree, ctx.stored.gaps);
    let partial_instruments = ctx.stored.partials.len();
    let bytes: u64 = tree.iter().map(|v| v.total.bytes).sum();
    let mut go = None;

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        tile(
            ui,
            &counts.gaps.to_string(),
            "SERIES WITH GAPS",
            &format!("of {}", counts.series),
            counts.gaps > 0,
        );
        tile(ui, &stale.to_string(), "STALE SERIES", "> 35% of the span behind", stale > 0);
        tile(
            ui,
            &partial_instruments.to_string(),
            "PARTIAL INSTRUMENTS",
            "a day some kinds miss",
            partial_instruments > 0,
        );
        tile(ui, &fmt_bytes(bytes), "ON DISK", &format!("{} series", counts.series), false);
    });

    ui.add_space(4.0);
    data_rail::strip_rule(ui);
    ui.label(
        egui::RichText::new(format!(
            "NEEDS ATTENTION   ·   judged against {} → {}",
            day(span.0),
            day(span.1)
        ))
        .size(9.5)
        .color(theme::TEXT3),
    );
    ui.add_space(5.0);

    // The rows are the REAL outstanding items, named from the maps the window already holds — not a
    // fixed list. An empty list is the good state and says so, rather than rendering an empty box
    // that reads as a failed load.
    let mut shown = 0usize;
    for (key, ranges) in ctx.stored.gaps.iter().take(3) {
        let days: i64 = ranges.iter().map(|(a, b)| (b - a) / 86_400_000 + 1).sum();
        if let Some(d) = task(
            ui,
            "\u{26A0}",
            &format!("{} / {} / {}", key.venue, key.symbol, key.kind),
            &format!(
                "{} missing day{} across {} range{}.",
                days,
                if days == 1 { "" } else { "s" },
                ranges.len(),
                if ranges.len() == 1 { "" } else { "s" }
            ),
            Some(("Has gaps", DataDest::HasGaps)),
        ) {
            go = Some(d);
        }
        shown += 1;
    }
    for (key, days) in ctx.stored.partials.iter().take(2) {
        let kinds: Vec<&str> =
            days.iter().flat_map(|d| d.missing_kinds.iter().map(String::as_str)).collect();
        if let Some(d) = task(
            ui,
            "\u{26A0}",
            &format!("{} / {}", key.venue, key.label),
            &format!(
                "{} day{} where some kinds have data and others do not ({}).",
                days.len(),
                if days.len() == 1 { "" } else { "s" },
                kinds.first().copied().unwrap_or("—")
            ),
            Some(("All series", DataDest::AllSeries)),
        ) {
            go = Some(d);
        }
        shown += 1;
    }
    if shown == 0 {
        ui.label(
            egui::RichText::new(if ctx.stored.loading {
                "Still loading — nothing to report yet."
            } else {
                "Nothing needs attention."
            })
            .size(12.0)
            .color(theme::TEXT3),
        );
    }

    ui.add_space(6.0);
    data_rail::strip_rule(ui);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("GO TO").size(9.5).color(theme::TEXT3));
        for d in
            [DataDest::AllSeries, DataDest::VenueArming, DataDest::DataSets, DataDest::ActivityLog]
        {
            if ui
                .add(
                    egui::Button::new(egui::RichText::new(d.label()).size(11.0).color(theme::TEXT))
                        .fill(theme::CARD)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER)),
                )
                .clicked()
            {
                go = Some(d);
            }
        }
    });
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "Venues armed is not a tile: producing it reads the credential store, which this \
             window pays for only on the Venues screen.",
        )
        .size(10.5)
        .color(theme::TEXT3),
    );
    go
}

/// **By venue** — one row per venue, no individual series.
pub fn by_venue(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    let tree = ctx.stored.tree;
    let (lo, hi) = vike_data_manager::global_span(tree);
    ui.label(
        egui::RichText::new(format!("Window  {} → {}", day(lo), day(hi)))
            .size(10.5)
            .color(theme::TEXT3),
    );
    ui.add_space(5.0);

    if tree.is_empty() {
        ui.label(egui::RichText::new("No stored data.").size(12.0).color(theme::TEXT3));
        return;
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("dm_by_venue").show(
        ui,
        |ui| {
            egui::Grid::new("dm_by_venue_grid")
                .num_columns(7)
                .spacing([14.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    for h in ["VENUE", "SYMBOLS", "SERIES", "ROWS", "SIZE", "GAPS", "STALE"] {
                        ui.label(egui::RichText::new(h).size(9.5).color(theme::TEXT3));
                    }
                    ui.end_row();

                    let (mut t_sym, mut t_ser, mut t_rows, mut t_bytes) =
                        (0usize, 0usize, 0u64, 0u64);
                    let (mut t_gap, mut t_stale) = (0usize, 0usize);
                    for v in tree {
                        // Per-venue gap and stale counts, folded from the maps the window already holds.
                        let gaps = ctx.stored.gaps.keys().filter(|k| k.venue == v.venue).count();
                        let stale = v
                            .symbols
                            .iter()
                            .flat_map(|s| s.series.iter())
                            .filter(|r| vike_data_manager::is_stale(r.cov.last_ts, lo, hi))
                            .count();

                        ui.label(egui::RichText::new(&v.venue).size(12.0).color(theme::TEXT));
                        ui.label(
                            egui::RichText::new(v.symbols.len().to_string())
                                .size(11.5)
                                .color(theme::TEXT2),
                        );
                        ui.label(
                            egui::RichText::new(v.total.series.to_string())
                                .size(11.5)
                                .color(theme::TEXT2),
                        );
                        ui.label(
                            egui::RichText::new(fmt_count_compact(v.total.rows))
                                .size(11.5)
                                .color(theme::TEXT2),
                        );
                        ui.label(
                            egui::RichText::new(fmt_bytes(v.total.bytes))
                                .size(11.5)
                                .color(theme::TEXT2),
                        );
                        ui.label(egui::RichText::new(gaps.to_string()).size(11.5).color(
                            if gaps > 0 { ui.visuals().warn_fg_color } else { theme::TEXT3 },
                        ));
                        ui.label(egui::RichText::new(stale.to_string()).size(11.5).color(
                            if stale > 0 { ui.visuals().warn_fg_color } else { theme::TEXT3 },
                        ));
                        ui.end_row();

                        t_sym += v.symbols.len();
                        t_ser += v.total.series;
                        t_rows += v.total.rows;
                        t_bytes += v.total.bytes;
                        t_gap += gaps;
                        t_stale += stale;
                    }

                    ui.label(
                        egui::RichText::new(format!("{} venues", tree.len()))
                            .size(12.0)
                            .color(theme::TEXT),
                    );
                    for (val, warn) in [
                        (t_sym.to_string(), false),
                        (t_ser.to_string(), false),
                        (fmt_count_compact(t_rows), false),
                        (fmt_bytes(t_bytes), false),
                        (t_gap.to_string(), t_gap > 0),
                        (t_stale.to_string(), t_stale > 0),
                    ] {
                        ui.label(egui::RichText::new(val).size(11.5).color(if warn {
                            ui.visuals().warn_fg_color
                        } else {
                            theme::TEXT
                        }));
                    }
                    ui.end_row();
                });
        },
    );
}

/// **Store** — which stores are mounted, what each can do, and what the layout on disk holds.
pub fn store(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    let remote = ctx.stored.delete_unavailable.is_some();
    let tree = ctx.stored.tree;

    ui.label(egui::RichText::new("MOUNTED STORES").size(9.5).color(theme::TEXT3));
    ui.add_space(4.0);
    egui::Grid::new("dm_stores").num_columns(4).spacing([16.0, 5.0]).striped(true).show(ui, |ui| {
        for h in ["STORE", "ACCESS", "SERIES", "SIZE"] {
            ui.label(egui::RichText::new(h).size(9.5).color(theme::TEXT3));
        }
        ui.end_row();

        let series: usize = tree.iter().map(|v| v.total.series).sum();
        let bytes: u64 = tree.iter().map(|v| v.total.bytes).sum();
        // The CURRENT mount is the one the grid is reading, and which one that is comes off
        // `delete_unavailable` — the same field the grid greys Delete from — rather than a second
        // probe that could disagree with it.
        let (cur, other) = if remote {
            ("remote datahub", "local store")
        } else {
            ("local store", "remote datahub")
        };
        ui.label(egui::RichText::new(format!("\u{25CF} {cur}")).size(12.0).color(theme::ACCENT));
        ui.label(
            egui::RichText::new(if remote { "read · list" } else { "read · write · list" })
                .size(11.5)
                .color(theme::TEXT2),
        );
        ui.label(egui::RichText::new(series.to_string()).size(11.5).color(theme::TEXT2));
        ui.label(egui::RichText::new(fmt_bytes(bytes)).size(11.5).color(theme::TEXT2));
        ui.end_row();

        ui.label(egui::RichText::new(format!("\u{25CB} {other}")).size(12.0).color(theme::TEXT3));
        for cell in ["not mounted", "—", "—"] {
            ui.label(egui::RichText::new(cell).size(11.5).color(theme::TEXT3));
        }
        ui.end_row();
    });

    ui.add_space(8.0);
    if let Some(reason) = ctx.stored.delete_unavailable {
        // The whole reason store identity is in the header. A read-only mount costs exactly two
        // things, and before this screen each announced itself separately, in its own hover text.
        ui.label(
            egui::RichText::new(format!("\u{26A0} On this mount: {reason}"))
                .size(11.0)
                .color(ui.visuals().warn_fg_color),
        );
    }
    if let Some(note) = ctx.stored.partials_note {
        ui.label(
            egui::RichText::new(format!("\u{26A0} {note}"))
                .size(11.0)
                .color(ui.visuals().warn_fg_color),
        );
    }

    data_rail::strip_rule(ui);
    ui.label(egui::RichText::new("LAYOUT ON DISK").size(9.5).color(theme::TEXT3));
    ui.add_space(4.0);

    // Per-KIND totals, folded from the same tree. No new walk: `kind` is already on every
    // `SeriesRow`, so this is a group-by over data the grid rendered a moment ago.
    let mut by_kind: std::collections::BTreeMap<&str, (usize, u64, u64)> =
        std::collections::BTreeMap::new();
    for v in tree {
        for s in &v.symbols {
            for r in &s.series {
                let e = by_kind.entry(r.kind.as_str()).or_default();
                e.0 += 1;
                e.1 += r.cov.rows;
                e.2 += r.cov.bytes;
            }
        }
    }
    if by_kind.is_empty() {
        ui.label(egui::RichText::new("Nothing stored.").size(12.0).color(theme::TEXT3));
        return;
    }
    egui::Grid::new("dm_kinds").num_columns(4).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
        for h in ["KIND", "SERIES", "ROWS", "SIZE"] {
            ui.label(egui::RichText::new(h).size(9.5).color(theme::TEXT3));
        }
        ui.end_row();
        for (kind, (n, rows, bytes)) in by_kind {
            ui.label(egui::RichText::new(kind).size(12.0).color(theme::TEXT));
            ui.label(egui::RichText::new(n.to_string()).size(11.5).color(theme::TEXT2));
            ui.label(egui::RichText::new(fmt_count_compact(rows)).size(11.5).color(theme::TEXT2));
            ui.label(egui::RichText::new(fmt_bytes(bytes)).size(11.5).color(theme::TEXT2));
            ui.end_row();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `day` refuses to render the sentinel an empty tree used to produce.
    ///
    /// ⚠ This is the reason `global_span` was fixed rather than worked around here. Before it
    /// returned `(0, 0)` for an empty tree it returned `(i64::MAX, i64::MIN)`, and By venue is the
    /// first caller that RENDERS the window as a date — `i64::MAX` epoch-ms is a date about 292
    /// million years out.
    #[test]
    fn an_empty_window_renders_as_a_dash_not_a_sentinel_date() {
        assert_eq!(day(0), "—");
        assert_eq!(day(-1), "—");
        assert_ne!(day(1_700_000_000_000), "—");
    }

    /// The empty tree's window is `(0, 0)`, which is what the doc has always claimed.
    #[test]
    fn global_span_of_an_empty_tree_is_zero_width() {
        assert_eq!(vike_data_manager::global_span(&[]), (0, 0));
    }
}
