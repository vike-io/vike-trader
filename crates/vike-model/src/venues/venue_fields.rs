//! **The declared venue-settings catalog** — every field an operator may write with
//! `vike-cli config set venue.<venue>[.<tier>].<field> <value>`, one row per `(venue, field)`.
//!
//! `docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md`: venue configuration
//! lives in the settings database's `venue_setting` table and nothing that configures a venue is
//! read from the process environment. This table is that table's vocabulary. `vike-cli config set`
//! refuses a field it does not declare, so a typo is an error instead of a row nothing reads, and
//! `crates/vike-ops/tests/settings_secrets/venue_fields_gate.rs` requires every row to name the code that reads it.
//!
//! # The per-venue playbook shape
//!
//! [`VENUE_FIELDS`] is keyed `(venue, field)`, so a venue with no settable field has no row there.
//! [`VENUES_WITHOUT_FIELDS`] names those venues, each with its reason, and
//! `every_roster_venue_is_classified_exactly_once` holds the two tables complete over
//! [`crate::VENUES`]. A venue scaffolded by `just new-venue` lands in [`VENUES_WITHOUT_FIELDS`] with a
//! tagged reason the scaffold gate refuses until a human writes one — or moves the venue into
//! [`VENUE_FIELDS`] by declaring its first field (and deleting its row there).
//!
//! # Spelling
//!
//! `field` is the lower-case spelling an operator types. The `venue_setting` table stores it upper-case
//! (`vike_secrets::venue_setting::parse_venue_setting_key`), and
//! `vike_secrets::venue_setting::VenueSettings::get` accepts this spelling. No field may be spelled as
//! an account tier (`paper`/`demo`/`live`, the pre-rename `sim`) or as `any`: the key grammar tells a
//! tier segment from a field by vocabulary.

/// How a field's VALUE is checked when it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldGrammar {
    /// The switch idiom: `1` turns it on, `0` leaves it off.
    ExactOne,
    /// `true`/`false`, also spelled `1`/`0`, `yes`/`no`, `on`/`off`, any case.
    Bool,
    /// An unsigned decimal integer.
    UInt,
    /// Any non-empty, single-line text.
    Text,
    /// A URL (`<scheme>://…`), or the words `none`/`direct` where the field's doc gives them.
    Url,
    /// Tokens separated by commas and/or whitespace.
    List,
}

impl FieldGrammar {
    /// `Ok` when `value` is a legal spelling. ⚠ The `Err` names what was EXPECTED and never quotes
    /// `value`: a secret field's value must not reach an error message.
    pub fn check(self, value: &str) -> Result<(), String> {
        let v = value.trim();
        if v.is_empty() {
            return Err("an empty value (write the field's default to reset it)".to_string());
        }
        if v.contains('\n') || v.contains('\r') {
            return Err("a value that spans lines".to_string());
        }
        let ok = match self {
            FieldGrammar::ExactOne => v == "1" || v == "0",
            FieldGrammar::Bool => matches!(
                v.to_ascii_lowercase().as_str(),
                "true" | "false" | "1" | "0" | "yes" | "no" | "on" | "off"
            ),
            FieldGrammar::UInt => v.bytes().all(|b| b.is_ascii_digit()),
            FieldGrammar::Text | FieldGrammar::List => true,
            FieldGrammar::Url => {
                v.contains("://")
                    || v.eq_ignore_ascii_case("none")
                    || v.eq_ignore_ascii_case("direct")
            }
        };
        if ok { Ok(()) } else { Err(format!("expected {}", self.expects())) }
    }

    /// What a legal value looks like, in an operator's words.
    #[must_use]
    pub fn expects(self) -> &'static str {
        match self {
            FieldGrammar::ExactOne => "`1` (on) or `0` (off)",
            FieldGrammar::Bool => "`true` or `false`",
            FieldGrammar::UInt => "a whole number",
            FieldGrammar::Text => "one line of text",
            FieldGrammar::Url => "a URL such as `socks5h://host:port`, or `none`/`direct`",
            FieldGrammar::List => "a comma- or space-separated list",
        }
    }
}

/// One settable field of one venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueField {
    /// A [`crate::VENUES`] id.
    pub venue: &'static str,
    /// The lower-case `config set venue.<venue>.<field>` spelling.
    pub field: &'static str,
    /// `true`: one value per account tier (`venue.<venue>.<tier>.<field>`); `false`: one value for the
    /// machine (`venue.<venue>.<field>`, stored under the tier word `any`).
    pub tier_scoped: bool,
    /// The value is a secret: `config set` takes it on stdin (`-`) only, and nothing prints it.
    pub secret: bool,
    /// How a written value is checked.
    pub grammar: FieldGrammar,
    /// The value in force when no row exists, as an operator would type it. Empty means "unset — the
    /// doc says what the reader does".
    pub default: &'static str,
    /// What the field does, one or two sentences.
    pub doc: &'static str,
}

/// Every declared venue field. Tasks 4–6 of the settings-in-SQLite plan add rows; each new row also
/// needs a reader row in `crates/vike-ops/tests/settings_secrets/venue_fields_gate.rs`'s `FIELD_READERS`.
pub const VENUE_FIELDS: &[VenueField] = &[
    VenueField {
        venue: "aster",
        field: "mark_streams",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "0",
        doc: "`1` pairs every perp bar subscription with Aster's `@markPrice@1s` stream. Off by \
              default: that frame shape is assumed from the binance family and has not been \
              observed on Aster's own feed.",
    },
    VenueField {
        venue: "binance",
        field: "mark_streams",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "1",
        doc: "`1` pairs every perp bar subscription with Binance's `@markPrice@1s` stream, the \
              valuation's mark. `0` turns it off: a perp is then valued at the venue mark a fill \
              or a reconcile pass carries, else the live bid (long) or ask (short), else the last \
              trade, and the bar close only when none of those is there.",
    },
    VenueField {
        venue: "binance",
        field: "trade_lite_fill",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "0",
        doc: "`1` turns the USDⓈ-M perp user stream's TRADE_LITE frame into an EARLY fill, so \
              inventory skew reacts sooner; the authoritative ORDER_TRADE_UPDATE still drives the \
              order. `0` drops those frames.",
    },
    VenueField {
        venue: "bybit",
        field: "fast_exec",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "0",
        doc: "`1` subscribes Bybit's low-latency `execution.fast` topic beside `execution` on the \
              private socket, so a fill is seen early; `0` keeps the two-topic subscription.",
    },
    VenueField {
        venue: "bybit",
        field: "mark_streams",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "1",
        doc: "`1` pairs every perp bar subscription with Bybit's mark-price stream, the \
              valuation's mark. `0` turns it off: a perp is then valued at the venue mark a fill \
              or a reconcile pass carries, else the live bid (long) or ask (short), else the last \
              trade, and the bar close only when none of those is there.",
    },
    VenueField {
        venue: "dukascopy",
        field: "server",
        tier_scoped: true,
        secret: false,
        grammar: FieldGrammar::Text,
        default: "",
        doc: "The JForex platform host for this tier's accounts. Empty: the sidecar resolves its \
              built-in demo JNLP host.",
    },
    VenueField {
        venue: "fxcm",
        field: "url",
        tier_scoped: true,
        secret: false,
        grammar: FieldGrammar::Url,
        default: "",
        doc: "The ForexConnect host URL. Empty: the bridge's own DEFAULT_HOST_URL.",
    },
    VenueField {
        venue: "fxcm",
        field: "connection",
        tier_scoped: true,
        secret: false,
        grammar: FieldGrammar::Text,
        default: "",
        doc: "`Demo` or `Real`. Empty: `Demo` on the demo tier, `Real` on the live tier.",
    },
    VenueField {
        venue: "hyperliquid",
        field: "mark_streams",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "1",
        doc: "`1` pairs every perp bar subscription with Hyperliquid's `markPx` stream, the \
              valuation's mark. `0` turns it off: a perp is then valued at the venue mark a fill \
              or a reconcile pass carries, else the live bid (long) or ask (short), else the last \
              trade, and the bar close only when none of those is there.",
    },
    VenueField {
        venue: "ibkr",
        field: "backend",
        tier_scoped: true,
        secret: false,
        grammar: FieldGrammar::Text,
        default: "socket",
        doc: "`socket` (the TWS/Gateway API) or `cpapi` (the Client Portal gateway).",
    },
    VenueField {
        venue: "ibkr",
        field: "cpapi_url",
        tier_scoped: true,
        secret: false,
        grammar: FieldGrammar::Text,
        default: "https://127.0.0.1:5000",
        doc: "The Client Portal gateway's base URL, for the `cpapi` backend and the IBKR reconcile \
              client.",
    },
    VenueField {
        venue: "ibkr",
        field: "host",
        tier_scoped: true,
        secret: false,
        grammar: FieldGrammar::Text,
        default: "127.0.0.1",
        doc: "The TWS/Gateway host.",
    },
    VenueField {
        venue: "ibkr",
        field: "port",
        tier_scoped: true,
        secret: false,
        grammar: FieldGrammar::UInt,
        default: "",
        doc: "The TWS/Gateway port. Empty: 7497 (paper/demo) or 7496 (live), the TWS ports; an IB \
              Gateway needs 4002/4001 written here.",
    },
    VenueField {
        venue: "okx",
        field: "mark_streams",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "1",
        doc: "`1` pairs every `-SWAP` bar subscription with OKX's mark-price stream, the \
              valuation's mark. `0` turns it off: a swap is then valued at the venue mark a fill \
              or a reconcile pass carries, else the live bid (long) or ask (short), else the last \
              trade, and the bar close only when none of those is there.",
    },
    VenueField {
        venue: "polymarket",
        field: "proxy_enabled",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::Bool,
        default: "true",
        doc: "Route every Polymarket connection through the SOCKS tunnel; `false` connects direct.",
    },
    VenueField {
        venue: "polymarket",
        field: "proxy_host",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::Text,
        default: "127.0.0.1",
        doc: "The SOCKS tunnel host.",
    },
    VenueField {
        venue: "polymarket",
        field: "proxy_port",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::UInt,
        default: "1080",
        doc: "The SOCKS tunnel port.",
    },
    VenueField {
        venue: "polymarket",
        field: "socks_proxy",
        tier_scoped: false,
        secret: true,
        grammar: FieldGrammar::Url,
        default: "",
        doc: "A whole proxy URL, which may carry `user:password@`; it overrides host and port, and \
              `none`/`direct` connects direct. Written from stdin (`-`) and never printed.",
    },
    VenueField {
        venue: "polymarket",
        field: "ws_proxy_enabled",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::Bool,
        default: "",
        doc: "Override for the WebSocket lanes only: `false` sends them direct while REST stays on \
              the tunnel. Empty: they follow `proxy_enabled`.",
    },
    VenueField {
        venue: "polymarket",
        field: "rate_gate",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "0",
        doc: "`1`: the local rate-budget mirror REFUSES a submit that would overdraw the venue's \
              order bucket (the order is rejected locally); `0` only counts and logs it.",
    },
    VenueField {
        venue: "polymarket",
        field: "exec_markets",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::List,
        default: "",
        doc: "The CLOB market CONDITION ids (not outcome token ids) the user channel subscribes to. \
              Empty: every market the account trades.",
    },
    VenueField {
        venue: "polymarket",
        field: "presubmit_register",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::ExactOne,
        default: "0",
        doc: "`1`: register each order's derived id before it is submitted, so a fill that beats \
              the HTTP ack is matched at once instead of being staged.",
    },
    VenueField {
        venue: "polymarket",
        field: "ws_tokens_per_socket",
        tier_scoped: false,
        secret: false,
        grammar: FieldGrammar::UInt,
        default: "50",
        doc: "How many outcome tokens one market-data socket carries in the data daemon. `0` keeps \
              the default.",
    },
];

/// The roster venues with no settable field, each with its reason — see the module doc. A venue
/// that gains its first field leaves this table in the same commit.
pub const VENUES_WITHOUT_FIELDS: &[(&str, &str)] = &[
    (
        "deribit",
        "its exec is testnet-only and its market data keyless; every value it reads is a credential",
    ),
    ("oanda", "its tier is fixed at practice and every value it reads is a credential"),
    ("ig", "its tier is fixed at demo and every value it reads is a credential"),
    ("ctrader", "every value it reads is a credential or its OAuth application pair"),
    ("alpaca", "its tier is fixed at sandbox and every value it reads is a credential"),
    // vike:new-venue:row // TODO(new-venue: {venue}): does this venue read a value an operator should set? Declare each
    // vike:new-venue:row // in VENUE_FIELDS (plus a FIELD_READERS row in crates/vike-ops/tests/settings_secrets/venue_fields_gate.rs) and
    // vike:new-venue:row // delete this row; a venue with none keeps this row with its own reason (>30 chars).
    // vike:new-venue:row ("{venue}", "a freshly scaffolded bridge reads no venue setting at all"),
];

/// The declared field `field` (the lower-case spelling) of `venue`, or `None`.
#[must_use]
pub fn venue_field(venue: &str, field: &str) -> Option<&'static VenueField> {
    VENUE_FIELDS.iter().find(|f| f.venue == venue && f.field == field)
}

/// Every declared field of `venue`, in table order.
pub fn fields_of(venue: &str) -> impl Iterator<Item = &'static VenueField> + '_ {
    VENUE_FIELDS.iter().filter(move |f| f.venue == venue)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The playbook's completeness test: every roster venue is NAMED in exactly one of the two
    /// tables, and neither table names a venue off the roster.
    #[test]
    fn every_roster_venue_is_classified_exactly_once() {
        let with: BTreeSet<&str> = VENUE_FIELDS.iter().map(|f| f.venue).collect();
        let without: BTreeSet<&str> = VENUES_WITHOUT_FIELDS.iter().map(|(v, _)| *v).collect();
        assert_eq!(without.len(), VENUES_WITHOUT_FIELDS.len(), "a fieldless venue is listed twice");
        for v in crate::VENUES {
            assert!(
                with.contains(v) ^ without.contains(v),
                "{v}: declare its fields in VENUE_FIELDS, or give it one VENUES_WITHOUT_FIELDS row \
                 with its reason — exactly one of the two"
            );
        }
        for v in with.iter().chain(without.iter()) {
            assert!(crate::VENUES.contains(v), "{v} is not a roster venue");
        }
    }

    #[test]
    fn every_field_is_spelled_the_way_config_set_parses_it() {
        let mut seen = BTreeSet::new();
        for f in VENUE_FIELDS {
            let at = format!("{}.{}", f.venue, f.field);
            assert!(
                !f.field.is_empty()
                    && f.field
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{at}: fields are lower-case [a-z0-9_]"
            );
            assert!(
                !matches!(f.field, "paper" | "demo" | "live" | "sim" | "any"),
                "{at}: a field spelled as a tier word is unreachable through the key grammar"
            );
            assert!(seen.insert((f.venue, f.field)), "{at} is declared twice");
            assert!(f.doc.len() > 20, "{at}: the doc must say what the field does");
        }
    }

    #[test]
    fn every_non_empty_default_is_a_legal_value() {
        for f in VENUE_FIELDS.iter().filter(|f| !f.default.is_empty()) {
            f.grammar.check(f.default).unwrap_or_else(|e| panic!("{}.{}: {e}", f.venue, f.field));
        }
    }

    #[test]
    fn every_reason_for_a_fieldless_venue_is_an_argument() {
        for (v, why) in VENUES_WITHOUT_FIELDS {
            assert!(why.len() > 30, "{v}: say WHY this venue has nothing to set");
        }
    }

    #[test]
    fn the_grammars_accept_and_refuse_what_they_say() {
        use FieldGrammar::*;
        assert!(ExactOne.check("1").is_ok() && ExactOne.check("0").is_ok());
        assert!(ExactOne.check("true").is_err());
        assert!(Bool.check("False").is_ok() && Bool.check("off").is_ok());
        assert!(Bool.check("maybe").is_err());
        assert!(UInt.check("1080").is_ok());
        assert!(UInt.check("-1").is_err() && UInt.check("10.5").is_err());
        assert!(Url.check("socks5h://127.0.0.1:1080").is_ok() && Url.check("none").is_ok());
        assert!(Url.check("127.0.0.1:1080").is_err());
        assert!(List.check("0xaa, 0xbb 0xcc").is_ok());
        for g in [ExactOne, Bool, UInt, Text, Url, List] {
            assert!(g.check("  ").is_err(), "{g:?}: blank is not a value");
            assert!(g.check("a\nb").is_err(), "{g:?}: one line only");
        }
    }

    /// Review Focus 5 at its root: a refusal names the grammar, never the value.
    #[test]
    fn a_grammar_error_never_quotes_the_value() {
        let secret = "socks5h-user:hunter2@1.2.3.4";
        for g in [FieldGrammar::ExactOne, FieldGrammar::Bool, FieldGrammar::UInt, FieldGrammar::Url]
        {
            let e = g.check(secret).expect_err("not a legal value for any of these grammars");
            assert!(!e.contains("hunter2"), "{g:?}: {e}");
        }
    }

    #[test]
    fn a_lookup_finds_exactly_the_declared_field() {
        assert_eq!(venue_field("polymarket", "proxy_host").map(|f| f.default), Some("127.0.0.1"));
        assert!(venue_field("polymarket", "PROXY_HOST").is_none(), "the lower-case spelling only");
        assert!(venue_field("binance", "proxy_host").is_none());
        // backend, cpapi_url, host, port — the gateway (`cpapi_url` joined in decision 0095's Task 7).
        assert_eq!(fields_of("ibkr").count(), 4);
    }

    /// Decision 0095: the `VIKE_` venue toggles are declared fields — exact-`1` switches set once
    /// for the machine — and each mark-stream venue carries its own charter default. The withdraw
    /// override and HIP-3 are NOT fields: they stay `flags.*` rows.
    #[test]
    fn the_venue_toggles_are_declared_switches() {
        for (venue, field, default) in [
            ("aster", "mark_streams", "0"),
            ("binance", "mark_streams", "1"),
            ("binance", "trade_lite_fill", "0"),
            ("bybit", "fast_exec", "0"),
            ("bybit", "mark_streams", "1"),
            ("hyperliquid", "mark_streams", "1"),
            ("okx", "mark_streams", "1"),
        ] {
            let f = venue_field(venue, field)
                .unwrap_or_else(|| panic!("{venue}.{field} is not declared"));
            assert_eq!(f.grammar, FieldGrammar::ExactOne, "{venue}.{field}");
            assert!(!f.tier_scoped, "{venue}.{field} is one value for the machine");
            assert!(!f.secret, "{venue}.{field}");
            assert_eq!(f.default, default, "{venue}.{field}");
        }
        assert!(
            venue_field("binance", "allow_withdraw_keys").is_none(),
            "a flags row, not a field"
        );
        assert!(venue_field("hyperliquid", "hip3").is_none(), "a flags row, not a field");
        for v in ["aster", "binance", "bybit", "hyperliquid", "okx"] {
            assert!(!VENUES_WITHOUT_FIELDS.iter().any(|(w, _)| *w == v), "{v} declares fields now");
        }
    }
}
