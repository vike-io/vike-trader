//! WHERE a Data-Manager / chart-gap kline backfill runs (split-plane REQ-9, the GUI half):
//! against the LOCAL store (through the fat build's own `vike_backfill::backfill_*_klines` calls
//! until that build was deleted on 2026-09-09 — the desktop's local arm is a tombstone that runs
//! nothing and says so, `crates/vike-desktop/src/app_methods.rs`'s `spawn_stored_backfill_local`),
//! or over the WIRE through the datahub backfill-on-demand verb
//! (`crates/vike-datahub-client/src/client/backfill.rs`'s `backfill`) — "History is fetched by the
//! backend, once, into the store — clients request, never fetch" (split-plane Principle 3).
//!
//! The ONE input is the RESOLVED datahub address — since REQ-2 that is
//! [`crate::data::datahub_resolve::resolve_datahub_addr`] over the explicit `config.datahub_addr` and
//! the active backend's `Welcome` advertisement — the SAME resolution split-plane B12's store
//! branch reads (`crates/vike-desktop/src/main.rs`'s `open_studio_store`): `Some` means the GUI sits
//! on a REMOTE store, so a backfill written anywhere else would be invisible to every reader;
//! `None` means the local store, exactly as before. Keeping this decision a pure function of that
//! one resolved value means the store branch and the backfill branch cannot disagree about which
//! plane owns the data.

/// Where the planned backfill jobs run. Decided once per bulk run by [`backfill_route`], then
/// dispatched in the shell (`vike-desktop`'s `App::maybe_spawn_stored_backfill`; `vike-app`'s
/// `main.rs` when this was written) — wiring only, split-plane Principle 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillRoute {
    /// No datahub resolved (no explicit `config.datahub_addr`, no backend advertisement): the
    /// GUI owned a local store, and the fat build's `vike_backfill::backfill_*_klines` calls wrote
    /// into it directly (split-plane Principle 7: nothing regresses before Phase-5 parity). ⚠ That
    /// build is deleted (2026-09-09), so this route reaches the shell's tombstone, which runs
    /// nothing and tells the operator to point it at a datahub. (It read "today's path,
    /// untouched" until 2026-09-28.)
    Local,
    /// A datahub resolved: the GUI is a CLIENT of the datahub at `addr`. Gap requests go
    /// over the wire verb; the backend fetches from the venue into ITS store (write-through
    /// before serve), and the GUI re-reads the bars from the remote store afterwards.
    Wire {
        /// The datahub server's address, verbatim from the resolved value.
        addr: String,
    },
}

/// The route decision: `datahub_addr` is the RESOLVED datahub address as the composition root
/// resolved it (`None` = no datahub) — `App::resolved_datahub_addr` in `vike-desktop`, through
/// [`crate::data::datahub_resolve::resolve_datahub_addr`]. Deliberately mirrors the `Some`/`None`
/// branch of `open_studio_store` byte-for-byte — same input, same split — so the store the
/// Studio READS and the plane a backfill WRITES can never diverge.
pub fn backfill_route(datahub_addr: Option<&str>) -> BackfillRoute {
    match datahub_addr {
        Some(addr) => BackfillRoute::Wire { addr: addr.to_string() },
        None => BackfillRoute::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_datahub_addr_routes_local() {
        assert_eq!(backfill_route(None), BackfillRoute::Local);
    }

    #[test]
    fn a_datahub_addr_routes_over_the_wire_carrying_the_addr_verbatim() {
        assert_eq!(
            backfill_route(Some("<host>:7878")),
            BackfillRoute::Wire { addr: "<host>:7878".to_string() }
        );
    }
}
