//! The data-plane-only declaration (`data_only`): parse, refusals, and its venue subset.

use super::*;

// ---------------------------------------------------------------------------------------------
// The data-plane-only declaration (`data_only` — the data-only credential seam).
// ---------------------------------------------------------------------------------------------

/// The declaration parses on every eligible venue, lowers per `[[mounts]]` row, and the
/// effective accessor reads absent and explicit `false` as the SAME (default) answer — the
/// property `live_mount`'s withhold decision keys on.
#[test]
fn data_only_parses_on_eligible_venues_and_defaults_off() {
    for venue in DATA_ONLY_VENUES {
        let p = DaemonProfile::from_toml_str(&format!(
            "venue = \"{venue}\"\nsymbol = \"X\"\ndata_only = true"
        ))
        .unwrap_or_else(|e| panic!("{venue}: the declaration must parse: {e}"));
        assert!(p.data_only_effective(), "{venue}: an explicit true must read true");
    }
    let absent = DaemonProfile::from_toml_str("venue = \"oanda\"\nsymbol = \"X\"").unwrap();
    assert!(!absent.data_only_effective(), "absent is the default: exec follows credentials");
    let explicit_false =
        DaemonProfile::from_toml_str("venue = \"oanda\"\nsymbol = \"X\"\ndata_only = false")
            .unwrap();
    assert!(!explicit_false.data_only_effective(), "explicit false IS the default");
}

/// A keyless-data venue is REFUSED the declaration at load, naming the eligible set and the
/// venue's own data-only path (withhold the credentials) — see `DATA_ONLY_VENUES`' doc for why
/// this is a refusal rather than a widening.
#[test]
fn data_only_is_refused_on_a_keyless_data_venue_naming_the_eligible_set() {
    for venue in ["binance", "bybit", "okx", "deribit", "hyperliquid", "aster"] {
        let err = DaemonProfile::from_toml_str(&format!(
            "venue = \"{venue}\"\nsymbol = \"X\"\ndata_only = true"
        ))
        .unwrap_err();
        for needle in ["data_only", "keyless", "oanda"] {
            assert!(err.contains(needle), "{venue}: the refusal must carry {needle}: {err}");
        }
    }
}

/// Two `[[mounts]]` rows on ONE venue disagreeing on the declaration are refused at load,
/// naming both rows — the withhold is per venue account, so no per-row split exists to grant.
/// Rows AGREEING (or on different venues) pass.
#[test]
fn data_only_rows_sharing_a_venue_must_agree() {
    let disagree = "\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ndata_only = true\n\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ninterval = \"5m\"\n";
    let err = DaemonProfile::from_toml_str(disagree).unwrap_err();
    for needle in ["mounts[0]", "mounts[1]", "data_only"] {
        assert!(err.contains(needle), "the refusal must carry {needle}: {err}");
    }
    let agree = "\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ndata_only = true\n\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ninterval = \"5m\"\ndata_only = \
                     true\n";
    DaemonProfile::from_toml_str(agree).expect("agreeing rows are one venue-wide declaration");
}

/// A top-level `data_only` beside a `[[mounts]]` array joins the both-spellings refusal — the
/// key would configure nothing while reading as real, exactly like every other top-level
/// mount field there.
#[test]
fn data_only_joins_the_both_spellings_refusal() {
    let err = DaemonProfile::from_toml_str(
        "data_only = true\n[[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\n",
    )
    .unwrap_err();
    assert!(err.contains("data_only"), "the refusal must name the offending key: {err}");
    assert!(err.contains("[[mounts]]"), "…and the spelling conflict: {err}");
}

/// Every eligible venue is live-wired — the declaration can only name venues `live_mount` has
/// a feed arm for, in both feature builds (the subset relation, not a hand copy of either
/// list).
#[test]
fn data_only_venues_are_a_subset_of_the_live_wired_set() {
    for venue in DATA_ONLY_VENUES {
        assert!(
            LIVE_WIRED_VENUES.contains(venue),
            "{venue} is declared data-only-eligible but is not live-wired at all"
        );
    }
}
