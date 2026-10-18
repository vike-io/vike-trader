use super::*;

/// Records which hooks fired via a SHARED counter (`Rc<Cell<_>>`), so the boxed-forwarding
/// test can keep an outside handle and observe what the strategy INSIDE the box actually saw
/// — proving the blanket impl really forwards, not just that the box compiles.
#[derive(Default, Clone)]
struct RecordingStrategy {
    bar_calls: std::rc::Rc<std::cell::Cell<usize>>,
    quote_calls: std::rc::Rc<std::cell::Cell<usize>>,
    last_bar_close: std::rc::Rc<std::cell::Cell<f64>>,
}

impl Strategy<MockBroker> for RecordingStrategy {
    fn on_bar(&mut self, _broker: &mut MockBroker, bar: &Bar) {
        self.bar_calls.set(self.bar_calls.get() + 1);
        self.last_bar_close.set(bar.close);
    }
    fn on_quote_tick(&mut self, _broker: &mut MockBroker, _q: &QuoteTick) {
        self.quote_calls.set(self.quote_calls.get() + 1);
    }
}

fn bar(close: f64) -> Bar {
    Bar {
        ts: 0,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn quote() -> QuoteTick {
    QuoteTick {
        ts: 0,
        local_ts: 0,
        bid: 1.0,
        ask: 1.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    }
}

#[test]
fn boxed_strategy_forwards_on_bar_and_on_quote_tick_to_the_inner_strategy() {
    let recorder = RecordingStrategy::default();
    // `Box<dyn Strategy<MockBroker>>` must itself satisfy `Strategy<MockBroker>` — this line
    // only compiles because of the blanket impl.
    let mut boxed: Box<dyn Strategy<MockBroker>> = Box::new(recorder.clone());
    let mut broker = MockBroker::default();

    boxed.on_bar(&mut broker, &bar(123.5));
    boxed.on_quote_tick(&mut broker, &quote());
    boxed.on_quote_tick(&mut broker, &quote());

    // `recorder`'s shared cells see what the strategy INSIDE the box observed.
    assert_eq!(recorder.bar_calls.get(), 1);
    assert_eq!(recorder.quote_calls.get(), 2);
    assert_eq!(recorder.last_bar_close.get(), 123.5);
}

#[test]
fn boxed_strategy_forwards_save_and_load_state() {
    // a tiny strategy that stores a counter in its state
    struct S {
        n: i64,
    }
    impl<B: Broker> Strategy<B> for S {
        fn save_state(&self) -> Option<serde_json::Value> {
            Some(serde_json::json!({ "n": self.n }))
        }
        fn load_state(&mut self, state: &serde_json::Value) {
            self.n = state["n"].as_i64().unwrap_or(0);
        }
    }
    let mut boxed: Box<dyn Strategy<MockBroker>> = Box::new(S { n: 7 });
    // save through the box must reach S, not the default None
    assert_eq!(boxed.save_state(), Some(serde_json::json!({ "n": 7 })));
    // load through the box must reach S, not the default no-op
    boxed.load_state(&serde_json::json!({ "n": 42 }));
    assert_eq!(boxed.save_state(), Some(serde_json::json!({ "n": 42 })));
}
