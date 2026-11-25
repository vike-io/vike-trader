use super::*;

/// [`welcome_plane`] names a plane only when the `Welcome` names exactly ONE: a pre-split daemon (both
/// sentinels) and an unknown peer (neither) answer `None`, and so does a feature that merely CONTAINS a
/// sentinel: capability strings are compared whole, never by substring.
#[test]
fn a_welcome_names_a_plane_only_when_it_names_exactly_one() {
    let f = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let auth = crate::auth::FEATURE_AUTH;
    assert_eq!(welcome_plane(&f(&[BACKTEST_PLANE_SENTINEL, auth])), Some(Plane::Backtest));
    assert_eq!(welcome_plane(&f(&[DATA_PLANE_SENTINEL, auth])), Some(Plane::Data));
    // A pre-split daemon serves BOTH planes, so it is neither: a Write session there carries the store
    // verbs.
    assert_eq!(welcome_plane(&f(&[BACKTEST_PLANE_SENTINEL, DATA_PLANE_SENTINEL, auth])), None);
    assert_eq!(welcome_plane(&f(&[auth])), None);
    assert_eq!(welcome_plane(&[]), None);
    // Whole-string equality: a longer token that happens to start with a sentinel is not it.
    assert_eq!(welcome_plane(&f(&["backtest_v2", "load_bars_v2"])), None);
    assert_ne!(BACKTEST_PLANE_SENTINEL, DATA_PLANE_SENTINEL);
}

/// The tag is read off both shapes an externally-tagged enum encodes to, and off nothing else: a body
/// with no such shape is not a verb of any plane.
#[test]
fn a_frame_tag_is_the_outer_key_or_the_bare_string() {
    assert_eq!(frame_tag(br#""ListStrategies""#).as_deref(), Some("ListStrategies"));
    assert_eq!(frame_tag(br#"{"RunBacktest":"[data]"}"#).as_deref(), Some("RunBacktest"));
    assert_eq!(
        frame_tag(br#"{"RunParamscan":{"spec":{"Rhai":"x"},"paramscan":{"axes":[]}}}"#).as_deref(),
        Some("RunParamscan")
    );
    for not_a_tag in [&b"not json"[..], b"5", b"[]", b"{}", b"null", br#"{"a":1,"b":2}"#, b""] {
        assert_eq!(frame_tag(not_a_tag), None, "{}", String::from_utf8_lossy(not_a_tag));
    }
}

/// ⚠ **The refusal's decider.** A verb of the other plane is named whatever its payload, a verb of this
/// plane is not foreign, and neither the shared frames nor an unknown tag are on any roster: those stay
/// the undecodable refusal a newer peer has always been told.
#[test]
fn a_foreign_verb_is_the_other_planes_and_nothing_else() {
    assert_eq!(foreign_verb(br#"{"RunBacktest":"x"}"#, Plane::Data), Some("RunBacktest"));
    assert_eq!(foreign_verb(br#""ListStrategies""#, Plane::Data), Some("ListStrategies"));
    assert_eq!(foreign_verb(br#"{"RunBacktest":5}"#, Plane::Data), Some("RunBacktest"));
    assert_eq!(foreign_verb(br#"{"LoadBars":{}}"#, Plane::Backtest), Some("LoadBars"));
    assert_eq!(foreign_verb(br#""Inventory""#, Plane::Backtest), Some("Inventory"));

    assert_eq!(foreign_verb(br#"{"RunBacktest":"x"}"#, Plane::Backtest), None, "its own verb");
    assert_eq!(foreign_verb(br#"{"LoadBars":{}}"#, Plane::Data), None, "its own verb");
    for shared_or_unknown in
        [&br#""Ping""#[..], br#"{"Hello":{}}"#, br#"{"Auth":{}}"#, br#""Bogus""#]
    {
        assert_eq!(foreign_verb(shared_or_unknown, Plane::Data), None);
        assert_eq!(foreign_verb(shared_or_unknown, Plane::Backtest), None);
    }
}

/// The two rosters are disjoint, or a verb would be refused by both daemons.
#[test]
fn no_verb_is_on_both_planes() {
    for verb in Plane::Backtest.verbs() {
        assert!(!Plane::Data.verbs().contains(verb), "{verb} is on both planes");
    }
}

/// The refusal's exact sentence, both ways. The bytes each daemon really answers are frozen in
/// `fixtures/datahub_wire/` and read by the daemons' own suites; this is the text at its source.
#[test]
fn the_refusal_names_the_verb_the_command_and_the_key() {
    assert_eq!(
        wrong_plane_message("RunBacktest", Plane::Data, Plane::Backtest),
        "RunBacktest is a Backtest-plane verb and this is the Data daemon — it is served by \
         `vike-backend backtesthub --addr`. Point the client at that daemon \
         (config.backtest_addr); this connection stays open and every Data-plane verb still works \
         on it."
    );
    assert_eq!(
        wrong_plane_message("LoadBars", Plane::Backtest, Plane::Data),
        "LoadBars is a Data-plane verb and this is the Backtest daemon — it is served by \
         `vike-backend datahub`. Point the client at that daemon (config.datahub_addr); this \
         connection stays open and every Backtest-plane verb still works on it."
    );
}
