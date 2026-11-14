//! The per-mount `StrategyStatus` rows: the boot-time seed, its arming completion, the params line.

use vike_model::accounts::account_keys::{AccountLabel, route_key_of};

use crate::MountResolved;

/// Complete the per-mount `StrategyStatus` rows from the mount's ARMING RECORD.
///
/// `MountViewWire::live` is documented as "true iff this MOUNT trades LIVE" — a question about the
/// ACCOUNT the mount trades: `venues` holds ROUTE KEYS (the bare venue id for the default account,
/// `venue#LABEL` for a labelled one), so the lookup key is the mount's account route key
/// ([`MountViewWireSeed::account`]), never the bare venue (decision 0172: a mount of `bybit#DEMO` that
/// trades the demo venue read `live: false` and a client drew it as PAPER).
/// It used to be filled with `flags.tradehub_live`, a per-PROCESS boolean resolved before any venue
/// was mounted and never revised, so it over-claimed in two reachable ways at once:
///
/// - a mount whose venue has NO credentials reported `live: true` while `vike_tradehub_core::make_engine`
///   had put it on the paper exchange (`vike_tradehub_core::assemble_runtime`'s whole credential gate), and
/// - a `data_only = true` mount reported `live: true` while [`crate::venue_arming::withhold_data_only_credentials`] had taken
///   its keys away from exec BY DECLARATION — the one combination the profile loader guarantees is
///   reachable, since it REFUSES `data_only` unless the live gate is on.
///
/// `venues` excludes both by construction (no real `ExecutionClient` was built for either), along
/// with a ctrader/ibkr venue whose synchronous connect demoted it and polymarket's recon-only
/// fallback — so one substitution fixes every case.
///
/// ⚠ This is the answer AT BOOT. An account can change tier while the node runs, so the
/// `StrategyStatus` reply writes `live` again, with `tier`, off the newest block of `route_key`
/// (`crates/vike-tradehub/src/server/session.rs`'s `strategy_status_response`), and a mount can
/// come and go, so the reply matches these rows to the core's by `mount_id`
/// (`crates/vike-tradehub/src/publish.rs`'s `mount_rows_now`).
pub(super) fn wire_mount_rows(
    seeds: Vec<MountViewWireSeed>,
    venues: &std::collections::HashSet<String>,
) -> Vec<vike_tradehub_client::wire::MountViewWire> {
    seeds
        .into_iter()
        .map(|s| {
            let route_key = route_key_of(&s.venue, &s.account);
            vike_tradehub_client::wire::MountViewWire {
                // The four characters a client decodes this mount's ids by (decision 0270), absent for `0000`.
                order_client_id_code: s.order_client_id_code.map(|c| c.as_str().to_string()),
                // The boot's answer: the server's overlay writes it again from the account's tier once a block
                // says it, because a tier change moves the mount to a new engine.
                live: venues.contains(&route_key),
                strategy: s.strategy,
                params: s.params,
                // The addressing key and the typed params are the LIVE overlay's, not the boot block's:
                // `server.rs`'s `Request::StrategyStatus` arm writes them from the core's own published
                // mount rows, because a boot-time copy of either would be stale the moment the first
                // `UpdateParams` lands — the defect the read exists to fix. Left empty here so there is
                // exactly one source rather than two that can disagree.
                venue: String::new(),
                symbol: String::new(),
                interval: String::new(),
                // The id the reply matches this row to the core's own `MountView::mount_id` by, so a runtime
                // mount or unmount cannot shift one mount's text onto another.
                mount_id: s.mount_id,
                typed_params: None,
                // ⚠ Filled from the SEED, unlike the three fields above it. Those are left empty
                // because the server's live overlay owns them and a boot-time copy would go stale; a
                // mount's PRODUCT cannot go stale, because a mount that changed product would be a
                // different mount. So the boot block is the right source for it, and for the ACCOUNT below.
                asset_class: s.asset_class,
                // The core's own state, written by the server's live overlay (decision 0239), never a boot fact.
                latched: false,
                // The core's published mount row names no account, so the seed is the only source.
                route_key,
                // The account's state NOW, written by the server's overlay from `route_key`.
                tier: None,
                not_running: false,
            }
        })
        .collect()
}

/// One mount's wire row, MINUS the fact that does not exist yet.
///
/// `main` must capture the per-mount strings BEFORE the mount (both arms MOVE `resolved`) and can
/// only fill in `live` AFTER it (that answer is `assemble_runtime`'s arming record, which the mount
/// produces). The seam is a struct rather than a tuple so neither half can be silently reordered,
/// and `venue` is carried explicitly because it is the KEY the arming record is queried with.
///
/// ⚠ This used to add "`MountViewWire` itself deliberately holds no addressing field", which was
/// true when written and stopped being true on 2026-09-07: that row now carries
/// `venue`/`symbol`/`interval`, and its own doc retires the deferral by name. The BOOT block still
/// leaves them empty, for a different and narrower reason — `wire_mount_rows`'s comment on the
/// empty fields is the authority: the addressing key is the LIVE overlay's, so a boot-time copy
/// would be a second source that goes stale at the first `UpdateParams`.
pub(super) struct MountViewWireSeed {
    pub(super) strategy: String,
    pub(super) params: String,
    /// The mount's venue id: half of the route key the row carries and `live` is decided by. NOT
    /// published FROM HERE as `venue`: `MountViewWire`'s addressing key is filled by the server's
    /// `Request::StrategyStatus` overlay from the LIVE core, never by this boot-time seed. (It read
    /// "a `MountViewWire` carries no addressing fields yet" until that row grew them.)
    pub(super) venue: String,
    /// The account of [`Self::venue`] this mount trades (`MountSpec::account`; the default account
    /// for a mount naming none): with the venue, the route key looked up in `live_venues`.
    pub(super) account: AccountLabel,
    /// WHAT PRODUCT this mount trades, as `vike_model::AssetClass`'s stored word.
    ///
    /// Taken from the profile row, which is where 0061 Phase 5 put it. ⚠ `Option` because
    /// `crate::config::MountConfig`'s field is one: a ROW-backed mount always has a class (the column
    /// is `NOT NULL` and `profile_rows` refuses a row without it), and a TOML-backed mount may not
    /// have been migrated yet. That absence is HONEST and is not the same as a node too old to
    /// carry the field at all — `vike_tradehub_client::proto::FEATURE_MOUNT_CLASS` is what tells
    /// the two apart, and it is advertised beside this.
    ///
    /// ⚠ Unlike the addressing key above, this is a BOOT-TIME fact and is filled HERE rather than
    /// by the server's live overlay. A mount's product does not change while it runs — if it did,
    /// it would be a different mount — so there is no staleness for the overlay to fix.
    pub(super) asset_class: Option<String>,
    /// The id the core gives this mount (`vike_core::strategy_state::mount_id_with`).
    pub(super) mount_id: String,
    /// The mount's strategy code (decision 0270: `vike_tradehub_core::MountPlacement::order_client_id_code`), a boot fact like the
    /// product: `None` mints `0000`.
    pub(super) order_client_id_code:
        Option<vike_model::orders::client_id::OrderClientIdStrategyCode>,
}

/// One mount's SELF-ADDRESSED params line — the `[[mounts]]` rendering of
/// `MountResolvedStrategy::effective_params`, prefixed with the mount's own venue/symbol/interval so that
/// N rows of `size=2`-style strings are not indistinguishable.
///
/// ⚠ **This prefix is the OLD route to a mount's addressing key, and the reason given for it here
/// is retired.** It used to read "because a `MountViewWire` deliberately carries no structured
/// addressing fields yet"; that row grew `venue`/`symbol`/`interval` on 2026-09-07 and its own doc
/// says string-parsing this prefix "stops being anybody's answer". The prefix stays because this
/// `params` STRING is still what an operator reads and what a pre-`FEATURE_STRATEGY_PARAMS` node
/// can offer — a client that wants to ADDRESS a row reads the structured fields instead.
pub(super) fn mounts_wire_params(m: &MountResolved) -> String {
    mount_row_params(&m.spec, &m.strategy_params)
}

/// [`mounts_wire_params`] over a mount's spec and a params line: what a `.rs` mount's row says once its
/// build answers (`super::awaited_builds`).
pub(super) fn mount_row_params(
    spec: &vike_tradehub_core::MountPlacement,
    strategy_params: &str,
) -> String {
    format!(
        "venue={} symbol={} interval={} :: {strategy_params}",
        spec.venue, spec.symbol, spec.interval
    )
}
