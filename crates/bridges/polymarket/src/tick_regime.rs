//! The **dynamic tick-size regime** for Polymarket CLOB quoting (CLOB hygiene).
//!
//! Polymarket does not run one fixed price grid per token: the CLOB REDUCES the minimum tick near
//! the price extremes (the ordinary `0.01` cent grid becomes `0.001` once a token trades above
//! `1 - 0.01` or below `0.01`, and can tighten again further out). A maker that resolved the tick
//! once at subscribe time and kept quoting a longshot on that stale grid therefore starts getting
//! its orders REJECTED with an invalid-tick-size error the moment the market crosses the regime
//! boundary — and, because the venue only ever refuses the order, nothing in the adapter would
//! otherwise learn that the grid moved.
//!
//! [`TickRegime`] is the fix, and it is **opt-in by handle**: a caller constructs one and threads it
//! in. The LIVE mount does exactly that — [`crate::live_mount_from_vars`] builds ONE, hands a clone
//! to the exec thread on [`PolymarketLiveConfig::tick_regime`](crate::PolymarketLiveConfig) and
//! returns the other on [`PolymarketMount::tick_regime`](crate::PolymarketMount), so the exec thread
//! and a quoting path read the same cache. Every other entry point
//! ([`PolymarketExecutionClient::spawn`](crate::PolymarketExecutionClient::spawn) and
//! `spawn_tracked`) passes `None`, which is byte-identical to before this module was wired: no
//! transport is built, no price is rounded, no reject is observed. The market feed and
//! `instruments::fetch_token_tick_size` are still untouched by this module.
//!
//! The contract:
//! - [`is_tick_size_reject`] is the PURE decision: does this venue reject reason mean "your price
//!   was off the current grid" (⇒ the cached tick is stale) rather than any other refusal?
//! - [`TickRegime::on_reject`] re-fetches the tick (the direct `/tick-size` point lookup, with the
//!   `/markets` walk fallback — `instruments::fetch_tick_size_direct` then
//!   `instruments::fetch_token_tick_size_paged`) for exactly those rejects and caches the new
//!   value; every other reject is a no-op, so an ordinary balance/allowance refusal never triggers
//!   REST traffic. A refresh that resolves NOTHING (both lookups failed — a transient blip through
//!   the Dublin proxy) leaves the cached grid untouched: it must never overwrite a known-good tick
//!   with the venue-default `0.01`, which would silently break rounding until the next success.
//! - [`TickRegime::properties`] exposes the cached grid as the `SymbolProperties` the rounding path
//!   consumes (tick-only, other fields `0.0` — Polymarket binary markets have no lot/notional grid,
//!   the same shape [`crate::filters_rec::record_token_tick`] records), and
//!   [`TickRegime::round_price`] is that rounding applied (`vike_model::round_to`, the workspace's
//!   one rounding convention). An UNKNOWN token rounds to itself — never guessed onto a default
//!   grid, which would silently move a price the caller chose.
//!
//! Cloning shares the state (`Arc<Mutex<…>>`, the `registry::PolymarketRegistry` shape) so
//! the exec thread and a quoting strategy can hold the same regime.
//!
//! **The accepted residual** (documented, not fixed — the honest limit of a learn-from-rejects
//! cache). A reject is the ONLY thing that refreshes an entry, and the venue only refuses a price
//! that is too FINE for the grid it enforces. So a cache holding a tick COARSER than the venue's
//! current one never self-heals: every coarse price is accepted, no reject is ever raised, and a
//! caller's finer price is snapped onto that coarse grid and rests a fraction of a cent from where it
//! was aimed. That is the direction the venue moves when a token walks BACK toward the middle, and it
//! degrades a price rather than looping on rejects — the strictly worse failure this module exists to
//! break is the other direction, where every order is refused indefinitely. Closing the residual
//! needs a periodic re-resolve, or the market feed seeding [`TickRegime::set`] from each `book`
//! frame's embedded `tick_size` (`ws.rs` already notes that frame carries it); both are follow-ups,
//! deliberately not this wiring.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_bridge_core::transport::RestTransport;
use vike_model::SymbolProperties;

use super::instruments::{fetch_tick_size_direct, fetch_token_tick_size_paged};

/// Substrings (matched case-insensitively) that mark a venue reject as an OFF-GRID price refusal.
/// Deliberately broad: every Polymarket refusal that mentions the tick size means the price we sent
/// was not on the grid the venue is currently enforcing, whatever the exact wording
/// (`"invalid tick size"`, `"tick size must be ..."`, an `INVALID_TICK_SIZE` code echo), and the
/// only cost of a false positive is ONE extra `/tick-size` GET.
const TICK_SIZE_REJECT_MARKERS: [&str; 3] = ["tick size", "tick_size", "tick-size"];

/// Does this venue reject `reason` mean the price was off the CURRENT tick grid (⇒ re-fetch the
/// tick size)? Pure — the decision half of [`TickRegime::on_reject`], unit-tested on its own.
pub fn is_tick_size_reject(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    TICK_SIZE_REJECT_MARKERS.iter().any(|m| lower.contains(m))
}

/// A shared, opt-in per-`token_id` tick-size cache with re-fetch-on-off-grid-reject. See the module
/// doc for the contract; `Clone` shares one state.
#[derive(Clone, Default)]
pub struct TickRegime(Arc<Mutex<HashMap<String, f64>>>);

impl TickRegime {
    /// An empty regime — nothing cached; every token is UNKNOWN until fetched or seeded.
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached tick size for `token_id`, or `None` when this token has never been resolved.
    pub fn tick_size(&self, token_id: &str) -> Option<f64> {
        self.0.lock().unwrap().get(token_id).copied()
    }

    /// Seed/overwrite a token's tick size from a value resolved elsewhere (the market feed's
    /// subscribe-time lookup, a `book` frame's embedded `tick_size`). A non-finite or non-positive
    /// value is IGNORED — a zero tick would silently disable rounding downstream.
    pub fn set(&self, token_id: &str, tick_size: f64) {
        if tick_size.is_finite() && tick_size > 0.0 {
            self.0.lock().unwrap().insert(token_id.to_string(), tick_size);
        }
    }

    /// The cached grid as the `SymbolProperties` the rounding path consumes: tick-only, every other
    /// field `0.0` (Polymarket binary markets have no lot/min-notional grid — the same shape
    /// [`crate::filters_rec::record_token_tick`] records). `None` for an unknown token, so a caller
    /// can tell "no grid known" from "a grid of 0.0".
    pub fn properties(&self, token_id: &str) -> Option<SymbolProperties> {
        self.tick_size(token_id)
            .map(|tick_size| SymbolProperties { tick_size, ..Default::default() })
    }

    /// Round `price` onto this token's cached grid (`vike_model::round_to` — the workspace's one
    /// rounding convention, half-even). An UNKNOWN token returns `price` UNCHANGED: guessing a
    /// default grid here would silently move a price the caller deliberately chose.
    pub fn round_price(&self, token_id: &str, price: f64) -> f64 {
        match self.tick_size(token_id) {
            Some(tick) => vike_model::round_to(price, vike_model::nz_step(tick)),
            None => price,
        }
    }

    /// Force a re-fetch of `token_id`'s tick size and cache it ONLY when the venue actually
    /// answered. `Some(tick)` = resolved (direct `/tick-size` first, the `/markets` page walk as
    /// the fallback) and cached; `None` = NEITHER lookup resolved it, and the previously cached
    /// value — if any — is left **untouched**.
    ///
    /// This deliberately does NOT go through `instruments::fetch_token_tick_size`, which never
    /// fails: it falls back to `DEFAULT_TICK_SIZE` (`0.01`) on a REST failure or an unknown token.
    /// Caching
    /// that default would let ONE transient network blip (very plausible through the Dublin proxy
    /// this venue is reached over) silently degrade a known-good `0.001` grid to `0.01` — turning a
    /// recoverable hiccup into persistently wrong rounding. A refresh that cannot resolve anything
    /// must therefore leave the cache alone, not overwrite it with a guess.
    pub fn refresh<T: RestTransport>(&self, t: &T, token_id: &str) -> Option<f64> {
        let tick = fetch_tick_size_direct(t, token_id)
            .or_else(|| fetch_token_tick_size_paged(t, token_id))?;
        if !tick.is_finite() || tick <= 0.0 {
            return None; // a degenerate answer is no answer — never cache it (see `set`)
        }
        self.set(token_id, tick);
        Some(tick)
    }

    /// The re-fetch-on-reject hook: called with a venue reject `reason`, re-fetch + cache the tick
    /// size when (and ONLY when) that reason is an off-grid refusal ([`is_tick_size_reject`]), and
    /// return the freshly resolved tick. Any other reject is a no-op returning `None` — an ordinary
    /// balance/allowance refusal must never cost a REST round-trip.
    ///
    /// `None` also covers the second case: the reject WAS off-grid but neither lookup resolved a
    /// tick (see [`TickRegime::refresh`]) — the cached grid is then preserved, not defaulted.
    ///
    /// Instrumented at this per-order boundary only (never per message — the hot-fold rule).
    pub fn on_reject<T: RestTransport>(&self, t: &T, token_id: &str, reason: &str) -> Option<f64> {
        if !is_tick_size_reject(reason) {
            return None;
        }
        let before = self.tick_size(token_id);
        match self.refresh(t, token_id) {
            Some(tick) => {
                tracing::warn!(
                    token_id,
                    reason,
                    previous_tick = ?before,
                    tick_size = tick,
                    "polymarket rejected an off-grid price — tick-size regime re-fetched"
                );
                Some(tick)
            }
            None => {
                tracing::warn!(
                    token_id,
                    reason,
                    cached_tick = ?before,
                    "polymarket rejected an off-grid price but the tick-size re-fetch failed — \
                     keeping the cached grid (never defaulting over a known-good one)"
                );
                None
            }
        }
    }
}

/// The REST test doubles this module's tests AND [`crate::client`]'s wiring tests both drive, so the
/// two cannot drift into asserting different transport shapes. `#[cfg(test)]` ⇒ a real build never
/// compiles them (the "one owned double" convention, crate-local because these are two ten-line
/// stubs, not a `test-support` feature's worth of surface).
#[cfg(test)]
pub(crate) mod stubs {
    use vike_bridge_core::transport::{RestTransport, VenueApiError};

    /// A `/tick-size`-only transport: answers the direct point lookup with `tick` and counts how
    /// many REST calls the regime made (so a no-op reject can be proven to cost none).
    pub(crate) struct TickStub {
        tick: f64,
        calls: std::cell::RefCell<usize>,
    }

    impl TickStub {
        pub(crate) fn new(tick: f64) -> Self {
            TickStub { tick, calls: std::cell::RefCell::new(0) }
        }
        pub(crate) fn calls(&self) -> usize {
            *self.calls.borrow()
        }
    }

    impl RestTransport for TickStub {
        fn signed(
            &self,
            _base: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, String)],
            _signer: &dyn vike_bridge_core::signer::Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            panic!("the tick regime never signs a request")
        }
        fn public(
            &self,
            _base: &str,
            path: &str,
            _params: &[(&str, String)],
        ) -> Result<serde_json::Value, VenueApiError> {
            *self.calls.borrow_mut() += 1;
            if path == "/tick-size" {
                Ok(serde_json::json!({ "minimum_tick_size": self.tick.to_string() }))
            } else {
                Err(VenueApiError { code: 404, msg: "only /tick-size is canned".into() })
            }
        }
    }

    /// A transport whose every read fails — the transient-blip shape (the Dublin proxy dropping a
    /// request), which must NEVER be allowed to degrade a known-good grid to the venue default.
    #[derive(Default)]
    pub(crate) struct DeadStub {
        calls: std::cell::RefCell<usize>,
    }

    impl DeadStub {
        pub(crate) fn calls(&self) -> usize {
            *self.calls.borrow()
        }
    }

    impl RestTransport for DeadStub {
        fn signed(
            &self,
            _base: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, String)],
            _signer: &dyn vike_bridge_core::signer::Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            panic!("the tick regime never signs a request")
        }
        fn public(
            &self,
            _base: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<serde_json::Value, VenueApiError> {
            *self.calls.borrow_mut() += 1;
            Err(VenueApiError { code: 503, msg: "connection reset".into() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stubs::{DeadStub, TickStub};
    use super::*;

    #[test]
    fn the_reject_decision_matches_only_off_grid_refusals() {
        assert!(is_tick_size_reject("invalid tick size"));
        assert!(is_tick_size_reject("INVALID_TICK_SIZE"));
        assert!(is_tick_size_reject("price 0.965 is not on the tick-size grid"));
        // every other refusal must stay a no-op — no REST traffic on a balance/allowance reject
        assert!(!is_tick_size_reject("not enough balance / allowance"));
        assert!(!is_tick_size_reject("order rejected"));
        assert!(!is_tick_size_reject(""));
    }

    /// Rounded prices are compared with a tolerance, NOT `==`: `n * tick` is a floating-point
    /// product whose last bit depends on the multiplier, and nothing here is a parity fixture — the
    /// property under test is "which grid was used", not a bit pattern.
    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn an_off_grid_reject_refetches_and_updates_the_rounding_grid() {
        let t = TickStub::new(0.001);
        let regime = TickRegime::new();
        regime.set("111", 0.01); // the stale cent grid the maker was quoting on
        let stale = regime.round_price("111", 0.9636);
        assert!(approx(stale, 0.96), "the stale cent grid rounds 0.9636 to 0.96, got {stale}");

        assert_eq!(regime.on_reject(&t, "111", "invalid tick size"), Some(0.001));
        assert_eq!(t.calls(), 1, "exactly one direct /tick-size lookup");
        assert_eq!(regime.tick_size("111"), Some(0.001));
        assert_eq!(
            regime.properties("111"),
            Some(SymbolProperties { tick_size: 0.001, ..Default::default() }),
            "the SymbolProperties the rounding path consumes carries the NEW grid"
        );
        let fresh = regime.round_price("111", 0.9636);
        assert!(approx(fresh, 0.964), "the tightened grid keeps 0.964, got {fresh}");
    }

    #[test]
    fn an_unrelated_reject_is_a_no_op_and_costs_no_rest_call() {
        let t = TickStub::new(0.001);
        let regime = TickRegime::new();
        regime.set("111", 0.01);
        assert_eq!(regime.on_reject(&t, "111", "not enough balance / allowance"), None);
        assert_eq!(t.calls(), 0, "a non-tick reject must never hit REST");
        assert_eq!(regime.tick_size("111"), Some(0.01), "the cached grid is untouched");
    }

    #[test]
    fn an_unknown_token_rounds_to_itself_and_has_no_properties() {
        let regime = TickRegime::new();
        assert_eq!(regime.tick_size("nope"), None);
        assert_eq!(regime.properties("nope"), None);
        // an untouched price is returned by value, so `==` IS exact here (no arithmetic ran)
        assert_eq!(regime.round_price("nope", 0.96351), 0.96351, "never guessed onto a grid");
    }

    #[test]
    fn a_degenerate_tick_is_ignored_rather_than_disabling_rounding() {
        let regime = TickRegime::new();
        regime.set("111", 0.01);
        regime.set("111", 0.0);
        regime.set("111", -1.0);
        regime.set("111", f64::NAN);
        assert_eq!(regime.tick_size("111"), Some(0.01), "a bad value never overwrites a good grid");
    }

    #[test]
    fn clone_shares_state() {
        let a = TickRegime::new();
        let b = a.clone();
        a.set("111", 0.001);
        assert_eq!(b.tick_size("111"), Some(0.001));
    }

    #[test]
    fn refresh_caches_the_fetched_tick() {
        let t = TickStub::new(0.0001);
        let regime = TickRegime::new();
        assert_eq!(regime.refresh(&t, "222"), Some(0.0001));
        assert_eq!(regime.tick_size("222"), Some(0.0001));
    }

    /// The regression this guards: `fetch_token_tick_size` ALWAYS answers (0.01 on any failure), so
    /// caching its result would have silently rewritten a correct 0.001 grid to 0.01 on one blip.
    #[test]
    fn a_failed_refresh_preserves_the_previously_cached_tick() {
        let t = DeadStub::default();
        let regime = TickRegime::new();
        regime.set("111", 0.001); // the known-good tightened grid

        assert_eq!(regime.refresh(&t, "111"), None, "nothing resolved ⇒ nothing to report");
        assert!(t.calls() >= 2, "the direct lookup AND the paged fallback were both tried");
        assert_eq!(
            regime.tick_size("111"),
            Some(0.001),
            "the cached grid survives a failed re-fetch — never clobbered by DEFAULT_TICK_SIZE"
        );
        // …and the same holds through the reject hook
        assert_eq!(regime.on_reject(&t, "111", "invalid tick size"), None);
        assert_eq!(regime.tick_size("111"), Some(0.001));
    }

    /// An UNKNOWN token whose lookups fail stays unknown — it must not be born on the default grid.
    #[test]
    fn a_failed_refresh_of_an_unknown_token_caches_nothing() {
        let t = DeadStub::default();
        let regime = TickRegime::new();
        assert_eq!(regime.refresh(&t, "999"), None);
        assert_eq!(regime.tick_size("999"), None, "no grid known is not the same as a 0.01 grid");
        assert_eq!(regime.round_price("999", 0.9636), 0.9636, "and rounding stays a no-op");
    }
}
