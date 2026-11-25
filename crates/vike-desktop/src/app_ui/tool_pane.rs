//! One TOOL window per frame, its drained intents and actions, and the window controls.

use super::*;

/// Window phase 3b — one TOOL window: the title bar and `tool_content` inside `show_window`
/// (the tool's own state split off `app` and restored after), then every OUT slot the tool left on
/// its view drained into [`WindowFrameOutput`] ([`drain_cockpit_intents`], [`drain_tool_view_actions`]).
/// Returns the title bar's actions for the window controls the loop applies to every kind.
pub(super) fn draw_tool_window(
    app: &mut App,
    ctx: &egui::Context,
    w: &mut window::WindowState,
    fr: &WindowFrame,
    snapshot: &FrameSnapshot,
    books: &Arc<data_sink::L2BookStore>,
    out: &mut WindowFrameOutput,
) -> WindowTitleBarActions {
    // A Trade window's book is "stale" if no update landed within this window (or none has yet).
    const TRADE_STALE_MS: i64 = 2000;
    let wid = w.id;
    let is_max = w.maximized;
    let mut act = WindowTitleBarActions::default();
    let kind = w.kind;
    let mut tv = app.window_views.remove(&wid).unwrap_or_default();
    // A Trade window's venue and venue-native symbol (spec §4.2), a cockpit's live token on
    // "polymarket"; "" for the other tools. Owned: `w` goes `&mut` into `show_window`.
    let symbol = windows::follow_cockpit(w, &mut tv, &app.catalog, fr.now_ms);
    let venue = if kind == window::WindowKind::Polymarket {
        "polymarket".to_string()
    } else {
        w.venue.clone()
    };
    // Studio (Task 7): the singleton session, split off `app` the same way `tv`
    // is (see `App::studio`'s doc — it's one instance, not keyed per-window), so
    // it's usable inside the `show_window` closure below and restored after.
    let mut studio = app.studio.take();
    let mut studio_error = app.studio_error.take();
    // Backends editor (split-plane I2): split off `app` the same way `studio`
    // is — one App-level instance (the same form whichever Connections window
    // renders it), usable inside the closure below and restored after.
    let mut backend_editor = std::mem::take(&mut app.backend_editor);
    // The book a book-backed window reads, keyed (venue, symbol) in the ONE `L2BookStore`.
    // `None` (no book yet) counts as stale so it never reads as live. A Trade window's
    // depth stream is requested by its drain below (`TradeWindowFrame::books`), after picks;
    // a cockpit's token stream here, unless it is the placeholder still resolving.
    // Cockpit books update far less often than a crypto book, hence `POLYMARKET_STALE_MS`.
    // NOTE: books reach this crate over the datahub market-data link
    // (`vike_app_core::data::market_data_session`), so a cockpit's stays stale until that link
    // populates the store under "polymarket".
    let book_ms = match kind {
        window::WindowKind::Trade if !symbol.is_empty() => Some(TRADE_STALE_MS),
        window::WindowKind::Polymarket => {
            if symbol != POLYMARKET_PLACEHOLDER_TOKEN {
                out.polymarket_book_requests.push(symbol.clone());
            }
            Some(POLYMARKET_STALE_MS)
        }
        _ => None,
    };
    let (book, book_stale) = match book_ms.map(|ms| (ms, books.get(&venue, &symbol))) {
        Some((ms, Some((book, timestamp)))) => (Some(book), fr.now_ms - timestamp > ms),
        Some((_, None)) => (None, true),
        None => (None, false),
    };
    if window::show_window(ctx, w, fr.desktop, |ui, bounds| {
        // ⚠ `tabs` is the rect this bar RESERVED between the window title and its
        // `─ □ ✕` controls for a tabbed tool's segmented control (Connections' tabs, the
        // Trade window's view controls).
        // The bar cannot draw those chips — their labels carry counts the BODY folds
        // one step later — so it hands the geometry down and the body paints into it
        // in the same pass. See `window::title_bar`'s module doc.
        let (acts, drag, tabs) = window::tool_title_bar(ui, kind, is_max);
        act = WindowTitleBarActions {
            close: acts.close,
            minimize: acts.minimize,
            toggle_max: acts.toggle_max,
            ..WindowTitleBarActions::default()
        };
        // vike tool bodies are inset ~8px from the window edges (the title bar stays
        // flush); the chart-title flush fix zeroed the window margin, so re-add it
        // here. ⚠ The inset is `window::TOOL_BODY_MARGIN` rather than a literal
        // because it is GEOMETRY the edge-resize bands depend on — that constant's
        // doc carries what each side buys, and the headless harness that gates it
        // models the shipped value rather than a second copy.
        egui::Frame::new().inner_margin(window::TOOL_BODY_MARGIN).show(ui, |ui| {
            // `bounds` is `show_window`'s body wrapper: for a NON-`fills` kind it
            // applies a `ScrollArea` whose SIZING follows whether this window is
            // app-owned yet (the user grabbed an edge band — see
            // `WindowState::user_sized`), the container itself being present from frame
            // one so the body's id chain never shifts. ⚠ This call site also
            // dispatches the two `fills` TOOL kinds — Trade and Polymarket — and for
            // those `WindowBodyBounds::show` wraps NOTHING and hands the body straight
            // through, so their id chains stay byte-identical to the unwrapped
            // spelling they had before this wrapper existed (an extra `Ui` level
            // would discard every egui-persisted widget state inside the ladder
            // and the cockpit, once, on upgrade). It lives in vike-app-core, not
            // here: `vike-desktop` is CI-excluded, so a wrap spelled at this call
            // site was gated by nothing.
            bounds.show(ui, |ui| {
                tool_content(
                    ui,
                    kind,
                    &symbol,
                    &venue,
                    book.as_ref(),
                    book_stale,
                    &app.trades,
                    &fr.tool_feeds,
                    &fr.feeds,
                    &fr.country_flags,
                    &fr.logos,
                    &fr.symbol_set_store,
                    &fr.core_snapshot,
                    &mut tv,
                    fr.display_time_zone,
                    &mut studio,
                    &mut studio_error,
                    &app.stored,
                    &snapshot.stored_backfill_status,
                    &app.feed_statuses,
                    &windows::BackendPickerSources {
                        backends: &app.backends,
                        active: app.active_backend.as_ref().map(|b| &b.record),
                        reported: vike_app_core::backend::identity::BackendIdentityReport::of(
                            fr.daemon_self_report.as_ref().map(|id| id.advertise_addr.as_str()),
                        ),
                    },
                    &mut out.backend_action,
                    &mut backend_editor,
                    &mut out.backend_registry_update,
                    &fr.backend_settings_view,
                    &mut out.backend_settings_refresh,
                    &mut out.backend_settings_edit,
                    &mut out.backend_settings_write,
                    fr.resolved_datahub_address.as_deref(),
                    &tabs,
                    &app.catalog,
                    &mut app.appearance,
                    &snapshot.symbols_catalog,
                    fr.last_trade.as_ref(),
                    fr.directory.as_deref(),
                    fr.directory_unavailable,
                    &fr.node_words,
                    fr.control_link,
                    fr.node_takes_post_only,
                    app.market_data_session.unreachable_why().as_deref(),
                    &app.env,
                );
            });
        });
        drag
    }) {
        out.user_moved_any = true;
    }
    // Trade: the window's picks, notes, seed and size, and its intents addressed with
    // the address it drew — `TradeWindowFrame::drain`'s whole decision.
    if kind == window::WindowKind::Trade {
        out.trade_frame.drain(w, &mut tv);
    }
    if kind == window::WindowKind::Polymarket {
        drain_cockpit_intents(&mut tv, book.as_ref(), &symbol, &mut out.cockpit_commands);
    }
    drain_tool_view_actions(ctx, &mut tv, fr.display_time_zone, out);
    app.window_views.insert(wid, tv);
    app.studio = studio;
    // The Studio window closing: its workspace edits are written now, not at its next frame.
    if kind == window::WindowKind::Studio && act.close {
        flush_studio_workspace(&mut app.studio);
    }
    app.studio_error = studio_error;
    app.backend_editor = backend_editor;
    act
}

/// Tool phase — the Polymarket cockpit: translate this frame's ladder + ticket intents into
/// `CockpitCommand`s here (where the ticket stake + live book are still in scope) for the post-loop
/// command fold.
fn drain_cockpit_intents(
    tv: &mut windows::WindowView,
    book: Option<&vike_marketdata::L2Book>,
    symbol: &str,
    cockpit_commands: &mut Vec<CockpitCommand>,
) {
    // Polymarket cockpit: translate this frame's ladder + ticket intents into
    // `CockpitCommand`s here (where the ticket stake + live book are still in scope) for
    // the post-loop command fold. The stake sizes both the ladder limits (shares =
    // stake/price) and the ticket market buys. Buying "Down" is modeled as SELL YES
    // (−1) on the SAME token — the paired NO-token id is not wired yet (see report).
    let stake = tv.cockpit_ticket.size;
    let up_px = book.and_then(|b| b.best_ask()).map(|l| l.price);
    let dn_px = book.and_then(|b| b.best_bid()).map(|l| 1.0 - l.price);
    for a in tv.cockpit_ladder_actions.drain(..) {
        match a {
            vike_order_entry::cockpit::CockpitProbabilityLadderAction::PlaceLimit {
                side,
                price,
            } => {
                let qty = if price > 0.0 { (stake / price).max(0.0) } else { 0.0 };
                cockpit_commands.push(CockpitCommand::Submit {
                    token: symbol.to_string(),
                    side,
                    price: Some(price),
                    quantity: qty,
                });
            }
            vike_order_entry::cockpit::CockpitProbabilityLadderAction::CancelOrder(
                order_client_id,
            ) => {
                cockpit_commands.push(CockpitCommand::Cancel(order_client_id));
            }
        }
    }
    for a in tv.cockpit_ticket_actions.drain(..) {
        match a {
            vike_order_entry::cockpit::CockpitTicketAction::BuyUp => {
                let px = up_px.unwrap_or(0.0);
                let qty = if px > 0.0 { (stake / px).max(0.0) } else { 0.0 };
                cockpit_commands.push(CockpitCommand::Submit {
                    token: symbol.to_string(),
                    side: 1,
                    price: None,
                    quantity: qty,
                });
            }
            vike_order_entry::cockpit::CockpitTicketAction::BuyDown => {
                let px = dn_px.unwrap_or(0.0);
                let qty = if px > 0.0 { (stake / px).max(0.0) } else { 0.0 };
                cockpit_commands.push(CockpitCommand::Submit {
                    token: symbol.to_string(),
                    side: -1,
                    price: None,
                    quantity: qty,
                });
            }
            // Size/arm are pure UI state already applied by the widget to
            // `tv.cockpit_ticket` — nothing to route to the core.
            vike_order_entry::cockpit::CockpitTicketAction::SetSize(_)
            | vike_order_entry::cockpit::CockpitTicketAction::ToggleArm => {}
        }
    }
}

/// Tool phase — every OUT slot a tool left on its `WindowView` this frame, drained into
/// [`WindowFrameOutput`] (or acted on at once where no `&mut App` is needed): the Options order ticket and
/// marker cancel, News links, Data-manager stop / DataSets save-delete-test, the Account cancel,
/// the Stored view's refresh / proxy box / deletes / backfills / opens.
fn drain_tool_view_actions(
    ctx: &egui::Context,
    tv: &mut windows::WindowView,
    zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
    out: &mut WindowFrameOutput,
) {
    let WindowFrameOutput {
        opt_orders,
        opt_cancels,
        to_stop,
        account_cancels,
        symbol_set_save,
        symbol_set_delete,
        symbol_set_open,
        stored_refresh_requested,
        stored_deletes,
        stored_opens,
        stored_backfills,
        ..
    } = out;
    // Options: drain a confirmed order ticket (only set AFTER the user clicks Confirm
    // in the ticket modal — never on the raw chain click) for submit below.
    if let Some(t) = tv.options_submit.take() {
        opt_orders.push(t);
    }
    // Options: drain a chain working-order-marker cancel (deribit order client id) for routing below.
    if let Some(order_client_id) = tv.options_cancel.take() {
        opt_cancels.push(order_client_id);
    }
    if let Some(u) = tv.news.open_url.take() {
        ctx.open_url(egui::OpenUrl::new_tab(u));
    }
    if let Some(k) = tv.data_delete.take() {
        let timestamp =
            vike_ui_theme::time_zone::to_naive(chrono::Utc::now().timestamp_millis(), zone)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_default();
        tv.data_log.push(format!("{timestamp}  Stopped {k}"));
        to_stop.push(k);
    }
    if let Some(order_client_id) = tv.account.cancel.take() {
        account_cancels.push(order_client_id);
    }
    if tv.symbol_set_save {
        tv.symbol_set_save = false;
        let v = &tv;
        *symbol_set_save = Some(datasets::SymbolSet {
            name: v.symbol_set_name.trim().to_string(),
            symbols: datasets::parse_symbols(&v.symbol_set_symbols_text),
            provider: v.symbol_set_provider.clone(),
            interval: v.symbol_set_interval.clone(),
            benchmark: v.symbol_set_benchmark.trim().to_string(),
            user: true, // corrected in the apply (preserve an existing flag)
        });
    }
    if let Some(n) = tv.symbol_set_delete.take() {
        *symbol_set_delete = Some(n);
    }
    if let Some(s) = tv.symbol_set_test.take() {
        *symbol_set_open = Some(s);
    }
    // Data Manager "Stored" view (Task 3): collect this window's OUT actions —
    // applied after the loop below, once `app` is fully borrowable again (same
    // deferred-mutation pattern as `symbol_set_save`/`symbol_set_delete`/`symbol_set_open` above).
    if tv.stored_refresh {
        tv.stored_refresh = false;
        *stored_refresh_requested = true;
    }
    // Polymarket proxy box: seed ONCE from the `venue.polymarket.*` rows the data
    // daemon reads, then leave it alone — re-seeding every frame would overwrite
    // whatever the operator is mid-way through typing. A stored `none`/`direct` shows
    // as an empty box (the bridge's own precedence, `polymarket_proxy_url`), so "no proxy"
    // looks like no proxy.
    if !tv.stored_proxy_loaded {
        tv.stored_proxy_loaded = true;
        tv.stored_proxy.buffer = windows::seed_polymarket_proxy_box(
            crate::SETTINGS_DIR.get().and_then(Option::as_deref),
        );
    }
    if let Some(v) = tv.stored_proxy_save.take() {
        // A free function borrowing no `app`, so unlike the `stored_*` actions
        // around it this needs no deferral to after the window loop.
        windows::save_polymarket_proxy(&v, credential_home().write_context(vike_model::now_ms()));
    }
    if let Some(key) = tv.stored_delete.take() {
        stored_deletes.push(key);
    }
    // Grid v2 multi-select bulk delete: same drain target as the single-row
    // delete above — one `delete_series` call per selected key.
    for k in tv.stored_bulk_delete.drain(..) {
        stored_deletes.push((k.venue, k.symbol, k.kind, k.interval));
    }
    // dm-bulk-backfill: same drain-into-a-Vec shape as bulk delete above, spawned
    // after the loop (see `stored_backfills`' declaration).
    stored_backfills.append(&mut tv.stored_backfill);
    if let Some(open) = tv.stored_open.take() {
        stored_opens.push(open);
    }
}

/// Window-loop tail — the title-bar window controls every window kind shares: close, minimize,
/// and the maximize toggle.
pub(super) fn apply_window_controls(
    w: &mut window::WindowState,
    act: &WindowTitleBarActions,
    desktop: egui::Rect,
) {
    if act.close {
        w.open = false;
    }
    if act.minimize {
        w.minimized = true;
    }
    if act.toggle_max {
        if w.maximized {
            window::unmaximize(w);
        } else {
            window::maximize(w, desktop);
        }
    }
}
