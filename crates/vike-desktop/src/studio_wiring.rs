//! The Studio's composition: the store it opens and the session a Studio window builds once.

use vike_data::HistoryStore;

use std::sync::Arc;

use super::*;

/// Open the STUDIO's store as the TRAIT handle it consumes (split-plane B12), branching on the
/// RESOLVED datahub address (`App::resolved_datahub_address` — the explicit `config.datahub_addr`,
/// else the active backend's `Welcome` advertisement; REQ-2):
///
/// - **`Some(address)`** → dial a `vike_datahub_client::HistoryStoreDatahub` at that address — the
///   CLIENT-side address of a `vike-datahub` server (⚠ deliberately distinct from
///   `config.tradehub_addr`, the tradehub DAEMON's BIND address). The constructor never dials
///   (connect-per-read), so a one-shot `list_series` probe runs here: a down/unreachable server
///   degrades to the same "Studio has no data store: …" view a broken local open takes, instead
///   of a Studio whose every pane errors separately. Returns the dialled address as the second
///   element so the caller can arm the remote-store UI rules.
/// - **`None`** → an `Err` NAMING both fixes: set `config.datahub_addr`, or connect a backend that
///   advertises its datahub. ⚠ TOMBSTONE — this arm used to OPEN A LOCAL STORE
///   (`open_local_hist_store`, a concrete `vike_store::HistoryStoreDataFusion`) under the `fat` feature.
///   The desktop links vike-data with DEFAULT features now — the trait-only `HistoryStore` seam — so
///   there is no engine here to open and the Studio is REMOTE-ONLY by construction rather than by
///   configuration.
///
/// Best-effort and called at most once per device key, like the local opener (see `App::studio`'s
/// doc: a success sticks for the process lifetime, a failure until the device key moves). ⚠ The one-shot is also the REQ-2 timing
/// contract's sharp edge: a Studio opened BEFORE any backend connects resolves from the explicit
/// key alone and keeps that store for the session — connect first, then open Studio, to pick the
/// advertisement up. Every other datahub consumer re-resolves per read.
fn open_studio_store(
    datahub_address: Option<String>,
) -> Result<(Arc<dyn HistoryStore + Send + Sync>, Option<String>), String> {
    match datahub_address {
        Some(address) => {
            use vike_data::HistoryStore as _; // list_series — HistoryStoreDatahub serves it as a trait verb
            let store = open_history_store_datahub(address.clone());
            store
                .list_series()
                .map_err(|e| format!("datahub {address}: {e} (the resolved datahub address — config.datahub_addr, or the backend's advertisement)"))?;
            Ok((Arc::new(store) as Arc<dyn HistoryStore + Send + Sync>, Some(address)))
        }
        None => Err("this build has no local store engine — run `vike-cli config set \
             config.datahub_addr <addr>` to point it at a running vike-datahub server, or connect \
             a backend whose daemon advertises one (`config.datahub_advertise_addr` on the \
             daemon)"
            .to_string()),
    }
}

/// The `WindowKind::Studio` arm of [`tool_content`]: open the Studio's store once, build its
/// session from this root's keys and QA hooks, then draw it (or the reason it has no store).
pub(crate) fn open_studio(
    ui: &mut egui::Ui,
    studio: &mut Option<vike_studio::StudioState>,
    studio_error: &mut Option<(String, u64)>,
    resolved_datahub_address: Option<&str>,
    env: &GuiEnv,
) {
    // Read BEFORE anything below resolves a key, as every datahub dial reads it: a key made after
    // this read moves it again, so the next frame resolves once more.
    let epoch = vike_app_core::backend::registry::device_key_epoch();
    // A store open refused under another device key (none, before the Server group's Connect) is
    // tried again once the key moves, so a Studio opened before the Connect needs no restart.
    if studio_error.as_ref().is_some_and(|(_, at)| *at != epoch) {
        *studio_error = None;
    }
    // Lazy, one-shot store open (see `App::studio`'s doc) — tried once per device key; success or
    // failure both stick, so a broken store never retries `HistoryStoreDataFusion::open` (a real
    // cost: a tokio runtime + WAL recovery) every frame this window stays visible.
    // Unconditional since split-plane I7, and now REMOTE-ONLY: `open_studio_store` has one
    // arm (a resolved datahub — explicit key or backend advertisement) and one Err naming
    // both fixes. The old static "requires the full (fat) build" label went with the gate;
    // the local-store arm went with the `fat` feature itself.
    if studio.is_none() && studio_error.is_none() {
        match open_studio_store(resolved_datahub_address.map(str::to_string)) {
            // The two QA capture hooks come from the binary (`GuiEnv`, read in `main.rs`):
            // `vike-studio` is a library and takes them as parameters (the settings-registry
            // rule), and the autorun hook is parsed HERE, when the window first opens. Same
            // family as this binary's other capture hooks; absent/garbage ⇒ the ordinary
            // workspace-restoring construction (`StudioState::new_with_qa` with no override).
            //
            // The StudioChatPane's AI-provider keys are resolved here for the SAME rule
            // (split-plane I8): the pane used to call the workspace `.env` loader itself,
            // which made a GUI pane the thing that opened `<project>/settings/secrets.env`
            // and put its file on `CREDENTIAL_STORE_PIN`. They come out of the one
            // credential map this root already owns — see [`workspace_credentials`].
            Ok((store, remote_address)) => {
                let qa_tab = &env.studio_tab;
                let qa_autorun =
                    vike_studio::StudioAutorun::from_qa_str(env.studio_autorun.as_deref());
                // state_dir: the project's `settings/state` (the boot walk's, beside the
                // workspace snapshot), where the saved strategies and the copilot's memory live.
                // It was the LOCAL store root, `<project>/market_data/hist`, which a thin client
                // (every session now: the store is remote) does not have, so every Save was lost.
                // No project above the working directory: the store root, as before.
                let state_dir =
                    STATE_DIR.get().cloned().flatten().unwrap_or_else(studio_store_root);
                let mut st = vike_studio::StudioState::new_with_qa(
                    store,
                    state_dir,
                    vike_studio::StudioChatApiKeys::resolve(&workspace_credentials()),
                    qa_tab.as_deref(),
                    qa_autorun,
                );
                // The chat's model ids and request timeout: the `preferences.ai_*` rows, read in
                // `main.rs` — `vike-ai` is a library and keeps no default of its own.
                st.chat.settings = chat_settings();
                // The Import CSV dial (decision 0272): this datahub, and the device key the seed
                // below arms it with.
                st.arm_csv_upload(remote_address.clone());
                // A remote STORE says where the bars come from. It says NOTHING about
                // where compute happens, and this block used to assume it did.
                //
                // ⚠ It read: "the same datahub serves the HistoryStore verbs AND the run
                // verbs, so seed the Backend to it". That has been false since ruling 7
                // moved every `Run*` verb onto `Plane::Backtest` — the datahub refuses
                // them BY PLANE, before the scope check — and the address resolved here
                // is the DATAHUB's. So this line overwrote `StudioRunBackend`'s corrected default
                // with 7878 on every session that opened a remote store, and since
                // `open_studio_store` is remote-only that is every session: the operator
                // saw 7878 in the Backend row under a tooltip reading "backtesthub
                // address", and every ▶ Run came back a wrong-plane error.
                //
                // ⚠ It was SURVIVABLE while `StudioRunBackend::Local` existed — the user clicked
                // Local and it ran in-process. `Local` is deleted
                // (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`),
                // so the workaround is gone and the wrong seed is fatal. That is why a
                // defect older than this branch is fixed ON it.
                //
                // The backtesthub is named by `config.backtest_addr` (ruling 7's
                // client half); absent that, `StudioRunBackend::default()` is already
                // `Source { DEFAULT_BACKTEST_ADDRESS }` and is left alone.
                if remote_address.is_some() {
                    st.store_is_datahub = true;
                }
                if let Some(address) = settings().config.backtest_addr.clone() {
                    st.backend = vike_studio::StudioRunBackend::Source { address };
                }
                // The toolbar names the backtest server by the desktop's active server when the
                // two hosts agree, or both are mouths of the one recorded tunnel ("Server: the CI box").
                let dir = SETTINGS_DIR.get().and_then(Option::as_deref);
                (st.desktop_server, st.desktop_tunnel_ports) =
                    vike_app_core::backend::server_link::studio_server(dir);
                *studio = Some(st);
            }
            Err(e) => *studio_error = Some((e, epoch)),
        }
    }
    // ⚠ **THIS PC'S DEVICE KEY, for every hub dial Studio makes** (decision 0288) — seeded here for
    // the same composition-root reason as everything above: resolving it opens the settings
    // database's `device_key` table, and `vike-studio` is a library that resolves no project root.
    // It is the key the desktop's datahub dials already sign with, seeded at the open and again
    // whenever it moves (`hub_keys_due`), as they re-resolve; the backtesthub's access list decides
    // what it may do (`observe`: the Named run and the strategy-file list; `control`: Run, Sweep,
    // Walk-Forward, Save and Build). No shared key is resolved for Studio: the backtest key the
    // cutover retired is read by nothing, and
    // `crates/vike-ops/tests/gui/studio_backtest_key_reach_gate.rs` holds this line as Studio's one
    // seeding of a key.
    if let Some(st) = studio.as_mut()
        && st.hub_keys_due(epoch)
    {
        st.hub_keys = studio_device_keys();
        // The Import CSV dial copies the key: arm it again, at the same datahub.
        let upload_to = st.datahub_upload.take().map(|dial| dial.address);
        st.arm_csv_upload(upload_to);
        // The toolbar's "connected", asked with the key the server will judge.
        st.check_server();
    }
    match studio {
        Some(s) => s.ui(ui),
        None => {
            let message = studio_error.as_ref().map_or("unknown error", |(why, _)| why.as_str());
            let why = format!("Studio has no data store: {message}");
            vike_ui_theme::components::state::view(
                ui,
                vike_ui_theme::components::state::ThemeEmptyPane::Unreachable(&why),
            );
        }
    }
}

/// **Write the Studio's pending workspace edits now and wait** (`StudioState::flush_workspace`): at
/// the Studio window's close and at the app's exit, after which no Studio frame would write them.
/// A failed write is logged here, since the library links no logger; the edit stays pending.
pub(crate) fn flush_studio_workspace(studio: &mut Option<vike_studio::StudioState>) {
    if let Some(Err(why)) = studio.as_mut().map(vike_studio::StudioState::flush_workspace) {
        tracing::error!("{why}");
    }
}
