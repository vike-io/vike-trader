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
use vike_ui_theme::palette as theme;

/// The Data Manager's "Stored" tab (grid v2, dm-gridv2): mounts the shared
/// `vike_data_manager::views_sidebar` + `stored_catalog_grid` (dense venue-grouped grid +
/// checkbox multi-select + bulk-action bar, over `vike_app_core::inventory::build_tree`'s
/// output, left-nav-filterable by venue/smart-view/watchlist) and wraps it with the app-coupled
/// controls the shared crate deliberately excludes: a Refresh button, a byte/row totals summary,
/// and a confirm-gated delete (now driven by either a single row-open, unchanged, or the grid's
/// multi-select "Delete" bulk action). Watchlists are the app's `DataSet` names (Symbols tab),
/// passed straight through to `views_sidebar`'s Watchlists group. Pure render + `tv`-field OUT
/// actions — the actual store I/O (the background load, the delete(s), the chart-open) all
/// happen in `App` (see `refresh_stored` and the `stored_*` drains in `App::ui`), never here.
pub fn stored_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::{Align, Color32, Layout, RichText};
    const TEXT: Color32 = theme::TEXT;
    const T3: Color32 = theme::TEXT3;
    const SURF: Color32 = theme::SURFACE;
    const BORD: Color32 = theme::BORDER;
    let (tree, gaps, loading) = (ctx.stored.tree, ctx.stored.gaps, ctx.stored.loading);

    let (total_rows, total_bytes) = tree_totals(tree);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !loading,
                egui::Button::new(RichText::new("↻ Refresh").color(TEXT).size(13.0))
                    .fill(SURF)
                    .stroke(egui::Stroke::new(1.0, BORD)),
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
                egui::Button::new(RichText::new("🗑 Delete").color(TEXT).size(13.0))
                    .fill(SURF)
                    .stroke(egui::Stroke::new(1.0, BORD)),
            )
            .on_hover_text("Delete the last-opened series (irreversible)");
        if let Some(reason) = ctx.stored.delete_unavailable {
            del_resp = del_resp.on_disabled_hover_text(reason);
        }
        if del_resp.clicked() {
            tv.stored_confirm_delete = tv.stored_last_sel.clone();
        }
        if loading {
            ui.label(RichText::new("Loading…").size(12.0).color(T3));
        }
        // dm-bulk-backfill: the grid's bulk Backfill/Update status ("Backfilling N series (M
        // skipped)…" while `App::maybe_spawn_stored_backfill`'s worker is running, then the final
        // "N backfilled, M failed, K skipped" once it lands) — empty before the first bulk click.
        if !ctx.stored.backfill_status.is_empty() {
            ui.label(RichText::new(ctx.stored.backfill_status).size(12.0).color(T3));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} rows · {}",
                    fmt_thousands(total_rows as f64).trim_end_matches(".00"),
                    fmt_bytes(total_bytes),
                ))
                .size(11.0)
                .color(T3),
            );
        });
    });
    // Remote mode's honest "local-only column" disclosure (the #1378 seam close): the Partial
    // column cannot be computed over the wire, so it carries a visible note, never a silent empty.
    if let Some(note) = ctx.stored.partials_note {
        ui.label(RichText::new(format!("⚠ {note}")).size(11.0).color(T3));
    }
    ui.add_space(6.0);

    // First-shown trigger: the very first time this tab renders with an empty tree and no load
    // already in flight, request a load exactly once per window (see `stored_auto_requested`'s
    // doc on `ToolView`) — a manual Refresh click always works regardless of this flag.
    if !tv.stored_auto_requested && !loading && tree.is_empty() {
        tv.stored_auto_requested = true;
        tv.stored_refresh = true;
    }

    // The Polymarket egress proxy. It sits in the Data Manager because this is where an operator
    // comes when a venue's data is not arriving, and for Polymarket the usual reason is geo-blocking
    // rather than anything about the store. Pure render + one OUT action, like the grid below:
    // `App` owns the credential-store write.
    //
    // ⚠ ABOVE the loading early-return, deliberately. The proxy has nothing to do with the store's
    // inventory, and putting it after the grid meant it vanished on exactly the screen an operator
    // reaches it from: an empty or still-loading store — which is what "Polymarket data is not
    // arriving" looks like. Caught by looking at a real capture; every unit test still passed,
    // because a test that renders the row in isolation cannot see a `return` above it.
    if let Some(value) = vike_data_manager::polymarket_proxy_ui(ui, &mut tv.stored_proxy) {
        tv.stored_proxy_save = Some(value);
    }
    ui.separator();

    if loading && tree.is_empty() {
        ui.weak("Loading stored data…");
        return;
    }

    // [sidebar | grid]: the left-nav view filter (`views_sidebar`) and the rich grid share the
    // same `tv.stored_grid.active_view` — clicking a sidebar entry re-filters the grid next
    // frame. `Panel::left` (not a plain `ui.horizontal`) so the grid's `ScrollArea` still gets a
    // real bounded height from the remaining central area (mirrors vike-studio's editor/data
    // panel split).
    let watchlists: Vec<String> = ctx.dsets.sets.iter().map(|d| d.name.clone()).collect();
    egui::Panel::left("dm_stored_views_sidebar").resizable(true).default_size(180.0).show(
        ui,
        |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                vike_data_manager::views_sidebar(
                    ui,
                    tree,
                    &watchlists,
                    &mut tv.stored_grid.active_view,
                );
            });
        },
    );

    let resp = vike_data_manager::stored_catalog_grid(
        ui,
        tree,
        &mut tv.stored_grid,
        gaps,
        ctx.stored.partials,
        ctx.stored.delete_unavailable,
    );
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
