use std::time::Instant;
use vike_model::{Bar, Broker, Strategy};
use vike_script::RhaiStrategy;

#[derive(Default)]
struct NopBroker {
    bars: Vec<Bar>,
}
impl Broker for NopBroker {
    fn submit_market(&mut self, _s: &str, _side: i32, _q: f64) {}
    fn submit_limit(&mut self, _s: &str, _side: i32, _q: f64, _p: f64) {}
    fn position(&self, _s: &str) -> f64 {
        0.0
    }
    fn price(&self, _s: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _s: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
}
fn bar(c: f64) -> Bar {
    Bar {
        ts: 0,
        open: c,
        high: c,
        low: c,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("X".into()),
    }
}

fn main() {
    let n = 200_000;
    let mut strat = RhaiStrategy::<NopBroker>::compile(
        "fn on_bar() { let f = sma(5); let s = sma(20); if f > s { buy(1.0); } }",
    )
    .unwrap();
    let mut b = NopBroker::default();
    b.bars.push(bar(100.0));
    // warm
    for _ in 0..1000 {
        strat.on_bar(&mut b, &bar(100.0));
    }
    let t = Instant::now();
    for i in 0..n {
        strat.on_bar(&mut b, &bar(100.0 + (i % 7) as f64));
    }
    let per = t.elapsed().as_nanos() as f64 / n as f64;
    println!("vike-script on_bar (sma x2 + verb): {per:.0} ns/bar over {n} bars");
    println!("(compiled-Rust twin would be ~single-digit ns; this quantifies the Rhai tier cost)");
}
