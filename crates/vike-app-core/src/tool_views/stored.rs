//! The Data Manager's "Stored" sub-tab — the local `HistStore` inventory grid. Moved verbatim from
//! `vike-app`'s `main.rs` (tool-view extraction batch 2); the adaptations are mechanical: the five
//! threaded read-only params now arrive grouped in [`super::StoredCtx`] (plus the DataSet store,
//! which supplies the grid's Watchlists), and the header's rows/bytes fold became the named,
//! unit-tested [`tree_totals`].
//!
//! `chart::fmt_thousands` is spelled `vike_ui_theme::fmt::fmt_thousands` here — the SAME function
//! (vike-chart re-exports it from vike-ui-theme), reached directly since this crate already
//! depends on the leaf, and it sits next to the `fmt_bytes` this body already called that way.

use super::ToolCtx;
use crate::inventory;
use crate::tools;
use std::collections::HashMap;
use vike_ui_theme::fmt::{fmt_bytes, fmt_thousands};
use vike_ui_theme::palette;

/// The stored-inventory body: `stored_catalog_grid` (dense venue-grouped grid + checkbox
/// multi-select + bulk-action bar, over `vike_app_core::inventory::build_tree`'s output) wrapped
/// with the app-coupled controls the shared crate deliberately excludes — a Refresh button, a
/// byte/row totals summary, and a confirm-gated delete (driven by either a single row-open,
/// unchanged, or the grid's multi-select "Delete" bulk action). Pure render + `tv`-field OUT
/// actions — the actual store I/O (the background load, the delete(s), the chart-open) all
/// happen in `App` (see `refresh_stored` and the `stored_*` drains in `App::ui`), never here.
///
/// ⚠ It used to mount `vike_data_manager::views_sidebar` too, in a left panel of its own. That
/// sidebar is GONE — see the comment at its old site below, and
/// [`crate::tool_views::data_rail`] for what replaced it. The caller now owns
/// `tv.stored_grid.active_view`, so this function is called by three rail destinations (All series,
/// Has gaps, Stale) that differ only in the filter they set before calling it.
pub fn stored_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::{Align, Color32, Layout, RichText};
    let (tree, gaps, loading) = (ctx.stored.tree, ctx.stored.gaps, ctx.stored.loading);

    let (total_rows, total_bytes) = tree_totals(tree);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !loading,
                egui::Button::new(RichText::new("↻ Refresh").color(palette::TEXT).size(13.0))
                    .fill(palette::SURFACE)
                    .stroke(egui::Stroke::new(1.0, palette::BORDER)),
            )
            .clicked()
        {
            tv.stored_refresh = true;
        }
        // The mode gate (the #1378 seam close): in remote mode Delete is a local-store operation
        // the grid's store cannot perform, so the button grays with the reason as hover text
        // rather than deleting from the LOCAL store the grid is not showing.
        let can_del = tv.stored_last_sel.is_some() && ctx.stored.delete_unavailable.is_none();
        let mut del_resp = ui
            .add_enabled(
                can_del,
                egui::Button::new(RichText::new("🗑 Delete").color(palette::TEXT).size(13.0))
                    .fill(palette::SURFACE)
                    .stroke(egui::Stroke::new(1.0, palette::BORDER)),
            )
            .on_hover_text("Delete the last-opened series (irreversible)");
        if let Some(reason) = ctx.stored.delete_unavailable {
            del_resp = del_resp.on_disabled_hover_text(reason);
        }
        if del_resp.clicked() {
            tv.stored_confirm_delete = tv.stored_last_sel.clone();
        }
        if loading {
            ui.label(RichText::new("Loading…").size(12.0).color(palette::TEXT3));
        }
        // dm-bulk-backfill: the grid's bulk Backfill/Update status ("Backfilling N series (M
        // skipped)…" while `App::maybe_spawn_stored_backfill`'s worker is running, then the final
        // "N backfilled, M failed, K skipped" once it lands) — empty before the first bulk click.
        if !ctx.stored.backfill_status.is_empty() {
            ui.label(RichText::new(ctx.stored.backfill_status).size(12.0).color(palette::TEXT3));
        }
        // The coverage LEGEND, right-aligned. The grid's bars carry three states — covered, a gap
        // cut-out, and the stale tint — and before this the only way to learn which was which was
        // to already know. It is the half of the design that makes the bars readable.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} rows · {}",
                    fmt_thousands(total_rows as f64).trim_end_matches(".00"),
                    fmt_bytes(total_bytes),
                ))
                .size(11.0)
                .color(palette::TEXT3),
            );
            ui.separator();
            for (swatch, what, col) in [
                ("▬", "stale", palette::TEXT3),
                ("▬", "covered", palette::BLUE),
                ("⚠", "partial day", palette::WARN),
            ] {
                ui.label(RichText::new(what).size(10.5).color(palette::TEXT3));
                ui.label(RichText::new(swatch).size(10.5).color(col));
            }
        });
    });
    // Remote mode's honest "local-only column" disclosure (the #1378 seam close): the Partial
    // column cannot be computed over the wire, so it carries a visible note, never a silent empty.
    if let Some(note) = ctx.stored.partials_note {
        ui.label(RichText::new(format!("⚠ {note}")).size(11.0).color(palette::TEXT3));
    }
    crate::tool_views::data_rail::strip_rule(ui);

    // First-shown trigger: the very first time this tab renders with an empty tree and no load
    // already in flight, request a load exactly once per window (see `stored_auto_requested`'s
    // doc on `ToolView`) — a manual Refresh click always works regardless of this flag.
    if !tv.stored_auto_requested && !loading && tree.is_empty() {
        tv.stored_auto_requested = true;
        tv.stored_refresh = true;
    }

    // ⚠ The Polymarket egress proxy box MOVED to the Providers destination
    // (`crates/vike-app-core/src/tool_views/data.rs`'s `data_body`). It is not lost, and the reason
    // it sat here is PRESERVED rather than overridden: it had to stay above this function's
    // loading early-return, because the screen an operator reaches it from is precisely an empty or
    // still-loading store — which is what "Polymarket data is not arriving" looks like.
    //
    // Providers keeps that property and strengthens it. That destination has no early return at
    // all, it is reachable while this one is still loading, and "where can data come from, and why
    // is none arriving" is the question it exists to answer. Here it was the second thing on the
    // window's busiest screen, above the grid the screen is named for.
    if loading && tree.is_empty() {
        ui.weak("Loading stored data…");
        return;
    }

    // ⚠ THE UNREACHABLE-vs-EMPTY SPLIT, and the reason it is a branch of its own rather than a
    // label added below.
    //
    // An empty tree has always had TWO causes — a store with nothing recorded, and a store nothing
    // could read — and until `StoredCtx::load_error` existed they produced the identical picture:
    // the grid below, with "No stored data · 0 rows · 0 B" in its header. So a dead tunnel, a
    // datahub that was not running, a key the server refused and a genuinely fresh install all
    // looked the same, and the load logged nothing at any level that could separate them either.
    // Whichever one an operator guessed, the screen agreed with them.
    //
    // The two must therefore render DIFFERENTLY, and the difference has to be the first thing on
    // the screen rather than a footnote under an empty grid that reads as an answer. So this
    // returns instead of falling through: showing an empty grid AND a failure reason would be the
    // same ambiguity in two panes, and the grid's own totals row ("0 rows") is a claim about the
    // store that nothing here is in a position to make.
    if let Some(why) = ctx.stored.load_error {
        ui.add_space(4.0);
        ui.label(
            RichText::new("⚠ The stored catalog could not be read").size(13.0).color(palette::WARN),
        );
        ui.add_space(2.0);
        // The reason VERBATIM from the load — `stored_load`'s own text, which names which verb went
        // unanswered and says whether the store refused or simply went quiet. It is deliberately
        // not paraphrased here: the wording an operator needs to act on is the one in the log, and
        // two spellings of one failure is how the two stop matching.
        ui.label(RichText::new(why).size(11.5).color(palette::TEXT2));
        ui.add_space(2.0);
        ui.label(
            RichText::new(
                "This is NOT an empty store — nothing could be read from it, so the grid has \
                 nothing to show. Check that the datahub is running and reachable, then press \
                 ↻ Refresh.",
            )
            .size(11.0)
            .color(palette::TEXT3),
        );
        return;
    }

    // ⚠ There is NO sidebar here any more, and its absence is the redesign.
    //
    // This body used to open an `egui::Panel::left` holding `vike_data_manager::views_sidebar` — a
    // second navigation (Views / Venues / Smart views / Watchlists) nested inside ONE of the Data
    // Manager's seven sub-tabs. Two navigations stacked, and the inner one was where the work
    // happened: a smart view you had to already be in the Stored tab to discover is a smart view
    // nobody applies.
    //
    // `crates/vike-app-core/src/tool_views/data_rail.rs` is that rail, promoted to be the window's
    // only one. It owns `tv.stored_grid.active_view` now — `data_tool_content` sets it from the
    // destination before calling this function — so this body renders the grid and nothing else.
    // ── grid | inspector ──
    //
    // The inspector is concept D's contribution: a PERMANENT detail pane, so clicking a series
    // costs nothing and you can walk a hundred of them. It also gives the honest disclosures a
    // home — the coverage range, the parts count, the gap list and the partial days all live in
    // hover text today, which is where a fact goes to not be read.
    //
    // ⚠ It is rendered HERE, app-side, off `tv.stored_last_sel`, and NOT inside
    // `vike_data_manager::stored_catalog_grid`. That crate is mounted by vike-studio too, whose
    // `data_browser.rs` records that the shared grid's fixed columns already sum to ~690px against
    // a 340px panel; growing the shared render would clip Studio, and no test covers its Data tab.
    // The selection is already app-owned, so the split costs the shared crate nothing.
    let insp_w = (ui.available_width() * 0.30).clamp(0.0, 330.0);
    let grid_w = (ui.available_width() - insp_w - 10.0).max(200.0);
    let resp = ui
        .horizontal_top(|ui| {
            let resp = ui
                .allocate_ui_with_layout(
                    egui::vec2(grid_w, ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| {
                        vike_data_manager::stored_catalog_grid(
                            ui,
                            tree,
                            &mut tv.stored_grid,
                            gaps,
                            ctx.stored.partials,
                            ctx.stored.delete_unavailable,
                        )
                    },
                )
                .inner;
            if insp_w > 120.0 {
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(insp_w, ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| inspector(ui, ctx, tv),
                );
            }
            resp
        })
        .inner;
    if let Some(sel) = resp.opened {
        tv.stored_open = Some((sel.venue.clone(), sel.symbol.clone(), sel.interval.clone()));
        tv.stored_last_sel = Some((sel.venue, sel.symbol, sel.kind, sel.interval));
    }
    if let Some(action) = resp.bulk {
        match action {
            vike_data_manager::BulkAction::Delete => {
                tv.stored_confirm_bulk_delete =
                    Some(tv.stored_grid.selected.iter().cloned().collect());
            }
            // dm-bulk-backfill: Backfill and Update alias to the SAME OUT slot — MVP has no
            // separate "ignore gaps, always refetch to now" mode (see
            // `App::maybe_spawn_stored_backfill`'s doc). Non-destructive (network fetch + idempotent
            // ingest), so unlike Delete this needs no confirm modal — it's drained straight into the
            // App's background backfill spawn (`main.rs`'s per-window loop, mirrors
            // `stored_bulk_delete`'s deferred-mutation shape).
            vike_data_manager::BulkAction::Backfill | vike_data_manager::BulkAction::Update => {
                tv.stored_backfill.extend(tv.stored_grid.selected.iter().cloned());
            }
        }
    }

    // Destructive-action confirm modal (mandatory — Delete never fires without it). Mirrors the
    // app's other confirm-style popups: a small centered `egui::Window`, Cancel/Delete only.
    if let Some((venue, symbol, kind, interval)) = tv.stored_confirm_delete.clone() {
        let mut open = true;
        let mut confirmed = false;
        let mut canceled = false;
        egui::Window::new("Delete stored series?")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                let kind_label = match &interval {
                    Some(iv) => format!("{kind}/{iv}"),
                    None => kind.clone(),
                };
                ui.label(format!(
                    "Delete {kind_label} for {venue}:{symbol}? This removes the data on disk \
                     and cannot be undone."
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        canceled = true;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("Delete").color(Color32::WHITE)))
                        .clicked()
                    {
                        confirmed = true;
                    }
                });
            });
        if confirmed {
            tv.stored_delete = Some((venue, symbol, kind, interval));
        }
        if confirmed || canceled || !open {
            tv.stored_confirm_delete = None;
        }
    }

    // The grid's multi-select bulk-delete counterpart to the modal above: same shape, N series
    // instead of one, confirmed/canceled the same way.
    if let Some(keys) = tv.stored_confirm_bulk_delete.clone() {
        let mut open = true;
        let mut confirmed = false;
        let mut canceled = false;
        egui::Window::new("Delete selected series?")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "Delete {} selected series? This removes the data on disk and cannot be \
                     undone.",
                    keys.len()
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        canceled = true;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("Delete").color(Color32::WHITE)))
                        .clicked()
                    {
                        confirmed = true;
                    }
                });
            });
        if confirmed {
            tv.stored_bulk_delete = keys;
            tv.stored_grid.selected.clear();
        }
        if confirmed || canceled || !open {
            tv.stored_confirm_bulk_delete = None;
        }
    }
}

/// Sum a venue-grouped inventory tree into the header's `(rows, bytes)` totals. Reads each venue's
/// PRE-AGGREGATED `total` rather than re-walking its symbol/series children, so it stays O(venues)
/// and cannot double-count a series that appears under two nodes. An empty tree totals `(0, 0)` —
/// what the header shows before the first background load lands.
fn tree_totals(tree: &[inventory::VenueNode]) -> (u64, u64) {
    tree.iter().fold((0u64, 0u64), |(r, b), v| (r + v.total.rows, b + v.total.bytes))
}

#[cfg(test)]
mod tests {
    use super::tree_totals;
    use crate::inventory::{RollUp, VenueNode};

    fn venue(name: &str, rows: u64, bytes: u64) -> VenueNode {
        VenueNode {
            venue: name.to_string(),
            symbols: Vec::new(),
            total: RollUp { rows, bytes, ..Default::default() },
        }
    }

    #[test]
    fn totals_sum_every_venue() {
        let tree = [venue("binance", 10, 100), venue("okx", 5, 50), venue("bybit", 1, 7)];
        assert_eq!(tree_totals(&tree), (16, 157));
    }

    #[test]
    fn totals_of_an_empty_tree_are_zero() {
        assert_eq!(tree_totals(&[]), (0, 0));
    }
}
/// Seed the Data Manager's **Polymarket proxy** box with the value actually IN FORCE, not merely
/// the one this box last wrote.
///
/// The box writes `POLY_SOCKS_PROXY`, but that is only the highest tier of what
/// `vike_polymarket::egress`'s `proxy_url` resolves — a store that predates this box configures the
/// proxy as `POLY_PROXY_HOST`/`POLY_PROXY_PORT` instead. Seeding from `POLY_SOCKS_PROXY` alone
/// showed an EMPTY box to an operator whose proxy was live and working, which makes "see the
/// current value" a lie in exactly the setup this repo has shipped with for months.
///
/// Mirrors `egress`'s precedence rather than re-deriving it: the explicit URL wins; otherwise the
/// host/port pair composes one; `POLY_PROXY_ENABLED=false` and the `none`/`direct` sentinels all
/// mean an empty box. The vike-polymarket crate cannot be called here — its `polymarket` feature is
/// not in vike-app's `default = ["fat"]`, so a default build has no `egress` to ask.
pub fn seed_polymarket_proxy_box(vars: &HashMap<String, String>) -> String {
    let get = |k: &str| vars.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    if let Some(enabled) = get("POLY_PROXY_ENABLED")
        && matches!(enabled.to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off")
    {
        return String::new();
    }
    if let Some(url) = get("POLY_SOCKS_PROXY") {
        return vike_data_manager::proxy_display(Some(url));
    }
    match (get("POLY_PROXY_HOST"), get("POLY_PROXY_PORT")) {
        (None, None) => String::new(),
        (host, port) => {
            format!("socks5h://{}:{}", host.unwrap_or("127.0.0.1"), port.unwrap_or("1080"))
        }
    }
}

#[cfg(test)]
mod polymarket_proxy_seed_tests {
    use super::seed_polymarket_proxy_box;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// The case this function exists for: a store configured the way THIS repo has shipped for
    /// months — host/port, no explicit URL. Seeding from `POLY_SOCKS_PROXY` alone showed an empty
    /// box to an operator whose proxy was live, making "see the current value" a lie.
    #[test]
    fn a_host_port_store_seeds_the_composed_url() {
        let v = vars(&[("POLY_PROXY_HOST", "127.0.0.1"), ("POLY_PROXY_PORT", "1080")]);
        assert_eq!(seed_polymarket_proxy_box(&v), "socks5h://127.0.0.1:1080");
    }

    /// The explicit URL is the highest tier and wins, credentials intact.
    #[test]
    fn an_explicit_url_wins_over_host_port() {
        let v = vars(&[
            ("POLY_SOCKS_PROXY", "socks5h://user:hunter2@1.2.3.4:1080"),
            ("POLY_PROXY_HOST", "127.0.0.1"),
            ("POLY_PROXY_PORT", "1080"),
        ]);
        assert_eq!(seed_polymarket_proxy_box(&v), "socks5h://user:hunter2@1.2.3.4:1080");
    }

    /// A half-configured store still composes, using the same fallbacks `egress` documents, so the
    /// box never shows a half-URL that would be wrong if saved back verbatim.
    #[test]
    fn a_half_configured_store_fills_in_the_documented_defaults() {
        assert_eq!(
            seed_polymarket_proxy_box(&vars(&[("POLY_PROXY_HOST", "<host>")])),
            "socks5h://<host>:1080"
        );
        assert_eq!(
            seed_polymarket_proxy_box(&vars(&[("POLY_PROXY_PORT", "9050")])),
            "socks5h://127.0.0.1:9050"
        );
    }

    /// The master OFF and both direct sentinels all mean the same thing to the operator: an EMPTY
    /// box. Anything else would show a proxy that is not in force.
    #[test]
    fn disabled_and_the_direct_sentinels_all_seed_an_empty_box() {
        for off in ["false", "0", "no", "off", "OFF"] {
            let v = vars(&[("POLY_PROXY_ENABLED", off), ("POLY_PROXY_HOST", "1.2.3.4")]);
            assert!(seed_polymarket_proxy_box(&v).is_empty(), "{off} must read as no proxy");
        }
        for direct in ["none", "direct", "NONE"] {
            let v = vars(&[("POLY_SOCKS_PROXY", direct)]);
            assert!(seed_polymarket_proxy_box(&v).is_empty(), "{direct} must read as no proxy");
        }
    }

    /// An unconfigured store seeds nothing — a fresh install shows an empty box rather than
    /// inventing the localhost tunnel default, which is the whole point of the box for a user who
    /// is not geo-blocked.
    #[test]
    fn an_unconfigured_store_seeds_an_empty_box() {
        assert!(seed_polymarket_proxy_box(&HashMap::new()).is_empty());
        assert!(seed_polymarket_proxy_box(&vars(&[("POLY_PROXY_HOST", "  ")])).is_empty());
    }
}

/// Persist the **Polymarket proxy** box's value to the credential store, and RECORD that it changed.
///
/// The ONE sanctioned write: a byte-preserving UPSERT of exactly `POLY_SOCKS_PROXY`, leaving every
/// other line, comment, blank line and their ORDER untouched, landed atomically — reached through
/// `vike_connections::save_credentials_journalled`, so the write and its append-only
/// `credential_write` record cannot come apart at this call site.
///
/// ⚠ **`creds` is a PARAMETER, and it used to be a walk.** This function called
/// `vike_secrets::workspace_dotenv_path` — the `_from`-less resolver, which is
/// `$VIKE_SETTINGS_DIR`-BLIND — so a deployment that names its settings directory outright had this
/// box writing into whatever project the working directory sat above. The caller now hands over the
/// store its own boot resolved, together with the ledger derived from the same walk. That is the
/// half of "only a binary touches the store" that actually matters.
///
/// ⚠ The record's tier is [`vike_model::change_journal::TIER_UNTIERED`], not a tier. `POLY_SOCKS_PROXY`
/// governs every Polymarket tier at once, so `"SIM"`/`"DEMO"`/`"LIVE"` would each be a false claim
/// about the other two.
///
/// ⚠ NEVER logs the value, and structurally cannot journal it either. A bought proxy URL carries
/// `user:pass@`, so the success line names the key and the restart requirement and nothing else, and
/// `Change::credential_write` accepts no value parameter at all.
pub fn save_polymarket_proxy(value: &str, creds: vike_connections::CredentialWrite<'_>) {
    let updates = vec![("POLY_SOCKS_PROXY".to_string(), value.to_string())];
    match vike_connections::save_credentials_journalled(
        creds,
        vike_model::change_journal::Actor::Gui,
        "polymarket",
        vike_model::change_journal::TIER_UNTIERED,
        &updates,
    ) {
        // The console copy stays beside the ledger, exactly as the Connections editor's does: this
        // is what an operator sees NOW, the ledger is what survives to be read later. `kind` names
        // the channel so the two read as the same event.
        Ok(()) => tracing::info!(
            kind = "credential_write",
            key = "POLY_SOCKS_PROXY",
            "polymarket proxy saved; takes effect on restart"
        ),
        Err(e) => tracing::error!(error = %e, "failed to save the polymarket proxy"),
    }
}

/// The stored grid's **permanent detail pane** — concept D.
///
/// Renders whatever `tv.stored_last_sel` names, looked up in the tree the grid is already showing.
/// Nothing is fetched: the coverage, the gap ranges and the partial days all come off `StoredCtx`,
/// which the grid needed anyway.
///
/// ⚠ **Why the facts here are not in a tooltip.** Every one of them — the exact span, the parts
/// count, the gap list, the cross-kind partial days, the disabled reason on Delete — currently
/// lives in hover text on the grid. A disclosure that requires you to suspect it first is a
/// disclosure nobody reads; the pane is the design's answer, and it costs a click that was already
/// being paid to select the row.
///
/// ⚠ **`tv` is `&mut` and used to be `&`.** The pane now carries an ACTION — the per-series
/// Backfill in its `.acts` row — which writes the one OUT slot the grid's bulk bar already
/// extends. Nothing else about the borrow changed: every fact rendered here is still read off
/// `ctx`, and this function still performs no I/O.
fn inspector(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::RichText;

    let Some((venue, symbol, kind, interval)) = tv.stored_last_sel.clone() else {
        ui.add_space(6.0);
        ui.label(RichText::new("Select a series").size(12.0).color(palette::TEXT));
        ui.label(
            RichText::new("Its coverage, gaps and partial days appear here — no hover needed.")
                .size(10.5)
                .color(palette::TEXT3),
        );
        return;
    };

    // Find the row in the tree the grid is rendering. A selection can outlive a refresh that
    // removed the series, so an absent row is a NORMAL state and says so rather than blanking.
    let row =
        ctx.stored.tree.iter().filter(|v| v.venue == venue).flat_map(|v| &v.symbols).find_map(
            |s| {
                (s.symbol == symbol)
                    .then(|| s.series.iter().find(|r| r.kind == kind && r.interval == interval))
                    .flatten()
            },
        );
    let Some(row) = row else {
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{venue} / {symbol}")).size(12.5).color(palette::TEXT));
        ui.label(
            RichText::new(
                "No longer in the store — it was deleted, or the last refresh dropped it.",
            )
            .size(10.5)
            .color(palette::TEXT3),
        );
        return;
    };

    ui.add_space(4.0);
    ui.label(RichText::new(&symbol).size(14.0).color(palette::TEXT));
    ui.label(
        RichText::new(match &interval {
            Some(iv) => format!("{venue} · {kind} · {iv}"),
            None => format!("{venue} · {kind}"),
        })
        .size(10.5)
        .color(palette::TEXT3),
    );
    ui.add_space(8.0);

    let kv = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(78.0, 14.0),
                egui::Layout::left_to_right(egui::Align::Min),
                |ui| {
                    ui.label(RichText::new(k).size(9.5).color(palette::TEXT3));
                },
            );
            ui.label(RichText::new(v).size(11.5).color(palette::TEXT2));
        });
    };
    kv(ui, "COVERS", vike_data_manager::coverage_label(&row.cov));
    kv(ui, "ROWS", fmt_thousands(row.cov.rows as f64).trim_end_matches(".00").to_string());
    kv(ui, "SIZE", fmt_bytes(row.cov.bytes));
    kv(ui, "PARTS", format!("{} · {} days", row.cov.parts, row.cov.dates));

    // ── What is MISSING: the two cuts, and the control that chooses between them ───────────────
    //
    // They are NOT two filters over one set, which is why this is a segmented control rather than
    // one list with a checkbox over it. A per-series gap is a hole inside ONE series' own
    // timeline; a cross-kind partial day is a day on which the INSTRUMENT holds some of its kinds
    // and not others — structurally invisible per series, because each series is perfectly
    // contiguous on its own. `vike_data_manager::PartialDayMap`'s doc argues that distinction at
    // length, and it is the reason both facts were already rendered here unconditionally, stacked,
    // with nothing saying they answer different questions.
    //
    // `crates/vike-app-core/src/tool_views/data.rs`'s Has-gaps destination makes the same cut over
    // the WHOLE store; this one makes it for the SELECTED series. Each option carries its own
    // count, so the choice is informed BEFORE it is made — an empty cut behind an unlabelled
    // button is indistinguishable from a cut nobody looked at.
    let key = vike_data_manager::SeriesKey {
        venue: venue.clone(),
        symbol: symbol.clone(),
        kind: kind.clone(),
        interval: interval.clone(),
    };
    let ranges: &[(i64, i64)] = ctx.stored.gaps.get(&key).map(Vec::as_slice).unwrap_or(&[]);

    // ⚠ The instrument key is built from the TREE NODE, never from the `SeriesKey` above. It needs
    // `SymbolNode::grouped`, which no `SeriesKey` carries — and the node's `symbol` is already
    // `SeriesId::label()` (`build_tree` keys on it), which is the spelling a GROUPED series is
    // identified by. `crates/vike-app-core/src/stored_load.rs` records what the other spelling
    // cost: keyed on a grouped series' EMPTY `symbol`, every polymarket group rendered gap-free
    // and no test caught it, because every fixture in that file is `SeriesId::per_symbol`, where
    // the two spellings are indistinguishable.
    let sym_node = ctx
        .stored
        .tree
        .iter()
        .filter(|v| v.venue == venue)
        .flat_map(|v| &v.symbols)
        .find(|s| s.symbol == symbol);
    let days: &[vike_data::PartialDay] = sym_node
        .map(|n| vike_data_manager::view::instrument_key_of(&venue, n))
        .and_then(|k| ctx.stored.partials.get(&k))
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    // The cut lives in egui TEMP MEMORY keyed off this pane's `Ui` id — the same home
    // `data.rs`'s Has-gaps segmented filter and the grid's own `SortState` use, and deliberately
    // NOT a new `ToolView` field: a per-frame widget preference that nothing outside this function
    // reads has no business on the struct every tool body threads.
    let cut_id = ui.id().with("dm_insp_cut");
    let mut cut: InspectorCut =
        ui.data_mut(|d| d.get_temp::<InspectorCut>(cut_id)).unwrap_or_default();
    ui.add_space(8.0);
    let options = [
        (
            format!("per-series gaps ({})", ranges.len()),
            "Holes inside THIS series' own timeline — days it holds nothing, between days it does",
        ),
        (
            format!("cross-kind partial days ({})", days.len()),
            "Days on which this INSTRUMENT holds some of its kinds and not others — a hole no \
             single series' own coverage bar can show",
        ),
    ];
    let pressed = match cut {
        InspectorCut::PerSeries => 0,
        InspectorCut::CrossKind => 1,
    };
    if let Some(i) = inspector_segmented(ui, &options, Some(pressed)) {
        cut = if i == 0 { InspectorCut::PerSeries } else { InspectorCut::CrossKind };
    }
    ui.data_mut(|d| d.insert_temp(cut_id, cut));
    ui.add_space(4.0);

    // ⚠ The cut is STICKY across selections, which is the price of storing it per-pane rather than
    // per-series. So an operator parked on one cut can land on a series that has nothing to show
    // under it — and an empty pane is indistinguishable from a broken one unless it says where the
    // content went. That is what the "…the other cut above" lines below are for; they are the
    // empty state's whole job, not decoration.
    match cut {
        InspectorCut::PerSeries => {
            if ranges.is_empty() {
                insp_note(
                    ui,
                    "No gaps of this series' own — contiguous across every day it holds.",
                );
                if !days.is_empty() {
                    insp_note(
                        ui,
                        &format!(
                            "Its instrument does carry {} cross-kind partial day(s) — the other \
                             cut above.",
                            days.len()
                        ),
                    );
                }
            } else {
                // Spelled out, because the grid paints these as cut-outs in the coverage bar,
                // which says THAT there is a hole and never WHICH days.
                ui.label(
                    RichText::new(format!("⚠ {} GAP RANGE(S)", ranges.len()))
                        .size(9.5)
                        .color(ui.visuals().warn_fg_color),
                );
                for (a, b) in ranges.iter().take(6) {
                    ui.label(
                        RichText::new(format!(
                            "  {} → {}",
                            vike_model::time::epoch_ms_to_utc_date(*a),
                            vike_model::time::epoch_ms_to_utc_date(*b)
                        ))
                        .size(11.0)
                        .color(palette::TEXT2),
                    );
                }
                if ranges.len() > 6 {
                    insp_note(ui, &format!("  …and {} more", ranges.len() - 6));
                }
            }
        }
        InspectorCut::CrossKind => {
            if days.is_empty() {
                insp_note(
                    ui,
                    "No cross-kind partial day — every day this instrument holds, it holds in \
                     every kind it records.",
                );
                if !ranges.is_empty() {
                    insp_note(
                        ui,
                        &format!(
                            "This series does carry {} gap range(s) of its own — the other cut \
                             above.",
                            ranges.len()
                        ),
                    );
                }
            } else {
                if let Some(node) = sym_node {
                    let label =
                        vike_data_manager::symbol_partial_label(ctx.stored.partials, &venue, node);
                    if !label.is_empty() {
                        ui.label(
                            RichText::new(format!("⚠ {label}"))
                                .size(11.0)
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                }
                // ⚠ One row per day naming what is ABSENT — NOT the design's ✓/— matrix with a
                // column per kind — and the reason is that the PRESENT half is not knowable from
                // here. `vike_data::PartialDay` carries `missing_kinds` alone; its complement
                // needs `vike_data::InstrumentCoverage::recorded_kinds`, which no `StoredCtx`
                // field reaches. Deriving the columns from the tree node's own series would be
                // WRONG rather than merely approximate: `vike_data::coverage::join_coverage`
                // considers only `TICK_KINDS`, while the node also carries bars and properties, so
                // every such column would print a ✓ for a kind the coverage report never looked
                // at. A matrix that cannot be built truthfully is not a narrower matrix — it is a
                // different claim.
                for d in days.iter().take(8) {
                    ui.label(
                        RichText::new(format!(
                            "  {} · no {}",
                            vike_model::time::epoch_ms_to_utc_date(d.start_ms()),
                            d.missing_kinds.join(", ")
                        ))
                        .size(11.0)
                        .color(palette::TEXT2),
                    );
                }
                if days.len() > 8 {
                    insp_note(ui, &format!("  …and {} more", days.len() - 8));
                }
                // The design's `.warnbox`, stated for the gate this build actually has rather than
                // for one venue's missing source: the action below is klines-only, so NO tick-kind
                // day is closeable from this pane whatever the venue.
                insp_note(
                    ui,
                    "Some kinds have data on those days and others do not — invisible in any one \
                     series' own bar. Backfill below is klines-only, so a tick-kind day closes by \
                     recording it live, never from here.",
                );
            }
        }
    }

    if let Some(note) = ctx.stored.partials_note {
        ui.add_space(6.0);
        insp_note(ui, &format!("⚠ {note}"));
    }

    // ── The action row — the design's `.acts` ─────────────────────────────────────────────────
    //
    // ONE verb, scoped to the SELECTED series, into the very OUT slot the grid's bulk bar already
    // extends (`tv.stored_backfill`, drained by `App::maybe_spawn_stored_backfill`). It is a real
    // job, not a second implementation of one: the planner, the route, the executor and the status
    // line are all the bulk path's, and the only thing this adds is a selection of size one — the
    // case that previously cost a checkbox tick in the grid to express.
    ui.add_space(10.0);
    // The enabled test is a PREDICTION of what `crate::backfill_plan::plan_backfill_jobs` will do
    // with this exact key, not a second rule beside it: `kind == "bar"` with an interval, and
    // deliberately NO venue term. 0059 Phase 3 deleted `SUPPORTED_BACKFILL_VENUES` — which venues
    // have collectors is the SERVER's roster, answered in its own refusal text naming its own
    // supported set — so a client-side venue gate here would gray a button a capable server would
    // have served, and would read in review as coverage.
    let can_backfill = kind == "bar" && interval.is_some();
    // Bound rather than inlined: an `if` inside the `RichText` chain formats into a four-line
    // block wedged between the constructor and its builder calls, which hides the chain.
    let ink = if can_backfill { palette::TEXT } else { palette::TEXT3 };
    ui.horizontal(|ui| {
        let mut resp = ui.add_enabled(
            can_backfill,
            egui::Button::new(RichText::new("Backfill").size(12.0).color(ink))
                .fill(palette::SURFACE)
                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                .min_size(egui::vec2(0.0, 26.0)),
        );
        // The honest-disabled shape: the reason names the thing that DOES NOT EXIST, never "not
        // implemented". Both refusals below are permanent properties of the series, so they say
        // what would have to change instead of implying a later build will differ.
        resp = if can_backfill {
            resp.on_hover_text(if ranges.is_empty() {
                "Fetch this series over a default lookback ending now — it carries no known hole \
                 to target. The server answers for its own venue roster."
                    .to_string()
            } else {
                format!(
                    "Fetch exactly this series' {} known gap range(s). The server answers for its \
                     own venue roster.",
                    ranges.len()
                )
            })
        } else if kind == "bar" {
            resp.on_disabled_hover_text(
                "This bar series carries no interval, so no kline request can name one.",
            )
        } else {
            resp.on_disabled_hover_text(format!(
                "The backfill verb is klines-only — a {kind} series has nothing to request of any \
                 server. Days it is missing close by recording it live."
            ))
        };
        // De-duplicated because this is a one-click action on a PERMANENTLY-visible pane: the
        // drain is next-frame, and while `maybe_spawn_stored_backfill` folds the vec into a
        // `BTreeSet` anyway (so a double click can never mean two jobs), a duplicate would still
        // skew the skipped/queued tally that run reports.
        if resp.clicked() && !tv.stored_backfill.contains(&key) {
            tv.stored_backfill.push(key.clone());
        }
    });
    // ⚠ The outcome is deliberately NOT restated here. `ctx.stored.backfill_status` is the one
    // status slot both the bulk bar and this button feed, and it is already rendered in this tab's
    // header strip; a second copy beside the button could only ever be the same string twice.
    //
    // ⚠ Residual, INHERITED rather than introduced: a click landing while a run is already in
    // flight is a SILENT no-op — `App::maybe_spawn_stored_backfill` returns early on
    // `stored_backfill_running`, and the grid's bulk bar has had that property since it shipped.
    // Closing it means carrying that flag on `StoredCtx`, which is not this file's to add.
}

/// Which cut of "what is missing" the [`inspector`]'s body lists — the design's two-way segmented
/// control, whose two options are different QUESTIONS rather than two filters over one set (see the
/// argument at the control's render site, and `vike_data_manager::PartialDayMap`'s own doc).
///
/// [`Self::PerSeries`] is the default because this is a series-scoped pane reached by clicking a
/// series row, and because it is the cut the Backfill action below it can actually act on — a
/// cross-kind partial day is a tick-kind day, which the klines-only backfill verb can never close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum InspectorCut {
    /// Holes inside the SELECTED series' own timeline — `StoredCtx::gaps`.
    #[default]
    PerSeries,
    /// Days on which the selected series' INSTRUMENT holds some of its kinds and not others —
    /// `StoredCtx::partials`, keyed one level above `gaps`.
    CrossKind,
}

/// One segmented control sized for the inspector — the design's `.seg`: attached buttons, at most
/// one pressed, no "off" state. Returns the index clicked this frame, or `None`.
///
/// ⚠ `egui::SelectableLabel` does NOT exist in egui 0.36; `egui::Button::selectable` is what
/// replaced it. That constructor also applies `frame_when_inactive(selected)`, which drops the
/// FRAME from every unpressed segment — leaving a row of floating words with one boxed word in it.
/// The `.frame_when_inactive(true)` below puts the border back, and it MUST come after the
/// constructor or the constructor's own call is the one that wins.
///
/// ⚠ **`horizontal_wrapped`, not `horizontal`, and that is not a style preference.** This pane is
/// `insp_w` wide — `ui.available_width() * 0.30`, clamped to 330px in [`stored_tool_content`] —
/// while the design's two labels carry a count each, so the pair can genuinely exceed the pane on a
/// narrow window. Wrapping to a second line keeps both COUNTS readable; a plain `horizontal` clips
/// the trailing segment, and its count is the one fact the control exists to show before it is
/// clicked.
///
/// ⚠ It is a near-twin of `crates/vike-app-core/src/tool_views/data.rs`'s `segmented`, which is
/// private to that module. Deliberately not merged: that one is a strip control on a full-width
/// destination taking `&[(&str, &str)]`, this one is a pane control whose labels are COMPUTED each
/// frame (hence `String`) and must wrap. If the two are ever unified, the owned label and the wrap
/// are the two properties that have to survive the merge.
fn inspector_segmented(
    ui: &mut egui::Ui,
    options: &[(String, &str)],
    selected: Option<usize>,
) -> Option<usize> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        // 1px rather than 0: egui draws each segment's own 1px stroke, so butting them flush would
        // paint two strokes on the shared edge and read as a heavier rule between the pair.
        ui.spacing_mut().item_spacing.x = 1.0;
        for (i, (label, why)) in options.iter().enumerate() {
            let on = selected == Some(i);
            let resp = ui
                .add(
                    egui::Button::selectable(
                        on,
                        egui::RichText::new(label.as_str()).size(10.5).color(if on {
                            palette::TEXT
                        } else {
                            palette::TEXT2
                        }),
                    )
                    .frame_when_inactive(true)
                    .fill(if on { palette::CARD } else { palette::SURFACE })
                    .stroke(egui::Stroke::new(
                        1.0,
                        if on { palette::ACCENT } else { palette::BORDER },
                    ))
                    .corner_radius(2.0)
                    .min_size(egui::vec2(0.0, 22.0)),
                )
                .on_hover_text(*why);
            if resp.clicked() {
                picked = Some(i);
            }
        }
    });
    picked
}

/// One dim explanatory line inside the inspector — the design's `.lead`/`.legend`, and the shape
/// every empty state and every refusal rule in this pane is stated in.
fn insp_note(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(10.5).color(palette::TEXT3));
}
