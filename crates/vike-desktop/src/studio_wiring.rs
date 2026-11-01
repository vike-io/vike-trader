//! The Studio's composition: the store it opens and the session a Studio window builds once.

use super::*;

/// Open the STUDIO's store as the TRAIT handle it consumes (split-plane B12), branching on the
/// RESOLVED datahub address (`App::resolved_datahub_addr` — the explicit `config.datahub_addr`,
/// else the active backend's `Welcome` advertisement; REQ-2):
///
/// - **`Some(addr)`** → dial a `vike_datahub_client::RemoteHistStore` at that address — the
///   CLIENT-side address of a `vike-datahub` server (⚠ deliberately distinct from
///   `config.tradehub_addr`, the tradehub DAEMON's BIND address). The constructor never dials
///   (connect-per-read), so a one-shot `list_series` probe runs here: a down/unreachable server
///   degrades to the same "Studio has no data store: …" view a broken local open takes, instead
///   of a Studio whose every pane errors separately. Returns the dialled address as the second
///   element so the caller can arm the remote-store UI rules.
/// - **`None`** → an `Err` NAMING both fixes: set `config.datahub_addr`, or connect a backend that
///   advertises its datahub. ⚠ TOMBSTONE — this arm used to OPEN A LOCAL STORE
///   (`open_local_hist_store`, a concrete `vike_store::DataFusionHist`) under the `fat` feature.
///   The desktop links vike-data with DEFAULT features now — the trait-only `HistStore` seam — so
///   there is no engine here to open and the Studio is REMOTE-ONLY by construction rather than by
///   configuration.
///
/// Best-effort and called at most once, like the local opener (see `App::studio`'s doc: success
/// or failure both stick for the process lifetime). ⚠ The one-shot is also the REQ-2 timing
/// contract's sharp edge: a Studio opened BEFORE any backend connects resolves from the explicit
/// key alone and keeps that store for the session — connect first, then open Studio, to pick the
/// advertisement up. Every other datahub consumer re-resolves per read.
fn open_studio_store(
    datahub_addr: Option<String>,
    datahub_key_name: &str,
) -> Result<(vike_data::StoreHandle, Option<String>), String> {
    match datahub_addr {
        Some(addr) => {
            use vike_data::HistStore as _; // list_series — RemoteHistStore serves it as a trait verb
            let store = remote_hist_store(addr.clone(), datahub_key_name);
            store
                .list_series()
                .map_err(|e| format!("datahub {addr}: {e} (the resolved datahub address — config.datahub_addr, or the backend's advertisement — unset/disconnect it to use the local store)"))?;
            Ok((Arc::new(store) as vike_data::StoreHandle, Some(addr)))
        }
        None => Err("this build has no local store engine — run `vike-cli config set \
             config.datahub_addr <addr>` to point it at a running vike-datahub server, or connect \
             a backend whose daemon advertises one (`config.datahub_advertise_addr` on the \
             daemon)"
            .to_string()),
    }
}

/// The `WinKind::Studio` arm of [`tool_content`]: open the Studio's store once, build its
/// session from this root's keys and QA hooks, then draw it (or the reason it has no store).
pub(crate) fn open_studio(
    ui: &mut egui::Ui,
    studio: &mut Option<vike_studio::StudioState>,
    studio_error: &mut Option<String>,
    resolved_datahub_addr: Option<&str>,
    datahub_key_name: &str,
    env: &GuiEnv,
) {
    // Lazy, one-shot store open (see `App::studio`'s doc) — tried once; success or
    // failure both stick, so a broken store never retries `DataFusionHist::open` (a real
    // cost: a tokio runtime + WAL recovery) every frame this window stays visible.
    // Unconditional since split-plane I7, and now REMOTE-ONLY: `open_studio_store` has one
    // arm (a resolved datahub — explicit key or backend advertisement) and one Err naming
    // both fixes. The old static "requires the full (fat) build" label went with the gate;
    // the local-store arm went with the `fat` feature itself.
    if studio.is_none() && studio_error.is_none() {
        match open_studio_store(resolved_datahub_addr.map(str::to_string), datahub_key_name) {
            // The two QA capture hooks come from the binary (`GuiEnv`, read in `main.rs`):
            // `vike-studio` is a library and takes them as parameters (the settings-registry
            // rule), and the autorun hook is parsed HERE, when the window first opens. Same
            // family as this binary's other capture hooks; absent/garbage ⇒ the ordinary
            // workspace-restoring construction, byte-identical to `StudioState::new`.
            //
            // The ChatPane's AI-provider keys are resolved here for the SAME rule
            // (split-plane I8): the pane used to call the workspace `.env` loader itself,
            // which made a GUI pane the thing that opened `<project>/settings/secrets.env`
            // and put its file on `CREDENTIAL_STORE_PIN`. They come out of the one
            // credential map this root already owns — see [`workspace_credentials`].
            Ok((store, remote_addr)) => {
                let qa_tab = &env.studio_tab;
                let qa_autorun = vike_studio::QaAutorun::from_qa_str(env.studio_autorun.as_deref());
                // state_dir: the resolved LOCAL store root, remote or not — for a local
                // store this is the exact old `store.root()` colocation; a remote session
                // keeps its saved strategies at the same stable local path (the trait has
                // no root() — a filesystem concept an RPC store does not have).
                let mut st = vike_studio::StudioState::new_with_qa(
                    store,
                    studio_store_root(),
                    vike_studio::ChatApiKeys::resolve(&workspace_credentials()),
                    qa_tab.as_deref(),
                    qa_autorun,
                );
                // The Research pane's HOST — where `user_data` is and who is running.
                // Seeded here, after construction, for the same reason `store_is_remote`
                // and `backend` are: `vike-studio` is a library and resolves no project
                // root of its own.
                st.research.host = studio_study_host();
                // The chat's model ids and request timeout: the `preferences.ai_*` rows, read in
                // `main.rs` — `vike-ai` is a library and keeps no default of its own.
                st.chat.settings = chat_settings();
                // ⚠ **THE NAMED-RUN KEY** — seeded here for the same composition-root
                // reason as everything above it: resolving it opens the settings
                // database's `hub_key` table, and `vike-studio` is a library that
                // resolves no project root. It is the OBSERVE key this desktop already
                // holds for its store and market-data planes, and never a Control one —
                // `vike_app_core::backend::backend_registry`'s `datahub_observe_key` states why no
                // datahub dial of this process may hold the key that compiles
                // client-supplied Rhai, and
                // `docs/decisions/0064-a-named-run-carries-no-source.md` is the verb that
                // makes the weaker key enough to run a backtest on the backend at all.
                // (The ONE Control key this process may hold is `backtest_key` below, and
                // it is a different type from this one.)
                //
                // ⚠ The same key authenticates against the backtesthub unchanged:
                // `vike_backtesthub::backtesthub_server`'s `serve_authed` verifies under the SAME
                // `DATAHUB_DOMAIN` and the same observe/control pair, which its own ⚠
                // argues for. So the client-side gap 0064 names is the ADDRESS, not the
                // key — and the address is an editable field on the Backend row, defaulting
                // to `vike_config::DEFAULT_BACKTEST_ADDR`.
                st.named_run_keys = named_run_observe_keys(datahub_key_name);
                // The BUILDER's own key — a THIRD service, a THIRD domain separator, and
                // a WRITE scope, none of which the observe key above can stand in for
                // (`vike_strategy_builder::builder`'s Decisions 1 and 2 argue both: a key
                // minted to observe a data plane must not also authenticate a request to
                // run arbitrary `cargo` on that box). Seeded here for the same reason
                // every other key in this file is: only a BINARY opens a store, and the
                // credential map this root already owns is [`workspace_credentials`].
                //
                // ⚠ **The PC's own settings database is the ONLY place read, and it is
                // read for this one NAME** (the owner's ruling of 2026-10-31, "all in the
                // SQLite, not files"; `docs/decisions/0083-the-runtime-plugin-join-lands.md`,
                // question 2). It is a DEPLOYMENT row of the credential table, not a hub key:
                // `named_run_observe_keys` above falls through to the `hub_key` table, and
                // decision 0051's namespace rule keeps a builder-namespaced name out of that
                // ladder, so the key is the ordinary credential-map lookup the chat pane's
                // provider keys use. No variable and no key file is read.
                // `crates/vike-studio/src/backend/plugin_build.rs` names the row and the one
                // command that sets it (`vike-cli secrets set`) when it is absent, from that
                // service's own constant, never a second spelling.
                st.builder_keys =
                    vike_strategy_builder::builder::keys_from_credentials(&workspace_credentials());
                // ⚠ **STUDIO'S COMPUTE KEY — the datahub CONTROL key, resolved here and
                // handed to Studio's compute dial and NOTHING ELSE.** The owner ruled on
                // 2026-09-26 (`docs/decisions/0083-the-runtime-plugin-join-lands.md`,
                // question 1, option (a)) that the desktop resolves it for that one dial,
                // knowing what the key also authorises at `VerbScope::Write`: compiling
                // Rhai, running any artifact in the plugin directory, `Backfill` and
                // `DeleteSeries` — a store-mutation authority this process never held
                // before. So its reach is kept as narrow as the code allows:
                //
                // * it is read from its OWN row (`vike_studio::BACKTEST_KEY_NAME`) of this
                //   PC's settings database, through the credential map this root already
                //   loads — the owner's ruling of 2026-10-31, "all in the SQLite, not
                //   files": no variable and no key file is read, and `just studio` files
                //   the row with the value on a pipe. It is a DEPLOYMENT row, never the
                //   platform `VIKE_DATAHUB_CONTROL_KEY` of the `hub_key` table, so no
                //   datahub dial of this process and no `vike-cli datahub` verb on this PC
                //   asks for it by name;
                // * it arrives as a `vike_studio::BacktestKey`, which no datahub dial here
                //   accepts — the store, the chart seed, the catalog, the named run and the
                //   trading-node observer all take a `HubKeys`, and this is not one;
                // * Studio signs it in one place, toward a server whose pre-auth Welcome is
                //   the backtesthub's, and refuses the datahub before sending a byte.
                //
                // `crates/vike-ops/tests/gui/studio_backtest_key_reach_gate.rs` holds this line
                // as the resolver's ONE production call. `None` — no key handed over — is
                // the ordinary state, and Studio's Run then says where the key comes from.
                st.backtest_key =
                    vike_studio::backtest_key_from_credentials(&workspace_credentials());
                // A remote STORE says where the bars come from. It says NOTHING about
                // where compute happens, and this block used to assume it did.
                //
                // ⚠ It read: "the same datahub serves the HistStore verbs AND the run
                // verbs, so seed the Backend to it". That has been false since ruling 7
                // moved every `Run*` verb onto `Plane::Backtest` — the datahub refuses
                // them BY PLANE, before the scope check — and the address resolved here
                // is the DATAHUB's. So this line overwrote `Backend`'s corrected default
                // with 7878 on every session that opened a remote store, and since
                // `open_studio_store` is remote-only that is every session: the operator
                // saw 7878 in the Backend row under a tooltip reading "backtesthub
                // address", and every ▶ Run came back a wrong-plane error.
                //
                // ⚠ It was SURVIVABLE while `Backend::Local` existed — the user clicked
                // Local and it ran in-process. `Local` is deleted
                // (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`),
                // so the workaround is gone and the wrong seed is fatal. That is why a
                // defect older than this branch is fixed ON it.
                //
                // The backtesthub is named by `config.backtest_addr` (ruling 7's
                // client half); absent that, `Backend::default()` is already
                // `Remote { DEFAULT_BACKTESTHUB_ADDR }` and is left alone.
                if remote_addr.is_some() {
                    st.store_is_remote = true;
                }
                if let Some(addr) = settings().config.backtest_addr.clone() {
                    st.backend = vike_studio::Backend::Remote { addr };
                }
                *studio = Some(st);
            }
            Err(e) => *studio_error = Some(e),
        }
    }
    match studio {
        Some(s) => s.ui(ui),
        None => {
            let msg = studio_error.as_deref().unwrap_or("unknown error");
            let why = format!("Studio has no data store: {msg}");
            vike_ui_theme::components::state::view(
                ui,
                vike_ui_theme::components::state::Load::Unreachable(&why),
            );
        }
    }
}
