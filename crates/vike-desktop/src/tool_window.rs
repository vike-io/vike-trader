//! `tool_content`: the per-kind dispatcher every tool window's body is drawn through.

use super::*;

// allow, not expect: vike-desktop is built by no clippy suite of the measurement (CI's app-check judges it).
// window-loop dispatcher: all tool inputs arrive together
#[allow(clippy::too_many_arguments, clippy::allow_attributes)]
pub(crate) fn tool_content(
    ui: &mut egui::Ui,
    kind: window::WindowKind,
    symbol: &str,
    book_venue: &str,
    book: Option<&vike_marketdata::L2Book>,
    book_stale: bool,
    trades: &data_sink::TickStore,
    tool_feeds: &vike_datahub_client::tool_feeds::poll::DatahubPollSnapshot,
    feeds: &[(String, usize, i64, i64)],
    country_flags: &HashMap<String, egui::TextureHandle>,
    logos: &HashMap<String, egui::TextureHandle>,
    symbol_set_store: &datasets::SymbolSetStore,
    snapshot: &vike_execution::CoreSnapshot,
    tv: &mut windows::WindowView,
    display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
    studio: &mut Option<vike_studio::StudioState>,
    studio_error: &mut Option<(String, u64)>,
    // The Data Manager's Stored inputs (`App::stored`), folded as the background load answers.
    stored: &vike_app_core::data::data_manager::DataManagerInventory,
    stored_backfill_status: &str,
    feed_statuses: &HashMap<String, Arc<std::sync::Mutex<DataClientStatus>>>,
    backend_picker: &windows::BackendPickerSources<'_>,
    backend_action: &mut Option<vike_app_core::backend::connection::BackendAction>,
    backend_editor: &mut vike_app_core::backend::editor::BackendEditorState,
    backend_registry_update: &mut Option<vike_app_core::backend::editor::BackendRegistryUpdate>,
    backend_settings: &windows::BackendSettingsState,
    backend_settings_refresh: &mut bool,
    backend_settings_edit: &mut windows::BackendSettingsEditState,
    backend_settings_write: &mut Option<windows::BackendSettingsWriteRequest>,
    // The RESOLVED datahub address (`App::resolved_datahub_address` — explicit `config.datahub_addr`
    // first, else the active backend's Welcome advertisement; REQ-2), threaded as a PARAMETER so
    // this free fn reads the same resolution the App methods do, never the raw key.
    resolved_datahub_address: Option<&str>,
    // The rect `window::tool_title_bar` reserved inside this window's title bar for a tabbed
    // tool's segmented control. The Trade arm seats its view controls there; every other kind
    // answers `WindowKind::carries_title_tabs() == false`, slot inert. ⚠ The Connections arm used to
    // seat its own tabs here too — that arm and its `WindowKind::Connections` are deleted, 2026-10-05.
    title_tabs: &window::WindowTitleBarTabSlot,
    // The instrument catalog's owner (`App::catalog`) — read by the Data Manager's Instruments
    // destination for its rows and pressed by its Refresh button. A `&` because a refresh is a
    // `&self` call that spawns its own thread.
    catalog_refresh: &vike_app_core::data::catalog_refresh::DatahubCatalogRefresh,
    // The app's one appearance session (`App::appearance`) — the Settings window's body requests a
    // change on it; `draw_windows` applies it after every window has drawn.
    appearance: &mut ThemeAppearanceSession,
    // The Trade window's reads (`windows::WindowToolContext`'s `catalog`/`last_trade`/`directory`/
    // `directory_unavailable`/`control_link`): the instrument catalog as its `Arc` (its pointer is
    // the catalog's identity, for the window's cache), the window a new one copies, the ACTIVE
    // backend's directory (`None` before it) and whether its fetch ended with none, and whether
    // this desktop can send an order at all.
    catalog: &Arc<vike_catalog::Catalog>,
    last_trade: Option<&windows::TradeWindowPick>,
    directory: Option<&vike_tradehub_client::wire::AccountDirectoryWire>,
    directory_unavailable: bool,
    node_words: &vike_tradehub_client::node_words::BackendNodeWords,
    control_link: vike_app_core::backend::tradehub_control::BackendControlLink,
    // Whether the connected node carries a post-only order (the Trade ticket's Post only).
    node_takes_post_only: bool,
    // Why the market-data datahub cannot be reached (`MarketDataSession::unreachable_why`), else `None`.
    datahub_unreachable: Option<&str>,
    // The process environment `main` read once (`App::env`) — the Studio arm hands its two QA
    // capture hooks to the session it builds.
    env: &GuiEnv,
) {
    // Every arm names its kind by path: a `use WindowKind::*` turned an arm for a DELETED variant
    // into a binding that caught every kind (the `Dom` arm, A2's review). A path to a variant that
    // does not exist is a compile error instead.
    use window::WindowKind;
    // The density's gap between the title bar and the body (design system spec §3.4): 6 at Normal,
    // today's value, 4 at Compact and 8 at Comfortable — none for the Trade window, whose bar ends
    // where its body begins (`window::title_bar::body_gap`).
    ui.add_space(window::title_bar::body_gap(
        kind,
        vike_ui_theme::appearance::current(ui.ctx()).density.metrics().gap,
    ));
    // The moved tool bodies (vike-app-core `windows`) take their read-only inputs grouped in a
    // `WindowToolContext` (audit F8), built per arm — cheap: a handful of refs. The Account window's History
    // tab reads the RESOLVED datahub through THIS binary's dial (`open_history_store_datahub`: the device
    // key needs the settings directory, which a library does not read). It names no key (decision 0288).
    let execution_history = windows::AccountWindowHistoryContext::new(
        resolved_datahub_address,
        open_history_store_datahub,
    );
    let tool_context = || windows::WindowToolContext {
        tool_feeds,
        snapshot,
        country_flags,
        logos,
        execution_history,
        feeds,
        symbol_set_store,
        display_time_zone,
        // The Stored tab's mode gates (the #1378 seam close) are decided from the SAME resolved
        // datahub address `refresh_stored` branches on, and what the last load negotiated for the
        // coverage verb, so the read mode and the offered actions cannot disagree.
        stored: windows::DataManagerInventoryContext::of(
            stored,
            resolved_datahub_address,
            stored_backfill_status,
        ),
        feed_statuses,
        // Where a Save in the Connections editor LANDS and where it is RECORDED — the store and the
        // change journal, both off the ONE boot walk. See `credential_write_ctx`.
        credentials: credential_home().write_context(vike_model::now_ms()),
        // The two book-backed windows share ONE transport group (a window is EITHER a Trade
        // window or a cockpit, never both): for the cockpit `symbol` is the token-id, `book_venue`
        // is "polymarket" and `book` is that token's live L2 book from the same `L2BookStore`.
        market: windows::WindowToolContextMarket {
            symbol,
            venue: book_venue,
            book,
            stale: book_stale,
            trades: Some(trades),
        },
        catalog_refresh,
        catalog,
        last_trade,
        directory,
        directory_unavailable,
        node_words,
        control_link,
        node_takes_post_only,
        datahub_unreachable,
    };
    match kind {
        WindowKind::Trade => windows::trade_tool_content(ui, &tool_context(), tv, title_tabs),
        WindowKind::Account => windows::account_tool_content(ui, &tool_context(), tv),
        WindowKind::Polymarket => windows::cockpit_tool_content(ui, &tool_context(), tv),
        WindowKind::Options => windows::options_tool_content(ui, &tool_context(), tv),
        WindowKind::Settings => {
            let unswept = HashMap::new();
            let vars = PROCESS_ENV.get().unwrap_or(&unswept);
            windows::settings_tool_content(ui, appearance, &mut tv.claude, vars)
        }
        WindowKind::News => {
            vike_news::news_tool_content(ui, &tool_context().news_inputs(ui.ctx()), &mut tv.news)
        }
        WindowKind::Calendar => windows::calendar_tool_content(ui, &tool_context(), tv),
        // ⚠ **Updated 2026-10-05: the standalone Connections window (`WindowKind::Connections`) is
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
        // ⚠ **The CREDENTIAL READ itself is the one thing in that set that is NOT unconditional.**
        // Everything else in the ten-param set is cheap to build (references and small owned state)
        // whichever destination is showing; the credential read opens the settings database, so it
        // alone earns a gate.
        //
        // ⚠ The Gateway destination RENDERS what the store holds, so its grid is the account
        // rows read with their OWN verdict (`workspace_account_grids`: rows, key names and
        // credentials by field, decision 0156 item 6) — a store that would not open is a banner
        // and unknown cells, never `0 set`. The name map it is read from feeds the grid's
        // venue-wide attribution rows.
        //
        // ⚠ **Read on arrival, held, never per frame** (`vike_app_core`'s
        // `DataManagerCredentialReads`, which owns the decision and its tests): only
        // `DataManagerDestination::reads_credentials`'s two destinations read at all, once when one
        // opens, again after this app's own credential write (the editor's write count) or the
        // Gateway's Refresh. It used to read on every frame such a destination was shown, and an
        // unreadable store's `tracing::error!` (`vike_secrets::CredentialRead::report`) fired with it.
        WindowKind::Data => {
            let writes = vike_account_manager::credential_writes();
            let (read, fresh) = tv.data_credentials.read(tv.data_destination, writes, || {
                windows::DataManagerCredentialRead {
                    grids: workspace_account_grids(&workspace_credentials()),
                }
            });
            // ⚠ **The one place this box's credential set reaches the Instruments screen**, on each
            // FRESH read rather than once at `App::new`, because credentials are EDITED on this very
            // window: a set resolved once at startup would tell an operator who had just saved their
            // alpaca keys that no keys were saved (`docs/decisions/0066` decision 9's arming). The
            // set is the grid's own (`vike_account_manager::venues_with_credentials`), which is what
            // the panel beside it renders.
            if fresh {
                let venues = vike_account_manager::venues_with_credentials(&read.grids);
                catalog_refresh.set_credentialed_venues(venues);
            }
            windows::data_tool_content(
                ui,
                &tool_context(),
                tv,
                &read.grids,
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
        WindowKind::Studio => open_studio(ui, studio, studio_error, resolved_datahub_address, env),
        WindowKind::Chart => {}
    }
}
