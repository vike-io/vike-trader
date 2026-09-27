//! `mainnet` — the ONE shared PARSE rule for the per-venue `{VENUE}_MAINNET` real-money flag, plus
//! the per-venue switch table (capability-map playbook, STEP 2: the divergence STEP 1 declared
//! byte-identically is now CONVERGED — every switched venue reads and parses the flag the same way).
//!
//! **THE RULE — one dialect, every venue.** Mainnet is armed by the EXACT string `"1"` and nothing
//! else, taken from the real process env OR the workspace `.env` map, with the **process env
//! winning** whenever it carries a value at all. Every other value — unset, `""`, `"0"`, `"true"`,
//! `"TRUE"`, `"yes"` — resolves to DEMO/testnet, and a SET-but-disarming process value MASKS an
//! arming `.env` line (never the other way round). Exact-`"1"` is this workspace's established
//! arming idiom (`VIKE_RECONCILE`, `POLY_EXEC`, `VIKE_TRADEHUB_LIVE`), and reading BOTH sources
//! closes the repo-wide "the workspace `.env` is never exported to the process env" trap, so ONE
//! `{VENUE}_MAINNET=1` line arms every venue identically wherever the operator writes it.
//!
//! ## What STEP 2 flipped, and when (2026-07)
//!
//! STEP 1 (#841) declared — as a `MainnetDialect` table, changing nothing — that the same flag
//! FAMILY parsed under two different dialects. STEP 2 collapses them onto the rule above, so the
//! enum is gone and the table below declares one uniform row per switched venue. TWO behavior
//! changes, both deliberate and signed off:
//!
//! - **binance / bybit / okx GAINED the `.env`-map source.** They previously read ONLY
//!   `std::env::var`, so a `{VENUE}_MAINNET=1` line written in the workspace `.env` — the very file
//!   every credential comes from — parsed as UNSET and silently kept the venue on demo. That silent
//!   no-op was the audit finding this flip exists to close.
//! - **hyperliquid LOST the fuzzy `"true"` spelling.** It previously armed on `"1"` OR
//!   case-insensitive `"true"`; now only `"1"` arms it, so an operator whose deployment says
//!   `HYPERLIQUID_MAINNET=true` DROPS TO TESTNET after this change. That is the one
//!   regression-shaped row of the flip. It fails SAFE (testnet, never a surprise mainnet), but it
//!   does change a running deployment's network — mainnet-armed hyperliquid operators must respell
//!   the flag as `1`.
//!
//! The two alternatives were rejected: fuzzy-everywhere widens the value grammar of a real-money
//! flag, and process-env-only-everywhere keeps the `.env` trap while flipping hyperliquid down just
//! the same.
//!
//! **Env boundary — this module reads NOTHING.** [`mainnet_from`] and [`mainnet_for`] take the
//! already-read process-env and `.env`-map values; each call site keeps its OWN
//! `std::env::var` + `vars.get` pair, spelled with a `const *_ENV` or a literal name the
//! settings-registry gate (`crates/vike-ops/tests/settings_registry.rs`) can resolve. A shared
//! reader taking the variable NAME as a parameter was tried in STEP 1 and rejected: a parameterized
//! `env::var(var)` is exactly that gate's documented computed-key blind spot — it goes
//! `DYNAMIC_ALLOWLIST`-dark and strands each venue's registry row with no in-crate read to justify
//! its `Layer`. Sharing the FOLD instead of the READ keeps every row resolvable at its own site.
//!
//! Read sites — one process-env read plus one `.env`-map lookup each, all folding through
//! [`mainnet_for`] so no site can drift from the declared rule:
//!
//! - `vike_binance::exec::{mainnet_from, mainnet_enabled}` (`BINANCE_MAINNET`, via that file's
//!   `MAINNET_ENV` const)
//! - `vike_bybit::perp::{mainnet_from, mainnet_enabled}` (`BYBIT_MAINNET`, same const idiom)
//! - `vike_okx::perp::{mainnet_from, mainnet_enabled}` (`OKX_MAINNET`, same const idiom)
//! - `vike-mount/src/hyperliquid.rs::hl_env` (`HYPERLIQUID_MAINNET`, spelled as a literal)
//! - `vike-tradehub`'s live feed-network plan (`main.rs`, `HYPERLIQUID_MAINNET`, same literal)
//!
//! The three CEX venues resolve the flag ONCE per mount (`vike_mount::make_engine`, which owns the
//! `.env` map) and thread the resolved `bool` down into every adapter site that needs it — the exec
//! REST/WS hosts, the instrument-grid pre-fetch, the funding poller and the reconcile client —
//! rather than each spawned thread re-reading global process env for itself. So one mount's
//! credential tier, its hosts and its reconcile reads can never disagree about which network they
//! are on, even if the environment mutates mid-session.

/// A venue's `{VENUE}_MAINNET` switch, as declared by [`mainnet_switch_for`].
///
/// After STEP 2 there is exactly ONE rule ([`mainnet_from`]) and therefore exactly one shape of
/// row, so this type carries no per-venue variation — it is the marker meaning "this venue HAS the
/// switch, and it parses like every other switched venue". STEP 1's `MainnetDialect` enum (which
/// existed only to spell the two divergent parses) is gone. A future venue that genuinely cannot
/// use the shared rule would re-introduce a variant here — deliberately, behind its own sign-off,
/// never by drifting a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MainnetSwitch;

/// THE converged rule: fold the two already-read source values (`process` = the real process env,
/// `map` = the workspace `.env` map) into one armed/not-armed verdict. Pure, so the whole grammar
/// is unit-testable without mutating global env — the READS stay at the call sites, see the module
/// doc's env-boundary note.
///
/// `process.or(map)` IS the precedence rule: a SET process value wins outright, including a
/// disarming one, which therefore masks an arming `.env` line. (Byte-identical to the
/// `.ok().or_else(|| vars.get(..))` chain hyperliquid shipped before STEP 2 — only the accepted
/// VALUE grammar narrowed.)
#[must_use]
pub fn mainnet_from(process: Option<&str>, map: Option<&str>) -> bool {
    process.or(map) == Some("1")
}

/// Today's per-venue mainnet-switch reality, one NAMED row per roster venue
/// (`vike_model::VENUES`) — the capability-map playbook shape: a named `None` row proves the venue
/// was CLASSIFIED as switchless, not forgotten (`every_roster_venue_is_classified` is the gate).
/// `None` = the venue has NO `{VENUE}_MAINNET` env switch anywhere in the tree: its demo/live split
/// rides another mechanism entirely (credential tier/config, a gateway login, or the venue is
/// mainnet-only). Unknown non-roster strings also return `None`. Every `Some` row is CONSUMED on
/// the real path through [`mainnet_for`] (the sites are listed per row below), so a future
/// per-venue change is a one-row edit here.
#[must_use]
pub fn mainnet_switch_for(venue: &str) -> Option<MainnetSwitch> {
    match venue {
        // `vike_binance::exec::{mainnet_from, mainnet_enabled}` consume this row
        // (`BINANCE_MAINNET` via that file's `MAINNET_ENV` const).
        "binance" => Some(MainnetSwitch),
        // `vike_bybit::perp::{mainnet_from, mainnet_enabled}` consume this row
        // (`BYBIT_MAINNET` via that file's `MAINNET_ENV` const).
        "bybit" => Some(MainnetSwitch),
        // `vike_okx::perp::{mainnet_from, mainnet_enabled}` consume this row
        // (`OKX_MAINNET` via that file's `MAINNET_ENV` const).
        "okx" => Some(MainnetSwitch),
        // Read at the MOUNT layer, not the bridge crate: `vike-mount/src/hyperliquid.rs::hl_env`
        // and vike-tradehub's feed-network plan consume it (`HYPERLIQUID_MAINNET`, spelled as a
        // literal at both sites). STEP 1's ONE fuzzy row — converged by STEP 2, which is why
        // `HYPERLIQUID_MAINNET=true` no longer arms anything.
        "hyperliquid" => Some(MainnetSwitch),
        // Switchless roster venues, each NAMED: demo vs live rides the venue's credential/config
        // loading (deribit/oanda/ig/fxcm/dukascopy/ctrader/alpaca) — no `{VENUE}_MAINNET` env
        // read exists for any of them.
        "deribit" | "oanda" | "ig" | "fxcm" | "dukascopy" | "ctrader" | "alpaca" => None,
        // ibkr: paper vs live is the TWS/CP-Gateway login the client connects to, not an env
        // flag.
        "ibkr" => None,
        // polymarket: mainnet-ONLY (no testnet exists); its live gating is creds +
        // `POLY_EXEC`/`POLY_RECONCILE`, never a mainnet flag.
        "polymarket" => None,
        // ⚠ aster: switchless because the TIER is chosen by which credential PREFIX is present
        // (`ASTER_LIVE_*` vs `ASTER_TESTNET_*` — `crates/bridges/aster/src/signing.rs`'s
        // `load_aster_credentials`), never by a flag. Its testnet EXISTS and is routed
        // (`crates/bridges/aster/src/urls.rs`'s `urls_for` maps `Environment::Demo` onto it); what
        // is absent is the TESTNET credential pair, so a credentialed mount today is REAL MONEY.
        // The endpoint claim this comment used to carry was false, regrew four times, and is now
        // held dead by `crates/bridges/aster/tests/testnet_claim_gate.rs`.
        "aster" => None,
        // vike:new-venue:row // TODO(new-venue: {venue}): does a `{VENUE}_MAINNET` env switch exist anywhere in the tree?
        // vike:new-venue:row // `None` is the scaffolded answer (a fresh bridge reads no flag), and it must ALSO be
        // vike:new-venue:row // listed in `every_roster_venue_is_classified`'s SWITCHLESS set below.
        // vike:new-venue:row "{venue}" => None,
        _ => None,
    }
}

/// The production entry point every read site calls: fold `venue`'s two already-read flag values
/// through the converged rule, GATED on that venue having a declared switch at all. A venue with no
/// row ([`mainnet_switch_for`] `None`) can never be armed, even if a caller hands it an arming
/// value — so the table is load-bearing on the real path, not decoration, and a stray
/// `DERIBIT_MAINNET=1` is inert by construction rather than by nobody having wired a read.
#[must_use]
pub fn mainnet_for(venue: &str, process: Option<&str>, map: Option<&str>) -> bool {
    mainnet_switch_for(venue).is_some() && mainnet_from(process, map)
}

#[cfg(test)]
mod tests {
    use super::{MainnetSwitch, mainnet_for, mainnet_from, mainnet_switch_for};

    /// The converged VALUE grammar: the EXACT string `"1"` and nothing else arms mainnet, from
    /// EITHER source. `"true"` is REJECTED — for every venue, hyperliquid included (STEP 2's one
    /// regression-shaped change).
    #[test]
    fn only_exact_one_arms_mainnet_from_either_source() {
        assert!(mainnet_from(Some("1"), None), "process 1 ⇒ mainnet");
        assert!(mainnet_from(None, Some("1")), "map-only 1 ⇒ mainnet (the STEP-2 CEX gain)");
        assert!(!mainnet_from(None, None), "unset ⇒ demo");
        for v in ["", "0", "true", "TRUE", "True", "yes", "mainnet", " 1", "1 "] {
            assert!(!mainnet_from(Some(v), None), "process {v:?} ⇒ demo");
            assert!(!mainnet_from(None, Some(v)), "map {v:?} ⇒ demo");
        }
    }

    /// Precedence: a SET process value wins outright — including a disarming one, which masks an
    /// arming `.env` line. Never the other way round.
    #[test]
    fn process_env_wins_over_a_conflicting_dotenv_value() {
        assert!(!mainnet_from(Some("0"), Some("1")), "disarming process masks an arming map");
        assert!(mainnet_from(Some("1"), Some("0")), "arming process beats a disarming map");
        // A set-but-unrecognised process value still WINS (and disarms) — it is not "absent".
        assert!(!mainnet_from(Some("true"), Some("1")), "set-but-invalid process still masks");
    }

    /// THE audit finding, now as a CONVERGED-behavior gate (STEP 1 pinned the divergence here;
    /// STEP 2 fixed it). Both halves of the old divergence are gone: the same `.env`-only line arms
    /// EVERY switched venue, and the fuzzy `"true"` spelling arms NONE of them. Reverting either
    /// half must fail this test deliberately, never drift.
    #[test]
    fn the_hl_vs_cex_divergence_is_converged() {
        const SWITCHED: &[&str] = &["binance", "bybit", "okx", "hyperliquid"];
        for &v in SWITCHED {
            // ONE `.env`-map-only `{VENUE}_MAINNET=1` line (the map is never exported to the
            // process env) now arms EVERY switched venue — it used to arm only hyperliquid.
            assert!(mainnet_for(v, None, Some("1")), "{v}: a .env-only `1` must arm mainnet");
            // ONE shell-exported `{VENUE}_MAINNET=true` now arms NONE of them — it used to arm
            // hyperliquid. The regression-shaped half of the flip.
            assert!(!mainnet_for(v, Some("true"), None), "{v}: fuzzy `true` must NOT arm mainnet");
            // …and the shared happy path + default, spelled per venue.
            assert!(mainnet_for(v, Some("1"), None), "{v}: exported `1` arms mainnet");
            assert!(!mainnet_for(v, None, None), "{v}: unset ⇒ demo/testnet");
        }
    }

    /// The table is load-bearing on the REAL path: a venue with no declared switch can never be
    /// armed, whatever values reach [`mainnet_for`].
    #[test]
    fn a_switchless_venue_can_never_be_armed() {
        for v in ["deribit", "aster", "polymarket", "ibkr", "ctrader", "no-such-venue"] {
            assert!(!mainnet_for(v, Some("1"), Some("1")), "{v} has no switch — never armed");
        }
    }

    /// Completeness vs the canonical roster (`vike_model::VENUES`), the playbook shape: every
    /// roster venue is classified exactly once — a SWITCHED row or a DECLARED switchless `None`
    /// row. Adding a venue to the roster fails here until it is classified one way or the other.
    #[test]
    fn every_roster_venue_is_classified() {
        const SWITCHED: &[&str] = &["binance", "bybit", "okx", "hyperliquid"];
        // `#[rustfmt::skip]`: the last line here is a `just new-venue` marker at the TAIL of the
        // literal, and rustfmt re-indents such a marker once a row ending in a trailing `//`
        // comment is generated above it — see `crates/vike-ops/tests/new_venue_gate.rs`'s
        // `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
        #[rustfmt::skip]
        const SWITCHLESS: &[&str] = &[
            "deribit",
            "oanda",
            "ig",
            "fxcm",
            "dukascopy",
            "polymarket",
            "ibkr",
            "ctrader",
            "alpaca",
            "aster",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to SWITCHED if a real env flag is wired
        ];
        assert_eq!(
            SWITCHED.len() + SWITCHLESS.len(),
            vike_model::VENUES.len(),
            "every roster venue classified exactly once"
        );
        for &v in vike_model::VENUES {
            let switched = SWITCHED.contains(&v);
            let switchless = SWITCHLESS.contains(&v);
            assert!(
                switched ^ switchless,
                "roster venue {v} must be classified exactly once (switch row or declared \
                 switchless)"
            );
        }
        for v in SWITCHED {
            assert_eq!(mainnet_switch_for(v), Some(MainnetSwitch), "{v}: switch row drifted");
        }
        for v in SWITCHLESS {
            assert_eq!(mainnet_switch_for(v), None, "{v} declared switchless");
        }
        // Unknown non-roster strings have no switch either.
        assert_eq!(mainnet_switch_for("no-such-venue"), None);
    }
}
