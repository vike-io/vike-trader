use super::*;
use std::assert_matches;
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
    assert!(e.websocket_lane);
}

#[test]
fn tolerates_a_probe_without_geo_fields() {
    let e = parse_egress(r#"{"ip":"203.0.113.9"}"#, false).unwrap();
    assert_eq!(e.ip, "203.0.113.9");
    assert!(e.country.is_none());
    assert!(!e.websocket_lane);
}

#[test]
fn rejects_a_response_without_an_ip() {
    assert!(parse_egress(r#"{"country":"IE"}"#, false).is_err());
    assert!(parse_egress(r#"{"ip":""}"#, false).is_err());
    assert!(parse_egress("not json", false).is_err());
}

#[test]
fn display_names_the_region_and_the_websocket_lane() {
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
    let mismatch = crate::refusals::PolymarketEgressRefusal::Mismatch {
        observed: observed.clone(),
        expected: "US".into(),
    };
    assert_eq!(err, mismatch.to_string());
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
/// courtesy fields are not something a start decision may hang on.
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
/// able to refuse a start — the caller's half of that rule is
/// `crates/bridges/polymarket/src/adapter.rs`'s `geoblock_action`.
#[test]
fn a_probe_failure_is_unknown_never_blocked() {
    let v = geoblock_verdict(Err("geoblock probe: dial tcp: timed out".to_string()));
    assert_eq!(v, GeoblockVerdict::Unknown("geoblock probe: dial tcp: timed out".to_string()));
    // ...and the same for a body that reached us but said nothing usable
    let v = geoblock_verdict(parse_geoblock("<html>403</html>"));
    assert_matches!(v, GeoblockVerdict::Unknown(_));
}

/// The probe URL is the SITE's route, not the CLOB API host — the trap that makes this
/// endpoint easy to wire against the wrong base: the two are different rows of the seed.
#[cfg(feature = "polymarket")]
#[test]
fn the_probe_url_is_the_site_not_the_clob_host() {
    let seed = vike_venues::venue_urls::VenueUrls::seed(crate::VENUE);
    let site = crate::config::geoblock_base(&seed).expect("the seed's geoblock row");
    let clob = crate::config::clob_base(&seed).expect("the seed's CLOB row");
    assert_eq!(GEOBLOCK_PATH, "/api/geoblock");
    assert!(!format!("{site}{GEOBLOCK_PATH}").starts_with(clob));
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
    EgressSettings::from_lookup(|field| rows.get(field.operator_spelling()).cloned())
}

/// [`proxy_url`] over one settings value, the way the declared path reads it.
fn url(s: &EgressSettings) -> Option<String> {
    vike_venues::venue_fields::polymarket_proxy_url(|f| s.field(f))
}

/// [`websocket_proxy`] over one settings value.
fn ws(s: &EgressSettings) -> Option<vike_transport::websocket_proxy::WebsocketProxy> {
    websocket_proxy_with(|f| s.field(f))
}

/// **The proxy is OFF by default** (owner ruling 2026-11-22): an empty lookup, and nothing configured,
/// dial direct on both lanes.
#[test]
fn nothing_configured_dials_direct() {
    assert_eq!(vike_venues::venue_fields::polymarket_proxy_url(|_| None), None);
    assert_eq!(websocket_proxy_with(|_| None), None);
    assert_eq!(url(&EgressSettings::default()), None);
}

/// `proxy_enabled = true` with nothing else is the tunnel at its built-in endpoint.
#[test]
fn proxy_enabled_true_is_the_localhost_tunnel() {
    let on = settings(&[("proxy_enabled", "true")]);
    assert_eq!(url(&on).as_deref(), Some("socks5h://127.0.0.1:1080"));
    assert!(ws(&on).is_some(), "the WS lane inherits the proxied HTTP lane");
    for truthy in ["1", "yes", "ON"] {
        assert!(url(&settings(&[("proxy_enabled", truthy)])).is_some(), "{truthy}");
    }
    for other in ["false", "0", "off", "maybe", ""] {
        assert!(url(&settings(&[("proxy_enabled", other)])).is_none(), "{other:?} dials direct");
    }
}

/// The host and port compose the URL only when the proxy is enabled; alone they configure nothing.
#[test]
fn host_and_port_compose_the_url() {
    let s =
        settings(&[("proxy_enabled", "true"), ("proxy_host", "<host>"), ("proxy_port", "1081")]);
    assert_eq!(url(&s).as_deref(), Some("socks5h://<host>:1081"));
    assert!(url(&settings(&[("proxy_host", "<host>"), ("proxy_port", "1081")])).is_none());
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

/// The WS lanes inherit the HTTP lane; `websocket_proxy_enabled` can only narrow them; the master
/// switch, off by default, decides both.
#[test]
fn the_websocket_lane_inherits_and_its_override_narrows() {
    assert!(ws(&settings(&[("socks_proxy", "socks5h://h:1")])).is_some());
    assert!(
        ws(&settings(&[("socks_proxy", "socks5h://h:1"), ("ws_proxy_enabled", "off")])).is_none()
    );
    assert!(ws(&settings(&[("proxy_enabled", "false"), ("ws_proxy_enabled", "1")])).is_none());
    assert!(ws(&settings(&[("ws_proxy_enabled", "1")])).is_none(), "no tunnel by default");
    // A value the `Bool` grammar does not spell is no override: the lane inherits the tunnel, never
    // dialling direct around it.
    assert!(
        ws(&settings(&[("socks_proxy", "socks5h://h:1"), ("ws_proxy_enabled", "maybe")])).is_some()
    );
}

/// A row carried over from the old `.env` may still hold its trailing `# comment` — annotation.
#[test]
fn a_trailing_comment_is_annotation() {
    let s = settings(&[
        ("proxy_enabled", "true  # Dublin"),
        ("proxy_host", "127.0.0.1  # ssh -D"),
        ("proxy_port", "11080   # tunnel"),
    ]);
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
    assert_eq!(e, crate::refusals::PolymarketEgressRefusal::AlreadyDeclared);

    let late = OnceLock::new();
    let e = declare_in(&late, &AtomicBool::new(true), EgressSettings::default()).unwrap_err();
    assert_eq!(e, crate::refusals::PolymarketEgressRefusal::ResolvedBeforeDeclared);
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
type Loaded = Option<Result<BTreeMap<String, VenueSettings>, StoreError>>;

/// A FRESH settings store holding `polymarket` rows written the way `vike-cli config set`
/// writes them — machine-scoped (tier `None`, stored as `any`), the field
/// in the store's UPPER-case spelling, which is how the CI box holds its three — then read back through
/// the real loader. The `TempDir` keeps the store alive for the test.
fn planted(rows: &[(&str, &str)]) -> (tempfile::TempDir, Loaded) {
    let dir = tempfile::tempdir().expect("a temp dir");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
        .expect("a fresh settings store");
    for (field, value) in rows {
        let name = VenueFieldName::normalised(field);
        vike_secrets::set_venue_setting_in_unjournalled(
            dir.path(),
            "polymarket",
            None,
            &name,
            value,
        )
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
    assert_eq!(rows.get(SettingTier::Any, &PROXY_ENABLED), Some("false"));
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
    assert_eq!(url(&declared), None, "the default is a direct connection");
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
    assert_matches!(bad(), Some(Err(_)), "the planted store must not read");

    let (declared, rows) = declared_by(bad());
    assert_eq!(declared, EgressSettings::default(), "the default is declared, not skipped");
    assert!(rows.is_none());
    assert!(logs_contain("could not be read"), "the error line says so");
    assert!(logs_contain("a direct connection"), "…and names what the box will dial instead");
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
    assert_eq!(
        rows.get(SettingTier::Any, &vike_venues::venue_fields::WS_TOKENS_PER_SOCKET),
        Some("200")
    );
    assert!(logs_contain("already declared"));
}

/// The five egress fields, in the order [`EgressSettings::from_lookup`] reads them. Test-only since
/// decision 0095's Task 7 deleted `egress_legacy_names`, the last production reader of this list.
const EGRESS_FIELDS: [VenueFieldName; 5] =
    [SOCKS_PROXY, PROXY_ENABLED, PROXY_HOST, PROXY_PORT, WS_PROXY_ENABLED];

/// The five field names are ONE list: what [`EgressSettings::from_lookup`] asks and what
/// [`EgressSettings::field`] answers. A sixth field added to one of them and not the other would be
/// silently ignored by the roots.
#[test]
fn egress_fields_are_exactly_what_the_settings_read() {
    let asked = std::cell::RefCell::new(Vec::new());
    let s = EgressSettings::from_lookup(|f| {
        asked.borrow_mut().push(f.clone());
        Some(format!("v-{}", f.operator_spelling()))
    });
    let mut asked = asked.into_inner();
    asked.sort();
    let mut want = EGRESS_FIELDS.to_vec();
    want.sort();
    assert_eq!(asked, want, "from_lookup asks exactly EGRESS_FIELDS");
    for f in EGRESS_FIELDS {
        let spelled = f.operator_spelling();
        assert_eq!(s.field(&f), Some(format!("v-{spelled}")), "{spelled} must round-trip");
    }
    assert!(
        s.field(&vike_venues::venue_fields::RATE_GATE).is_none(),
        "a field outside the list is not an egress field"
    );
}

/// `agent_for(_, false)` is the untunnelled agent the Polygon RPC client builds (its pre-merge
/// `build_agent` copy): a 3xx comes back as a plain response (never followed, never an error) and
/// the request carries the crate's user agent. No proxy is read, so this needs no declaration.
#[test]
fn the_untunnelled_agent_keeps_the_shared_recipe() {
    use std::io::{Read, Write};
    let target = std::net::TcpListener::bind("127.0.0.1:0").expect("bind target");
    target.set_nonblocking(true).expect("nonblocking target");
    let target_port = target.local_addr().expect("target addr").port();
    let origin = std::net::TcpListener::bind("127.0.0.1:0").expect("bind origin");
    let url = format!("http://127.0.0.1:{}/rpc", origin.local_addr().expect("origin addr").port());
    let server = std::thread::spawn(move || {
        let (mut s, _) = origin.accept().expect("one request");
        s.set_read_timeout(Some(std::time::Duration::from_secs(2))).expect("read timeout");
        let mut buffer = [0u8; 4096];
        let n = s.read(&mut buffer).unwrap_or(0);
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target_port}/stolen\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = s.write_all(reply.as_bytes());
        String::from_utf8_lossy(&buffer[..n]).to_ascii_lowercase()
    });
    let response = agent_for(std::time::Duration::from_secs(5), false).get(&url).call();
    assert_eq!(response.expect("a 3xx is a response, not an error").status().as_u16(), 302);
    let user_agent = format!("user-agent: {}", vike_transport::http::USER_AGENT);
    assert!(server.join().unwrap().contains(&user_agent));
    assert!(target.accept().is_err(), "the Location was never dialled");
}

/// What the child half of [`a_proxy_in_the_environment_is_never_used_and_polymarkets_own_proxy_wins`] is started with:
/// a second name filter that matches no test (libtest refuses an unknown `--` option).
const PROXY_CHILD_MARK: &str = "vike-polymarket-proxy-environment-child";

/// **A proxy variable in the process environment is never used; polymarket's own proxy row is the only proxy.**
/// ureq's default config reads `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`; the workspace forbids
/// `unsafe`, so the variables are set on a CHILD run of this test binary ([`the_proxy_environment_child`]), with
/// `NO_PROXY` removed so a loopback target would be proxied too. The "proxy" is a listener nothing may dial.
#[test]
fn a_proxy_in_the_environment_is_never_used_and_polymarkets_own_proxy_wins() {
    let (proxy, proxy_address) = listener();
    let mut child = std::process::Command::new(std::env::current_exe().expect("this test binary"));
    child.args([
        "transport::transport_tests::the_proxy_environment_child",
        "--exact",
        "--ignored",
        "--nocapture",
        PROXY_CHILD_MARK,
    ]);
    for name in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
    {
        child.env(name, format!("http://{proxy_address}"));
    }
    let out = child.env_remove("NO_PROXY").env_remove("no_proxy").output().expect("run the child");
    let said =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "the child failed:\n{said}");
    assert!(said.contains("1 passed"), "the child ran no test:\n{said}");
    assert!(proxy.accept().is_err(), "a request went through the environment's proxy");
}

/// The child half of the test above: it runs only when started with [`PROXY_CHILD_MARK`].
#[test]
#[ignore = "the child half of a_proxy_in_the_environment_is_never_used_and_polymarkets_own_proxy_wins, which starts it"]
fn the_proxy_environment_child() {
    if !std::env::args().any(|a| a == PROXY_CHILD_MARK) {
        return;
    }
    assert!(ureq::Proxy::try_from_env().is_some(), "the parent set the proxy variables");
    let d = std::time::Duration::from_secs(5);
    for (name, agent) in [("direct", agent_through(d, None)), ("untunnelled", agent_for(d, false))]
    {
        assert!(agent.config().proxy().is_none(), "{name}: {:?}", agent.config().proxy());
    }
    // A loopback target answering 200 is reached directly.
    let target = std::net::TcpListener::bind("127.0.0.1:0").expect("bind target");
    let url = format!("http://{}/x", target.local_addr().expect("addr"));
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut s, _) = target.accept().expect("one request");
        let _ = s.read(&mut [0u8; 4096]);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
    });
    let response = agent_through(d, None).get(&url).call().expect("direct");
    assert_eq!(response.status().as_u16(), 200);
    // The settings row's proxy, never the environment's.
    let port = |agent: &ureq::Agent| agent.config().proxy().map(|p| (p.port(), p.is_from_env()));
    assert_eq!(port(&agent_through(d, Some("socks5h://127.0.0.1:1080"))), Some((1080, false)));
    assert_eq!(port(&agent_through(d, Some("http://127.0.0.1:3128"))), Some((3128, false)));
}

/// A non-blocking loopback listener and its `host:port`, so `accept` says whether anything dialled it.
fn listener() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = format!("127.0.0.1:{}", listener.local_addr().expect("addr").port());
    (listener, addr)
}

/// **A declared proxy that does not parse refuses, and never goes direct**, whichever row spoiled it.
/// The refusal is the kind `send_error_is_definite` calls unsent, so a submit reads as a terminal
/// reject rather than an order of unknown fate, and the log never carries the URL's userinfo.
#[traced_test]
#[test]
fn a_proxy_url_that_does_not_parse_refuses_every_request_and_dials_nothing() {
    let (target, addr) = listener();
    let enabled_at = |host: &str, port: &str| {
        url(&settings(&[("proxy_enabled", "true"), ("proxy_host", host), ("proxy_port", port)]))
            .expect("a declared proxy")
    };
    let bad_host = enabled_at("[bad", "1080");
    let bad_port = enabled_at("127.0.0.1", "not-a-port");
    for bad in [
        "socks9://h:1",
        "socks5h://u:hunter2@bad host:1",
        bad_host.as_str(),
        // `http::Uri` takes all of these and reads no port, so they dialled the host's default port.
        bad_port.as_str(),
        "socks5h://u:hunter2@127.0.0.1:not-a-port",
        "socks5h://127.0.0.1:99999",
        "socks5h://127.0.0.1:",
        "socks5h://[::1]:not-a-port",
    ] {
        let agent = agent_through(std::time::Duration::from_secs(5), Some(bad));
        let err = agent.get(format!("http://{addr}/x")).call().expect_err(bad);
        assert_matches!(&err, ureq::Error::InvalidProxyUrl, "{bad}");
        assert!(vike_transport::send_outcome::send_error_is_definite(&err), "{bad}");
        assert!(target.accept().is_err(), "{bad}: the request went DIRECT");
    }
    assert!(logs_contain("every HTTP request is REFUSED"));
    assert!(!logs_contain("hunter2"));
}

/// A URL that parses still takes the tunnel: the first dial is the proxy's, not the target's.
#[test]
fn a_proxy_url_that_parses_dials_the_proxy_and_not_the_target() {
    let (target, addr) = listener();
    let (proxy, proxy_address) = listener();
    let accepted = std::thread::spawn(move || {
        (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            proxy.accept().is_ok()
        })
    });
    let agent = agent_through(
        std::time::Duration::from_secs(5),
        Some(&format!("socks5h://{proxy_address}")),
    );
    assert!(agent.get(format!("http://{addr}/x")).call().is_err(), "the proxy hung up");
    assert!(accepted.join().expect("the proxy's thread"), "the request went through the proxy");
    assert!(target.accept().is_err(), "the request went DIRECT");
}

/// **A name that does not resolve is a failure provably before the write** (owner ruling F-9): the agent resolves
/// through the shared `DnsResolver`, so the lookup's failure is `HostNotFound`, which `send_error_is_definite` calls
/// unsent, and a submit that never left the box is rejected instead of left UNKNOWN. Before, ureq's own resolver
/// answered `Io`, which the rule must read as possibly after the write. (`.invalid` is reserved: it never resolves.)
#[test]
fn a_name_that_does_not_resolve_is_a_failure_before_the_write() {
    let agent = agent_through(std::time::Duration::from_secs(5), None);
    let err = agent.post("http://no-such-host.invalid/order").send_empty().expect_err("no address");
    assert_matches!(&err, ureq::Error::HostNotFound, "{err:?}");
    assert!(vike_transport::send_outcome::send_error_is_definite(&err), "{err}");
}

/// The time limit of the requests below that a proxy never answers, and how far past it a loaded
/// lane's scheduling may carry the answer. The dial this pins used to answer after about 130 s.
const DEAD_PROXY_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);
const SCHEDULING_SLACK: std::time::Duration = std::time::Duration::from_secs(10);

/// One GET through `proxy` under `limit`, on its own thread, so a dial that outlives the limit fails
/// the test here instead of holding it. The status, or the send error.
fn get_through(limit: std::time::Duration, proxy: &str, url: &str) -> Result<u16, ureq::Error> {
    let agent = agent_through(limit, Some(proxy));
    let url = url.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(agent.get(&url).call().map(|r| r.status().as_u16())));
    rx.recv_timeout(limit + SCHEDULING_SLACK)
        .expect("the request outlived its time limit, so a dead proxy holds the order path")
}

/// `result` failed before the request existed: the kind `send_error_is_definite` calls unsent, so a
/// submit reads as a terminal reject. Answers the failure's words.
fn unsent(row: &str, result: Result<u16, ureq::Error>) -> String {
    let err = result.expect_err(row);
    assert_matches!(&err, ureq::Error::ConnectProxyFailed(_), "{row}");
    assert!(vike_transport::send_outcome::send_error_is_definite(&err), "{row}");
    err.to_string()
}

/// **A dead proxy ends a request inside the agent's limit, as unsent, and never direct.** Up to ureq
/// 3.4 its own SOCKS dial joined the thread it waited for, so `timeout_global` reached the caller only
/// when the dial ended by itself: never, for the first two rows (`vike_transport::socks`'s module doc).
#[test]
fn a_dead_proxy_ends_a_request_inside_the_limit_as_unsent_and_never_direct() {
    use std::io::{Read, Write};
    let (target, addr) = listener();
    let url = format!("http://{addr}/x");
    let through = |proxy: &str| get_through(DEAD_PROXY_LIMIT, proxy, &url);

    // The kernel takes the connection and nobody ever answers the greeting.
    let (_silent, silent_address) = listener();
    let words = unsent("silent", through(&format!("socks5h://{silent_address}")));
    assert!(words.contains("(limit 1 s)"), "the failure states the limit: {words}");

    // The greeting is answered and the CONNECT never is: a hung `ssh -D`, whose far side is gone.
    let hung = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let hung_address = hung.local_addr().expect("addr");
    let held = std::thread::spawn(move || {
        let (mut s, _) = hung.accept().expect("the dial");
        s.read_exact(&mut [0u8; 3]).expect("the greeting");
        s.write_all(&[5, 0]).expect("the answer");
        s // alive until the join below, so the client meets silence and not a hang-up
    });
    unsent("hung tunnel", through(&format!("socks5h://{hung_address}")));
    drop(held.join().expect("the proxy's thread"));

    // A reserved address no packet reaches: minutes of SYN retries, or at once on a box with no route.
    unsent("black hole", through("socks5h://192.0.2.1:1080"));

    assert!(target.accept().is_err(), "a request went DIRECT");
}

/// **A dead proxy costs [`vike_transport::socks::SOCKS_DIAL_LIMIT`], however long the agent may wait.** The order
/// path's agent may wait 30 s, and the exec thread paid all of it per order on a proxy that nobody
/// answers, so the orders queued behind it outlived the core's patience unsent.
#[test]
fn a_dead_proxy_costs_the_dial_limit_and_not_the_agents() {
    let agents = vike_transport::socks::SOCKS_DIAL_LIMIT * 4;
    let (target, addr) = listener();
    let (_silent, silent_address) = listener();
    let started = std::time::Instant::now();
    let proxy = format!("socks5h://{silent_address}");
    let answer = get_through(agents, &proxy, &format!("http://{addr}/x"));
    let took = started.elapsed();
    let words = unsent("silent", answer);
    assert!(words.contains("(limit 5 s)"), "the failure states the dial's limit: {words}");
    assert!(took < agents - SCHEDULING_SLACK / 2, "it spent the agent's limit: {took:?}");
    assert!(target.accept().is_err(), "the request went DIRECT");
}

/// **A slow tunnel that does answer inside the limit keeps working.** The double takes 3 s over the
/// `CONNECT` that a working Dublin tunnel answers in tens of milliseconds.
#[test]
fn a_slow_tunnel_still_carries_the_request() {
    use std::io::{Read, Write};
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let proxy_address = proxy.local_addr().expect("addr");
    let held = std::thread::spawn(move || {
        let (mut s, _) = proxy.accept().expect("the dial");
        s.read_exact(&mut [0u8; GREETING.len()]).expect("the greeting");
        s.write_all(&NO_PASSWORD).expect("the answer");
        s.read_exact(&mut vec![0; connect_by_name().len()]).expect("the CONNECT");
        std::thread::sleep(
            vike_transport::socks::SOCKS_DIAL_LIMIT - std::time::Duration::from_secs(2),
        );
        s.write_all(&TUNNEL_UP).expect("the tunnel");
        assert!(s.read(&mut [0u8; 1024]).expect("the request") > 0, "the request");
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("the response");
    });
    let proxy = format!("socks5h://{proxy_address}");
    let answer = get_through(std::time::Duration::from_secs(30), &proxy, NAMED_TARGET);
    assert_eq!(answer.expect("a slow tunnel is a working tunnel"), 200);
    held.join().expect("the proxy's thread");
}

/// A SOCKS proxy double for one connection: it reads each `(expected, answer)` in turn, then hands
/// back what the client said next, answering a plain GET with a 200.
fn scripted_proxy(script: Vec<(Vec<u8>, Vec<u8>)>) -> (String, std::thread::JoinHandle<Vec<u8>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = format!("127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let proxy = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("the dial");
        s.set_read_timeout(Some(std::time::Duration::from_secs(20))).expect("read timeout");
        for (expected, answer) in script {
            let mut said = vec![0; expected.len()];
            s.read_exact(&mut said).expect("the client's next message");
            assert_eq!(said, expected);
            s.write_all(&answer).expect("the proxy's answer");
        }
        let mut after = [0u8; 1024];
        let n = s.read(&mut after).unwrap_or(0);
        if after.starts_with(b"GET") {
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("the response");
        }
        after[..n].to_vec()
    });
    (addr, proxy)
}

/// A target no local lookup can answer, so a row that resolved it here would fail, and its SOCKS5
/// `CONNECT` by name, port 8080.
const NAMED_TARGET: &str = "http://clob.invalid:8080/x";
fn connect_by_name() -> Vec<u8> {
    [&[5, 1, 0, 3, 12][..], b"clob.invalid", &[0x1f, 0x90]].concat()
}
const GREETING: [u8; 3] = [5, 1, 0];
const NO_PASSWORD: [u8; 2] = [5, 0];
const TUNNEL_UP: [u8; 10] = [5, 0, 0, 1, 0, 0, 0, 0, 0, 0];

/// The bytes of each scheme the proxy URL may name, pinned on the dial `socks` used to speak so the
/// bounded dial changes nothing a proxy can see, then the request itself through the tunnel.
#[test]
fn a_tunnel_speaks_each_scheme_and_carries_the_request() {
    let limit = std::time::Duration::from_secs(20);
    let by_address = vec![5, 1, 0, 1, 127, 0, 0, 1, 0x1f, 0x90];
    let up4 = vec![0, 90, 0, 0, 0, 0, 0, 0];
    let up = TUNNEL_UP.to_vec();
    let hello = (GREETING.to_vec(), NO_PASSWORD.to_vec());
    let v6_bound = [&[5, 0, 0, 4][..], &[0; 18]].concat();
    let offer_password = vec![5, 2, 2, 0];
    let password = [&[1, 5][..], b"alice", &[6], b"s3cret"].concat();
    let ip_target = "http://127.0.0.1:8080/x";
    for (row, scheme, url, script) in [
        (
            "socks5h sends the name",
            "socks5h://",
            NAMED_TARGET,
            vec![hello.clone(), (connect_by_name(), up.clone())],
        ),
        (
            "socks5h sends an address as an address",
            "socks5h://",
            ip_target,
            vec![hello.clone(), (by_address.clone(), up.clone())],
        ),
        (
            "socks5 sends the address this box resolved",
            "socks5://",
            ip_target,
            vec![hello.clone(), (by_address, up.clone())],
        ),
        (
            "a tunnel bound to a name",
            "socks5h://",
            NAMED_TARGET,
            vec![
                hello.clone(),
                (connect_by_name(), [&[5, 0, 0, 3, 3][..], b"pxy", &[0, 1]].concat()),
            ],
        ),
        (
            "a tunnel bound to an IPv6 address",
            "socks5h://",
            NAMED_TARGET,
            vec![hello.clone(), (connect_by_name(), v6_bound)],
        ),
        (
            "a password the proxy asks for",
            "socks5h://alice:s3cret@",
            NAMED_TARGET,
            vec![
                (offer_password.clone(), vec![5, 2]),
                (password, vec![1, 0]),
                (connect_by_name(), up.clone()),
            ],
        ),
        (
            "a password the proxy waives",
            "socks5h://alice:s3cret@",
            NAMED_TARGET,
            vec![(offer_password, NO_PASSWORD.to_vec()), (connect_by_name(), up)],
        ),
        (
            "socks4a sends the name",
            "socks4a://",
            NAMED_TARGET,
            vec![(
                [&[4, 1, 0x1f, 0x90, 0, 0, 0, 1, 0][..], b"clob.invalid", &[0]].concat(),
                up4.clone(),
            )],
        ),
        (
            "socks4 sends the address",
            "socks4://",
            ip_target,
            vec![(vec![4, 1, 0x1f, 0x90, 127, 0, 0, 1, 0], up4)],
        ),
    ] {
        let (addr, proxy) = scripted_proxy(script);
        let got = get_through(limit, &format!("{scheme}{addr}"), url);
        let after = proxy.join().expect(row);
        assert_eq!(got.expect(row), 200, "{row}");
        assert!(after.starts_with(b"GET /x HTTP/1.1"), "{row}: {after:?}");
    }
}

/// An `https` target gets TLS INSIDE the tunnel, port 443 by default: the first bytes past the
/// `CONNECT` are a TLS handshake record, never a request in the clear.
#[test]
fn an_https_target_starts_tls_inside_the_tunnel() {
    let limit = std::time::Duration::from_secs(20);
    let connect = [&[5, 1, 0, 3, 12][..], b"clob.invalid", &[1, 0xbb]].concat();
    let (addr, proxy) = scripted_proxy(vec![
        (GREETING.to_vec(), NO_PASSWORD.to_vec()),
        (connect, TUNNEL_UP.to_vec()),
    ]);
    let got = get_through(limit, &format!("socks5h://{addr}"), "https://clob.invalid/x");
    let after = proxy.join().expect("the proxy's thread");
    assert!(got.is_err(), "the proxy hung up on the handshake");
    assert_eq!(after.first(), Some(&0x16), "{after:?}");
}

/// Every way a proxy can refuse or hang up, ahead of the tunnel, comes before the request exists: all
/// unsent, none carrying the password. The `socks` crate's `Io` errors read a hang-up as an order of
/// unknown fate.
#[test]
fn a_proxy_that_refuses_the_tunnel_leaves_the_request_unsent_and_names_no_secret() {
    let limit = std::time::Duration::from_secs(20);
    let hello = (GREETING.to_vec(), NO_PASSWORD.to_vec());
    let offer_password = vec![5, 2, 2, 0];
    let password = [&[1, 5][..], b"alice", &[7], b"hunter2"].concat();
    let socks4a = [&[4, 1, 0x1f, 0x90, 0, 0, 0, 1, 0][..], b"clob.invalid", &[0]].concat();
    for (row, scheme, script) in [
        ("no method it takes", "socks5h://", vec![(GREETING.to_vec(), vec![5, 0xff])]),
        ("not SOCKS5", "socks5h://", vec![(GREETING.to_vec(), vec![4, 0])]),
        (
            "a password it wants and is not given",
            "socks5h://",
            vec![(GREETING.to_vec(), vec![5, 2])],
        ),
        ("hung up after the greeting", "socks5h://", vec![hello.clone()]),
        ("hung up before the greeting's answer", "socks5h://", vec![]),
        (
            "refused the CONNECT",
            "socks5h://",
            vec![hello.clone(), (connect_by_name(), vec![5, 5, 0, 1, 0, 0, 0, 0, 0, 0])],
        ),
        ("a short refusal", "socks5h://", vec![hello.clone(), (connect_by_name(), vec![5, 1])]),
        (
            "a reply of no address kind",
            "socks5h://",
            vec![hello, (connect_by_name(), vec![5, 0, 0, 9])],
        ),
        (
            "the wrong password",
            "socks5h://alice:hunter2@",
            vec![(offer_password, vec![5, 2]), (password, vec![1, 1])],
        ),
        ("socks4 rejects", "socks4a://", vec![(socks4a, vec![0, 91, 0, 0, 0, 0, 0, 0])]),
    ] {
        let (addr, proxy) = scripted_proxy(script);
        let got = get_through(limit, &format!("{scheme}{addr}"), NAMED_TARGET);
        let after = proxy.join().expect(row);
        let words = unsent(row, got);
        assert!(!words.contains("hunter2"), "{row}: {words}");
        assert!(!after.starts_with(b"GET"), "{row}: the request went out");
    }
}

/// The WS twin: `None` is what `connect_websocket` dials direct, so a URL `WebsocketProxy::parse` refuses answers
/// an endpoint that fails instead, and the master OFF stays the one direct answer.
#[traced_test]
#[test]
fn a_proxy_url_the_websocket_lane_cannot_parse_fails_every_dial_and_dials_nothing() {
    let (target, addr) = listener();
    let declared = settings(&[("socks_proxy", "http://u:hunter2@h:1")]);
    let proxy = ws(&declared).expect("never `None`, which is a direct dial");
    assert_eq!(proxy.host, UNUSABLE_PROXY_HOST);
    let dial = vike_transport::websocket_proxy::connect_websocket(
        &format!("ws://{addr}/"),
        Some(&proxy),
        Some(std::time::Duration::from_secs(5)),
    );
    assert!(dial.is_err(), "the unusable endpoint must not connect");
    assert!(target.accept().is_err(), "the dial went DIRECT");
    assert!(logs_contain("every WS dial FAILS"));
    assert!(!logs_contain("hunter2"));
    assert!(ws(&settings(&[("socks_proxy", "none")])).is_none(), "a declared direct stays direct");
}

/// The crate's `#[ignore]`d unit-level live tests' twin of `tests/polymarket_smokes/egress_support.rs`: declare the
/// process's Polymarket egress from the settings database, exactly as a composition root does
/// (decision 0095 — the bridge reads no environment and opens no store): the root READS the rows
/// and hands them to [`declare_from_rows`], which is the one place the store-error policy
/// and the precedence live.
///
/// `settings_dir_override` is a PARAMETER rather than a `std::env::var` read here — this file is a
/// `src/` module (folded into `transport/mod.rs` by the settings registry's src walk), so a direct
/// environment read here would be scored `Layer::Library` and trip that gate's growth ratchet. The
/// caller (an `#[ignore]`d test elsewhere in this crate) passes
/// `std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()` itself, from a file the registry already
/// exempts (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `SOURCE_TEST_MODULE_OVERRIDES`). A dev box
/// writes its tunnel as rows (`vike-cli config set`) before running a live test.
#[cfg(feature = "polymarket")]
pub(crate) fn declare_from_the_settings_database(settings_dir_override: Option<&str>) {
    let loaded = vike_paths::state_path::project_settings_dir_from(
        settings_dir_override,
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| load_venue_settings(&dir));
    // Two tests of one binary declare the same settings, which is `Ok`.
    super::declare_from_rows(loaded);
}
