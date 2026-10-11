//! [`PARAM_KEYS`]: the keys each reader reads, and the two refusals over it (unknown, mistyped).

use toml::Value;

#[cfg(doc)]
use super::{PORTABLE_STRATEGIES, capability};

// Unqualified HERE only, so the ~70 [`PARAM_KEYS`] entries read as the key names they state.
use ParamType::{Bool, Integer, Number, Str, StrOrInteger, Table};

/// The TOML value types a params key's READER actually takes — declared per key beside its name in
/// [`PARAM_KEYS`], because a reader ignores a value of the wrong type exactly as silently as a key
/// it does not know.
///
/// ⚠ Every variant is named for what some reader in this crate DOES: [`Number`](ParamType::Number)
/// is the lenient `as_f64` convention (`qty = 1` and `qty = 1.0` are one knob) and
/// [`StrOrInteger`](ParamType::StrOrInteger) is `grid_dca`'s `read_side` (`side = "short"` OR
/// `side = -1`). `crates/vike-strategy/tests/param_keys_gate.rs`'s
/// `every_declared_key_type_matches_its_reader` reads each reader's accessor out of its source, in
/// both directions — so a variant here is a machine-checked claim about the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    /// `as_f64` — a TOML float OR integer.
    Number,
    /// `Value::as_integer` — a TOML integer only. A float is REFUSED: `rungs = 4.0` reads as
    /// nothing and the count silently stays at the compiled default.
    Integer,
    /// `Value::as_str`.
    Str,
    /// `Value::as_bool`.
    Bool,
    /// `Value::as_table` (the `[strategy.params.venues]` routing table).
    Table,
    /// `grid_dca::read_side` — a TOML string OR integer, each with its own meaning.
    StrOrInteger,
}

impl ParamType {
    /// The `toml::Value::type_str()` names this reader accepts — the wire vocabulary the gate
    /// compares against the accessors at the reader's own lookup sites.
    pub const fn accepted(&self) -> &'static [&'static str] {
        match self {
            ParamType::Number => &["float", "integer"],
            ParamType::Integer => &["integer"],
            ParamType::Str => &["string"],
            ParamType::Bool => &["boolean"],
            ParamType::Table => &["table"],
            ParamType::StrOrInteger => &["integer", "string"],
        }
    }

    /// Whether `v` is a value this key's reader will actually take.
    pub fn accepts(&self, v: &Value) -> bool {
        self.accepted().contains(&v.type_str())
    }

    /// How to say the requirement to an operator, in the vocabulary of the TOML they typed.
    pub const fn expected(&self) -> &'static str {
        match self {
            ParamType::Number => "a number (integer or float)",
            ParamType::Integer => "an integer",
            ParamType::Str => "a string",
            ParamType::Bool => "a boolean",
            ParamType::Table => "a table",
            ParamType::StrOrInteger => "a string or an integer",
        }
    }
}

/// What a name's `[strategy.params]` table may legally contain.
///
/// ⚠ A params READER cannot fail: `qtyy = 0.005` is not an error, it is `qty` at the compiled
/// default — on a mount, a live order at a size nobody typed, behind at most the OPTIONAL
/// `policy.max_notional_per_order` ceiling. And the KEY is only half of it: `size = "2"` is a known
/// key carrying a value `and_then(as_f64)` cannot take, which is why a
/// [`Declared`](ParamKeys::Declared) row carries a [`ParamType`] per key; [`mistyped_params`] reads
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamKeys {
    /// Exactly the keys this name's reader looks at, each with the TOML type that reader accepts. A
    /// key outside this set changes nothing, and a declared key of the wrong type changes nothing
    /// either — so a consumer that cares (a live mount) can refuse both.
    Declared(&'static [(&'static str, ParamType)]),
    /// Deliberately NOT enumerated, with the reason. [`unknown_params`] then reports nothing, and
    /// the consumer owes its own stricter rule — see the `spread_maker` row.
    NotEnumerated(&'static str),
}

/// Per-name `[strategy.params]` key declaration, exhaustive over [`PORTABLE_STRATEGIES`]
/// (`param_keys_table_is_exhaustive`) and checked against the readers' own source, both directions,
/// by `crates/vike-strategy/tests/param_keys_gate.rs`, whose `READERS` names the FILES each row's
/// keys are read in.
///
/// The [`ParamType`] beside each key is the same claim one level finer: the ACCESSOR it is looked
/// up through (`Number` for the lenient `as_f64`, `Integer` for `Value::as_integer`, …) — read off
/// the source by the gate, never chosen.
pub const PARAM_KEYS: &[(&str, ParamKeys)] = &[
    // `BuyHold::from_params` (`registry.rs`).
    ("buy_hold", ParamKeys::Declared(&[("size", Number), ("symbol", Str)])),
    // `Grid::from_params` / `DcaAccumulate::from_params` (`grid_dca.rs`, incl. `read_rungs` /
    // `read_side`). ⚠ `rungs` is `Integer`: `read_rungs` is `Value::as_integer`, so `rungs = 4.0`
    // sets nothing.
    (
        "grid",
        ParamKeys::Declared(&[
            ("anchor", Str),
            ("anchor_price", Number),
            ("step", Number),
            ("rungs", Integer),
            ("size", Number),
            ("band", Number),
            ("bounded01", Bool),
            ("tick", Number),
            ("symbol", Str),
        ]),
    ),
    (
        "dca_accumulate",
        ParamKeys::Declared(&[
            // `read_side` takes EITHER spelling — `side = "short"` or `side = -1`.
            ("side", StrOrInteger),
            ("anchor", Str),
            ("anchor_price", Number),
            ("step", Number),
            ("rungs", Integer),
            ("size", Number),
            ("tp", Number),
            ("symbol", Str),
        ]),
    ),
    // ⚠ The ~60-key A-S/GLFT bag is documented on `vike_mm::SpreadMaker::from_params`, in ANOTHER
    // crate; a hand copy here would rot. Not enumerated, and not needed: the daemon REFUSES a
    // `[strategy.params]` table under these two names (it builds the maker from the profile's own
    // maker fields), which is strictly stronger than key-checking.
    (
        "spread_maker",
        ParamKeys::NotEnumerated(
            "the ~60-key A-S/GLFT bag is documented on `vike_mm::SpreadMaker::from_params`, in \
             another crate; a hand copy here would rot. The live consumer refuses this table \
             entirely and builds the maker from the profile's own maker fields",
        ),
    ),
    (
        "gueant_maker",
        ParamKeys::NotEnumerated("the `spread_maker` bag, forced to `SpreadModel::Gueant`"),
    ),
    // `TrailingScalper::from_params` (`trailing_scalper.rs`). ⚠ The four ms gates and
    // `exit_delay_ms` go through its `i` closure (`Value::as_integer`), so
    // `exit_delay_ms = 2_000.0` sets nothing; `qty`/`half_spread`/`profit_target` take either.
    (
        "trailing_scalper",
        ParamKeys::Declared(&[
            ("qty", Number),
            ("half_spread", Number),
            ("exit_delay_ms", Integer),
            ("profit_target", Number),
            ("entry_open_delay_ms", Integer),
            ("entry_cutoff_before_close_ms", Integer),
            ("market_open_ms", Integer),
            ("market_close_ms", Integer),
        ]),
    ),
    // The two CONTROLLERS: their own `from_params` + `barriers_from_params` (`controller.rs`) + the
    // harness-level knobs `controller_harness` reads (`registry.rs`).
    (
        "momentum",
        ParamKeys::Declared(&[
            ("qty", Number),
            ("threshold", Number),
            ("tp", Number),
            ("sl", Number),
            ("time_limit_ms", Integer),
            ("trailing", Number),
            ("venue", Str),
            ("cooldown_ms", Integer),
            ("venues", Table),
        ]),
    ),
    (
        "funding_carry",
        ParamKeys::Declared(&[
            ("symbol", Str),
            ("qty", Number),
            ("tp", Number),
            ("sl", Number),
            ("time_limit_ms", Integer),
            ("trailing", Number),
            ("hold_periods", Number),
            ("entry_threshold", Number),
            ("venue", Str),
            ("cooldown_ms", Integer),
            ("venues", Table),
        ]),
    ),
    // `FundingCapture::from_params` (`funding_capture.rs`).
    (
        "funding_capture",
        ParamKeys::Declared(&[("threshold", Number), ("qty", Number), ("symbol", Str)]),
    ),
    // `PairsZScore::from_params` (`pairs.rs`). ⚠ `period` is a COUNT read through `as_f64` and
    // rounded, so `period = 2.6` is legal input resolving to `3` — visible only in the echo.
    (
        "pairs_zscore",
        ParamKeys::Declared(&[
            ("symbol_a", Str),
            ("symbol_b", Str),
            ("period", Number),
            ("entry_z", Number),
            ("exit_z", Number),
            ("beta", Number),
            ("notional", Number),
            ("taker_fee", Number),
            ("half_spread_bps", Number),
            ("hold_intervals", Number),
            ("funding_a", Number),
            ("funding_b", Number),
            ("max_half_life", Number),
        ]),
    ),
];

/// This name's [`PARAM_KEYS`] row, or `None` for a name this registry does not resolve.
pub fn param_keys(name: &str) -> Option<&'static ParamKeys> {
    PARAM_KEYS.iter().find(|(n, _)| *n == name).map(|(_, k)| k)
}

/// The keys in `params` that `name`'s reader will NOT look at — the ones that configure nothing.
/// Sorted, so an error message is stable. Empty for an unknown name ([`capability`] already
/// refuses it), for a [`ParamKeys::NotEnumerated`] row (the consumer owes its own rule) and for a
/// non-table `params`.
pub fn unknown_params(name: &str, params: &Value) -> Vec<String> {
    let Some(ParamKeys::Declared(known)) = param_keys(name) else {
        return Vec::new();
    };
    let Some(table) = params.as_table() else {
        return Vec::new();
    };
    let mut unknown: Vec<String> =
        table.keys().filter(|k| !known.iter().any(|(n, _)| n == k)).cloned().collect();
    unknown.sort();
    unknown
}

/// One declared key whose VALUE is of a type its reader cannot take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamTypeError {
    /// The `[strategy.params]` key, exactly as the operator spelled it.
    pub key: String,
    /// What the reader takes, in TOML vocabulary — [`ParamType::expected`].
    pub expected: &'static str,
    /// What the profile handed it — `toml::Value::type_str()`.
    pub got: &'static str,
}

impl std::fmt::Display for ParamTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` wants {}, got {}", self.key, self.expected, self.got)
    }
}

/// The keys in `params` whose VALUE `name`'s reader cannot take — the type twin of
/// [`unknown_params`], and the half that catches the ordinary slip.
///
/// ⚠ A wrong TYPE fails exactly as silently as a wrong KEY: every accessor here is an
/// `and_then(Value::as_…)`, `None` for another type just as for an absent value, so `size = "2"`
/// mounts `size = 1` — on a live mount, an order at a size nobody typed.
///
/// The same three abstentions as [`unknown_params`]. Keys ABSENT from the table are not judged
/// (absent is the default, which is legal); only a PRESENT key with an unusable value is reported.
/// Sorted by key, so a message is stable.
pub fn mistyped_params(name: &str, params: &Value) -> Vec<ParamTypeError> {
    let Some(ParamKeys::Declared(declared)) = param_keys(name) else {
        return Vec::new();
    };
    let Some(table) = params.as_table() else {
        return Vec::new();
    };
    let mut bad: Vec<ParamTypeError> = declared
        .iter()
        .filter_map(|(key, ty)| {
            let v = table.get(*key)?;
            (!ty.accepts(v)).then(|| ParamTypeError {
                key: (*key).to_string(),
                expected: ty.expected(),
                got: v.type_str(),
            })
        })
        .collect();
    bad.sort_by(|a, b| a.key.cmp(&b.key));
    bad
}
