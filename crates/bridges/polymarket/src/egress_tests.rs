use super::*;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

const IPINFO: &str = r#"{"ip":"52.16.1.2","hostname":"ec2-52-16-1-2.eu-west-1.compute.amazonaws.com","city":"Dublin","region":"Leinster","country":"IE","loc":"53.3331,-6.2489","org":"AS16509 Amazon.com, Inc.","timezone":"Europe/Dublin"}"#;

#[test]
fn parses_an_ipinfo_response() {
    let e = parse_egress(IPINFO, true).unwrap();
    assert_eq!(e.ip, "52.16.1.2");
    assert_eq!(e.country.as_deref(), Some("IE"));
    assert_eq!(e.city.as_deref(), Some("Dublin"));
    assert!(e.ws_lane);
}

#[test]
fn tolerates_a_probe_without_geo_fields() {
    let e = parse_egress(r#"{"ip":"203.0.113.9"}"#, false).unwrap();
    assert_eq!(e.ip, "203.0.113.9");
    assert!(e.country.is_none());
    assert!(!e.ws_lane);
}

#[test]
fn rejects_a_response_without_an_ip() {
    assert!(parse_egress(r#"{"country":"IE"}"#, false).is_err());
    assert!(parse_egress(r#"{"ip":""}"#, false).is_err());
    assert!(parse_egress("not json", false).is_err());
}

#[test]
fn display_names_the_region_and_the_ws_lane() {
    let e = parse_egress(IPINFO, false).unwrap();
    let s = e.to_string();
    assert!(s.contains("52.16.1.2") && s.contains("IE/Dublin"), "{s}");
    assert!(s.contains("ws lane DIRECT"), "{s}");
}

#[test]
fn mismatch_collapses_to_a_named_error_and_ok_to_the_observation() {
    let observed = parse_egress(IPINFO, true).unwrap();
    let err = EgressCheck::Mismatch { expected: "US".into(), observed: observed.clone() }
        .into_result()
        .unwrap_err();
    assert!(err.contains("expected egress country is US"), "{err}");
    assert!(err.contains("52.16.1.2"), "{err}");
    assert_eq!(EgressCheck::Ok(observed.clone()).into_result().unwrap(), Some(observed));
    assert_eq!(EgressCheck::NotConfigured.into_result().unwrap(), None);
}

/// The guard is INERT (no network, no failure) when the caller declares no expectation — the
/// read-only smokes' constant. It reads no environment: the expectation is a parameter.
#[test]
fn unconfigured_is_a_no_op() {
    assert_eq!(
        check_expected_egress(None, DEFAULT_EGRESS_PROBE).unwrap(),
        EgressCheck::NotConfigured
    );
    assert_eq!(
        check_expected_egress(Some("  "), DEFAULT_EGRESS_PROBE).unwrap(),
        EgressCheck::NotConfigured
    );
}

/// The one region the tunnel exits from, named once.
#[test]
fn the_dublin_region_is_ireland() {
    assert_eq!(DUBLIN_EGRESS_COUNTRY, "IE");
}

// --- the venue's own order-placement geoblock (see `parse_geoblock`) ---------------------
//
// Fixture-only, like every other parser here: nothing below touches the network, and the one
// network-shaped case is proven through `geoblock_verdict`'s pure argument.

/// The MEASURED refusal, through a German exit (2026-08-23). Reads were all green through that
/// same egress; only order submission came back 403.
const GEOBLOCK_DE: &str =
    r#"{"blocked":true,"ip":"2a01:aaaa:bbbb:cccc::1","country":"DE","region":"SN"}"#;

#[test]
fn parses_a_blocked_response_with_its_country_and_region() {
    let g = parse_geoblock(GEOBLOCK_DE).unwrap();
    assert!(g.blocked);
    assert_eq!(g.country.as_deref(), Some("DE"));
    assert_eq!(g.region.as_deref(), Some("SN"));
    assert_eq!(g.ip.as_deref(), Some("2a01:aaaa:bbbb:cccc::1"));
    // ...and the rendering an operator actually reads names all three.
    let s = g.to_string();
    assert!(s.contains("country=DE") && s.contains("region=SN"), "{s}");
}

#[test]
fn parses_an_allowed_response() {
    let body = r#"{"blocked":false,"ip":"52.16.1.2","country":"IE","region":"L"}"#;
    let g = parse_geoblock(body).unwrap();
    assert!(!g.blocked);
    assert_eq!(g.country.as_deref(), Some("IE"));
    assert_eq!(geoblock_verdict(Ok(g.clone())), GeoblockVerdict::Allowed(g));
}

/// Every field but `blocked` is optional, and a field of the WRONG TYPE reads as absent rather
/// than as an error or a panic — the endpoint is an undocumented web-app route, so its
/// courtesy fields are not something a mount decision may hang on.
#[test]
fn tolerates_missing_and_malformed_optional_fields() {
    let g = parse_geoblock(r#"{"blocked":true}"#).unwrap();
    assert!(g.blocked && g.country.is_none() && g.region.is_none() && g.ip.is_none());
    assert_eq!(g.to_string(), "country=? region=? ip=?");
    let odd = r#"{"blocked":false,"ip":42,"country":null,"region":{"a":1},"extra":[1]}"#;
    let g = parse_geoblock(odd).unwrap();
    assert!(!g.blocked && g.ip.is_none() && g.country.is_none() && g.region.is_none());
    // an empty string is not a country either
    assert!(parse_geoblock(r#"{"blocked":true,"country":""}"#).unwrap().country.is_none());
    // the two string spellings of the boolean are honoured (a web route, not a contract)
    assert!(parse_geoblock(r#"{"blocked":"TRUE"}"#).unwrap().blocked);
    assert!(!parse_geoblock(r#"{"blocked":"false"}"#).unwrap().blocked);
}

/// THE LOAD-BEARING TOLERANCE RULE: a body we cannot read `blocked` out of is an ERROR, never
/// an all-clear. `Ok(blocked: false)` would be a positive claim that the venue permits trading
/// — the exact false confidence this whole probe exists to remove.
#[test]
fn an_unusable_blocked_field_is_an_error_not_an_all_clear() {
    for body in [
        "",
        "not json",
        "[]",
        r#""blocked""#,
        r#"{"error":"rate limited"}"#,
        r#"{"blocked":1}"#,
        r#"{"blocked":null}"#,
        r#"{"blocked":"maybe"}"#,
        r#"{"blocked":{"api":true}}"#,
    ] {
        assert!(parse_geoblock(body).is_err(), "{body} must not parse as an all-clear");
    }
}

/// An unreachable or unreadable probe is `Unknown`, NOT `Blocked`. A venue outage must not be
/// able to refuse a mount — the caller's half of that rule is
/// `crates/bridges/polymarket/src/exec_plane/mount.rs`'s `geoblock_action`.
#[test]
fn a_probe_failure_is_unknown_never_blocked() {
    let v = geoblock_verdict(Err("geoblock probe: dial tcp: timed out".to_string()));
    assert!(matches!(&v, GeoblockVerdict::Unknown(why) if why.contains("timed out")));
    // ...and the same for a body that reached us but said nothing usable
    let v = geoblock_verdict(parse_geoblock("<html>403</html>"));
    assert!(matches!(v, GeoblockVerdict::Unknown(_)));
}

/// The probe URL is the SITE's route, not the CLOB API host — the trap that makes this
/// endpoint easy to wire against the wrong base.
#[test]
fn the_probe_url_is_the_site_not_the_clob_host() {
    assert_eq!(GEOBLOCK_URL, "https://polymarket.com/api/geoblock");
    assert!(!GEOBLOCK_URL.starts_with(crate::config::CLOB_BASE));
}

// --- the declared egress settings (decision 0095) ----------------------------------------------
//
// Egress reads no environment and opens no store: the composition root reads the
// `venue.polymarket.*` rows and DECLARES them ([`declare_egress`]). These tests drive the pure
// `*_with` cores over one `EgressSettings`, and the declaration rule over LOCAL cells, so no test
// touches the process-wide declaration.

fn settings(pairs: &[(&str, &str)]) -> EgressSettings {
    let rows: HashMap<String, String> =
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    EgressSettings::from_lookup(|field| rows.get(field).cloned())
}

/// [`proxy_url`] over one settings value, the way the declared path reads it.
fn url(s: &EgressSettings) -> Option<String> {
    proxy_url_with(|f| s.field(f))
}

/// [`ws_proxy`] over one settings value.
fn ws(s: &EgressSettings) -> Option<vike_bridge_core::ws_proxy::WsProxy> {
    ws_proxy_with(|f| s.field(f))
}

/// Nothing configured is the built-in default every tunnelled box has always relied on.
#[test]
fn nothing_configured_is_the_localhost_tunnel() {
    assert_eq!(url(&EgressSettings::default()).as_deref(), Some("socks5h://127.0.0.1:1080"));
}

#[test]
fn host_and_port_compose_the_url() {
    let s = settings(&[("proxy_host", "<host>"), ("proxy_port", "1081")]);
    assert_eq!(url(&s).as_deref(), Some("socks5h://<host>:1081"));
}

#[test]
fn an_explicit_url_wins_and_the_direct_spellings_disable() {
    let s = settings(&[("socks_proxy", "socks5h://dub.internal:9050"), ("proxy_host", "<host>")]);
    assert_eq!(url(&s).as_deref(), Some("socks5h://dub.internal:9050"));
    assert!(url(&settings(&[("socks_proxy", "none")])).is_none());
    assert!(url(&settings(&[("socks_proxy", "direct")])).is_none());
    assert!(url(&settings(&[("proxy_enabled", "false")])).is_none());
    assert!(url(&settings(&[("proxy_enabled", "0")])).is_none());
}

/// The WS lanes inherit the HTTP lane; `ws_proxy_enabled` can only narrow them; the master OFF
/// disables both.
#[test]
fn the_ws_lane_inherits_and_its_override_narrows() {
    assert!(ws(&settings(&[("socks_proxy", "socks5h://h:1")])).is_some());
    assert!(
        ws(&settings(&[("socks_proxy", "socks5h://h:1"), ("ws_proxy_enabled", "off")])).is_none()
    );
    assert!(ws(&settings(&[("proxy_enabled", "false"), ("ws_proxy_enabled", "1")])).is_none());
}

/// A row carried over from the old `.env` may still hold its trailing `# comment` — annotation.
#[test]
fn a_trailing_comment_is_annotation() {
    let s = settings(&[("proxy_host", "127.0.0.1  # ssh -D"), ("proxy_port", "11080   # tunnel")]);
    assert_eq!(url(&s).as_deref(), Some("socks5h://127.0.0.1:11080"));
    assert!(url(&settings(&[("proxy_enabled", "false # direct")])).is_none());
}

/// Review Focus 5: `socks_proxy` may carry `user:password@` (a declared SECRET field), so the Debug
/// form a root logs says only that it is set.
#[test]
fn the_debug_form_never_prints_the_proxy_url() {
    let s =
        settings(&[("socks_proxy", "socks5h://u:hunter2@<host>:1080"), ("proxy_port", "1080")]);
    let shown = format!("{s:?}");
    assert!(!shown.contains("hunter2") && !shown.contains("<host>"), "{shown}");
    assert!(shown.contains("socks_proxy") && shown.contains("1080"), "{shown}");
}

/// One process has one egress: the first declaration stands, the same one again is `Ok`, a
/// different one is an error — and so is a FIRST declaration made after egress was already
/// resolved from the defaults.
#[test]
fn a_declaration_is_once_per_process_and_a_late_one_says_so() {
    let cell = OnceLock::new();
    let resolved_early = AtomicBool::new(false);
    let direct = settings(&[("proxy_enabled", "false")]);
    assert_eq!(declare_in(&cell, &resolved_early, direct.clone()), Ok(()));
    assert_eq!(declare_in(&cell, &resolved_early, direct), Ok(()), "the same declaration twice");
    let e = declare_in(&cell, &resolved_early, EgressSettings::default()).unwrap_err();
    assert!(e.contains("already declared"), "{e}");

    let late = OnceLock::new();
    let e = declare_in(&late, &AtomicBool::new(true), EgressSettings::default()).unwrap_err();
    assert!(e.contains("BEFORE it was declared"), "{e}");
}

// --- the composition root's one entry: `declare_from_rows` (decision 0095's review) -----------
//
// Every test below drives `declare_from_rows_with` over a CAPTURING declaration, never the
// process-wide one, so none of them touches the cell the crate's other tests share. The rows come
// from a REAL settings store written and read back by `vike-secrets`' own writer and loader — the
// shape the deployed boxes hold.

use tracing_test::traced_test;
use vike_secrets::venue_setting::load_venue_settings;

/// What `declare_from_rows` is given for the rows: the real loader's answer.
type Loaded = Option<Result<BTreeMap<String, VenueSettings>, DbError>>;

/// A FRESH settings store holding `polymarket` rows written the way `vike-cli config set` and
/// `secrets move-venue-config` write them — machine-scoped (tier `None`, stored as `any`), the field
/// in the store's UPPER-case spelling, which is how the CI box holds its three — then read back through
/// the real loader. The `TempDir` keeps the store alive for the test.
fn planted(rows: &[(&str, &str)]) -> (tempfile::TempDir, Loaded) {
    let dir = tempfile::tempdir().expect("a temp dir");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
        .expect("a fresh settings store");
    for (field, value) in rows {
        vike_secrets::set_venue_setting_in(dir.path(), "polymarket", None, field, value)
            .unwrap_or_else(|e| panic!("planting {field}: {e}"));
    }
    let loaded = Some(load_venue_settings(dir.path()));
    (dir, loaded)
}

/// Run the entry with a declaration that CAPTURES instead of declaring; returns what it would have
/// declared and the `polymarket` rows it hands back.
fn declared_by(loaded: Loaded) -> (EgressSettings, Option<VenueSettings>) {
    let mut got = None;
    let rows = declare_from_rows_with(loaded, |s| {
        got = Some(s);
        Ok(())
    });
    (got.expect("the entry always declares, whatever the rows say"), rows)
}

/// **THE PROPERTY THE TASK EXISTS FOR, TESTED rather than traced.** the CI box's store holds three
/// `venue_setting` rows for this venue, all tier `any` (measured read-only on a scratch copy of its
/// live store, 2026-09-30): `PROXY_ENABLED = false`, `PROXY_HOST = 127.0.0.1`, `PROXY_PORT = 1080`.
/// Plant exactly those, read them back through the real loader, run the entry the daemons run — the
/// egress is DIRECT on both lanes (the off switch outranks the host and port beside it), which is what
/// the unit line it replaced pinned.
#[test]
fn a_proxy_off_row_in_the_shape_prod2_stores_it_resolves_direct() {
    let (_dir, loaded) =
        planted(&[("PROXY_ENABLED", "false"), ("PROXY_HOST", "127.0.0.1"), ("PROXY_PORT", "1080")]);
    let (declared, rows) = declared_by(loaded);
    assert!(url(&declared).is_none(), "the HTTP lane must be direct: {declared:?}");
    assert!(ws(&declared).is_none(), "…and the WS lane, which inherits it: {declared:?}");
    let rows = rows.expect("the polymarket rows come back for a root that needs another field");
    assert_eq!(rows.get(SettingTier::Any, "proxy_enabled"), Some("false"));
}

/// The other shape: proxy ON with a host and port row — `true` plus the pair composes the URL.
#[test]
fn host_and_port_rows_with_the_proxy_on_compose_the_url() {
    let (_dir, loaded) =
        planted(&[("PROXY_ENABLED", "true"), ("PROXY_HOST", "<host>"), ("PROXY_PORT", "1081")]);
    let (declared, _) = declared_by(loaded);
    assert_eq!(url(&declared).as_deref(), Some("socks5h://<host>:1081"));
    assert!(ws(&declared).is_some(), "the WS lane inherits a proxied HTTP lane");
}

/// **No row for any of the five fields — the built-in default, declared quietly.** Since decision
/// 0095's Task 7 there is no second home to consult: a credential row under a field's legacy name
/// refuses the boot before a root gets here, and this entry takes no credential map at all.
#[traced_test]
#[test]
fn no_egress_row_declares_the_built_in_default_quietly() {
    let (_dir, loaded) = planted(&[("WS_TOKENS_PER_SOCKET", "200")]);
    let (declared, rows) = declared_by(loaded);
    assert_eq!(declared, EgressSettings::default());
    assert_eq!(url(&declared).as_deref(), Some("socks5h://127.0.0.1:1080"));
    assert!(rows.is_some(), "the venue's rows still come back for the root's other field");
    let (declared, _) = declared_by(Some(Ok(BTreeMap::new())));
    assert_eq!(declared, EgressSettings::default(), "an empty store says the same thing");
    assert!(!logs_contain("could not be read"));
}

/// **THE ONE STORE-ERROR POLICY** (see `declare_from_rows`): a store that exists and cannot
/// be read is LOGGED at `error!`, read as no rows, and the result is still DECLARED — the built-in
/// default. The store here is a file that is not a database, the shape `config check`'s own tests
/// use for "went bad since".
#[traced_test]
#[test]
fn an_unreadable_store_is_logged_and_the_default_applies() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let db = vike_secrets::db_path_in(dir.path());
    std::fs::create_dir_all(db.parent().expect("db has a parent")).expect("db dir");
    std::fs::write(&db, b"not a sqlite file").expect("plant a bad store");

    let bad = || Some(load_venue_settings(dir.path()));
    assert!(matches!(bad(), Some(Err(_))), "the planted store must not read");

    let (declared, rows) = declared_by(bad());
    assert_eq!(declared, EgressSettings::default(), "the default is declared, not skipped");
    assert!(rows.is_none());
    assert!(logs_contain("could not be read"), "the error line says so");
    assert!(logs_contain("127.0.0.1:1080"), "…and names what the box will dial instead");
}

/// `None` — the boot found no settings directory — reads nothing and is not an error.
#[traced_test]
#[test]
fn no_settings_directory_is_not_an_error() {
    let (declared, rows) = declared_by(None);
    assert_eq!(declared, EgressSettings::default());
    assert!(rows.is_none());
    assert!(!logs_contain("could not be read"));
}

/// A refused declaration (the process already declared something else) is LOGGED, never a panic, and
/// the rows still come back — a root's other field (the data server's socket sizing) does not depend
/// on the declaration succeeding.
#[traced_test]
#[test]
fn a_refused_declaration_is_logged_and_the_rows_still_come_back() {
    let (_dir, loaded) = planted(&[("WS_TOKENS_PER_SOCKET", "200")]);
    let rows = declare_from_rows_with(loaded, |_| Err("already declared".to_string()));
    let rows = rows.expect("the rows are returned whatever the declaration said");
    assert_eq!(rows.get(SettingTier::Any, "ws_tokens_per_socket"), Some("200"));
    assert!(logs_contain("already declared"));
}

/// The five egress fields, in the order [`EgressSettings::from_lookup`] reads them. Test-only since
/// decision 0095's Task 7 deleted `egress_legacy_names`, the last production reader of this list.
const EGRESS_FIELDS: [&str; 5] =
    ["socks_proxy", "proxy_enabled", "proxy_host", "proxy_port", "ws_proxy_enabled"];

/// The five field names are ONE list: what [`EgressSettings::from_lookup`] asks and what
/// [`EgressSettings::field`] answers. A sixth field added to one of them and not the other would be
/// silently ignored by the roots.
#[test]
fn egress_fields_are_exactly_what_the_settings_read() {
    let asked = std::cell::RefCell::new(Vec::new());
    let s = EgressSettings::from_lookup(|f| {
        asked.borrow_mut().push(f.to_string());
        Some(format!("v-{f}"))
    });
    let mut asked = asked.into_inner();
    asked.sort();
    let mut want: Vec<String> = EGRESS_FIELDS.iter().map(|f| (*f).to_string()).collect();
    want.sort();
    assert_eq!(asked, want, "from_lookup asks exactly EGRESS_FIELDS");
    for f in EGRESS_FIELDS {
        assert_eq!(s.field(f), Some(format!("v-{f}")), "{f} must round-trip through field()");
    }
    assert!(s.field("rate_gate").is_none(), "a field outside the list is not an egress field");
}

/// The crate's `#[ignore]`d unit-level live tests' twin of `tests/support/egress.rs`: declare the
/// process's Polymarket egress from the settings database, exactly as a composition root does
/// (decision 0095 — the bridge reads no environment and opens no store): the root READS the rows
/// and hands them to [`declare_from_rows`], which is the one place the store-error policy
/// and the precedence live.
///
/// `settings_dir_override` is a PARAMETER rather than a `std::env::var` read here — this file is a
/// `src/` module (folded into `egress.rs` by the settings registry's src walk), so a direct
/// environment read here would be scored `Layer::Library` and trip that gate's growth ratchet. The
/// caller (an `#[ignore]`d test elsewhere in this crate) passes
/// `std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()` itself, from a file the registry already
/// exempts (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `SRC_TEST_MODULE_OVERRIDES`). A dev box
/// whose tunnel is still a credential row files it with `vike-cli secrets move-venue-config` (or
/// writes the rows) before running a live test.
#[cfg(feature = "polymarket")]
pub(crate) fn declare_from_the_settings_database(settings_dir_override: Option<&str>) {
    let loaded = vike_model::paths::state_path::project_settings_dir_from(
        settings_dir_override,
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| load_venue_settings(&dir));
    // Two tests of one binary declare the same settings, which is `Ok`.
    super::declare_from_rows(loaded);
}
