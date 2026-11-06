use super::*;

/// The sink method each variant stands for. An EXHAUSTIVE `match` kept in the test on purpose: the
/// production table spells no method name (a `"book"` literal in non-test code is the shape the
/// stream-label literal gate polices), and a variant added without an arm here does not compile.
fn method(verb: SinkVerb) -> &'static str {
    match verb {
        SinkVerb::SeedBars => "seed_bars",
        SinkVerb::CloseBar => "close_bar",
        SinkVerb::FormingBar => "forming_bar",
        SinkVerb::MarkTick => "mark_tick",
        SinkVerb::BarCloseTick => "bar_close_tick",
        SinkVerb::L2Snapshot => "l2_snapshot",
        SinkVerb::Quote => "quote",
        SinkVerb::Trade => "trade",
        SinkVerb::Book => "book",
        SinkVerb::BookUpdate => "book_update",
        SinkVerb::StreamStatus => "stream_status",
        SinkVerb::DataDelay => "data_delay",
    }
}

/// **THE EXHAUSTIVENESS TEST: every method of `LiveDataSink` has a class, and the table names no
/// method the trait does not have.** It reads the trait's own source (`live.rs`), because a trait has
/// no reflection and a verb added to it must not be able to slip past the tubes that classify frames
/// by this table: that verb would otherwise be delivered by whatever a tube's catch-all does. This
/// fails the day a method is added to `LiveDataSink` without a `sink_verbs!` row, and the day a row
/// outlives its method.
#[test]
fn the_table_names_every_method_of_the_sink_and_nothing_else() {
    let src = include_str!("live.rs");
    let start = src.find("pub trait LiveDataSink").expect("live.rs declares the trait");
    let body = &src[start..];
    let body = &body[..body.find("\n}\n").expect("the trait body ends")];
    let mut in_trait: Vec<&str> = body
        .lines()
        .filter_map(|l| l.strip_prefix("    fn "))
        .filter_map(|l| l.split(['(', '<']).next())
        .collect();
    in_trait.sort_unstable();
    let mut in_table: Vec<&str> = SinkVerb::ALL.iter().map(|v| method(*v)).collect();
    in_table.sort_unstable();
    assert_eq!(
        in_trait, in_table,
        "LiveDataSink's methods and delivery.rs's sink_verbs! rows must be the same set: a new \
         verb needs a class (lossless / conflatable / control) and a recorded flag"
    );
    // The scan found the trait and not an empty body.
    assert_eq!(in_trait.len(), 12, "the twelve sink verbs");
}

/// **THE PIN: which verb is which class.** Spelled independently of the table, from the sink's
/// documented contracts (`live.rs`: `close_bar` is "the lossless lane (a missed close is a series
/// hole)", `forming_bar` is "the conflating lane (latest-wins)", `quote`/`trade`/`book` are the
/// lossless tick lane, `l2_snapshot` is "the DOM's conflating book lane", status and delay ride the
/// control lane). This is the table the gatewayhub queue's `Verb` carried; changing a row here
/// changes what a lagging reader loses.
#[test]
fn the_classes_are_the_sinks_documented_contracts() {
    let lossless = ["seed_bars", "close_bar", "quote", "trade", "book", "book_update"];
    let conflatable = ["forming_bar", "mark_tick", "bar_close_tick", "l2_snapshot"];
    let control = ["stream_status", "data_delay"];
    for v in SinkVerb::ALL {
        let name = method(*v);
        let want = if lossless.contains(&name) {
            Delivery::Lossless
        } else if conflatable.contains(&name) {
            Delivery::Conflatable
        } else if control.contains(&name) {
            Delivery::Control
        } else {
            panic!("{name} is in no documented class: classify it in this test too");
        };
        assert_eq!(v.base(), want, "{name}");
    }
    assert_eq!(lossless.len() + conflatable.len() + control.len(), SinkVerb::ALL.len());
}

/// The recorded set: the verbs `RecorderSink` writes into a `kind=` series. `forming_bar`,
/// `mark_tick` and `bar_close_tick` are its no-ops (`kind=mark` is deferred, the recorder records no
/// bars), `book` is folded by the recorder from `book_update`, and it stores no delay label.
#[test]
fn the_recorded_verbs_are_the_ones_the_recorder_writes() {
    let recorded: Vec<&str> =
        SinkVerb::ALL.iter().filter(|v| v.recorded()).map(|v| method(*v)).collect();
    assert_eq!(recorded, ["l2_snapshot", "quote", "trade", "book_update", "stream_status"]);
}

#[test]
fn stronger_is_the_maximum_and_the_order_is_conflatable_lossless_control() {
    assert!(Delivery::Conflatable < Delivery::Lossless && Delivery::Lossless < Delivery::Control);
    let all = [Delivery::Conflatable, Delivery::Lossless, Delivery::Control];
    for a in all {
        for b in all {
            assert_eq!(stronger(a, b), a.max(b), "{a:?} vs {b:?}");
            assert_eq!(stronger(a, b), stronger(b, a), "symmetric");
        }
    }
}

/// **A floor only strengthens; a recorder never has a recorded kind superseded.** `l2_snapshot` is the
/// one recorded verb that is conflatable, and for a recorder it is lossless; the recorder's
/// unrecorded verbs keep their base (a recorder holder does not get `forming_bar` lossless).
#[test]
fn a_recorder_floor_strengthens_the_recorded_verbs_and_only_those() {
    assert_eq!(SinkVerb::L2Snapshot.base(), Delivery::Conflatable);
    assert_eq!(SinkVerb::L2Snapshot.for_recorder(), Delivery::Lossless);
    for v in SinkVerb::ALL {
        let name = method(*v);
        assert!(v.for_recorder() >= v.base(), "{name}: a floor never weakens");
        if v.recorded() {
            assert!(
                v.for_recorder() >= RECORDER_FLOOR,
                "{name}: a recorded kind is never superseded"
            );
        } else {
            assert_eq!(v.for_recorder(), v.base(), "{name}: an unrecorded verb keeps its base");
        }
        assert_eq!(
            v.under(Delivery::Conflatable),
            v.base(),
            "{name}: the weakest floor is a no-op"
        );
        assert!(v.under(Delivery::Control) == Delivery::Control, "{name}");
    }
    // The control verbs stay control under the recorder's floor.
    assert_eq!(SinkVerb::StreamStatus.for_recorder(), Delivery::Control);
}

/// **The lane classes the broker applies.** Only `Depth` differs between the hub and the recorder:
/// conflatable for the first, lossless for the second. Every other lane is lossless for both, so a
/// broker that calls `set_delivery` only when a holder's need differs from the lane's default never
/// touches them.
#[test]
fn only_the_depth_lane_differs_between_the_hub_and_the_recorder() {
    for lane in StreamLabel::ALL {
        let (hub, rec) = (lane_delivery(*lane, false), lane_delivery(*lane, true));
        assert!(rec >= hub, "{lane:?}");
        match lane {
            StreamLabel::Depth => {
                assert_eq!((hub, rec), (Delivery::Conflatable, Delivery::Lossless));
            }
            StreamLabel::Quotes | StreamLabel::Trades | StreamLabel::Book => {
                assert_eq!((hub, rec), (Delivery::Lossless, Delivery::Lossless), "{lane:?}");
            }
        }
    }
}

/// **Every recorded data verb belongs to some lane.** A recorded verb no lane lists would escape the
/// broker's class logic (no holder's need would ever raise it), so adding one to the table without a
/// lane is a failure here and not a silently thinner tape.
#[test]
fn every_recorded_data_verb_is_delivered_by_some_lane() {
    for v in SinkVerb::ALL.iter().filter(|v| v.recorded() && v.base() != Delivery::Control) {
        assert!(
            StreamLabel::ALL.iter().any(|l| lane_verbs(*l).contains(v)),
            "{} is recorded and no lane delivers it",
            method(*v)
        );
    }
}
