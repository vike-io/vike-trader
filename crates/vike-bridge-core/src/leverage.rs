//! The ONE pure rule deciding what ACCOUNT LEVERAGE a perp exec arm POSTs to its venue at startup.
//!
//! ## The divergence this closes
//! Four perp adapters (binance `/fapi/v1/leverage`, bybit `/v5/position/set-leverage`, okx
//! `/api/v5/account/set-leverage`, aster `/fapi/v3/leverage`) each fired a one-shot set-leverage
//! from a HARDCODED `const LEVERAGE: f64 = 2.0` — "demo default; matches the smoke". Meanwhile the
//! operator-facing budget ([`ProfileRisk::max_leverage`], `[risk] max_leverage` in a profile TOML)
//! drives every margin decision the RiskGate makes: [`ProfileRisk::im_requirement`] converts it to
//! `im = 1.0 / max_leverage`, and `vike_model::has_sufficient_margin` sizes against THAT. So the
//! risk engine sized positions against one number while the ACCOUNT was configured with another —
//! dangerous in both directions:
//!
//! * profile leverage HIGHER than the venue's ⇒ a correctly-sized order is rejected by the venue
//!   for insufficient margin (the RiskGate believed there was buying power the account lacks);
//! * profile leverage LOWER than the venue's ⇒ a position the RiskGate believes is safe sits on an
//!   account configured to carry far more, and liquidates well before the gate's own maintenance
//!   math expects it to.
//!
//! This module is the single seam that makes ONE number govern both. Each venue's `exec.rs` wraps
//! it in a `leverage_for(profile)` of its own (its `DEFAULT_LEVERAGE` baked in), the mount resolves
//! it ONCE and threads the resolved `f64` into the exec spawn — the same "resolve once at the
//! mount, never re-read from a spawned thread" idiom `{VENUE}_MAINNET`, the attribution codes and
//! binance's `Environment` already follow.
//!
//! ## The contract
//! 1. **Unset ⇒ UNCHANGED.** No profile at all, or a profile that never mentions `max_leverage`,
//!    yields the venue's own `venue_default` — the exact literal it posted before this existed.
//!    An upgrade must never silently re-leverage a live account, so this is the property that
//!    matters most; it is pinned by a test in this module AND by one in every venue's `exec.rs`.
//! 2. **Set ⇒ the operator's number**, byte-for-byte the same quantity the RiskGate's
//!    `im_requirement` is derived from (see [`agrees_with_im_requirement`]'s doc for the invariant
//!    and its test).
//! 3. **Nonsense ⇒ 1× (no leverage)**, never the default and never a `< 1` value the venue would
//!    reject outright. This MIRRORS [`ProfileRisk::im_requirement`], which degrades exactly the
//!    same inputs to `Some(1.0)` — the two must agree, or the divergence this module exists to
//!    close reopens on the one input where the operator already got it wrong. (Unreachable from a
//!    loaded profile: `vike_core::RunProfile::validate` and `ProfileRisk::apply_to` both REJECT a
//!    non-finite or `< 1.0` leverage at the config edge. This is the hand-built-`ProfileRisk`
//!    path's floor.)
//!
//! ## The venue CEILING — parsed per SYMBOL, never a constant table
//! Rules 1-3 give the operator's number a floor; [`clamp_to_venue_cap`] gives it a ceiling, and
//! the ceiling is **read off the venue's own instrument payload for the mounted symbol**. There is
//! still deliberately no per-venue constant table: every real ceiling is per-SYMBOL and, on the
//! bracket venues, per-RISK-TIER, so a single hardcoded number would be wrong for most instruments
//! and would read as verified fact (the capability-map honesty rule — `CLAUDE.md`, "Per-venue
//! capability maps"). A cap this workspace cannot READ is a cap this workspace does not claim.
//!
//! Which of the four arms can supply one today, verified live 2026-08-05 against the public
//! endpoints:
//!
//! | venue | field | endpoint | cost |
//! |---|---|---|---|
//! | bybit | `leverageFilter.maxLeverage` (`"100.00"`) | `/v5/market/instruments-info` | FREE — the exec thread already fetches this exact response for the tick/lot grid |
//! | okx | `lever` (`"100"`) | `/api/v5/public/instruments` | FREE — same, the response that already yields `ctVal` |
//! | binance | — | `/fapi/v1/exchangeInfo` carries **no** leverage field (1.0 MB payload, zero case-insensitive `leverage` hits) | a WHOLE NEW SIGNED request: the cap lives in the leverage BRACKETS at `/fapi/v1/leverageBracket`, which answers `-2014 API-key format invalid` unauthenticated |
//! | aster | — | `/fapi/v3/exchangeInfo`, same: zero hits | same: `/fapi/v3/leverageBracket` answers `-1102` (mandatory `nonce`), i.e. a signed EIP-712 round-trip |
//!
//! So bybit and okx clamp; **binance and aster pass `None` and are byte-identical to before this
//! function existed** — adding a second blocking signed request to the mount path is a cost that
//! belongs to a deliberate PR, not to a doc-closing one. Their brackets are also RISK-TIERED
//! (`bracket[].initialLeverage` falls as notional rises), so "the cap" there is a function of
//! position size, not one number — a clamp against the tier-0 value would be a different, weaker
//! claim than the flat cap bybit/okx publish.
//!
//! The precedent for WHERE the parsed cap lives is `vike_hyperliquid`: its `maxLeverage` rides
//! `symbology::InstrumentRef` — the venue's own instrument struct — and its `instruments.rs`
//! states outright that "`max_leverage` is NOT a `SymbolProperties` field".
//! bybit/okx follow that verbatim (`BybitInstrument::max_leverage` / `OkxInstrument::max_leverage`)
//! rather than growing `SymbolProperties`, which is persisted in the `kind=properties` PIT series
//! and would drag a Parquet codec column decision along with it.
//!
//! ## ⚠ The residual this clamp does NOT close
//! When the clamp BITES, the account is set to the venue's cap while `ProfileRisk::im_requirement`
//! still derives `im = 1.0 / max_leverage` from the operator's ORIGINAL number — so the RiskGate
//! sizes against buying power the account does not have. That is the FIRST failure mode in the
//! divergence list at the top of this module, and clamping does not remove it; it makes it VISIBLE
//! (a warn naming both numbers) instead of leaving it to a venue rejection at submit time. Closing
//! it means feeding the cap back into the mount's `RiskLimits`/`ProfileRisk`, which changes how
//! every order is SIZED — a separate, deliberate change. Until then: an operator whose
//! `[risk] max_leverage` exceeds the venue cap should lower it, and the warn says so.
//!
//! Unknown cap ⇒ nothing changes anywhere: no clamp, no warn, the exact bytes of every mount
//! before this existed. That is the property `unknown_cap_is_byte_identical` pins.

use vike_exec::ProfileRisk;

/// Resolve the account leverage a perp exec arm POSTs at startup.
///
/// * `profile` — the operator's `[risk]` table as threaded into `vike_mount::make_engine`;
///   `None` (no profile supplied at all) is the common desktop/paper case.
/// * `venue_default` — the literal THIS venue posted before this function existed. Returned
///   verbatim whenever the operator expressed no leverage intent, so an upgrade is a no-op.
///
/// Never returns a non-finite value and never returns `< 1.0`, so the caller's wire formatting
/// (binance/aster cast `as i64`; a sub-1 value would serialize as `0` and be rejected) is safe by
/// construction. See the module doc for the full contract.
///
/// This is the operator's REQUEST, not necessarily what goes on the wire: a venue that publishes a
/// per-symbol ceiling has its adapter pass this through [`clamp_to_venue_cap`] first (bybit/okx
/// today). A venue with no readable cap posts this value verbatim.
pub fn resolve_startup_leverage(profile: Option<&ProfileRisk>, venue_default: f64) -> f64 {
    match profile.and_then(|p| p.max_leverage) {
        // Rule 1 — the operator expressed no leverage intent: today's literal, byte-identical.
        None => venue_default,
        // Rule 2 — the operator's own number, the SAME one `im_requirement` is derived from.
        Some(lev) if lev.is_finite() && lev >= 1.0 => lev,
        // Rule 3 — nonsense degrades to 1x, matching `ProfileRisk::im_requirement`'s own degrade.
        // Startup-only (once per mount), so a warn here is bounded and worth the noise: it names a
        // profile the config edge would have rejected outright.
        Some(bad) => {
            tracing::warn!(
                target: "vike_bridge_core::leverage",
                requested = bad,
                "risk.max_leverage is not a usable leverage (must be finite and >= 1.0) — setting \
                 the venue account to 1x (no leverage), matching the initial-margin requirement \
                 the RiskGate degrades to for the same value"
            );
            1.0
        }
    }
}

/// Is a venue-published ceiling one this module will clamp against?
///
/// A cap is USABLE only when it is finite and at least `1.0` — the same two conditions
/// [`resolve_startup_leverage`]'s rule 3 imposes on the operator's own number, for the same reason:
/// clamping to a non-finite or sub-1 value would post a leverage no venue accepts (binance/aster
/// cast `as i64`, so `0.5` serializes as `0`), and would do it while DE-leveraging a live account.
///
/// Anything else — `0.0` (the venue's own absent-is-zero convention leaking through a parse), a
/// negative, a NaN — means the parse produced garbage, not a ceiling. Treated as UNKNOWN, which is
/// the safe direction: unknown clamps nothing.
#[must_use]
pub fn is_usable_cap(cap: f64) -> bool {
    cap.is_finite() && cap >= 1.0
}

/// Clamp a resolved startup leverage to the ceiling the venue publishes for THIS symbol.
///
/// * `requested` — whatever [`resolve_startup_leverage`] returned (the operator's number, or the
///   venue default when they expressed no intent). Passed through untouched in every case but one.
/// * `venue_cap` — the per-SYMBOL ceiling parsed out of the instrument payload the caller ALREADY
///   fetched (`BybitInstrument::max_leverage` / `OkxInstrument::max_leverage`). `None` means this
///   venue does not publish one, or the fetch failed — see the module doc's table.
/// * `venue`/`symbol` — for the warn only; this function performs no I/O and no lookups.
///
/// **`None` ⇒ `requested`, byte-for-byte.** That is the contract for binance/aster (no readable
/// cap at all) and for any bybit/okx mount whose instrument fetch failed — a venue that cannot
/// report a ceiling behaves exactly as it did before this function existed, including its warn
/// silence. A cap that fails [`is_usable_cap`] is treated the same way, but warns: a present-but-
/// garbage cap is evidence of a parse or venue-shape change, not something to act on.
///
/// Called from the venue exec THREAD rather than the mount, unlike `mainnet`/attribution/leverage
/// itself. That is deliberate and not a break with the "resolve once at the mount" idiom: the idiom
/// exists so a spawned thread cannot re-read CONFIGURATION and disagree with the mount about it.
/// This is not configuration — it is venue data, already sitting in the instruments response the
/// exec thread fetches to build its own rounding grid, so clamping there costs ZERO extra requests
/// and covers every spawn path (including the `spawn` used by smokes, which never sees a profile).
#[must_use]
pub fn clamp_to_venue_cap(
    requested: f64,
    venue_cap: Option<f64>,
    venue: &str,
    symbol: &str,
) -> f64 {
    match venue_cap {
        // The whole "unknown ⇒ unchanged" contract, in one arm.
        None => requested,
        Some(cap) if !is_usable_cap(cap) => {
            tracing::warn!(
                target: "vike_bridge_core::leverage",
                %venue, %symbol, cap, requested,
                "venue published an unusable max-leverage (not finite, or < 1x) — ignoring it and \
                 posting the requested leverage unchanged; a cap this shape means the instrument \
                 payload changed, not that the account should be de-leveraged"
            );
            requested
        }
        Some(cap) if requested > cap => {
            // Startup-only (once per mount), and it names the exact residual the clamp leaves open
            // (see the module doc): the RiskGate is still sizing against `requested`.
            tracing::warn!(
                target: "vike_bridge_core::leverage",
                %venue, %symbol, requested, venue_cap = cap,
                "requested account leverage exceeds the venue's published maximum for this symbol \
                 — posting the venue cap instead. ⚠ the RiskGate still sizes against the REQUESTED \
                 number (im = 1/requested), so it believes in buying power this account does not \
                 have; lower [risk] max_leverage to the cap to make the two agree"
            );
            cap
        }
        // At or under the ceiling — nothing to do, and deliberately no log: this is the normal case.
        Some(_) => requested,
    }
}

/// The invariant tying this module to the RiskGate: whenever the operator SET a leverage, the
/// initial-margin fraction the RiskGate sizes against is exactly the reciprocal of the number
/// posted to the venue (`im == 1.0 / lev`). That equality IS "one number governs both"; it is
/// asserted over a value table below rather than merely asserted in prose.
///
/// Compared in THIS direction (`im` vs `1.0 / posted`) rather than the mirror (`posted` vs
/// `1.0 / im`) on purpose: `ProfileRisk::im_requirement` computes `1.0 / max_leverage`, so as long
/// as this module returns `max_leverage` verbatim the two sides are BIT-identical and the `==` is
/// exact. The mirror would round-trip an f64 through two divisions and could differ in the last
/// ulp for values like `1/3`, turning a true invariant into a flaky assertion.
///
/// `None` (the operator set nothing) is excluded on purpose: `im_requirement` is then `None` too
/// — the buying-power lane stays disarmed and there is no reciprocal to compare against — while
/// the venue still gets `venue_default`, by rule 1.
///
/// ⚠ This is the invariant over [`resolve_startup_leverage`] ALONE — the operator's request versus
/// the gate's margin fraction, which is exactly where the two are supposed to agree. It says
/// nothing about a mount where [`clamp_to_venue_cap`] BITES: the account then sits at the venue's
/// ceiling while `im_requirement` still reflects the request, and that gap is the documented
/// residual (module doc, "The residual this clamp does NOT close"), warned at the clamp rather
/// than silently folded in here.
pub fn agrees_with_im_requirement(profile: &ProfileRisk, venue_default: f64) -> bool {
    match profile.im_requirement() {
        None => true,
        Some(im) => im == 1.0 / resolve_startup_leverage(Some(profile), venue_default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ProfileRisk` carrying only a leverage — every other field at its serde default, which is
    /// exactly the shape a `[risk]` table that mentions only `max_leverage` deserializes into.
    fn profile(max_leverage: Option<f64>) -> ProfileRisk {
        ProfileRisk { max_leverage, ..Default::default() }
    }

    /// RULE 1, the property that matters most: no profile, or a profile silent about leverage,
    /// posts the venue's historical literal. An upgrade must not re-leverage a live account.
    #[test]
    fn unset_yields_the_venue_default() {
        assert_eq!(resolve_startup_leverage(None, 2.0), 2.0);
        assert_eq!(resolve_startup_leverage(Some(&profile(None)), 2.0), 2.0);
        // Not special-cased to 2.0 anywhere: any venue's own literal rides through.
        assert_eq!(resolve_startup_leverage(None, 5.0), 5.0);
    }

    /// RULE 2: a set leverage wins over the venue literal, in BOTH directions (up and down) —
    /// the two failure modes this module exists to close.
    #[test]
    fn set_leverage_overrides_the_venue_default() {
        assert_eq!(resolve_startup_leverage(Some(&profile(Some(10.0))), 2.0), 10.0);
        assert_eq!(resolve_startup_leverage(Some(&profile(Some(1.0))), 2.0), 1.0);
        // Fractional survives verbatim; the per-venue wire formatting owns any truncation.
        assert_eq!(resolve_startup_leverage(Some(&profile(Some(2.5))), 2.0), 2.5);
    }

    /// RULE 3: nonsense degrades to 1x — never to the default (which would silently re-arm the
    /// very divergence) and never to a sub-1 value the venue would reject.
    #[test]
    fn nonsense_leverage_degrades_to_one_x() {
        for bad in [0.0, 0.5, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                resolve_startup_leverage(Some(&profile(Some(bad))), 2.0),
                1.0,
                "{bad} must degrade to 1x"
            );
        }
    }

    /// The whole point, machine-checked: the venue leverage and the RiskGate's initial-margin
    /// fraction are the SAME number seen from two sides, for every value an operator can write.
    #[test]
    fn posted_leverage_is_the_reciprocal_of_the_gates_im_requirement() {
        for lev in [1.0, 2.0, 2.5, 3.0, 5.0, 10.0, 20.0, 125.0] {
            let p = profile(Some(lev));
            // Exactly the operator's number reaches the venue …
            assert_eq!(resolve_startup_leverage(Some(&p), 2.0), lev);
            // … and the gate's margin fraction is its exact reciprocal.
            assert!(
                agrees_with_im_requirement(&p, 2.0),
                "max_leverage = {lev}: posted {} vs im {:?}",
                resolve_startup_leverage(Some(&p), 2.0),
                p.im_requirement()
            );
        }
        // And on the degrade path, where both sides independently choose 1x.
        let p = profile(Some(0.0));
        assert_eq!(p.im_requirement(), Some(1.0));
        assert!(agrees_with_im_requirement(&p, 2.0));
        // Unset: the gate is disarmed, the venue keeps its literal — vacuously in agreement.
        assert!(agrees_with_im_requirement(&profile(None), 2.0));
    }

    // ---- the venue ceiling ----------------------------------------------------------------

    /// THE property the ceiling work must not break: a venue that publishes no cap (binance/aster
    /// today) — or a bybit/okx mount whose instrument fetch failed — posts EXACTLY what it posted
    /// before [`clamp_to_venue_cap`] existed, for every input, including the nonsense ones.
    #[test]
    fn unknown_cap_is_byte_identical() {
        for requested in [1.0, 2.0, 2.5, 10.0, 125.0, 1e9] {
            assert_eq!(
                clamp_to_venue_cap(requested, None, "binance", "BTCUSDT.P"),
                requested,
                "no cap ⇒ {requested} rides through untouched"
            );
        }
        // …and composed with the resolver, which is how the venues actually call it.
        assert_eq!(
            clamp_to_venue_cap(resolve_startup_leverage(None, 2.0), None, "aster", "BTCUSDT.P"),
            2.0,
            "unset profile + no cap ⇒ the historical venue literal"
        );
    }

    /// A cap ABOVE (or equal to) the request never touches it — the common case on every real
    /// mount (bybit BTCUSDT publishes 100x, okx BTC-USDT-SWAP 100x, against a 2x default).
    #[test]
    fn a_cap_above_the_request_changes_nothing() {
        assert_eq!(clamp_to_venue_cap(2.0, Some(100.0), "bybit", "BTCUSDT"), 2.0);
        assert_eq!(clamp_to_venue_cap(10.0, Some(100.0), "okx", "BTC-USDT-SWAP"), 10.0);
        // Exactly at the ceiling is NOT clamped (the venue accepts its own maximum).
        assert_eq!(clamp_to_venue_cap(100.0, Some(100.0), "bybit", "BTCUSDT"), 100.0);
    }

    /// A cap BELOW the request clamps DOWN to the cap — never up, never to the venue default, and
    /// never past it in either direction.
    #[test]
    fn a_cap_below_the_request_clamps_down_to_the_cap() {
        assert_eq!(clamp_to_venue_cap(50.0, Some(20.0), "bybit", "SOMEALTUSDT"), 20.0);
        assert_eq!(clamp_to_venue_cap(125.0, Some(10.0), "okx", "SOME-USDT-SWAP"), 10.0);
        // Fractional caps survive verbatim — bybit publishes `"100.00"`-shaped decimals and a
        // `leverageStep` of `0.01`, so a non-integer ceiling is a real venue value, not junk.
        assert_eq!(clamp_to_venue_cap(10.0, Some(7.5), "bybit", "SOMEALTUSDT"), 7.5);
        // The clamp is a MIN, so it is idempotent.
        let once = clamp_to_venue_cap(50.0, Some(20.0), "bybit", "SOMEALTUSDT");
        assert_eq!(clamp_to_venue_cap(once, Some(20.0), "bybit", "SOMEALTUSDT"), once);
    }

    /// A present-but-unusable cap is treated as UNKNOWN, not acted on: clamping a live account to
    /// `0`/NaN/a negative would de-leverage it on the strength of a bad parse.
    #[test]
    fn an_unusable_cap_is_ignored_not_applied() {
        for bad in [0.0, 0.5, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!is_usable_cap(bad), "{bad} must not count as a cap");
            assert_eq!(
                clamp_to_venue_cap(2.0, Some(bad), "bybit", "BTCUSDT"),
                2.0,
                "cap {bad} must leave the requested leverage untouched"
            );
        }
        // The boundary IS usable — 1x (no leverage) is a legitimate ceiling.
        assert!(is_usable_cap(1.0));
        assert_eq!(clamp_to_venue_cap(2.0, Some(1.0), "bybit", "SOMEALTUSDT"), 1.0);
    }

    /// The clamp composes with the resolver in the direction the venues use it: the operator's
    /// number wins over the venue default, and the venue's ceiling wins over the operator.
    #[test]
    fn resolver_then_clamp_is_min_of_operator_and_venue() {
        let asked_50 = profile(Some(50.0));
        let resolved = resolve_startup_leverage(Some(&asked_50), 2.0);
        assert_eq!(resolved, 50.0, "the operator's number, pre-clamp");
        assert_eq!(clamp_to_venue_cap(resolved, Some(20.0), "bybit", "X"), 20.0, "venue wins");
        assert_eq!(clamp_to_venue_cap(resolved, Some(75.0), "bybit", "X"), 50.0, "operator wins");
        // And the degrade floor is BELOW every usable cap, so rule 3 can never be clamped further.
        let nonsense = resolve_startup_leverage(Some(&profile(Some(f64::NAN))), 2.0);
        assert_eq!(nonsense, 1.0);
        assert_eq!(clamp_to_venue_cap(nonsense, Some(1.0), "okx", "X"), 1.0);
    }

    /// A profile that sets OTHER budget fields but not leverage is still rule 1 — the merge in
    /// `make_engine` takes every operator field from the profile, so "a profile is present" must
    /// never be mistaken for "a leverage was requested".
    #[test]
    fn a_profile_without_max_leverage_is_still_unset() {
        let p = ProfileRisk {
            max_notional_per_order: Some(250_000.0),
            max_total_exposure: Some(1_000_000.0),
            ..Default::default()
        };
        assert_eq!(resolve_startup_leverage(Some(&p), 2.0), 2.0);
    }
}
