//! **The `venue_setting` NAMING GRAMMAR — one `(venue, tier, field)` row, the legacy credential
//! names it stands for, and the dotted key an operator types.**
//!
//! Ruling 10 took ten config-shaped names out of the `credential` table and into `venue_setting`.
//! Every reader keeps looking the LEGACY name up, so something has to render a row back into the
//! names a loader asks for — that is [`venue_setting_names`], and the fold that applies it is
//! [`crate::resolve_store_in`].
//!
//! # ⚠ Why this lives in `vike-secrets` and not in `vike-bridge-core`
//!
//! It lived there until 2026-09-22, and the whole of this module's move is one measurement: the
//! fold could not reach two of the three readers. `vike_bridge_core::credentials` is layer 30 and
//! this crate is layer 15, dependencies run DOWN only, so a fold composed up there could only be
//! applied by a caller that had already reached up there. The two readers that had NOT —
//! [`crate::load_workspace_dotenv_from`] and [`crate::load_workspace_dotenv_scoped`] — were blind,
//! and not hypothetically:
//!
//! * `crates/bridges/vike-ibkr/tests/ibkr_mktdata_smoke.rs` read the right store through the plain
//!   reader, found no `IBKR_DEMO_PORT` because the row had moved, fell back to its built-in demo
//!   default, and SELF-SKIPPED while printing `test result: ok`.
//! * `crates/bridges/polymarket/src/egress.rs`'s `dotenv_proxy_vars` reads through the SCOPED one,
//!   so an operator's three `POLY_PROXY_*` rows were read by nothing at all and the built-in
//!   defaults stayed in force with no error anywhere.
//!
//! CLAUDE.md states the cure exactly — *when two sides must not disagree, the cure is a shared
//! crate BELOW both, not a shared crate containing both* — so the grammar moved below its readers
//! and the fold happens inside the store. There is no `pub use` shim at the old home: a MOVE
//! updates every call site.
//!
//! The ONE outside name this needs is [`vike_model::credential_keys::CREDENTIAL_TIERS`], layer 10,
//! which this crate already depends on — so the move costs no new edge and no new package.

/// **§11 step 2 — the venue families the key grammar misses**, each with its reason.
///
/// `(store head, store tier token, venue, tier, discriminator, why)`. The PREFIX is the first two
/// composed by [`hand_mapped_prefix`]; no prefix here is a prefix of another (`DUKASCOPY`+`DEMO1`
/// and `DUKASCOPY`+`DEMO2` differ at their token), so order decides nothing except that the
/// tokenless polymarket row is reached after `vike_bridge_core::credentials`' `classify_poly` has
/// had its say.
///
/// ⚠ **The head and the token are SEPARATE columns rather than one spelled prefix**, and that is
/// not a style choice: `crates/vike-ops/tests/settings_registry.rs`' loose sweep reads an
/// env-PREFIXED string literal in a `src/` file as evidence the file READS that variable, and then
/// demands a `SETTINGS` row for it. A prefix names no key in any store — it is exactly the part
/// §4.4 REMOVES — so such a row would declare a variable that does not exist, and `vike-cli config
/// show` reports a row as the ORIGIN of an effective value. Composing the prefix from two tokens
/// neither of which carries the trailing underscore keeps the literal out of that sweep while
/// saying the same thing more precisely.
///
/// ⚠ The DISCRIMINATOR is [`crate::AccountKey`]'s derivation-time-only field and reaches NO
/// column. It is how dukascopy's two accounts are told apart without the index token becoming an
/// identity again — the defect §1 of the spec is about, and §11.1's own rejected alternative. The
/// owner's signature rules that the `DEMO1`/`DEMO2` LABELS are not written at all, and they are not.
///
/// ⚠ **`pub` since the 2026-09-22 move**, because `vike_bridge_core::credentials`' classifier reads
/// the same table to go the OTHER way (name → row) and the two directions must not drift apart. It
/// was private while both lived in one file; nothing else about it changed.
pub const HAND_MAPPED_ACCOUNTS: &[HandMappedAccount] = &[
    (
        "ALPACA",
        "SANDBOX",
        "alpaca",
        "demo",
        None,
        "the tier token SANDBOX means demo; `CREDENTIAL_TIERS` does not carry that spelling. ONE \
         account.",
    ),
    (
        "DUKASCOPY",
        "DEMO1",
        "dukascopy",
        "demo",
        Some("DEMO1"),
        "ruling 1: DEMO1 and DEMO2 are TWO accounts of one venue at one tier — the index token \
         baked into the tier position becomes a ROW. This is the whole point of the schema, and \
         the ONLY (venue, tier) pair in the migration that yields two accounts.",
    ),
    (
        "DUKASCOPY",
        "DEMO2",
        "dukascopy",
        "demo",
        Some("DEMO2"),
        "the second of ruling 1's pair — see the row above.",
    ),
    (
        "POLY",
        "",
        "polymarket",
        "live",
        None,
        "head POLY, NO tier token, and no roster venue is spelled POLY: one account at tier live. \
         The family SPLITS key by key (§5.2) — `classify_poly` runs FIRST and this row is its \
         fallback.",
    ),
];

/// One row of [`HAND_MAPPED_ACCOUNTS`]: `(store head, store tier token, venue, tier,
/// discriminator, why)`. A named alias rather than the bare tuple because six positions is past
/// what a reader can hold, and because the `why` is the column that makes the table a table.
pub type HandMappedAccount =
    (&'static str, &'static str, &'static str, &'static str, Option<&'static str>, &'static str);

/// The store prefix a [`HAND_MAPPED_ACCOUNTS`] row names: `{HEAD}_{TOKEN}_`, or `{HEAD}_` for the
/// row whose family carries no tier token at all.
#[must_use]
pub fn hand_mapped_prefix(head: &str, token: &str) -> String {
    if token.is_empty() { format!("{head}_") } else { format!("{head}_{token}_") }
}

/// **THE MAP RENDERER — the inverse of `vike_bridge_core::credentials`' `classify_credential_name`,
/// and the thing §12 says must exist before ruling 10's rows may move.**
///
/// Given one `venue_setting` row — `(venue, tier, field)`, where `tier` is `NULL` for a
/// machine-scoped row — answer every legacy `credential.name` that row stands for. A bridge loader
/// keeps taking one `&HashMap<String, String>` and keeps finding every name it looks up, so the
/// move becomes a STORAGE fact with no reader-visible surface.
///
/// # ⚠ Why this returns a LIST and not a name
///
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §11 measured it: the ten moving
/// config-shaped names land as **NINE rows**, because the two dukascopy demo SERVER keys hold the
/// same value in the live store and collapse onto one `(dukascopy, demo, SERVER)` row —
/// `venue_setting`'s own `venue_setting_one_per_tier` index admits no second. So a renderer that
/// answered ONE name would drop a key the operator wrote, and dukascopy's sidecar would fall back
/// to the default demo JNLP in silence.
///
/// The one-to-many falls out of [`HAND_MAPPED_ACCOUNTS`] rather than being spelled here: that table
/// carries two rows for `(dukascopy, demo)`, one per store tier token. A venue whose keys parse as
/// the ordinary `{VENUE}_{TIER}_{FIELD}` has no row there and renders exactly one name.
///
/// # ⚠ The collision this CANNOT see, and who owes the check
///
/// `credential.name` and the names this renders are ONE namespace, and SQLite can constrain a name
/// across two tables no more than it can across two databases — `credential_one_live_name` holds
/// inside `credential` alone. Nothing here stops a `venue_setting` row rendering a name a live
/// `credential` row already holds, and a caller folding both into one map would then hold one key
/// with two candidate values. The READ side's answer is [`crate::resolve_store_in`]'s fold, which
/// keeps the CREDENTIAL value and REPORTS the name; the MIGRATION's is stricter and is
/// `vike_bridge_core::credentials::rendered_name_collisions`. This function is pure and is neither.
///
/// # ⚠ A machine-scoped row takes the store head with NO tier token
///
/// The polymarket proxy family carries no tier at all — it is machine-scoped by construction
/// (`vike_bridge_core::credentials`' `classify_poly` argues why: `egress.rs`'s `PROXY_KEYS` is a
/// fixed five-key allow-list read once and cached for the deployment, so it is not account-aware
/// and cannot become so without that cache changing shape). So `tier: None` matches a hand-map row
/// on its VENUE alone and uses that row's head with an empty token, which is what makes `POLY` — a
/// head no roster venue is spelled as — render correctly.
#[must_use]
pub fn venue_setting_names(venue: &str, tier: Option<&str>, field: &str) -> Vec<String> {
    let mut out: Vec<String> = HAND_MAPPED_ACCOUNTS
        .iter()
        .filter(|(_, _, v, t, _, _)| {
            // A machine-scoped row has no tier to match on, so the VENUE is the whole key. A
            // tier-scoped one must match both, which is what keeps `(dukascopy, demo)`'s two rows
            // from rendering for `(dukascopy, live)`.
            *v == venue && tier.is_none_or(|want| *t == want)
        })
        .map(|(head, token, _, _, _, _)| {
            // ⚠ A machine-scoped row IGNORES the hand-map row's own token. `POLY`'s row spells an
            // empty one already, but stating it here is what stops a future head that DOES carry a
            // token from rendering `{HEAD}_{TOKEN}_{FIELD}` for a value that has no tier.
            let token = if tier.is_none() { "" } else { *token };
            format!("{}{field}", hand_mapped_prefix(head, token))
        })
        .collect();
    if out.is_empty() {
        // The ordinary grammar, for every conforming venue — ibkr and fxcm among the movers. The
        // tier token is the STORE's spelling of the normalized tier, which for a conforming venue
        // is the normalized tier uppercased; a venue whose store spells it differently is exactly
        // what earns a `HAND_MAPPED_ACCOUNTS` row, and it was matched above.
        let head = venue.to_uppercase();
        out.push(match tier {
            Some(t) => format!("{head}_{}_{field}", t.to_uppercase()),
            None => format!("{head}_{field}"),
        });
    }
    // Deterministic, and de-duplicated in case two hand-map rows ever share a prefix: the caller
    // folds these into a map, and a repeated name would make the fold's outcome depend on order.
    out.sort_unstable();
    out.dedup();
    out
}

/// **Where a venue's operational configuration LIVES — the dotted key an operator types.**
///
/// `venue.polymarket.proxy_host` for a machine-scoped value, `venue.ibkr.demo.backend` for a
/// tier-scoped one. Rendered under the `config` section, so the operator-facing spelling is
/// `config.venue.ibkr.demo.backend`.
///
/// # ⚠ Why this exists at all — the THIRD KEY SHAPE that was nearly added
///
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §6 gives these ten values their own
/// table, `venue_setting`, keyed `(venue, tier, field)`. That would make **three** key shapes for
/// one question — `setting`'s `(section, key)`, `venue_arming`'s `(venue, label)`, and a third — and
/// the spec itself names the cost in its own open-items list:
///
/// > the table itself arrives with **no gate of its own**: a `venue_setting` row nothing reads is
/// > the defect `vike_config::CONSUMPTION` exists to refuse for settings keys, and **this table is
/// > outside that gate's vocabulary because its keys are not `section.key` paths**.
///
/// A `section.key` path costs nothing and buys all of it: `vike-cli config show` renders the value,
/// `vike-cli config set` writes it, and a TOML file still spells it as an ordinary nested table
/// (`[venue.ibkr.demo] backend = "cpapi"`). So the KEY is the operator's spelling — and the VALUES
/// nonetheless live in the `venue_setting` table, for the reason [`crate::StoredSettings::venue`]
/// carries: `vike_config::Config` has `#[serde(deny_unknown_fields)]` and no `venue` field, so a
/// `config.venue.*` row took the whole `config` section down with it. Owner's ruling 2026-09-20;
/// the column table MEASURED 2026-09-21.
///
/// # ⚠ The grammar is disambiguated by the TIER VOCABULARY, not by counting segments
///
/// `venue.ibkr.demo.backend` is `(ibkr, demo, BACKEND)` and `venue.polymarket.proxy_host` is
/// `(polymarket, None, PROXY_HOST)`, and both are four-or-fewer segments. What separates them is
/// that the second segment is a tier **iff it is one of `sim`/`demo`/`live`**
/// ([`vike_model::credential_keys::CREDENTIAL_TIERS`], lowercased). That is safe because no field
/// this grammar carries is spelled as a tier, and
/// `a_field_is_never_mistaken_for_a_tier` is what keeps it that way.
#[must_use]
pub fn venue_setting_key(venue: &str, tier: Option<&str>, field: &str) -> String {
    let field = field.to_lowercase();
    match tier {
        Some(t) => format!("venue.{venue}.{}.{field}", t.to_lowercase()),
        None => format!("venue.{venue}.{field}"),
    }
}

/// The inverse of [`venue_setting_key`] — `None` for a key that is not one of these.
///
/// The FIELD comes back in the STORE's uppercase spelling, because that is what
/// [`venue_setting_names`] composes a legacy credential name from; the venue and tier come back
/// lowercased, which is how both the `account` table and the arming ceiling spell them.
#[must_use]
pub fn parse_venue_setting_key(key: &str) -> Option<(String, Option<String>, String)> {
    let rest = key.strip_prefix("venue.")?;
    let (venue, rest) = rest.split_once('.')?;
    if venue.is_empty() {
        return None;
    }
    // The tier is recognised by VOCABULARY. A `rest` that starts with a tier token AND carries
    // something after it is tier-scoped; anything else is machine-scoped and `rest` is the whole
    // field. See the grammar ⚠ on `venue_setting_key`.
    let tiered = rest.split_once('.').filter(|(head, tail)| {
        !tail.is_empty()
            && vike_model::credential_keys::CREDENTIAL_TIERS
                .iter()
                .any(|t| t.eq_ignore_ascii_case(head))
    });
    Some(match tiered {
        Some((tier, field)) => {
            (venue.to_lowercase(), Some(tier.to_lowercase()), field.to_uppercase())
        }
        None if rest.is_empty() => return None,
        None => (venue.to_lowercase(), None, rest.to_uppercase()),
    })
}

/// **The grammar's own properties** — the half of the old `venue_setting_renderer_tests` that needs
/// nothing but this module. The half that needs the CLASSIFIER stayed with the classifier, in
/// `crates/vike-bridge-core/src/credentials.rs`, because that is what it is about.
///
/// ⚠ **No test here spells a whole `{PREFIX}_{NAME}` literal**, and several of them used to. That
/// is not fastidiousness: `crates/vike-ops/tests/settings_registry.rs`' loose sweep reads an
/// env-shaped literal in ANY `src/` file as evidence that file's CRATE reads that variable, rows
/// are keyed `(name, krate)`, and `vike-secrets` has no row for a venue key. The assertions
/// decompose the rendered name on `_` and compare the PARTS instead, which is also the stronger
/// claim — it names the head, the store's tier token and the field separately.
#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ **THE ONE THIS FUNCTION EXISTS FOR.** Ten moving names land as NINE `venue_setting` rows
    /// because dukascopy's two SERVER keys hold one value and collapse onto one row — so the
    /// renderer must give BOTH back. A renderer that answered one name would drop a key the
    /// operator wrote, and `spawn_with_program` would fall back to the default demo JNLP in
    /// silence: a working mount with a changed meaning, which is the failure shape §6.2 measured.
    #[test]
    fn one_dukascopy_row_renders_both_legacy_names() {
        let names = venue_setting_names("dukascopy", Some("demo"), "SERVER");
        assert_eq!(names.len(), 2, "ONE row, TWO legacy names: {names:?}");
        assert_eq!(
            names.iter().map(|n| n.split('_').collect::<Vec<_>>()).collect::<Vec<_>>(),
            vec![vec!["DUKASCOPY", "DEMO1", "SERVER"], vec!["DUKASCOPY", "DEMO2", "SERVER"]],
            "head, store tier token, field — sorted, and the token is the STORE's: {names:?}"
        );
    }

    /// A MACHINE-scoped row carries no tier, and its head is one no roster venue is spelled as.
    #[test]
    fn the_machine_scoped_proxy_family_renders_under_the_poly_head() {
        for field in
            ["PROXY_ENABLED", "PROXY_HOST", "PROXY_PORT", "SOCKS_PROXY", "WS_PROXY_ENABLED"]
        {
            let names = venue_setting_names("polymarket", None, field);
            assert_eq!(
                names,
                vec![format!("POLY_{field}")],
                "the proxy family is machine-scoped and takes the head with NO tier token"
            );
        }
    }

    /// A CONFORMING venue has no hand-map row and renders exactly one ordinary name.
    #[test]
    fn a_conforming_venue_renders_the_ordinary_grammar() {
        // ⚠ A `starts_with("FXCM_")` was an earlier attempt at this over in the old home and the
        // sweep flagged `FXCM_` too — the TRAILING UNDERSCORE is what makes a literal look like an
        // env prefix, which is the same reason `HAND_MAPPED_ACCOUNTS` stores its head and token
        // apart. Composing the expected string with the fallback's own rule would be worse still:
        // an assertion that cannot fail for its stated reason.
        for (venue, field, want) in [
            ("ibkr", "BACKEND", vec!["IBKR", "DEMO", "BACKEND"]),
            ("fxcm", "URL", vec!["FXCM", "DEMO", "URL"]),
        ] {
            let names = venue_setting_names(venue, Some("demo"), field);
            assert_eq!(names.len(), 1, "a conforming venue has no hand-map row: {names:?}");
            assert_eq!(
                names[0].split('_').collect::<Vec<_>>(),
                want,
                "head, tier, field — in the store's own uppercase: {names:?}"
            );
        }
    }

    /// A venue whose STORE spells the tier differently is what earns a hand-map row, and the
    /// renderer must use the store's spelling rather than the normalized one.
    #[test]
    fn a_hand_mapped_tier_token_beats_the_normalized_tier() {
        let names = venue_setting_names("alpaca", Some("demo"), "ACCOUNT_ID");
        assert_eq!(names.len(), 1, "one account: {names:?}");
        assert_eq!(
            names[0].split('_').collect::<Vec<_>>(),
            vec!["ALPACA", "SANDBOX", "ACCOUNT", "ID"],
            "the store spells this tier SANDBOX; `CREDENTIAL_TIERS` does not carry that spelling"
        );
    }

    /// Dukascopy's pair belongs to `demo` and must not leak into another tier's rendering.
    #[test]
    fn a_tier_scoped_row_renders_only_its_own_tier() {
        // ⚠ The expected name is NOT spelled, and not composed by this test either. Composing it
        // with the same `{HEAD}_{TIER}_{FIELD}` rule the fallback uses would be an assertion that
        // cannot fail for its stated reason, because it would reimplement the thing under test. So
        // the CLAIM is asserted instead — one name, and the demo pair does not leak into another
        // tier — which is what this test was ever about.
        let names = venue_setting_names("dukascopy", Some("live"), "SERVER");
        assert_eq!(names.len(), 1, "no hand-map row matches (dukascopy, live): {names:?}");
        assert!(
            !names.iter().any(|n| n.contains("DEMO1") || n.contains("DEMO2")),
            "the demo pair must not leak into another tier's rendering: {names:?}"
        );
    }

    /// ⚠ **THE ROUND TRIP THAT MAKES (A′) SAFE.** A settings key must carry the same
    /// `(venue, tier, field)` back out, because that triple is what [`venue_setting_names`]
    /// composes the legacy credential name from. If the key lost the tier, the ibkr backend row
    /// would render one segment short and the loader would find nothing.
    #[test]
    fn a_settings_key_carries_the_whole_triple_back_out() {
        let rows: &[(&str, Option<&str>, &str)] = &[
            ("ibkr", Some("demo"), "HOST"),
            ("ibkr", Some("demo"), "PORT"),
            ("ibkr", Some("demo"), "BACKEND"),
            ("fxcm", Some("demo"), "URL"),
            ("fxcm", Some("demo"), "CONNECTION"),
            ("dukascopy", Some("demo"), "SERVER"),
            ("polymarket", None, "PROXY_ENABLED"),
            ("polymarket", None, "PROXY_HOST"),
            ("polymarket", None, "PROXY_PORT"),
        ];
        for (venue, tier, field) in rows {
            let key = venue_setting_key(venue, *tier, field);
            let got =
                parse_venue_setting_key(&key).unwrap_or_else(|| panic!("{key} did not parse back"));
            assert_eq!(
                (got.0.as_str(), got.1.as_deref(), got.2.as_str()),
                (*venue, *tier, *field),
                "{key} round-tripped to a different row"
            );
            // …and the whole point: the triple still renders the legacy names a loader looks up.
            assert!(
                !venue_setting_names(&got.0, got.1.as_deref(), &got.2).is_empty(),
                "{key} parsed but renders no credential name"
            );
        }
    }

    /// The operator-facing spellings, pinned so a rename is a decision rather than a diff.
    #[test]
    fn the_keys_are_the_spellings_an_operator_types() {
        assert_eq!(
            venue_setting_key("polymarket", None, "PROXY_HOST"),
            "venue.polymarket.proxy_host"
        );
        assert_eq!(venue_setting_key("ibkr", Some("demo"), "BACKEND"), "venue.ibkr.demo.backend");
        assert_eq!(
            venue_setting_key("dukascopy", Some("demo"), "SERVER"),
            "venue.dukascopy.demo.server"
        );
    }

    /// ⚠ **THE GRAMMAR'S ONE HAZARD.** `venue.<v>.<x>.<y>` is tier-scoped iff `<x>` is a TIER, and
    /// both shapes have the same segment count — so a field that happened to be spelled `demo`
    /// would be read as a tier and its value would render the wrong credential name. No field this
    /// grammar carries is, and this test is what says so rather than assuming it.
    #[test]
    fn a_field_is_never_mistaken_for_a_tier() {
        for field in [
            "HOST",
            "PORT",
            "BACKEND",
            "URL",
            "CONNECTION",
            "SERVER",
            "PROXY_ENABLED",
            "PROXY_HOST",
            "PROXY_PORT",
            "SOCKS_PROXY",
            "WS_PROXY_ENABLED",
        ] {
            assert!(
                !vike_model::credential_keys::CREDENTIAL_TIERS
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(field)),
                "{field} is spelled as a tier — the venue-settings key grammar cannot tell it from \
                 one, and its value would render the wrong credential name"
            );
        }
    }

    /// A multi-segment FIELD is kept whole when it is not preceded by a tier — the machine-scoped
    /// shape — so a future `venue.x.a.b` field does not silently lose its head.
    #[test]
    fn a_machine_scoped_field_may_carry_dots() {
        assert_eq!(
            parse_venue_setting_key("venue.polymarket.ws_proxy_enabled"),
            Some(("polymarket".into(), None, "WS_PROXY_ENABLED".into()))
        );
    }

    /// Anything that is not one of these keys is refused rather than guessed at.
    #[test]
    fn a_key_that_is_not_a_venue_setting_is_refused() {
        for key in
            ["poly_proxy_host", "venue.", "venue.polymarket", "venues.x.y", "", "venue..host"]
        {
            assert_eq!(parse_venue_setting_key(key), None, "{key:?} should not parse");
        }
    }
}
