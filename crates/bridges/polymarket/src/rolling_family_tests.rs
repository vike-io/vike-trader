use super::*;
use crate::gamma::GammaMarket;

#[test]
fn a_family_names_its_window_slugs() {
    let spec = window_spec_for("btc-updown-5m").unwrap();
    assert_eq!(spec.bucket_ms, 300_000);
    // 1970-01-01T00:07:30Z floors to the 00:05 window ⇒ unix 300.
    assert_eq!(spec.window_slugs(450_000)[0], "btc-updown-5m-300");
}

/// The family key must be exactly what the venue calls the series, so a customer can verify it
/// by pasting the rendered slug into polymarket.com. This pins that `btc-updown-5m` produces
/// byte-identical slugs to the venue's own `WindowSpec::updown("btc", 300_000)` helper.
#[test]
fn the_family_key_agrees_with_the_venues_own_updown_spec() {
    let ours = window_spec_for("btc-updown-5m").unwrap();
    let theirs = WindowSpec::updown("btc", 300_000);
    assert_eq!(ours.slug_template, theirs.slug_template);
    assert_eq!(ours.bucket_ms, theirs.bucket_ms);
    assert_eq!(ours.window_slugs(1_700_000_000_000), theirs.window_slugs(1_700_000_000_000));
}

/// An hourly series is spelled `60m` by Gamma, not `1h`. Accepting `1h` here would build slugs
/// that resolve to nothing and record silence.
#[test]
fn only_the_minute_spelling_is_accepted() {
    assert_eq!(trailing_interval_minutes("eth-updown-60m"), Some(60));
    assert_eq!(trailing_interval_minutes("eth-updown-1h"), None);
    assert_eq!(trailing_interval_minutes("btc-updown-0m"), None, "a zero window is not a window");
    assert_eq!(trailing_interval_minutes("some-event-slug"), None);
    assert!(window_spec_for("eth-updown-1h").unwrap_err().contains("`-5m`"));
}

struct FixtureGamma(Vec<GammaMarket>);

impl GammaSource for FixtureGamma {
    fn list(&self, _a: bool, _l: usize, _o: usize) -> Result<Vec<GammaMarket>, String> {
        Ok(self.0.clone())
    }
    fn by_slug(&self, slug: &str, _s: FetchSpec) -> Result<Option<GammaMarket>, String> {
        Ok(self.0.iter().find(|m| m.slug == slug).cloned())
    }
}

struct FailingGamma;

impl GammaSource for FailingGamma {
    fn list(&self, _a: bool, _l: usize, _o: usize) -> Result<Vec<GammaMarket>, String> {
        Err("gamma: connection reset".into())
    }
    fn by_slug(&self, _slug: &str, _s: FetchSpec) -> Result<Option<GammaMarket>, String> {
        Err("gamma: connection reset".into())
    }
}

fn market(slug: &str, tokens: &[&str]) -> GammaMarket {
    GammaMarket {
        slug: slug.into(),
        condition_id: format!("0x{slug}"),
        active: true,
        tick_size: 0.01,
        outcomes: vec!["Up".into(), "Down".into()],
        token_ids: tokens.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn family(gamma: Box<dyn GammaSource + Send>) -> RollingFamily {
    RollingFamily::with_source("btc-updown-5m", gamma, FetchSpec::default()).unwrap()
}

/// The current window plus the look-ahead one — a recorder must already be streaming the next
/// window's book when the current one expires, or every rotation starts with a hole.
#[test]
fn the_desired_set_is_the_current_plus_next_windows_tokens() {
    let mut f = family(Box::new(FixtureGamma(vec![
        market("btc-updown-5m-300", &["A_UP", "A_DOWN"]),
        market("btc-updown-5m-600", &["B_UP", "B_DOWN"]),
    ])));

    let got = f.desired(450_000).unwrap();

    assert_eq!(got, ["A_DOWN", "A_UP", "B_DOWN", "B_UP"].iter().map(|s| s.to_string()).collect());
}

/// A future window Gamma has not listed yet is NORMAL — it is simply absent and retried, not an
/// error that would freeze the whole subscription.
#[test]
fn an_unlisted_future_window_is_not_an_error() {
    let mut f = family(Box::new(FixtureGamma(vec![market("btc-updown-5m-300", &["A_UP"])])));

    let got = f.desired(450_000).unwrap();

    assert_eq!(got, ["A_UP"].iter().map(|s| s.to_string()).collect());
}

/// A Gamma outage must surface as `Err` — the runtime reads that as UNKNOWN and changes nothing.
/// Returning an empty set here would unsubscribe every live book mid-outage.
#[test]
fn a_gamma_outage_is_an_error_not_an_empty_set() {
    let mut f = family(Box::new(FailingGamma));
    assert!(f.desired(450_000).is_err());
}
