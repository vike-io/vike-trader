use super::*;

use AccountTier::{Demo, Live, Paper};

/// The answer rendered as one word, so the shape table reads as a table.
fn answer(tiers: &[AccountTier], demo_row: Option<&str>, live_row: Option<&str>) -> String {
    let mut asked = Vec::new();
    let got = ibkr_session(tiers.iter().copied(), |tier| {
        asked.push(tier);
        match tier {
            Demo => demo_row,
            Live => live_row,
            Paper => panic!("the resolver never asks for a paper tier's backend row"),
        }
    });
    assert!(asked.len() <= 1, "the backend row is read at most once: {asked:?}");
    match got {
        Ok(IbkrSession::Off) => "off".to_string(),
        Ok(IbkrSession::Conflict) => "conflict".to_string(),
        Ok(IbkrSession::Pair { backend, tier }) => {
            assert_eq!(asked, vec![tier], "the row read is the RESOLVED tier's");
            format!("{backend}/{tier}")
        }
        Err(e) => format!("err {}={}", e.tier, e.word),
    }
}

/// One shape: the active tiers, the demo and live backend rows, the answer, and why.
type Shape = (
    &'static [AccountTier],
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    &'static str,
);

/// **Every shape of rows, one answer each** (decision 0140, item 1): off, the one pair, or a
/// conflict — and the backend row of the resolved tier, never the other tier's.
#[test]
fn ibkr_session_answers_every_shape_of_rows() {
    let rows: &[Shape] = &[
        (&[], None, None, "off", "no active row"),
        (&[Paper], Some("cpapi"), None, "off", "a paper row states no session"),
        (&[Paper, Paper], None, None, "off", "paper rows only"),
        (&[Demo], None, None, "socket/demo", "no backend row: socket"),
        (&[Demo], Some(""), None, "socket/demo", "a blank backend row reads as none"),
        (&[Demo], Some(" CPAPI "), None, "cpapi/demo", "trimmed, case-insensitive"),
        (&[Demo], Some("socket"), Some("cpapi"), "socket/demo", "the LIVE row is not demo's"),
        (&[Live], Some("cpapi"), None, "socket/live", "the DEMO row is not live's"),
        (&[Live], None, Some("cpapi"), "cpapi/live", "live reads its own row"),
        (&[Live], None, Some("oauth"), "oauth/live", "oauth parses; the mount refuses it"),
        (&[Demo, Paper], Some("cpapi"), None, "cpapi/demo", "a paper row never conflicts"),
        (&[Demo, Demo], None, None, "socket/demo", "two rows of ONE tier are one answer"),
        (&[Demo, Live], Some("cpapi"), Some("socket"), "conflict", "demo AND live: no pick"),
        (&[Live, Paper, Demo], None, None, "conflict", "order does not matter"),
        (&[Demo, Live], Some("nonsense"), None, "conflict", "a conflict reads no backend row"),
        (&[], Some("nonsense"), None, "off", "off reads no backend row"),
        (&[Demo], Some("tws"), None, "err demo=tws", "an unreadable row is refused, not defaulted"),
        (&[Live], None, Some("gateway"), "err live=gateway", "…on either tier"),
    ];
    for (tiers, demo_row, live_row, want, why) in rows {
        assert_eq!(answer(tiers, *demo_row, *live_row), *want, "{why}: {tiers:?}");
    }
}

#[test]
fn the_backend_word_round_trips_through_the_one_parser() {
    for backend in IbkrBackend::ALL {
        assert_eq!(IbkrBackend::parse(backend.as_str()), Some(backend));
        assert_eq!(IbkrBackend::parse(&backend.as_str().to_ascii_uppercase()), Some(backend));
        assert_eq!(backend.to_string(), backend.as_str());
    }
    assert_eq!(IbkrBackend::default(), IbkrBackend::Socket);
    assert_eq!(IbkrBackend::parse(""), Some(IbkrBackend::Socket));
    assert_eq!(IbkrBackend::parse("tws"), None);
}

#[test]
fn an_unreadable_backend_row_names_the_row_and_the_word() {
    let e = NotAnIbkrBackend { tier: Live, word: "tws".to_string() };
    let text = e.to_string();
    assert!(text.contains("venue.ibkr.live.backend") && text.contains("`tws`"), "{text}");
}
