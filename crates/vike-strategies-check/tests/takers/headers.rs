//! Each taker file's declared `default` IS what the file's reader lands on for an empty table.
//! (Each header was transcribed verbatim from the registry rows of its name, and held to them
//! until the registry was deleted with the compiled strategies.)

use vike_model::params::Params;
use vike_strategy::file::StrategyFileHeader;

/// A file's `echo` export: the "effective params" rows of a params table.
type Echo = fn(&Params) -> Vec<(&'static str, String)>;

/// Every taker file, by its stem.
pub(crate) const TAKERS: &[(&str, &str)] = &[
    ("buy_hold", include_str!("../../../../strategies/trend/buy_hold.rs")),
    ("grid", include_str!("../../../../strategies/grid_dca/grid.rs")),
    ("dca_accumulate", include_str!("../../../../strategies/grid_dca/dca_accumulate.rs")),
    ("momentum", include_str!("../../../../strategies/trend/momentum.rs")),
    ("trailing_scalper", include_str!("../../../../strategies/trend/trailing_scalper.rs")),
    ("funding_capture", include_str!("../../../../strategies/funding/funding_capture.rs")),
    ("funding_carry", include_str!("../../../../strategies/funding/funding_carry.rs")),
    ("pairs_zscore", include_str!("../../../../strategies/pairs/pairs_zscore.rs")),
    ("rotation_top_k", include_str!("../../../../strategies/portfolio/rotation_top_k.rs")),
    (
        "cheap_catch_updown_fair_value",
        include_str!("../../../../strategies/prediction_markets/cheap_catch_updown_fair_value.rs"),
    ),
    (
        "sport_copy_follower",
        include_str!("../../../../strategies/prediction_markets/sport_copy_follower.rs"),
    ),
];

pub(crate) fn header_of(name: &str) -> StrategyFileHeader {
    let (_, text) = TAKERS.iter().find(|(taker, _)| *taker == name).expect("a taker name");
    StrategyFileHeader::parse(text).unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// The header's declared defaults as a params table: what a user who typed every default would
/// hand the reader.
pub(crate) fn defaults(name: &str) -> Params {
    let mut table = Params::new();
    for (key, declaration) in &header_of(name).params {
        if let Some(default) = &declaration.default {
            table.insert(key.clone(), default.clone());
        }
    }
    table
}

/// Typing every declared default changes nothing: the reader lands where it lands on an empty
/// table. Judged by the file's own `echo` where it has one (every key it declares), else by the
/// reader's fields.
#[test]
fn the_declared_defaults_are_what_an_empty_table_resolves_to() {
    let empty = Params::new();
    let echoes: &[(&str, Echo)] = &[
        ("buy_hold", crate::buy_hold::echo),
        ("grid", crate::grid::echo),
        ("dca_accumulate", crate::dca_accumulate::echo),
        ("momentum", crate::momentum::echo),
        ("trailing_scalper", crate::trailing_scalper::echo),
        ("funding_capture", crate::funding_capture::echo),
        ("funding_carry", crate::funding_carry::echo),
        ("pairs_zscore", crate::pairs_zscore::echo),
    ];
    for (name, echo) in echoes {
        let typed = defaults(name);
        assert!(!typed.is_empty(), "{name}: no default declared, so this proves nothing");
        assert_eq!(
            echo(&typed),
            echo(&empty),
            "{name}: a declared default differs from the code's"
        );
        let keys: Vec<&str> = echo(&empty).iter().map(|(key, _)| *key).collect();
        let header = header_of(name);
        let declared: Vec<&str> = header.params.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, declared, "{name}: the echo reports exactly the declared keys, in order");
    }

    // The two tape strategies have no echo: their readers' fields, every one a declared key reads.
    use crate::cheap_catch_updown_fair_value::CheapNp;
    let fields = |cheap: CheapNp| {
        (
            cheap.spot_symbol,
            cheap.mode,
            cheap.size.to_bits(),
            cheap.theta.to_bits(),
            cheap.h.to_bits(),
            cheap.sigma_lookback_s.to_bits(),
            cheap.sigma_scale.to_bits(),
            cheap.taker_buys_only,
            cheap.entry_price,
            cheap.limit_at_edge,
        )
    };
    let typed = defaults("cheap_catch_updown_fair_value");
    assert_eq!(typed.len(), 10, "every key of the cheap file is defaulted: {typed:?}");
    assert_eq!(fields(CheapNp::from_params(&typed)), fields(CheapNp::from_params(&empty)));

    use crate::sport_copy_follower::SportTaker;
    let typed = defaults("sport_copy_follower");
    assert_eq!(typed.len(), 5, "every key of the sport file is defaulted: {typed:?}");
    assert_eq!(
        format!("{:?}", SportTaker::from_params(&typed)),
        format!("{:?}", SportTaker::from_params(&empty))
    );

    assert!(header_of("rotation_top_k").params.is_empty(), "the rotation reads no key");
}
