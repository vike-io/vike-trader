use std::sync::Mutex;

use super::*;

/// A spec answering `answer`, which records whether the request was cancelled when it was asked.
struct Scripted {
    answer: Result<Vec<TickQuote>, TicksError>,
    saw_stop: Mutex<Vec<bool>>,
}

impl TicksSpec for Scripted {
    fn fetch_until(
        &self,
        _symbol: &str,
        _start_ms: i64,
        _end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<TickQuote>, TicksError> {
        self.saw_stop.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(stop());
        self.answer.clone()
    }
}

fn scripted(answer: Result<Vec<TickQuote>, TicksError>) -> Scripted {
    Scripted { answer, saw_stop: Mutex::new(Vec::new()) }
}

/// **The spec's answer reaches the wire with its words unchanged**: a refusal is `Refused`, a venue
/// failure `Fetch`, rows are rows, and a stop is the engine's cancel words naming the spec's stage.
#[test]
fn the_specs_answer_maps_onto_the_wire_words_unchanged() {
    let ctx = FetchContext::new();
    let rows = vec![TickQuote {
        timestamp: 7,
        local_timestamp: 0,
        bid: 1.0,
        ask: 1.1,
        bid_size: 1.5,
        ask_size: 2.0,
        symbol: "EURUSD".to_string(),
    }];
    for (answer, want) in [
        (Ok(rows.clone()), Ok(rows)),
        (
            Err(TicksError::Refused("refused, in the venue's words".into())),
            Err(SourceError::Refused("refused, in the venue's words".into())),
        ),
        (
            Err(TicksError::Fetch("network: connection reset by peer".into())),
            Err(SourceError::Fetch("network: connection reset by peer".into())),
        ),
        (
            Err(TicksError::Stopped("between hour files")),
            Err(SourceError::Fetch("gatewayhub fetch cancelled between hour files".into())),
        ),
        (
            Err(TicksError::Stopped("before the venue was asked")),
            Err(SourceError::Fetch("gatewayhub fetch cancelled before the venue was asked".into())),
        ),
    ] {
        assert_eq!(fetch_ticks(&scripted(answer), "EURUSD", 0, 1, &ctx), want);
    }
}

/// The stop probe the spec is handed IS the request's cancel flag.
#[test]
fn the_stop_probe_reads_the_requests_cancel_flag() {
    let spec = scripted(Ok(Vec::new()));
    let ctx = FetchContext::new();
    fetch_ticks(&spec, "EURUSD", 0, 1, &ctx).expect("running");
    ctx.cancel();
    fetch_ticks(&spec, "EURUSD", 0, 1, &ctx).expect("the scripted spec answers anyway");
    assert_eq!(
        *spec.saw_stop.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        [false, true]
    );
}
