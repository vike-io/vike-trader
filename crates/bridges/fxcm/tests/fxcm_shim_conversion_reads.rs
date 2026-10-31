//! **The shim's two conversion reads (`fc_system_properties`, `fc_offers`) are what the Rust side parses**,
//! gated on a box with no ForexConnect SDK, like `fxcm_shim_envelope_grammar.rs` and
//! `fxcm_shim_table_reads.rs`: no CI runner has ever compiled `crates/bridges/fxcm/src/shim/fcshim.cpp`, so a
//! rename of a key in its `snprintf` literal, or a read that quietly went back to the login-time snapshot,
//! would change nothing any other gate can see.
//!
//! Decision 0183 (a symbol's money is booked in the account's currency) needs two facts the shim did not
//! export: the account's currency (the system property `BASE_CRNCY`; the Accounts table has no currency
//! column) and a CURRENT Offers table (the login-time one froze at login: a quote that never moved). These
//! checks pin the SHAPE of both. They cannot prove what a demo login returns; `tests/fxcm_currency_smoke.rs`
//! (`#[ignore]`, read-only) is that proof, and until it has run the figures are docs-shaped.

use std::collections::BTreeSet;
use std::path::PathBuf;

use vike_fxcm::home_conversion::{OFFER_KEYS, OFFER_TERMS_KEYS};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `fcshim.cpp` with `//` and `/* */` comments removed (the file's prose names calls its code must not
/// make). String literals are copied verbatim, so a `//` inside one does not eat the rest of its line.
fn shim_code() -> String {
    let path = crate_dir().join("src/shim/fcshim.cpp");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the shim source {}: {e}", path.display()));
    strip_comments(&src)
}

fn strip_comments(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < c.len() {
        match (c[i], c.get(i + 1).copied()) {
            ('/', Some('/')) => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ('"', _) => {
                out.push('"');
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' && i + 1 < c.len() {
                        out.push(c[i]);
                        i += 1;
                    }
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() {
                    out.push('"');
                    i += 1;
                }
            }
            // A CHAR literal (`'"'` in `jesc`) must not open a string.
            ('\'', _) => {
                out.push('\'');
                i += 1;
                if i < c.len() && c[i] == '\\' {
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() {
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() && c[i] == '\'' {
                    out.push('\'');
                    i += 1;
                }
            }
            (ch, _) => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// The text of the exported function `name`, from its `FC_EXPORT int name(` to `end` (a marker that follows
/// it in the file).
fn body_between(code: &str, name: &str, end: &str) -> String {
    let opener = format!("FC_EXPORT int {name}(");
    let start = code
        .find(&opener)
        .unwrap_or_else(|| panic!("fcshim.cpp no longer defines `{name}` — the roster moved"));
    let rest = &code[start + opener.len()..];
    let stop = rest
        .find(end)
        .unwrap_or_else(|| panic!("`{end}` no longer follows `{name}` in fcshim.cpp"));
    rest[..stop].to_string()
}

/// Every `\"IDENT\":` key a C++ string-literal slice writes.
fn harvest_keys(literal: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let needle = "\\\"";
    let mut i = 0;
    while let Some(off) = literal[i..].find(needle) {
        let key_start = i + off + needle.len();
        let Some(close) = literal[key_start..].find(needle) else { break };
        let key = &literal[key_start..key_start + close];
        let after = key_start + close + needle.len();
        // A KEY is followed by `:`; a VALUE (`\"%s\"`) is followed by `,` or `}`.
        if literal[after..].starts_with(':')
            && !key.is_empty()
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            out.insert(key.to_string());
        }
        i = key_start;
    }
    out
}

/// The account's currency comes from the login's SYSTEM PROPERTIES, read from the login rules, and the shim
/// parses none of them: the Rust side picks `BASE_CRNCY`, so a property this shim never heard of costs
/// nothing. A failed read is a negative return with its reason, before any JSON is built.
#[test]
fn the_system_properties_are_a_login_time_read_that_parses_nothing() {
    let code = shim_code();
    // Positive control: the finder sees the call where it is known to be.
    assert!(code.contains("FC_EXPORT int fc_system_properties("), "the export is gone");
    let body = body_between(&code, "fc_system_properties", "static double finite_or_zero");
    for needle in [
        "getLoginRules()",
        "rules->getSystemPropertiesResponse()",
        "createSystemPropertiesReader(resp)",
        "rd->getProperty(i, value)",
        "return emit_json(json, out, out_len)",
    ] {
        assert!(body.contains(needle), "fc_system_properties lost `{needle}`: {body}");
    }
    assert!(
        !body.contains("BASE_CRNCY") && !body.contains("refreshTable"),
        "fc_system_properties must dump every property and parse none, and must not send a request: {body}"
    );
    let first_build = body.find("std::string json").expect("the object is built here");
    for failure in ["return -4", "return -7"] {
        let at = body.find(failure).unwrap_or_else(|| panic!("no `{failure}` path: {body}"));
        assert!(at < first_build, "`{failure}` must come before the JSON is built: {body}");
    }
}

/// The Offers table is a REFRESH-TABLE request (never the login-time snapshot, which froze at login), a
/// failed refresh is its own code before any row is formatted, and a non-finite price is written as 0 so one
/// bad row cannot make the table unreadable (`nan` is not JSON) while the Rust side still refuses a 0 price.
#[test]
fn the_offers_read_is_a_fresh_refresh_and_a_failure_is_not_an_empty_table() {
    let code = shim_code();
    let body = body_between(&code, "fc_offers", "static bool sameStr");
    assert!(
        body.contains("refreshTable(h->session, Offers, 0, resp, err, FACTS_REFRESH_MS)"),
        "fc_offers no longer asks the venue for the Offers table, with its own bound: {body}"
    );
    // The bound is a few seconds, not the login-wait default: the door waits on this answer.
    let bound: u64 = code
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FACTS_REFRESH_MS"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .expect("fcshim.cpp no longer defines FACTS_REFRESH_MS as a number");
    assert!((1_000..=10_000).contains(&bound), "FACTS_REFRESH_MS is {bound} ms: 1000..=10000");
    assert!(
        !body.contains("getTableRefreshResponse"),
        "fc_offers must not read the login snapshot (it froze at login): {body}"
    );
    let fail = body
        .find("if (rc != 0) { cpy(out, out_len, err.c_str()); return rc; }")
        .unwrap_or_else(|| panic!("fc_offers has no refresh-failure return: {body}"));
    let first_row = body.find("snprintf(").expect("the row formatter is in the body");
    assert!(fail < first_row, "fc_offers formats rows before it checks the refresh");
    assert!(!body.contains("emit_json(\"[]\""), "a literal empty array on some path: {body}");
    for price in ["finite_or_zero(o->getBid())", "finite_or_zero(o->getAsk())"] {
        assert!(body.contains(price), "a price is written unguarded: `{price}` is gone: {body}");
    }
    assert!(
        code.contains(
            "static double finite_or_zero(double v) { return std::isfinite(v) ? v : 0.0; }"
        ),
        "finite_or_zero changed meaning"
    );
    // The lot: the SAME lookup `fc_base_unit_size` and `fc_place` make, on the account order flow targets.
    for needle in ["firstAccount(h->session)", "tsp->getBaseUnitSize(name, account)"] {
        assert!(body.contains(needle), "fc_offers lost `{needle}`: {body}");
    }
}

/// **The C++ and the Rust parser agree on the row's keys, in both directions.** The keys the `snprintf` literal
/// writes are exactly `home_conversion::OFFER_KEYS`, which `parse_offers` requires of every row (a row missing
/// one is skipped, so a renamed key would turn every instrument into "not in the Offers table").
#[test]
fn an_offers_row_carries_exactly_the_keys_the_parser_reads() {
    let code = shim_code();
    let body = body_between(&code, "fc_offers", "static bool sameStr");
    let emitted = harvest_keys(&body);
    assert!(!emitted.is_empty(), "the harvester found no keys: every comparison below is vacuous");
    let declared: BTreeSet<String> =
        OFFER_KEYS.iter().chain(OFFER_TERMS_KEYS.iter()).map(|k| (*k).to_string()).collect();
    assert_eq!(
        emitted, declared,
        "fcshim.cpp's fc_offers row and vike_fxcm::home_conversion::OFFER_KEYS disagree about the keys that \
         cross the boundary: whichever half changed, the other has to follow"
    );
}

/// **Each row carries the age of its OWN quote**, as a difference of two `DATE`s read from the SDK (the row's
/// `getTime()` and the session's `getServerTime()`), never as a time in a zone this crate would have to guess.
/// A row with no usable time writes `null` (the Rust side then refuses it as a rate), and a non-finite or absurd
/// difference is no age at all. UNVERIFIED-WITHOUT-A-SESSION: this pins the SHAPE only.
#[test]
fn an_offers_row_carries_the_age_of_its_own_quote_as_a_difference() {
    let code = shim_code();
    let start = code.find("static bool quote_age_ms(").expect("the age helper is gone");
    let helper = &code[start
        ..code[start..]
            .find("FC_EXPORT int fc_offers(")
            .map(|e| start + e)
            .expect("fc_offers follows")];
    for needle in [
        "o->isTimeValid()",
        "s->getServerTime()",
        "o->getTime()",
        "std::isfinite(now)",
        "std::isfinite(then)",
        "std::isfinite(ms)",
        "86400000.0",
    ] {
        assert!(helper.contains(needle), "quote_age_ms lost `{needle}`: {helper}");
    }
    let body = body_between(&code, "fc_offers", "static bool sameStr");
    for needle in
        ["char age[32] = \"null\";", "quote_age_ms(h->session, o, ageMs)", ",baseUnit,age,"]
    {
        let squeezed: String = body.split_whitespace().collect();
        let needle: String = needle.split_whitespace().collect();
        assert!(squeezed.contains(&needle), "fc_offers lost `{needle}`: {body}");
    }
}

/// Both reads are OPTIONAL exports bound by NAME alone (no ABI bump: no existing entry point changed
/// meaning), so a shim installed before them degrades to a named answer, never to a refusal of the venue.
#[test]
fn both_reads_are_optional_exports_and_the_abi_does_not_move() {
    let code = shim_code();
    let version: u32 = code
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FC_ABI_VERSION"))
        .and_then(|v| v.trim().parse().ok())
        .expect("fcshim.cpp no longer defines FC_ABI_VERSION as a number");
    assert_eq!(version, 2, "these reads are optional symbols; a bump would strand installed shims");

    let loader = std::fs::read_to_string(crate_dir().join("src/loader.rs"))
        .expect("read loader.rs")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for sym in ["fc_system_properties", "fc_offers"] {
        assert!(
            loader.contains(&format!("= bind_opt!(\"{sym}\", FcTable);")),
            "loader.rs must bind `{sym}` optionally, by name alone, with the table-read shape"
        );
    }
}

/// The harvester is the load-bearing half of the key comparison, so it gets a positive and a negative control.
#[test]
fn the_key_harvester_reads_keys_and_not_values() {
    let literal = r#"{\"instrument\":\"%s\",\"bid\":%.8f,\"base_unit\":%d}"#;
    assert_eq!(
        harvest_keys(literal).into_iter().collect::<Vec<_>>(),
        vec!["base_unit".to_string(), "bid".to_string(), "instrument".to_string()],
    );
    assert!(harvest_keys("no literal here").is_empty());
    let out = strip_comments("a // fc_offers getTableRefreshResponse\nb \"//x\"");
    assert!(!out.contains("getTableRefreshResponse") && out.contains("\"//x\""), "{out}");
}
