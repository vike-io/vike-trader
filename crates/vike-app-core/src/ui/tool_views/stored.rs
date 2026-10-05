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
use crate::tools;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Status, Tokens, section};
use vike_ui_theme::fmt::{fmt_bytes, fmt_thousands};
use vike_ui_theme::icons;
use vike_ui_theme::type_scale::TextRole;

/// The stored-inventory body: `stored_catalog_grid` (dense venue-grouped grid + checkbox
/// multi-select + bulk-action bar, over `vike_data_manager::build_tree`'s output) wrapped
/// with the app-coupled controls the shared crate deliberately excludes — a Refresh button, a
/// byte/row totals summary, and a confirm-gated delete (driven by either a single row-open,
/// unchanged, or the grid's multi-select "Delete" bulk action). Pure render + `tv`-field OUT
/// actions — the actual store I/O (the background load, the delete(s), the chart-open) all
/// happen in `App` (see `refresh_stored` and the `stored_*` drains in `App::ui`), never here.
///
/// ⚠ It used to mount `vike_data_manager::views_sidebar` too, in a left panel of its own. That
/// sidebar is GONE — see the comment at its old site below, and
/// [`crate::ui::tool_views::data_rail`] for what replaced it. The caller now owns
/// `tv.stored_grid.active_view`, so this function is called by three rail destinations (All series,
/// Has gaps, Stale) that differ only in the filter they set before calling it.
pub fn stored_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::{Align, Layout, RichText};
    let t = Tokens::of(ui.ctx());
    let (tree, gaps, loading) = (ctx.stored.tree, ctx.stored.gaps, ctx.stored.loading);

    let (total_rows, total_bytes) = tree_totals(tree);

    ui.horizontal(|ui| {
        let refresh = ActionButton::secondary((icons::REFRESH, "Refresh"));
        let refresh = if loading {
            refresh.disabled_because(super::data_screens::LOADING_WHY)
        } else {
            refresh
        };
        if ui.add(refresh).clicked() {
            tv.stored_refresh = true;
        }
        // The mode gate (the #1378 seam close) and the selection gate, each with its reason. With
        // no series open, Delete used to be dark and say nothing.
        let refusal = match (ctx.stored.delete_unavailable, &tv.stored_last_sel) {
            (Some(reason), _) => Some(reason),
            (None, None) => Some(NOTHING_OPENED),
            (None, Some(_)) => None,
        };
        let delete = ActionButton::secondary((icons::DELETE, "Delete"));
        let delete = match refusal {
            Some(why) => delete.disabled_because(why),
            None => delete,
        };
        if ui.add(delete).on_hover_text("Delete the last-opened series (irreversible)").clicked() {
            tv.stored_confirm_delete = tv.stored_last_sel.clone();
        }
        let status =
            |s: &str| egui::RichText::new(s).font(t.font(TextRole::Body)).color(t.theme.text3);
        if loading {
            ui.label(status("Loading…"));
        }
        // dm-bulk-backfill: the grid's bulk Backfill/Update status ("Backfilling N series (M
        // skipped)…" while `App::maybe_spawn_stored_backfill`'s worker is running, then the final
        // "N backfilled, M failed, K skipped" once it lands) — empty before the first bulk click.
        if !ctx.stored.backfill_status.is_empty() {
            ui.label(status(ctx.stored.backfill_status));
        }
        // The coverage LEGEND, right-aligned. It is the grid crate's own, painted in the bars' own
        // colours (owner decision 4). A nested horizontal keeps each mark left of its word inside
        // this right-to-left run.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} rows · {}",
                    fmt_thousands(total_rows as f64).trim_end_matches(".00"),
                    fmt_bytes(total_bytes),
                ))
                .font(t.font(TextRole::Caption))
                .color(t.theme.text3),
            );
            ui.separator();
            ui.horizontal(vike_data_manager::view::coverage_legend);
        });
    });
    // Remote mode's honest "local-only column" disclosure (the #1378 seam close): the Partial
    // column cannot be computed over the wire, so it carries a visible note, never a silent empty.
    if let Some(note) = ctx.stored.partials_note {
        let words = RichText::new(note).font(t.font(TextRole::Caption)).color(t.theme.text3);
        ui.label(icons::WARNING.before(ui.style(), words));
    }
    section::strip_rule(ui);

    // First-shown trigger: the very first time this tab renders with an empty tree and no load
    // already in flight, request a load exactly once per window (see `stored_auto_requested`'s
    // doc on `ToolView`) — a manual Refresh click always works regardless of this flag.
    if !tv.stored_auto_requested && !loading && tree.is_empty() {
        tv.stored_auto_requested = true;
        tv.stored_refresh = true;
    }

    // ⚠ The Polymarket egress proxy box MOVED to the Providers destination
    // (`crates/vike-app-core/src/ui/tool_views/data.rs`'s `data_body`). It is not lost, and the reason
    // it sat here is PRESERVED rather than overridden: it had to stay above this function's
    // loading early-return, because the screen an operator reaches it from is precisely an empty or
    // still-loading store — which is what "Polymarket data is not arriving" looks like.
    //
    // Providers keeps that property and strengthens it. That destination has no early return at
    // all, it is reachable while this one is still loading, and "where can data come from, and why
    // is none arriving" is the question it exists to answer. Here it was the second thing on the
    // window's busiest screen, above the grid the screen is named for.
    if loading && tree.is_empty() {
        state::view(ui, Load::Loading("Loading stored data…"));
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
        state::view(ui, Load::Unreachable("The stored catalog could not be read"));
        ui.vertical_centered(|ui| {
            // The reason VERBATIM from the load — `stored_load`'s own text, which names which verb
            // went unanswered and says whether the store refused or simply went quiet. It is
            // deliberately not paraphrased here: the wording an operator needs to act on is the one
            // in the log, and two spellings of one failure is how the two stop matching.
            ui.label(RichText::new(why).font(t.font(TextRole::Body)).color(t.theme.text2));
            ui.label(
                RichText::new(
                    "This is NOT an empty store — nothing could be read from it, so the grid has \
                     nothing to show. Check that the datahub is running and reachable, then press \
                     Refresh.",
                )
                .font(t.font(TextRole::Caption))
                .color(t.theme.text3),
            );
        });
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
    // `crates/vike-app-core/src/ui/tool_views/data_rail.rs` is that rail, promoted to be the window's
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
                    if ui.add(ActionButton::secondary("Cancel")).clicked() {
                        canceled = true;
                    }
                    if ui.add(ActionButton::danger("Delete")).clicked() {
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
                    if ui.add(ActionButton::secondary("Cancel")).clicked() {
                        canceled = true;
                    }
                    if ui.add(ActionButton::danger("Delete")).clicked() {
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

/// Why the stored strip's Delete is dark with no series open: it acts on the last one opened.
const NOTHING_OPENED: &str =
    "Open a series in the grid first — Delete acts on the last one opened.";

/// Sum a venue-grouped inventory tree into the header's `(rows, bytes)` totals. Reads each venue's
/// PRE-AGGREGATED `total` rather than re-walking its symbol/series children, so it stays O(venues)
/// and cannot double-count a series that appears under two nodes. An empty tree totals `(0, 0)` —
/// what the header shows before the first background load lands.
fn tree_totals(tree: &[vike_data_manager::model::VenueNode]) -> (u64, u64) {
    tree.iter().fold((0u64, 0u64), |(r, b), v| (r + v.total.rows, b + v.total.bytes))
}

#[cfg(test)]
mod tests {
    use super::tree_totals;
    use vike_data_manager::RollUp;
    use vike_data_manager::model::VenueNode;

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
/// Seed the Data Manager's **Polymarket proxy** box with the value actually IN FORCE — the
/// `venue.polymarket.*` rows the data daemon declares to the bridge (decision 0095), read from the
/// settings directory this binary's boot resolved. A store that will not open seeds an empty box and
/// says why in the log.
pub fn seed_polymarket_proxy_box(settings_dir: Option<&std::path::Path>) -> String {
    let polymarket = settings_dir.and_then(|dir| {
        match vike_secrets::venue_setting::load_venue_settings(dir) {
            Ok(mut all) => all.remove("polymarket"),
            Err(e) => {
                tracing::warn!(error = %e, "the venue_setting table could not be read; the proxy box opens empty");
                None
            }
        }
    });
    proxy_box_value(polymarket.as_ref())
}

/// [`seed_polymarket_proxy_box`]'s pure half. Mirrors `vike_polymarket::egress`'s precedence rather
/// than calling it (this crate links no Polymarket bridge): the explicit URL wins; otherwise the
/// host/port pair composes one; `proxy_enabled = false` and the `none`/`direct` sentinels all mean an
/// empty box.
fn proxy_box_value(polymarket: Option<&vike_secrets::venue_setting::VenueSettings>) -> String {
    use vike_secrets::venue_setting::SettingTier;
    let get = |field: &str| {
        polymarket
            .and_then(|s| s.get(SettingTier::Any, field))
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    if let Some(enabled) = get("proxy_enabled")
        && matches!(enabled.to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off")
    {
        return String::new();
    }
    if let Some(url) = get("socks_proxy") {
        return vike_data_manager::proxy_display(Some(url));
    }
    match (get("proxy_host"), get("proxy_port")) {
        (None, None) => String::new(),
        (host, port) => {
            format!("socks5h://{}:{}", host.unwrap_or("127.0.0.1"), port.unwrap_or("1080"))
        }
    }
}

#[cfg(test)]
mod polymarket_proxy_seed_tests {
    use super::proxy_box_value;
    use vike_secrets::venue_setting::VenueSettings;

    fn settings(pairs: &[(&str, &str)]) -> VenueSettings {
        let rows: Vec<vike_secrets::VenueSettingRow> = pairs
            .iter()
            .map(|(field, value)| vike_secrets::VenueSettingRow {
                venue: "polymarket".to_string(),
                tier: None,
                field: field.to_ascii_uppercase(),
                value: (*value).to_string(),
            })
            .collect();
        VenueSettings::from_rows("polymarket", &rows)
    }

    /// The case this function exists for: a store configured the way THIS repo has shipped for
    /// months — host/port, no explicit URL. Seeding from `socks_proxy` alone showed an empty
    /// box to an operator whose proxy was live, making "see the current value" a lie.
    #[test]
    fn a_host_port_store_seeds_the_composed_url() {
        let v = settings(&[("proxy_host", "127.0.0.1"), ("proxy_port", "1080")]);
        assert_eq!(proxy_box_value(Some(&v)), "socks5h://127.0.0.1:1080");
    }

    /// The explicit URL is the highest tier and wins, credentials intact.
    #[test]
    fn an_explicit_url_wins_over_host_port() {
        let v = settings(&[
            ("socks_proxy", "socks5h://user:hunter2@1.2.3.4:1080"),
            ("proxy_host", "127.0.0.1"),
            ("proxy_port", "1080"),
        ]);
        assert_eq!(proxy_box_value(Some(&v)), "socks5h://user:hunter2@1.2.3.4:1080");
    }

    /// A half-configured store still composes, using the same fallbacks `egress` documents, so the
    /// box never shows a half-URL that would be wrong if saved back verbatim.
    #[test]
    fn a_half_configured_store_fills_in_the_documented_defaults() {
        assert_eq!(
            proxy_box_value(Some(&settings(&[("proxy_host", "<host>")]))),
            "socks5h://<host>:1080"
        );
        assert_eq!(
            proxy_box_value(Some(&settings(&[("proxy_port", "9050")]))),
            "socks5h://127.0.0.1:9050"
        );
    }

    /// The master OFF and both direct sentinels all mean the same thing to the operator: an EMPTY
    /// box. Anything else would show a proxy that is not in force.
    #[test]
    fn disabled_and_the_direct_sentinels_all_seed_an_empty_box() {
        for off in ["false", "0", "no", "off", "OFF"] {
            let v = settings(&[("proxy_enabled", off), ("proxy_host", "1.2.3.4")]);
            assert!(proxy_box_value(Some(&v)).is_empty(), "{off} must read as no proxy");
        }
        for direct in ["none", "direct", "NONE"] {
            let v = settings(&[("socks_proxy", direct)]);
            assert!(proxy_box_value(Some(&v)).is_empty(), "{direct} must read as no proxy");
        }
    }

    /// An unconfigured store seeds nothing — a fresh install shows an empty box rather than
    /// inventing the localhost tunnel default, which is the whole point of the box for a user who
    /// is not geo-blocked.
    #[test]
    fn an_unconfigured_store_seeds_an_empty_box() {
        assert!(proxy_box_value(Some(&settings(&[]))).is_empty());
        assert!(proxy_box_value(Some(&settings(&[("proxy_host", "  ")]))).is_empty());
        assert!(proxy_box_value(None).is_empty());
    }
}

/// Persist the box's value as the `venue.polymarket.socks_proxy` row, and RECORD that it changed —
/// a declared SECRET field: never logged, journalled as `<secret>`.
///
/// ⚠ **`creds` is a PARAMETER, and it used to be a walk.** This function called
/// `vike_secrets::workspace_dotenv_path` — the `_from`-less resolver, which is
/// `$VIKE_SETTINGS_DIR`-BLIND — so a deployment that names its settings directory outright had this
/// box writing into whatever project the working directory sat above. The caller now hands over the
/// settings directory its own boot resolved. That is the half of "only a binary touches the store"
/// that actually matters.
pub fn save_polymarket_proxy(value: &str, creds: vike_connections::CredentialWrite<'_>) {
    // The grammar validates the TRIMMED value, so the row stores that value — a pasted URL's
    // trailing space or newline is not part of it (`vike-cli config set venue.*` does the same).
    let value = value.trim();
    // The catalog's grammar first, as `vike-cli config set` checks it — a refused value writes nothing.
    if let Some(field) = vike_model::venues::venue_fields::venue_field("polymarket", "socks_proxy")
        && let Err(why) = field.grammar.check(value)
    {
        tracing::error!("polymarket proxy NOT saved: {why}");
        return;
    }
    let settings_dir = vike_secrets::settings_dir_of_store(creds.store);
    match vike_secrets::set_venue_setting_in_journalled(
        &settings_dir,
        "polymarket",
        None,
        "SOCKS_PROXY",
        value,
        true,
        vike_secrets::AccountJournal {
            actor: vike_model::change_journal::Actor::Gui,
            proc: creds.proc.clone(),
            now_ms: creds.now_ms,
        },
    ) {
        Ok((_previous, journal_error)) => {
            if let Some(err) = &journal_error {
                tracing::error!(
                    error = %err,
                    dir = %err.dir.display(),
                    "polymarket proxy write NOT recorded to the change journal (the row IS saved)"
                );
            }
            tracing::info!(
                kind = "set_setting",
                key = "venue.polymarket.socks_proxy",
                "polymarket proxy saved; takes effect when the data daemon restarts"
            );
        }
        Err(refusal) => {
            let (e, _journal_error) = *refusal;
            tracing::error!(error = %e, "failed to save the polymarket proxy");
        }
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
    let t = Tokens::of(ui.ctx());

    let Some((venue, symbol, kind, interval)) = tv.stored_last_sel.clone() else {
        ui.add_space(6.0);
        ui.label(RichText::new("Select a series").font(t.font(TextRole::Body)).color(t.theme.text));
        ui.label(
            RichText::new("Its coverage, gaps and partial days appear here — no hover needed.")
                .font(t.font(TextRole::Caption))
                .color(t.theme.text3),
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
        ui.label(
            RichText::new(format!("{venue} / {symbol}"))
                .font(t.mono(TextRole::Body))
                .color(t.theme.text),
        );
        ui.label(
            RichText::new(
                "No longer in the store — it was deleted, or the last refresh dropped it.",
            )
            .font(t.font(TextRole::Caption))
            .color(t.theme.text3),
        );
        return;
    };

    ui.add_space(4.0);
    ui.label(RichText::new(&symbol).font(t.mono(TextRole::Title)).color(t.theme.text));
    ui.label(
        RichText::new(match &interval {
            Some(iv) => format!("{venue} · {kind} · {iv}"),
            None => format!("{venue} · {kind}"),
        })
        .font(t.font(TextRole::Caption))
        .color(t.theme.text3),
    );
    ui.add_space(8.0);

    let kv = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(78.0, 14.0),
                egui::Layout::left_to_right(egui::Align::Min),
                |ui| {
                    ui.label(RichText::new(k).font(t.font(TextRole::Caption)).color(t.theme.text3));
                },
            );
            ui.label(RichText::new(v).font(t.mono(TextRole::Body)).color(t.theme.text2));
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
    // `crates/vike-app-core/src/ui/tool_views/data.rs`'s Has-gaps destination makes the same cut over
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
    // identified by. `crates/vike-app-core/src/data/stored_load.rs` records what the other spelling
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
    let gaps_label = format!("per-series gaps ({})", ranges.len());
    let days_label = format!("cross-kind partial days ({})", days.len());
    let cuts = [
        Segment {
            value: InspectorCut::PerSeries,
            label: gaps_label.as_str(),
            why: "Holes inside THIS series' own timeline — days it holds nothing, between days it does",
        },
        Segment {
            value: InspectorCut::CrossKind,
            label: days_label.as_str(),
            why: "Days on which this INSTRUMENT holds some of its kinds and not others — a hole no \
                  single series' own coverage bar can show",
        },
    ];
    // The kit's segmented wraps on a narrow row, which is what this pane's two counted labels
    // need (the property `inspector_segmented` existed for).
    segmented::segmented(ui, &mut cut, &cuts);
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
                    icons::HAS_GAPS.before(
                        ui.style(),
                        RichText::new(format!("{} GAP RANGE(S)", ranges.len()))
                            .font(t.font(TextRole::Caption))
                            .color(Status::Warning.color()),
                    ),
                );
                for (a, b) in ranges.iter().take(6) {
                    ui.label(
                        RichText::new(format!(
                            "  {} → {}",
                            vike_model::time::epoch_ms_to_utc_date(*a),
                            vike_model::time::epoch_ms_to_utc_date(*b)
                        ))
                        .font(t.mono(TextRole::Body))
                        .color(t.theme.text2),
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
                            icons::WARNING.before(
                                ui.style(),
                                RichText::new(label)
                                    .font(t.font(TextRole::Body))
                                    .color(Status::Warning.color()),
                            ),
                        );
                    }
                }
                // ⚠ One row per day naming what is ABSENT — NOT the design's ✓/— matrix with a
                // column per kind — and the reason is that the PRESENT half is not knowable from
                // here. `vike_data::PartialDay` carries `missing_kinds` alone; its complement
                // needs `vike_data::InstrumentCoverage::recorded_kinds`, which no `StoredCtx`
                // field reaches. Deriving the columns from the tree node's own series would be
                // WRONG rather than merely approximate: `vike_data::store::coverage::join_coverage`
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
                        .font(t.mono(TextRole::Body))
                        .color(t.theme.text2),
                    );
                }
                if days.len() > 8 {
                    insp_note(ui, &format!("  …and {} more", days.len() - 8));
                }
                // The design's `.warnbox`, stated for the gate this build actually has rather than
                // for one venue's missing source: the action below requests BAR series only, so no
                // tick-kind day can be ASKED for from this pane whatever the venue. Dukascopy is the
                // one venue where a bar backfill stores ticks at all — its tick lane downloads the
                // bar window's ticks and keeps them as quotes — and the note says so.
                insp_note(
                    ui,
                    "Some kinds have data on those days and others do not — invisible in any one \
                     series' own bar. Backfill below requests bar series only, so a tick-kind day \
                     closes by recording it live — except on Dukascopy, where a bar backfill also \
                     stores the ticks it downloads for that window.",
                );
            }
        }
    }

    if let Some(note) = ctx.stored.partials_note {
        ui.add_space(6.0);
        insp_warning(ui, note);
    }

    // ── The action row — the design's `.acts` ─────────────────────────────────────────────────
    //
    // ONE verb, scoped to the SELECTED series, into the very OUT slot the grid's bulk bar already
    // extends (`tv.stored_backfill`, drained by `App::maybe_spawn_stored_backfill`). It is a real
    // job, not a second implementation of one: the planner, the route, the executor and the status
    // line are all the bulk path's, and the only thing this adds is a selection of size one — the
    // case that previously cost a checkbox tick in the grid to express.
    ui.add_space(10.0);
    // The enabled test is a PREDICTION of what `crate::data::backfill_plan::plan_backfill_jobs` will do
    // with this exact key, not a second rule beside it: `kind == "bar"` with an interval, and
    // deliberately NO venue term. 0059 Phase 3 deleted `SUPPORTED_BACKFILL_VENUES` — which venues
    // have collectors is the SERVER's roster, answered in its own refusal text naming its own
    // supported set — so a client-side venue gate here would gray a button a capable server would
    // have served, and would read in review as coverage.
    let can_backfill = kind == "bar" && interval.is_some();
    ui.horizontal(|ui| {
        // The honest-disabled shape: the reason names the thing that DOES NOT EXIST, never "not
        // implemented". Both refusals below are permanent properties of the series, so they say
        // what would have to change instead of implying a later build will differ (the existing
        // comment, kept).
        let refusal = if kind == "bar" {
            "This bar series carries no interval, so no bar request can name one.".to_string()
        } else {
            format!(
                "The backfill verb fills bar series only — a {kind} series has nothing to request \
                 of any server. Days it is missing close by recording it live (on Dukascopy, a \
                 bar backfill also stores the ticks it downloads)."
            )
        };
        let hover = if ranges.is_empty() {
            "Fetch this series over a default lookback ending now — it carries no known hole to \
             target. The server answers for its own venue roster."
                .to_string()
        } else {
            format!(
                "Fetch exactly this series' {} known gap range(s). The server answers for its own \
                 venue roster.",
                ranges.len()
            )
        };
        let backfill = ActionButton::secondary("Backfill");
        let backfill = if can_backfill { backfill } else { backfill.disabled_because(&refusal) };
        // De-duplicated because this is a one-click action on a PERMANENTLY-visible pane: the
        // drain is next-frame, and while `maybe_spawn_stored_backfill` folds the vec into a
        // `BTreeSet` anyway (so a double click can never mean two jobs), a duplicate would still
        // skew the skipped/queued tally that run reports.
        if ui.add(backfill).on_hover_text(hover).clicked() && !tv.stored_backfill.contains(&key) {
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
/// cross-kind partial day is a tick-kind day, which the backfill verb, filling bar series only,
/// cannot be asked to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum InspectorCut {
    /// Holes inside the SELECTED series' own timeline — `StoredCtx::gaps`.
    #[default]
    PerSeries,
    /// Days on which the selected series' INSTRUMENT holds some of its kinds and not others —
    /// `StoredCtx::partials`, keyed one level above `gaps`.
    CrossKind,
}

/// One dim explanatory line inside the inspector — the design's `.lead`/`.legend`, and the shape
/// every empty state and every refusal rule in this pane is stated in.
fn insp_note(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::of(ui.ctx());
    ui.label(egui::RichText::new(text).font(t.font(TextRole::Caption)).color(t.theme.text3));
}

/// An [`insp_note`] that must be read before the pane is trusted: it leads with `icons::WARNING`.
fn insp_warning(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::of(ui.ctx());
    let words = egui::RichText::new(text).font(t.font(TextRole::Caption)).color(t.theme.text3);
    ui.label(icons::WARNING.before(ui.style(), words));
}
