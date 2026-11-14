use crate::proto::*;
use crate::wire_studio::{DataSliceWire, ReplayKindWire, StrategySpecWire, WalkforwardWire};
use vike_hub_protocol::auth::{Scope, VerbScope, scope_admits};
use vike_hub_protocol::conn::ReplyWait;
use vike_hub_protocol::plane::Plane;

/// ⚠ THE THREE CLASSIFIERS, asserted rather than trusted to the compiler. Each of them is
/// exhaustive, so a missing arm is a build failure — but WHICH arm was chosen is a judgement,
/// and these are the judgements: a study WRITES a run directory on the backend (Control, which
/// `crates/vike-cli/src/cmd/study.rs`'s connect already negotiates under), and it names itself in a
/// refusal.
#[test]
fn the_study_verb_is_control_scoped() {
    let r = Request::RunStudy(Box::new(StudyRunRequestWire {
        study: "cohort".to_string(),
        recipe_toml: String::new(),
        from: "1".to_string(),
        to: "2".to_string(),
    }));
    assert_eq!(request_kind(&r), "RunStudy");
    assert_eq!(required_scope(&r), VerbScope::Write);
}

/// The variant each request maps to, through a match with NO `_` arm — so a new verb fails to
/// COMPILE here until it is given a row, and then fails
/// [`every_verb_names_itself_by_its_wire_tag`] until it is given a sample.
fn variant_index(r: &Request) -> usize {
    match r {
        Request::Hello { .. } => 0,
        Request::Auth { .. } => 1,
        Request::Ping => 2,
        Request::RunBacktest(_) => 3,
        Request::RunSlice { .. } => 4,
        Request::RunParamscan { .. } => 5,
        Request::RunWalkforward { .. } => 6,
        Request::RunParamscanProfile { .. } => 7,
        Request::RunWalkforwardProfile { .. } => 8,
        Request::RunStudy(_) => 9,
        Request::ListStrategies => 10,
        Request::NamedStrategies => 11,
        Request::RunNamed(_) => 12,
        Request::ListStrategyFiles => 13,
        Request::ReadStrategyFile { .. } => 14,
        Request::SaveStrategyFile { .. } => 15,
    }
}

/// How many [`Request`] variants [`variant_index`] numbers.
const REQUEST_VARIANTS: usize = 16;

/// One sample of EVERY [`Request`] variant.
fn one_of_every_request() -> Vec<Request> {
    let slice = || {
        Box::new(DataSliceWire {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: None,
            end: None,
            kind: ReplayKindWire::Bars,
        })
    };
    let spec = || StrategySpecWire::Rhai("fn on_bar() {}".to_string());
    vec![
        Request::Hello { proto_version: PROTO_VERSION },
        Request::Auth { scope: Scope::Read, mac: vec![0u8; 32] },
        Request::Ping,
        Request::RunBacktest(String::new()),
        Request::RunSlice { spec: spec(), slice: slice(), params: None },
        Request::RunParamscan {
            spec: spec(),
            slice: slice(),
            paramscan: crate::wire_studio::ParamscanWire { axes: vec![] },
            params: None,
        },
        Request::RunWalkforward {
            spec: spec(),
            slice: slice(),
            walkforward: WalkforwardWire::fixed(2),
            params: None,
        },
        Request::RunParamscanProfile { profile_toml: String::new(), rank_by: None, search: None },
        Request::RunWalkforwardProfile { profile_toml: String::new() },
        Request::RunStudy(Box::new(StudyRunRequestWire {
            study: "cohort".to_string(),
            recipe_toml: String::new(),
            from: "1".to_string(),
            to: "2".to_string(),
        })),
        Request::ListStrategies,
        Request::NamedStrategies,
        Request::RunNamed(Box::new(crate::named_run::NamedRunSpec {
            strategy: "buy_hold".to_string(),
            params: Vec::new(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
            start: 0,
            end: 3_600_000,
        })),
        Request::ListStrategyFiles,
        Request::ReadStrategyFile { file: "trend/momentum.rs".to_string() },
        Request::SaveStrategyFile {
            file: "trend/momentum.rs".to_string(),
            text: "//! x\n".to_string(),
        },
    ]
}

/// A request's WIRE TAG, read off its encoding — the externally-tagged key, or the bare string of a
/// unit variant.
fn wire_tag(r: &Request) -> String {
    match serde_json::to_value(r).expect("every request encodes") {
        serde_json::Value::String(s) => s,
        serde_json::Value::Object(m) => {
            assert_eq!(m.len(), 1, "an externally-tagged request has ONE outer key: {m:?}");
            m.keys().next().expect("one key").clone()
        }
        other => panic!("a request encoded as neither a tag nor a tagged object: {other}"),
    }
}

/// ⚠ **A NEW VERB CANNOT BE MISSED.** [`variant_index`] has no `_` arm, so a variant added to
/// [`Request`] fails to compile until it is numbered; this test then fails until the sample set
/// carries it — and for every sample it holds the exhaustive classifiers to one rule a compiler cannot
/// check: [`request_kind`] names the verb by the tag the WIRE carries, the one an operator can grep a
/// capture for.
#[test]
fn every_verb_names_itself_by_its_wire_tag() {
    let samples = one_of_every_request();
    let mut seen = [false; REQUEST_VARIANTS];
    for r in &samples {
        let i = variant_index(r);
        assert!(i < REQUEST_VARIANTS, "REQUEST_VARIANTS is stale: {r:?} maps to {i}");
        seen[i] = true;
        assert_eq!(request_kind(r), wire_tag(r), "request_kind must name the WIRE tag: {r:?}");
        // Exhaustive and total: neither may panic on any variant.
        let _ = (required_scope(r), reply_wait(r));
    }
    let missing: Vec<usize> = (0..REQUEST_VARIANTS).filter(|i| !seen[*i]).collect();
    assert!(missing.is_empty(), "variant indices with no sample: {missing:?}");
}

/// The tags serde itself says this enum has, off the `expected one of ...` list of an unknown tag. That
/// list is what a daemon of this plane refuses a frame with, so reading it here means the test sees the
/// enum the wire sees, and no sample list can go stale.
fn the_enums_own_tags() -> Vec<String> {
    let err = serde_json::from_str::<Request>(r#""__no_such_verb__""#)
        .expect_err("an unknown tag does not decode")
        .to_string();
    assert!(err.contains("unknown variant `__no_such_verb__`"), "{err}");
    let list = err.split("expected one of").nth(1).expect("serde lists the variants");
    list.split('`').skip(1).step_by(2).map(str::to_string).collect()
}

/// ⚠ **THE WRONG-PLANE REFUSAL STANDS ON THIS.** The DATA daemon names a verb of this plane only if the
/// tag is on `vike_hub_protocol::plane`'s roster, and that roster cannot name this enum's variants by
/// itself. So: this enum's tags are exactly that roster plus the three frames both daemons serve. A verb
/// added here without its row would be answered `undecodable` by the other daemon, a parse failure where
/// an operator is owed the address.
#[test]
fn this_enum_is_the_backtest_planes_roster_and_the_shared_frames() {
    let mut own = the_enums_own_tags();
    own.sort();
    let mut roster: Vec<String> = Plane::Backtest.verbs().iter().map(|v| v.to_string()).collect();
    roster.extend(["Hello", "Auth", "Ping"].map(str::to_string));
    roster.sort();
    assert_eq!(own, roster);
}

/// ⚠ WHAT SERDE DOES WITH A VERB OF THE OTHER PLANE, the fact the refusal is built on: it is an `Err`
/// naming the tag, for a unit tag and a tagged object alike, never a panic and never a default.
#[test]
fn a_verb_of_the_other_plane_does_not_decode_here() {
    for body in [&br#""Inventory""#[..], br#"{"LoadBars":{"venue":"x"}}"#] {
        let err = serde_json::from_slice::<Request>(body).expect_err("not this enum's").to_string();
        assert!(err.contains("unknown variant"), "{err}");
    }
    assert_eq!(
        vike_hub_protocol::plane::foreign_verb(br#""Inventory""#, Plane::Backtest),
        Some("Inventory")
    );
}

/// ⚠ WHICH VERBS WAIT UNBOUNDED, pinned verb by verb over the full sample set: [`reply_wait`] is
/// exhaustive, so a new verb cannot compile without a class, but WHICH class it got is a judgement. A
/// verb moved INTO `Unbounded` parks its caller on a dead link; one moved OUT clips a live run at 300 s.
#[test]
fn only_the_client_sized_verbs_wait_unbounded() {
    let unbounded = [
        "RunBacktest",
        "RunSlice",
        "RunParamscan",
        "RunWalkforward",
        "RunParamscanProfile",
        "RunWalkforwardProfile",
        "RunStudy",
        "RunNamed",
    ];
    for r in one_of_every_request() {
        let kind = request_kind(&r);
        let expected = match kind {
            "Hello" | "Auth" => ReplyWait::Handshake,
            k if unbounded.contains(&k) => ReplyWait::Unbounded,
            _ => ReplyWait::Bounded,
        };
        assert_eq!(reply_wait(&r), expected, "{kind}");
    }
}

/// **Who may call each verb, per authenticated scope**: every verb of the sample set against every
/// `Scope`. `Scope::Trade` sends nothing here and `Scope::Account` sends nothing.
#[test]
fn who_may_call_each_verb_is_pinned_per_scope() {
    // `request_kind`'s WIRE names (`RunSweep` is `Request::RunParamscan`).
    const WRITE: [&str; 8] = [
        "RunBacktest",
        "RunSlice",
        "RunParamscan",
        "RunWalkforward",
        "RunParamscanProfile",
        "RunWalkforwardProfile",
        "RunStudy",
        "SaveStrategyFile",
    ];
    let kinds: Vec<&str> = one_of_every_request().iter().map(request_kind).collect();
    for w in WRITE {
        assert!(kinds.contains(&w), "the sample set lacks the Write verb {w}");
    }
    for r in one_of_every_request() {
        let kind = request_kind(&r);
        let callers: &[Scope] = match kind {
            "Hello" | "Auth" => &[],
            k if WRITE.contains(&k) => &[Scope::Write],
            _ => &[Scope::Read, Scope::Write],
        };
        for authed in [Scope::Read, Scope::Write, Scope::Account, Scope::Trade] {
            let admitted = scope_admits(authed, required_scope(&r));
            assert_eq!(admitted, callers.contains(&authed), "{authed:?} {kind}");
        }
    }
}
