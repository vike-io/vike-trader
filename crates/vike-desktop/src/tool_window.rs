//! `tool_content`: the per-kind dispatcher every tool window's body is drawn through.

use super::*;

// allow, not expect: vike-desktop is built by no clippy suite of the measurement (CI's app-check judges it).
// window-loop dispatcher: all tool inputs arrive together
#[allow(clippy::too_many_arguments, clippy::allow_attributes)]
pub(crate) fn tool_content(
    ui: &mut egui::Ui,
    kind: workspace::WinKind,
    symbol: &str,
    book_venue: &str,
    book: Option<&vike_marketdata::L2Book>,
    book_stale: bool,
    trades: &data_sink::TradeStore,
    td: &tools::ToolData,
    feeds: &[(String, usize, i64, i64)],
    flags: &HashMap<String, egui::TextureHandle>,
    logos: &HashMap<String, egui::TextureHandle>,
    dsets: &datasets::Store,
    snap: &vike_exec::CoreSnapshot,
    tv: &mut tools::ToolView,
    display_tz: vike_chart::DisplayTz,
    studio: &mut Option<vike_studio::StudioState>,
    studio_error: &mut Option<String>,
    stored_tree: &Arc<Vec<vike_data_manager::model::VenueNode>>,
    stored_gaps: &Arc<vike_data_manager::GapMap>,
    stored_partials: &Arc<vike_data_manager::PartialDayMap>,
    stored_coverage: vike_app_core::data::stored_mode::RemoteCoverage,
    stored_history: Option<&vike_app_core::data::history_column::HistoryLoad>,
    stored_loading: bool,
    // `Some(reason)` → the last load could not READ the store (see `App::stored_error`). A separate
    // parameter rather than a richer `stored_loading` deliberately: every reader of that flag —
    // the Refresh enable, three "Loading…" labels, the rail — asks a question this reason does not
    // answer, and folding the two would have touched all of them to say the same thing.
    stored_load_error: Option<&str>,
    stored_backfill_status: &str,
    feed_statuses: &HashMap<String, Arc<std::sync::Mutex<String>>>,
    backend_picker: &tool_views::BackendPicker<'_>,
    backend_action: &mut Option<vike_app_core::backend::backend_conn::BackendAction>,
    backend_editor: &mut vike_app_core::backend::backend_editor::EditorState,
    backend_registry_update: &mut Option<vike_app_core::backend::backend_editor::RegistryUpdate>,
    backend_settings: &tool_views::BackendSettingsState,
    backend_settings_refresh: &mut bool,
    backend_settings_edit: &mut tool_views::SettingsEditState,
    backend_settings_write: &mut Option<tool_views::SettingsWriteRequest>,
    // The RESOLVED datahub address (`App::resolved_datahub_addr` — explicit `config.datahub_addr`
    // first, else the active backend's Welcome advertisement; REQ-2), threaded as a PARAMETER so
    // this free fn reads the same resolution the App methods do, never the raw key.
    resolved_datahub_addr: Option<&str>,
    // The datahub observe-key NAME to sign the STORE plane's dials with, resolved beside the
    // address by the caller (`App::datahub_observe_key_name`) for the reason that method's own doc
    // gives: both follow the ACTIVE backend, so a name taken at a different instant than the
    // address signs backend B's datahub with backend A's key — `bad mac`, with nothing on screen
    // that could explain it.
    datahub_key_name: &str,
    // The rect `workspace::tool_title_bar` reserved inside this window's title bar for a tabbed
    // tool's segmented control. The Trade arm seats its view controls there; every other kind
    // answers `WinKind::carries_title_tabs() == false`, slot inert. ⚠ The Connections arm used to
    // seat its own tabs here too — that arm and its `WinKind::Connections` are deleted, 2026-10-05.
    title_tabs: &workspace::TitleTabSlot,
    // The instrument catalog's owner (`App::catalog`) — read by the Data Manager's Instruments
    // destination for its rows and pressed by its Refresh button. A `&` because a refresh is a
    // `&self` call that spawns its own thread.
    catalog: &vike_app_core::data::catalog_refresh::CatalogRefresh,
    // The app's one appearance session (`App::appearance`) — the Settings window's body requests a
    // change on it; `draw_windows` applies it after every window has drawn.
    appearance: &mut AppearanceSession,
    // The Trade window's reads (`tool_views::ToolCtx`'s `symbols`/`last_trade`/`directory`/
    // `directory_unavailable`/`control_link`): the instrument catalog as its `Arc` (its pointer is
    // the catalog's identity, for the window's cache), the window a new one copies, the ACTIVE
    // backend's directory (`None` before it) and whether its fetch ended with none, and whether
    // this desktop can send an order at all.
    symbols: &Arc<vike_catalog::Catalog>,
    last_trade: Option<&tool_views::TradePick>,
    directory: Option<&vike_tradehub_client::wire::WireDirectory>,
    directory_unavailable: bool,
    key_changed: &[String],
    control_link: tool_views::ControlLink,
    // The process environment `main` read once (`App::env`) — the Studio arm hands its two QA
    // capture hooks to the session it builds.
    env: &GuiEnv,
) {
    // Every arm names its kind by path: a `use WinKind::*` turned an arm for a DELETED variant
    // into a binding that caught every kind (the `Dom` arm, A2's review). A path to a variant that
    // does not exist is a compile error instead.
    use workspace::WinKind;
    // The density's gap between the title bar and the body (design system spec §3.4): 6 at Normal,
    // today's value, 4 at Compact and 8 at Comfortable — none for the Trade window, whose bar ends
    // where its body begins (`workspace::title_bar::body_gap`).
    ui.add_space(workspace::title_bar::body_gap(
        kind,
        vike_ui_theme::appearance::current(ui.ctx()).density.metrics().gap,
    ));
    // The Stored tab's mode gates (the #1378 seam close): which of the grid's actions/columns
    // exist over the store the grid is reading — resolved from the SAME resolved datahub address
    // `refresh_stored` branches on, so the read mode and the offered actions cannot disagree.
    // Two inputs, two features: the RESOLVED datahub address (REQ-2 — explicit key, else the
    // active backend's advertisement) and `stored_coverage`, the §6-Q2 answer about what the last
    // load NEGOTIATED for the coverage verb, which the address alone cannot say (a remote server
    // may be older than the verb).
    let stored_mode_table =
        vike_app_core::data::stored_mode::stored_mode(resolved_datahub_addr, stored_coverage);
    // The moved tool bodies (vike-app-core `tool_views`) take their read-only inputs grouped in a
    // `ToolCtx` (audit F8), built per arm — cheap: a handful of refs. The Tearsheet's execution
    // history reads the RESOLVED datahub through THIS binary's dial (`remote_hist_store`: the observe
    // key needs the settings directory and the environment, which a library does not read).
    let history =
        tool_views::HistoryCtx::new(resolved_datahub_addr, datahub_key_name, remote_hist_store);
    let tool_ctx = || tool_views::ToolCtx {
        td,
        snap,
        flags,
        logos,
        history,
        feeds,
        dsets,
        display_tz,
        stored: tool_views::StoredCtx {
            tree: stored_tree.as_slice(),
            gaps: stored_gaps,
            partials: stored_partials,
            loading: stored_loading,
            load_error: stored_load_error,
            backfill_status: stored_backfill_status,
            delete_unavailable: stored_mode_table.delete_unavailable,
            partials_note: stored_mode_table.partials_unavailable,
            history: stored_history,
        },
        feed_statuses,
        // Where a Save in the Connections editor LANDS and where it is RECORDED — the store and the
        // change journal, both off the ONE boot walk. See `credential_write_ctx`.
        credentials: credential_home().write_ctx(vike_model::now_ms()),
        // The two book-backed windows share ONE transport group (a window is EITHER a Trade
        // window or a cockpit, never both): for the cockpit `symbol` is the token-id, `book_venue`
        // is "polymarket" and `book` is that token's live L2 book from the same `BookStore`.
        book: tool_views::BookCtx {
            symbol,
            venue: book_venue,
            book,
            stale: book_stale,
            trades: Some(trades),
        },
        catalog,
        symbols,
        last_trade,
        directory,
        directory_unavailable,
        key_changed,
        control_link,
    };
    match kind {
        WinKind::Trade => tool_views::trade_tool_content(ui, &tool_ctx(), tv, title_tabs),
        WinKind::Account => tool_views::account_tool_content(ui, &tool_ctx(), tv),
        WinKind::Polymarket => tool_views::cockpit_tool_content(ui, &tool_ctx(), tv),
        WinKind::Options => {
            // Resolve the selected underlying's bundle (default = first fetched, i.e. BTC), reset
            // an expiry the server does not list, and record the pick the options poll asks for.
            let bundle =
                tools::options_selection(td, &tv.opt_underlying_sel, &mut tv.opt_expiry_sel);
            let empty_chains = std::collections::BTreeMap::new();
            let empty_expiries: Vec<vike_marketdata::Expiry> = Vec::new();
            let (chains, default_expiry, expiries) = match bundle {
                Some(b) => (&b.chains, b.default_expiry.as_str(), b.expiries.as_slice()),
                None => (&empty_chains, "", empty_expiries.as_slice()),
            };
            // The user's OWN deribit working orders + positions, keyed by instrument name — the
            // chain paints per-strike markers from this (empty ⇒ grid unchanged). Built fresh each
            // frame from the lossy snapshot (cheap: O(orders+positions), both small).
            let books =
                vike_app_core::orders::options_books::build_books(&snap.orders, &snap.positions);
            let acts = vike_chart::options_chain::draw(
                ui,
                vike_chart::OptionChainInputs {
                    chains,
                    default_expiry,
                    expiries,
                    expiry_sel: &mut tv.opt_expiry_sel,
                    underlyings: &td.opt_underlyings,
                    underlying_sel: &mut tv.opt_underlying_sel,
                    strike_window: &mut tv.opt_strike_window,
                    books: &books,
                    // "deribit" is the chain's data-source KEY (an address, not a spelling).
                    venue_label: tool_views::venue_label(directory, "deribit"),
                },
            );
            // A working-order marker click → stash the coid in the OUT slot; the update loop drains
            // it into an `OrderIntent::Cancel` (mirrors the Trade window's `TradeAction::Cancel`).
            if let Some(coid) = acts.cancel {
                tv.opt_cancel = Some(coid);
            }
            // Refresh pill → wake the options poll thread for an IMMEDIATE ask instead of waiting
            // out its cadence (a closed channel at shutdown is ignored, never a panic).
            if acts.refresh_clicked {
                td.opt_link.refresh();
            }

            // A chain bid/ask click prefills a CONFIRM ticket — it NEVER submits an order (see the
            // confirm-gated submit path in `update`). Side/instrument/price come from the click;
            // qty defaults to a sensible deribit option size the user can edit.
            if let Some(click) = acts.order {
                tv.opt_order_ticket = Some(tools::OptOrderTicket {
                    instrument: click.instrument,
                    side: click.side,
                    price: click.price,
                    qty: 0.1,
                    kind: click.kind,
                    strike: click.strike,
                });
            }
            // SAFETY: the order is handed to the core (via `tv.opt_submit`) ONLY on Confirm.
            if let Some(mut ticket) = tv.opt_order_ticket.take() {
                match tool_views::order_ticket(ui.ctx(), &mut ticket) {
                    tool_views::TicketChoice::Confirmed => tv.opt_submit = Some(ticket),
                    tool_views::TicketChoice::Open => tv.opt_order_ticket = Some(ticket),
                    tool_views::TicketChoice::Cancelled => {}
                }
            }
        }
        WinKind::Greeks => tool_views::greeks_tool_content(ui, &tool_ctx()),
        WinKind::Tearsheet => tool_views::tearsheet_tool_content(ui, &tool_ctx(), tv),
        WinKind::Settings => tool_views::settings_tool_content(ui, appearance),
        WinKind::News => tool_views::news_tool_content(ui, &tool_ctx(), tv),
        WinKind::Calendar => tool_views::calendar_tool_content(ui, &tool_ctx(), tv),
        // ⚠ **Updated 2026-10-05: the standalone Connections window (`WinKind::Connections`) is
        // deleted outright**, and its real body call is merged onto this arm: the full
        // backend-picker/action/editor/registry/settings/settings_refresh/settings_edit/
        // settings_write argument set that used to run only inside the `Connections` arm is now
        // BUILT and PASSED every frame this window is open, regardless of which Data Manager rail
        // destination is selected — the same tab-independent argument `connections_tool_content`'s
        // own call made becomes a destination-independent one, exactly as `data_body`'s own doc
        // comment already predicts ("these ten are not reachable from `tv`/`ctx` alone").
        // `title_tabs` is dropped from the call: Data Manager navigates by RAIL, not title-bar
        // tabs, so `data_tool_content` takes no slot parameter at all.
        //
        // ⚠ **The CREDENTIAL READ itself is the one thing in that set that is NOT unconditional —
        // see the scoping note below.** Everything else in the ten-param set is cheap to build
        // (references and small owned state) whichever destination is showing; the credential
        // read is the one that opens a file, so it alone earns a gate.
        //
        // ⚠ The Gateway destination RENDERS what the store holds, so its grid is the account
        // rows read with their OWN verdict (`workspace_account_grids`: rows, key names and
        // credentials by field, decision 0156 item 6) — a store that would not open is a banner
        // and unknown cells, never `0 set`. The name map beside it feeds the Backend tab and the
        // grid's venue-wide attribution rows.
        //
        // ⚠ **Scoped to `DataDest::reads_credentials`'s three destinations, not every frame this
        // window is open.** The merge that deleted `WinKind::Connections` first landed this call
        // UNCONDITIONALLY on `WinKind::Data` — the standalone Connections window was the only
        // thing that opened this read, so nothing had to gate it there, and that scoping did not
        // survive the merge. Left ungated, Overview/AllSeries/Store/… (ten of thirteen
        // destinations, none of which read `creds`/`grids`/the catalog's credentialed-venues set)
        // paid a full credential-store open every frame, and an unreadable store's
        // `tracing::error!` (`vike_secrets::Resolved::report` on the per-frame read path)
        // fired unthrottled regardless of which destination was on screen.
        WinKind::Data => {
            let (creds, grids) = if tool_views::data_tab_reads_credentials(tv) {
                let creds = workspace_credentials();
                let grids = workspace_account_grids(&creds);
                // ⚠ **The one place this box's credential set reaches the Instruments screen**, and
                // it is here rather than at `App::new` because credentials are EDITED on this very
                // screen: a set resolved once at startup would tell an operator who had just saved
                // their alpaca keys that no keys were saved (`docs/decisions/0066` decision 9's
                // arming). This arm already performs the FRESH read — the comment above says so —
                // so the push costs one derivation over a grid already in hand, and the set is
                // the grid's own (`vike_gateway_panel::credentialed_venues`), which is what the panel
                // beside it renders. Gated the same as the read it is built from: a destination
                // that does not read credentials this frame leaves the catalog's set at whatever it
                // was last pushed, which is exactly as stale as every OTHER per-frame fact this
                // screen does not currently render.
                catalog.set_credentialed_venues(vike_gateway_panel::credentialed_venues(&grids));
                (creds, grids)
            } else {
                (HashMap::new(), vike_gateway_panel::AccountGrids::unread())
            };
            tool_views::data_tool_content(
                ui,
                &tool_ctx(),
                tv,
                &creds,
                &grids,
                backend_picker,
                backend_action,
                backend_editor,
                backend_registry_update,
                backend_settings,
                backend_settings_refresh,
                backend_settings_edit,
                backend_settings_write,
            );
        }
        WinKind::Studio => {
            open_studio(ui, studio, studio_error, resolved_datahub_addr, datahub_key_name, env)
        }
        WinKind::Chart => {}
    }
}
