//! The per-mount `StrategyStatus` rows: the boot-time seed, its arming completion, the params line.

use crate::ResolvedMount;

/// Complete the per-mount `StrategyStatus` rows from the mount's ARMING RECORD.
///
/// `WireMountRow::live` is documented as "true iff this MOUNT trades LIVE" — a per-VENUE question.
/// It used to be filled with `flags.tradehub_live`, a per-PROCESS boolean resolved before any venue
/// was mounted and never revised, so it over-claimed in two reachable ways at once:
///
/// - a mount whose venue has NO credentials reported `live: true` while `vike_mount::make_engine`
///   had put it on the paper exchange (`vike_mount::build_node`'s whole credential gate), and
/// - a `data_only = true` mount reported `live: true` while [`vike_mount::startup::withhold_venue_credentials`] had taken
///   its keys away from exec BY DECLARATION — the one combination the profile loader guarantees is
///   reachable, since it REFUSES `data_only` unless the live gate is on.
///
/// `venues` excludes both by construction (no real `ExecutionClient` was built for either), along
/// with a ctrader/ibkr venue whose synchronous connect demoted it and polymarket's recon-only
/// fallback — so one substitution fixes every case.
pub(super) fn wire_mount_rows(
    seeds: Vec<WireMountSeed>,
    venues: &std::collections::HashSet<String>,
) -> Vec<vike_tradehub_client::wire::WireMountRow> {
    seeds
        .into_iter()
        .map(|s| vike_tradehub_client::wire::WireMountRow {
            live: venues.contains(&s.venue),
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
            // ...and the MOUNT ID, for the same reason: the live overlay writes it from the core's own
            // `MountView::mount_id`, the one source of the stored (sanitized) id.
            mount_id: String::new(),
            typed_params: None,
            // ⚠ Filled from the SEED, unlike the three fields above it. Those are left empty
            // because the server's live overlay owns them and a boot-time copy would go stale; a
            // mount's PRODUCT cannot go stale, because a mount that changed product would be a
            // different mount. So this is the one row field the boot block is the right source for.
            asset_class: s.asset_class,
        })
        .collect()
}

/// One mount's wire row, MINUS the fact that does not exist yet.
///
/// `main` must capture the per-mount strings BEFORE the mount (both arms MOVE `resolved`) and can
/// only fill in `live` AFTER it (that answer is `build_node`'s arming record, which the mount
/// produces). The seam is a struct rather than a tuple so neither half can be silently reordered,
/// and `venue` is carried explicitly because it is the KEY the arming record is queried with.
///
/// ⚠ This used to add "`WireMountRow` itself deliberately holds no addressing field", which was
/// true when written and stopped being true on 2026-09-07: that row now carries
/// `venue`/`symbol`/`interval`, and its own doc retires the deferral by name. The BOOT block still
/// leaves them empty, for a different and narrower reason — `wire_mount_rows`'s comment on the
/// empty fields is the authority: the addressing key is the LIVE overlay's, so a boot-time copy
/// would be a second source that goes stale at the first `UpdateParams`.
pub(super) struct WireMountSeed {
    pub(super) strategy: String,
    pub(super) params: String,
    /// The mount's venue id — looked up in `build_node`'s `live_venues` to decide `live`, then
    /// dropped. NOT published FROM HERE: `WireMountRow`'s addressing key is filled by the server's
    /// `Request::StrategyStatus` overlay from the LIVE core, never by this boot-time seed. (It read
    /// "a `WireMountRow` carries no addressing fields yet" until that row grew them.)
    pub(super) venue: String,
    /// WHAT PRODUCT this mount trades, as `vike_model::AssetClass`'s stored word.
    ///
    /// Taken from the profile row, which is where 0061 Phase 5 put it. ⚠ `Option` because
    /// `crate::config::MountCfg`'s field is one: a ROW-backed mount always has a class (the column
    /// is `NOT NULL` and `profile_rows` refuses a row without it), and a TOML-backed mount may not
    /// have been migrated yet. That absence is HONEST and is not the same as a node too old to
    /// carry the field at all — `vike_tradehub_client::proto::FEATURE_MOUNT_CLASS` is what tells
    /// the two apart, and it is advertised beside this.
    ///
    /// ⚠ Unlike the addressing key above, this is a BOOT-TIME fact and is filled HERE rather than
    /// by the server's live overlay. A mount's product does not change while it runs — if it did,
    /// it would be a different mount — so there is no staleness for the overlay to fix.
    pub(super) asset_class: Option<String>,
}

/// One mount's SELF-ADDRESSED params line — the `[[mounts]]` rendering of
/// `DaemonProfile::effective_params`, prefixed with the mount's own venue/symbol/interval so that
/// N rows of `size=2`-style strings are not indistinguishable.
///
/// ⚠ **This prefix is the OLD route to a mount's addressing key, and the reason given for it here
/// is retired.** It used to read "because a `WireMountRow` deliberately carries no structured
/// addressing fields yet"; that row grew `venue`/`symbol`/`interval` on 2026-09-07 and its own doc
/// says string-parsing this prefix "stops being anybody's answer". The prefix stays because this
/// `params` STRING is still what an operator reads and what a pre-`FEATURE_STRATEGY_PARAMS` node
/// can offer — a client that wants to ADDRESS a row reads the structured fields instead.
pub(super) fn mounts_wire_params(m: &ResolvedMount) -> String {
    format!(
        "venue={} symbol={} interval={} :: {}",
        m.spec.venue,
        m.spec.symbol,
        m.spec.interval,
        m.row.effective_params(&m.cfg)
    )
}
