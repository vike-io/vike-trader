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
//! [`vike_model::VENUES`]. A venue scaffolded by `just new-venue` lands in [`VENUES_WITHOUT_FIELDS`] with a
//! tagged reason the scaffold gate refuses until a human writes one — or moves the venue into
//! [`VENUE_FIELDS`] by declaring its first field (and deleting its row there).
//!
//! # Spelling: one type, two spellings
//!
//! A field name has two spellings: the lower-case one an operator types (`socks_proxy`) and the
//! upper-case one the `venue_setting` table stores (`SOCKS_PROXY`). [`VenueFieldName`] holds the name
//! ONCE and gives both ([`VenueFieldName::operator_spelling`], [`VenueFieldName::store_spelling`]), and
//! every venue-setting read, write and key in `vike_secrets::venue_setting` takes it, so no caller
//! converts case by hand (a probe copy that forgot to once left a secret proxy password in every
//! copy). A declared name is one of the constants below, which the [`VENUE_FIELDS`] rows carry; any
//! other spelling, typed or stored, in any case, becomes a name through [`VenueFieldName::normalised`].
//! No field may be spelled as an account tier (`paper`/`demo`/`live`, the pre-rename `sim`) or as
//! `any`: the key grammar tells a tier segment from a field by vocabulary.

use std::borrow::Cow;

/// **A venue setting field's NAME, in its one canonical spelling**: the lower-case word an operator
/// types (`socks_proxy`). See the module doc's "Spelling". Two names are equal exactly when their
/// spellings are equal in any case.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VenueFieldName(Cow<'static, str>);

impl VenueFieldName {
    /// A declared name, checked at compile time to be lower-case `[a-z0-9_]`: the constants below are
    /// the only callers.
    const fn declared(name: &'static str) -> VenueFieldName {
        let bytes = name.as_bytes();
        assert!(!bytes.is_empty(), "a declared venue field name is not empty");
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            assert!(
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_',
                "a declared venue field name is lower-case [a-z0-9_]"
            );
            i += 1;
        }
        VenueFieldName(Cow::Borrowed(name))
    }

    /// The name `spelling` spells in any case: what an operator typed (`venue.polymarket.SOCKS_PROXY`)
    /// or a store holds (`SOCKS_PROXY`, or a hand-typed row's `socks_proxy`). It checks nothing else: a
    /// name no field declares is still a name ([`venue_field`] answers `None` for it).
    #[must_use]
    pub fn normalised(spelling: &str) -> VenueFieldName {
        VenueFieldName(Cow::Owned(spelling.to_ascii_lowercase()))
    }

    /// The spelling an operator types and every dotted key carries: `socks_proxy`.
    #[must_use]
    pub fn operator_spelling(&self) -> &str {
        &self.0
    }

    /// The spelling every writer stores in the `venue_setting.field` column: `SOCKS_PROXY`.
    #[must_use]
    pub fn store_spelling(&self) -> String {
        self.0.to_ascii_uppercase()
    }
}

/// `mark_streams` (aster, binance, bybit, hyperliquid, okx).
pub const MARK_STREAMS: VenueFieldName = VenueFieldName::declared("mark_streams");
/// `trade_lite_fill` (binance).
pub const TRADE_LITE_FILL: VenueFieldName = VenueFieldName::declared("trade_lite_fill");
/// `fast_exec` (bybit).
pub const FAST_EXEC: VenueFieldName = VenueFieldName::declared("fast_exec");
/// `backend` (ibkr, per tier).
pub const BACKEND: VenueFieldName = VenueFieldName::declared("backend");
/// `extended_hours` (ibkr, per tier).
pub const EXTENDED_HOURS: VenueFieldName = VenueFieldName::declared("extended_hours");
/// `proxy_enabled` (polymarket).
pub const PROXY_ENABLED: VenueFieldName = VenueFieldName::declared("proxy_enabled");
/// `proxy_host` (polymarket).
pub const PROXY_HOST: VenueFieldName = VenueFieldName::declared("proxy_host");
/// `proxy_port` (polymarket).
pub const PROXY_PORT: VenueFieldName = VenueFieldName::declared("proxy_port");
/// `tunnel_host` (polymarket).
pub const TUNNEL_HOST: VenueFieldName = VenueFieldName::declared("tunnel_host");
/// `tunnel_port` (polymarket).
pub const TUNNEL_PORT: VenueFieldName = VenueFieldName::declared("tunnel_port");
/// `tunnel_user` (polymarket).
pub const TUNNEL_USER: VenueFieldName = VenueFieldName::declared("tunnel_user");
/// `tunnel_host_key` (polymarket).
pub const TUNNEL_HOST_KEY: VenueFieldName = VenueFieldName::declared("tunnel_host_key");
/// `tunnel_key_file` (polymarket).
pub const TUNNEL_KEY_FILE: VenueFieldName = VenueFieldName::declared("tunnel_key_file");
/// `socks_proxy` (polymarket, a secret).
pub const SOCKS_PROXY: VenueFieldName = VenueFieldName::declared("socks_proxy");
/// `ws_proxy_enabled` (polymarket).
pub const WS_PROXY_ENABLED: VenueFieldName = VenueFieldName::declared("ws_proxy_enabled");
/// `rate_gate` (polymarket).
pub const RATE_GATE: VenueFieldName = VenueFieldName::declared("rate_gate");
/// `exec_markets` (polymarket).
pub const EXEC_MARKETS: VenueFieldName = VenueFieldName::declared("exec_markets");
/// `presubmit_register` (polymarket).
pub const PRESUBMIT_REGISTER: VenueFieldName = VenueFieldName::declared("presubmit_register");
/// `ws_tokens_per_socket` (polymarket).
pub const WS_TOKENS_PER_SOCKET: VenueFieldName = VenueFieldName::declared("ws_tokens_per_socket");

/// Polymarket's egress proxy URL from its rows (`get` answers one field), or `None` for a direct
/// connection: the ONE precedence the bridge dials by (`crates/bridges/polymarket/src/transport/mod.rs`'s
/// `proxy_url`) and the desktop's proxy box shows. A [`SOCKS_PROXY`] row wins (`none`/`direct`/blank:
/// direct); otherwise the tunnel is OFF unless [`PROXY_ENABLED`] reads ON by its declared switch grammar
/// ([`VenueFieldGrammar::switch_value`], the one word list; an illegal spelling is off) (owner ruling
/// 2026-11-22), and then it is `socks5h://<`[`PROXY_HOST`]` or 127.0.0.1>:<`[`PROXY_PORT`]` or 1080>`.
pub fn polymarket_proxy_url(get: impl Fn(&VenueFieldName) -> Option<String>) -> Option<String> {
    if let Some(u) = get(&SOCKS_PROXY) {
        let u = u.trim();
        if u.is_empty() || u.eq_ignore_ascii_case("none") || u.eq_ignore_ascii_case("direct") {
            return None;
        }
        return Some(u.to_string());
    }
    let enabled = venue_field("polymarket", &PROXY_ENABLED)
        .zip(get(&PROXY_ENABLED))
        .and_then(|(declared, v)| declared.grammar.switch_value(&v))
        == Some(true);
    if !enabled {
        return None;
    }
    let host =
        get(&PROXY_HOST).filter(|s| !s.is_empty()).unwrap_or_else(|| "127.0.0.1".to_string());
    let port = get(&PROXY_PORT).filter(|s| !s.is_empty()).unwrap_or_else(|| "1080".to_string());
    Some(format!("socks5h://{host}:{port}"))
}

/// How a field's VALUE is checked when it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueFieldGrammar {
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

impl VenueFieldGrammar {
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
            VenueFieldGrammar::ExactOne | VenueFieldGrammar::Bool => self.switch_value(v).is_some(),
            VenueFieldGrammar::UInt => v.bytes().all(|b| b.is_ascii_digit()),
            VenueFieldGrammar::Text | VenueFieldGrammar::List => true,
            VenueFieldGrammar::Url => {
                v.contains("://")
                    || v.eq_ignore_ascii_case("none")
                    || v.eq_ignore_ascii_case("direct")
            }
        };
        if ok { Ok(()) } else { Err(format!("expected {}", self.expects())) }
    }

    /// A SWITCH grammar's value as the bool it states, trimmed: [`VenueFieldGrammar::ExactOne`] takes
    /// exactly `1`/`0`; [`VenueFieldGrammar::Bool`] takes `true`/`1`/`yes`/`on` and `false`/`0`/`no`/`off`,
    /// any case. `None` for any other value, and for every grammar that is not a switch. The ONE list of
    /// those words: [`VenueFieldGrammar::check`] accepts exactly what this reads.
    #[must_use]
    pub fn switch_value(self, value: &str) -> Option<bool> {
        let v = value.trim();
        match self {
            VenueFieldGrammar::ExactOne => match v {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            },
            VenueFieldGrammar::Bool => match v.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            },
            VenueFieldGrammar::UInt
            | VenueFieldGrammar::Text
            | VenueFieldGrammar::Url
            | VenueFieldGrammar::List => None,
        }
    }

    /// What a legal value looks like, in an operator's words.
    #[must_use]
    pub fn expects(self) -> &'static str {
        match self {
            VenueFieldGrammar::ExactOne => "`1` (on) or `0` (off)",
            VenueFieldGrammar::Bool => "`true` or `false`",
            VenueFieldGrammar::UInt => "a whole number",
            VenueFieldGrammar::Text => "one line of text",
            VenueFieldGrammar::Url => "a URL such as `socks5h://host:port`, or `none`/`direct`",
            VenueFieldGrammar::List => "a comma- or space-separated list",
        }
    }
}

/// One settable field of one venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueField {
    /// A [`vike_model::VENUES`] id.
    pub venue: &'static str,
    /// The field's name: one of this module's constants.
    pub field: VenueFieldName,
    /// `true`: one value per account tier (`venue.<venue>.<tier>.<field>`); `false`: one value for the
    /// machine (`venue.<venue>.<field>`, stored under the tier word `any`).
    pub tier_scoped: bool,
    /// The value is a secret: `config set` takes it on stdin (`-`) only, and nothing prints it.
    pub secret: bool,
    /// How a written value is checked.
    pub grammar: VenueFieldGrammar,
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
        field: MARK_STREAMS,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "0",
        doc: "`1` pairs every perp bar subscription with Aster's `@markPrice@1s` stream. Off by \
              default: that frame shape is assumed from the binance family and has not been \
              observed on Aster's own feed.",
    },
    VenueField {
        venue: "binance",
        field: MARK_STREAMS,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "1",
        doc: "`1` pairs every perp bar subscription with Binance's `@markPrice@1s` stream, the \
              valuation's mark. `0` turns it off: a perp is then valued at the venue mark a fill \
              or a reconcile pass carries, else the live bid (long) or ask (short), else the last \
              trade, and the bar close only when none of those is there.",
    },
    VenueField {
        venue: "binance",
        field: TRADE_LITE_FILL,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "0",
        doc: "`1` turns the USDⓈ-M perp user stream's TRADE_LITE frame into an EARLY fill, so \
              inventory skew reacts sooner; the authoritative ORDER_TRADE_UPDATE still drives the \
              order. `0` drops those frames. ⚠ TRADE_LITE carries no fee, so the early fill books \
              a commission of 0 and the authoritative fill (with the real fee) is then dropped as \
              a duplicate: while this is `1` every fill's fee is missing from `fees_paid` and from \
              a Delta-mode balance (a venue wallet frame or a reconcile pass replaces the balance \
              later; `fees_paid` stays short).",
    },
    VenueField {
        venue: "bybit",
        field: FAST_EXEC,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "0",
        doc: "`1` subscribes Bybit's low-latency `execution.fast` topic beside `execution` on the \
              private socket, so a fill is seen early; `0` keeps the two-topic subscription. ⚠ \
              The fast row carries no fee, so the early fill books a commission of 0 and the \
              `execution` row (with the real fee) is then dropped as a duplicate: while this is \
              `1` every fill's fee is missing from `fees_paid` and from a Delta-mode balance (a \
              reconcile pass replaces the balance later; `fees_paid` stays short).",
    },
    VenueField {
        venue: "bybit",
        field: MARK_STREAMS,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "1",
        doc: "`1` pairs every perp bar subscription with Bybit's mark-price stream, the \
              valuation's mark. `0` turns it off: a perp is then valued at the venue mark a fill \
              or a reconcile pass carries, else the live bid (long) or ask (short), else the last \
              trade, and the bar close only when none of those is there.",
    },
    VenueField {
        venue: "hyperliquid",
        field: MARK_STREAMS,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "1",
        doc: "`1` pairs every perp bar subscription with Hyperliquid's `markPx` stream, the \
              valuation's mark. `0` turns it off: a perp is then valued at the venue mark a fill \
              or a reconcile pass carries, else the live bid (long) or ask (short), else the last \
              trade, and the bar close only when none of those is there.",
    },
    VenueField {
        venue: "ibkr",
        field: BACKEND,
        tier_scoped: true,
        secret: false,
        grammar: VenueFieldGrammar::Text,
        default: "socket",
        doc: "`socket` (the TWS/Gateway API) or `cpapi` (the Client Portal gateway).",
    },
    VenueField {
        venue: "ibkr",
        field: EXTENDED_HOURS,
        tier_scoped: true,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "0",
        doc: "`1` sends every US-stock LIMIT order of this tier's account with IBKR's \
              outside-regular-hours and overnight flags, so it can fill pre-market, after-hours \
              and in the overnight session (20:00-03:50 ET); market and stop orders are sent as \
              before. `0` sends no flag. Socket backend only: the `cpapi` backend refuses the \
              account's start while this is `1`. Read when the account starts.",
    },
    VenueField {
        venue: "okx",
        field: MARK_STREAMS,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "1",
        doc: "`1` pairs every `-SWAP` bar subscription with OKX's mark-price stream, the \
              valuation's mark. `0` turns it off: a swap is then valued at the venue mark a fill \
              or a reconcile pass carries, else the live bid (long) or ask (short), else the last \
              trade, and the bar close only when none of those is there.",
    },
    VenueField {
        venue: "polymarket",
        field: PROXY_ENABLED,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::Bool,
        default: "true",
        doc: "Route every Polymarket connection through the SOCKS tunnel; `false` connects direct.",
    },
    VenueField {
        venue: "polymarket",
        field: PROXY_HOST,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::Text,
        default: "127.0.0.1",
        doc: "The SOCKS tunnel host.",
    },
    VenueField {
        venue: "polymarket",
        field: PROXY_PORT,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::UInt,
        default: "1080",
        doc: "The SOCKS tunnel port.",
    },
    // The tunnel ITSELF, run beside the daemons by `deploy/docker/vike-sidecars` (`ssh -N -D
    // 127.0.0.1:1080`, decision 0260): where it dials. None is a secret; the private key stays in its file.
    VenueField {
        venue: "polymarket",
        field: TUNNEL_HOST,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::Text,
        default: "",
        doc: "The SSH host the box's SOCKS tunnel dials, an address or a name. No row, or `none`: this \
              box runs no tunnel.",
    },
    VenueField {
        venue: "polymarket",
        field: TUNNEL_PORT,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::UInt,
        default: "22",
        doc: "The SSH port of `tunnel_host`.",
    },
    VenueField {
        venue: "polymarket",
        field: TUNNEL_USER,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::Text,
        default: "",
        doc: "The user the tunnel logs in as on `tunnel_host`.",
    },
    VenueField {
        venue: "polymarket",
        field: TUNNEL_HOST_KEY,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::Text,
        default: "",
        doc: "The ONE host key the tunnel accepts for `tunnel_host`, as `<type> <base64>` (the last two \
              words of its known_hosts line); any other key refuses the connection.",
    },
    VenueField {
        venue: "polymarket",
        field: TUNNEL_KEY_FILE,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::Text,
        default: "",
        doc: "The path of the private key file the tunnel logs in with; the key stays in that file.",
    },
    VenueField {
        venue: "polymarket",
        field: SOCKS_PROXY,
        tier_scoped: false,
        secret: true,
        grammar: VenueFieldGrammar::Url,
        default: "",
        doc: "A whole proxy URL, which may carry `user:password@`; it overrides host and port, and \
              `none`/`direct` connects direct. Written from stdin (`-`) and never printed.",
    },
    VenueField {
        venue: "polymarket",
        field: WS_PROXY_ENABLED,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::Bool,
        default: "",
        doc: "Override for the WebSocket lanes only: `false` sends them direct while REST stays on \
              the tunnel. Empty: they follow `proxy_enabled`.",
    },
    VenueField {
        venue: "polymarket",
        field: RATE_GATE,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "0",
        doc: "`1`: the local rate-budget mirror REFUSES a submit that would overdraw the venue's \
              order bucket (the order is rejected locally); `0` only counts and logs it.",
    },
    VenueField {
        venue: "polymarket",
        field: EXEC_MARKETS,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::List,
        default: "",
        doc: "The CLOB market CONDITION ids (not outcome token ids) the user channel subscribes to. \
              Empty: every market the account trades.",
    },
    VenueField {
        venue: "polymarket",
        field: PRESUBMIT_REGISTER,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::ExactOne,
        default: "0",
        doc: "`1`: register each order's derived id before it is submitted, so a fill that beats \
              the HTTP ack is matched at once instead of being staged.",
    },
    VenueField {
        venue: "polymarket",
        field: WS_TOKENS_PER_SOCKET,
        tier_scoped: false,
        secret: false,
        grammar: VenueFieldGrammar::UInt,
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
    (
        "dukascopy",
        "its JNLP and history feed are `venue_url` address rows and every other value it reads is \
         a credential",
    ),
    (
        "fxcm",
        "its Hosts.jsp login (the connection is that row's product) and candle archive are \
         `venue_url` address rows and every other value it reads is a credential",
    ),
    // vike:new-venue:row // TODO(new-venue: {venue}): does this venue read a value an operator should set? Declare each
    // vike:new-venue:row // in VENUE_FIELDS (plus a FIELD_READERS row in crates/vike-ops/tests/settings_secrets/venue_fields_gate.rs) and
    // vike:new-venue:row // delete this row; a venue with none keeps this row with its own reason (>30 chars).
    // vike:new-venue:row ("{venue}", "a freshly scaffolded bridge reads no venue setting at all"),
];

/// The declared field `field` of `venue`, or `None`.
#[must_use]
pub fn venue_field(venue: &str, field: &VenueFieldName) -> Option<&'static VenueField> {
    VENUE_FIELDS.iter().find(|f| f.venue == venue && f.field == *field)
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
        for v in vike_model::VENUES {
            assert!(
                with.contains(v) ^ without.contains(v),
                "{v}: declare its fields in VENUE_FIELDS, or give it one VENUES_WITHOUT_FIELDS row \
                 with its reason — exactly one of the two"
            );
        }
        for v in with.iter().chain(without.iter()) {
            assert!(vike_model::VENUES.contains(v), "{v} is not a roster venue");
        }
    }

    #[test]
    fn every_field_is_spelled_the_way_config_set_parses_it() {
        let mut seen = BTreeSet::new();
        for f in VENUE_FIELDS {
            let field = f.field.operator_spelling();
            let at = format!("{}.{field}", f.venue);
            assert!(
                !field.is_empty()
                    && field
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{at}: fields are lower-case [a-z0-9_]"
            );
            assert!(
                !matches!(field, "paper" | "demo" | "live" | "sim" | "any"),
                "{at}: a field spelled as a tier word is unreachable through the key grammar"
            );
            assert!(seen.insert((f.venue, field)), "{at} is declared twice");
            assert!(f.doc.len() > 20, "{at}: the doc must say what the field does");
        }
    }

    #[test]
    fn every_non_empty_default_is_a_legal_value() {
        for f in VENUE_FIELDS.iter().filter(|f| !f.default.is_empty()) {
            f.grammar
                .check(f.default)
                .unwrap_or_else(|e| panic!("{}.{}: {e}", f.venue, f.field.operator_spelling()));
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
        use VenueFieldGrammar::*;
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

    /// A switch reads exactly what its grammar accepts at the write: the `1 x` an annotated row is, a
    /// truthy word under `ExactOne` and a non-switch grammar read as no value, never as on.
    #[test]
    fn a_switch_reads_exactly_the_words_its_check_accepts() {
        use VenueFieldGrammar::*;
        assert_eq!(
            (ExactOne.switch_value(" 1 "), ExactOne.switch_value("0")),
            (Some(true), Some(false))
        );
        for not_one in ["true", "yes", "on", "1 x", "11", ""] {
            assert_eq!(ExactOne.switch_value(not_one), None, "{not_one:?}");
        }
        for on in ["true", "TRUE", "1", "yes", "On"] {
            assert_eq!(Bool.switch_value(on), Some(true), "{on:?}");
        }
        for off in ["false", "0", "No", "off"] {
            assert_eq!(Bool.switch_value(off), Some(false), "{off:?}");
        }
        assert_eq!(Bool.switch_value("maybe"), None);
        for g in [UInt, Text, Url, List] {
            assert_eq!(g.switch_value("1"), None, "{g:?} is not a switch");
        }
        for (g, value) in [(ExactOne, "1"), (ExactOne, "true"), (Bool, "on"), (Bool, "maybe")] {
            assert_eq!(g.check(value).is_ok(), g.switch_value(value).is_some(), "{g:?} {value:?}");
        }
    }

    /// B5.02: `proxy_enabled` reads through its declared grammar, and that is exactly the word list it kept
    /// by hand before (`true`/`1`/`yes`/`on`, trimmed, any case, is on; anything else is off).
    #[test]
    fn the_polymarket_proxy_switch_reads_its_declared_grammar_as_the_old_word_list_did() {
        let old_rule =
            |v: &str| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on");
        for value in [
            "true", "TRUE", " 1 ", "1", "yes", "On", "false", "0", "no", "off", "", "maybe", "1 x",
            "11",
        ] {
            let url = polymarket_proxy_url(|f| (*f == PROXY_ENABLED).then(|| value.to_string()));
            assert_eq!(url.is_some(), old_rule(value), "{value:?}");
        }
        assert_eq!(
            polymarket_proxy_url(|f| (*f == PROXY_ENABLED).then(|| "on".to_string())).as_deref(),
            Some("socks5h://127.0.0.1:1080")
        );
    }

    /// Review Focus 5 at its root: a refusal names the grammar, never the value.
    #[test]
    fn a_grammar_error_never_quotes_the_value() {
        let secret = "socks5h-user:hunter2@1.2.3.4";
        for g in [
            VenueFieldGrammar::ExactOne,
            VenueFieldGrammar::Bool,
            VenueFieldGrammar::UInt,
            VenueFieldGrammar::Url,
        ] {
            let e = g.check(secret).expect_err("not a legal value for any of these grammars");
            assert!(!e.contains("hunter2"), "{g:?}: {e}");
        }
    }

    #[test]
    fn a_lookup_finds_exactly_the_declared_field() {
        assert_eq!(venue_field("polymarket", &PROXY_HOST).map(|f| f.default), Some("127.0.0.1"));
        assert!(venue_field("binance", &PROXY_HOST).is_none(), "another venue's field");
        // `backend` and `extended_hours`, the one order-flag switch. The gateway's ADDRESSES are
        // `venue_url` rows (`crate::venue_urls`), not fields.
        assert_eq!(fields_of("ibkr").count(), 2);
        assert!(venue_field("ibkr", &VenueFieldName::normalised("port")).is_none());
    }

    /// The class of bug the type exists to remove: a name spelled in any case is ONE name, the
    /// operator's spelling is lower-case and the store's upper-case, and both come from it.
    #[test]
    fn a_name_in_any_case_is_one_name_with_two_spellings() {
        for typed in ["socks_proxy", "SOCKS_PROXY", "Socks_Proxy", "sOcKs_pRoXy"] {
            let name = VenueFieldName::normalised(typed);
            assert_eq!(name, SOCKS_PROXY, "{typed}");
            assert_eq!(name.operator_spelling(), "socks_proxy", "{typed}");
            assert_eq!(name.store_spelling(), "SOCKS_PROXY", "{typed}");
            assert_eq!(
                venue_field("polymarket", &name).map(|f| f.secret),
                Some(true),
                "{typed}: the declared secret field, whatever the case"
            );
        }
    }

    /// `venue.ibkr.<tier>.extended_hours` is an exact-`1` switch per tier, OFF unless written: the
    /// bridge's loader (`vike_ibkr::config::load_ibkr_config_from`) reads it at the tier of the account.
    #[test]
    fn the_ibkr_extended_hours_switch_is_a_per_tier_off_by_default_switch() {
        let f = venue_field("ibkr", &EXTENDED_HOURS).expect("declared");
        assert!(f.tier_scoped && !f.secret);
        assert_eq!(f.grammar, VenueFieldGrammar::ExactOne);
        assert_eq!(f.default, "0");
    }

    /// Decision 0095: the `VIKE_` venue toggles are declared fields — exact-`1` switches set once
    /// for the machine — and each mark-stream venue carries its own charter default. The withdraw
    /// override and HIP-3 are NOT fields: they stay `flags.*` rows.
    #[test]
    fn the_venue_toggles_are_declared_switches() {
        for (venue, field, default) in [
            ("aster", MARK_STREAMS, "0"),
            ("binance", MARK_STREAMS, "1"),
            ("binance", TRADE_LITE_FILL, "0"),
            ("bybit", FAST_EXEC, "0"),
            ("bybit", MARK_STREAMS, "1"),
            ("hyperliquid", MARK_STREAMS, "1"),
            ("okx", MARK_STREAMS, "1"),
        ] {
            let at = format!("{venue}.{}", field.operator_spelling());
            let f = venue_field(venue, &field).unwrap_or_else(|| panic!("{at} is not declared"));
            assert_eq!(f.grammar, VenueFieldGrammar::ExactOne, "{at}");
            assert!(!f.tier_scoped, "{at} is one value for the machine");
            assert!(!f.secret, "{at}");
            assert_eq!(f.default, default, "{at}");
        }
        assert!(
            venue_field("binance", &VenueFieldName::normalised("allow_withdraw_keys")).is_none(),
            "a flags row, not a field"
        );
        assert!(
            venue_field("hyperliquid", &VenueFieldName::normalised("hip3")).is_none(),
            "a flags row, not a field"
        );
        for v in ["aster", "binance", "bybit", "hyperliquid", "okx"] {
            assert!(!VENUES_WITHOUT_FIELDS.iter().any(|(w, _)| *w == v), "{v} declares fields now");
        }
    }
}
