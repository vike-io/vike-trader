use super::proxy_box_value;
use vike_secrets::venue_setting::VenueSettings;

fn settings(pairs: &[(&str, &str)]) -> VenueSettings {
    let rows: Vec<vike_secrets::VenueSettingRow> = pairs
        .iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: "polymarket".to_string(),
            tier: None,
            field: vike_venues::venue_fields::VenueFieldName::normalised(field).store_spelling(),
            value: (*value).to_string(),
        })
        .collect();
    VenueSettings::from_rows("polymarket", &rows)
}

/// The box shows what the bridge dials: a host/port store with `proxy_enabled = true` seeds the
/// composed URL. Seeding from `socks_proxy` alone showed an empty box to an operator whose proxy
/// was live, making "see the current value" a lie.
#[test]
fn an_enabled_host_port_store_seeds_the_composed_url() {
    let v =
        settings(&[("proxy_enabled", "true"), ("proxy_host", "127.0.0.1"), ("proxy_port", "1080")]);
    assert_eq!(proxy_box_value(Some(&v)), "socks5h://127.0.0.1:1080");
}

/// The proxy is OFF by default (owner ruling 2026-11-22): host/port with no `proxy_enabled = true`
/// is a direct connection, so an empty box.
#[test]
fn host_port_without_proxy_enabled_seeds_an_empty_box() {
    let v = settings(&[("proxy_host", "127.0.0.1"), ("proxy_port", "1080")]);
    assert!(proxy_box_value(Some(&v)).is_empty());
}

/// The explicit URL is the highest tier and wins, credentials intact.
#[test]
fn an_explicit_url_wins_over_host_port() {
    let v = settings(&[
        ("socks_proxy", "socks5h://user:hunter2@1.2.3.4:1080"),
        ("proxy_host", "127.0.0.1"),
        ("proxy_port", "1080"),
    ]);
    assert_eq!(proxy_box_value(Some(&v)), "socks5h://user:hunter2@1.2.3.4:1080");
}

/// A half-configured enabled store still composes, using the bridge's fallbacks, so the box never
/// shows a half-URL that would be wrong if saved back verbatim.
#[test]
fn a_half_configured_store_fills_in_the_documented_defaults() {
    assert_eq!(
        proxy_box_value(Some(&settings(&[("proxy_enabled", "1"), ("proxy_host", "<host>")]))),
        "socks5h://<host>:1080"
    );
    assert_eq!(
        proxy_box_value(Some(&settings(&[("proxy_enabled", "on"), ("proxy_port", "9050")]))),
        "socks5h://127.0.0.1:9050"
    );
}

/// The master OFF and both direct sentinels all mean the same thing to the operator: an EMPTY
/// box. Anything else would show a proxy that is not in force.
#[test]
fn disabled_and_the_direct_sentinels_all_seed_an_empty_box() {
    for off in ["false", "0", "no", "off", "OFF"] {
        let v = settings(&[("proxy_enabled", off), ("proxy_host", "1.2.3.4")]);
        assert!(proxy_box_value(Some(&v)).is_empty(), "{off} must read as no proxy");
    }
    for direct in ["none", "direct", "NONE"] {
        let v = settings(&[("socks_proxy", direct)]);
        assert!(proxy_box_value(Some(&v)).is_empty(), "{direct} must read as no proxy");
    }
}

/// Clearing the box stores `none`, and that wins over an enabled tunnel: the path a user takes when
/// they stop being geo-blocked comes back as an empty box.
#[test]
fn a_cleared_box_wins_over_an_enabled_tunnel() {
    let v = settings(&[("proxy_enabled", "true"), ("socks_proxy", "none")]);
    assert!(proxy_box_value(Some(&v)).is_empty());
}

/// An unconfigured store seeds nothing — a fresh install shows an empty box rather than
/// inventing the localhost tunnel, which is the whole point of the box for a user who is not
/// geo-blocked.
#[test]
fn an_unconfigured_store_seeds_an_empty_box() {
    assert!(proxy_box_value(Some(&settings(&[]))).is_empty());
    assert!(proxy_box_value(Some(&settings(&[("proxy_host", "  ")]))).is_empty());
    assert!(proxy_box_value(None).is_empty());
}
