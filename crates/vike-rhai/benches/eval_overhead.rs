use std::time::Instant;
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderListLeg, OrderPlacement, Strategy};
use vike_rhai::RhaiStrategy;
use vike_rhai::test_support::bar;

#[derive(Default)]
struct NopBroker {
    bars: Vec<Bar>,
}
// a script's orders go through the order seam: placed here, never filled, so one resting order
// under one name is amended each bar the cross holds
impl OrderBroker for NopBroker {
    fn place(&mut self, _s: &str, _t: &str, _side: i32, _q: f64, _o: OrderPlacement, _r: bool) {}
    fn place_group(&mut self, _s: &str, _legs: Vec<(String, OrderListLeg)>) {}
    fn amend(&mut self, _t: &str, _q: Option<f64>, _p: Option<f64>, _g: Option<f64>) {}
    fn cancel(&mut self, _t: &str) {}
    fn deny(&mut self, _s: &str, _t: &str, _q: f64, _r: &str) {}
    fn average_price(&self, _s: &str) -> f64 {
        0.0
    }
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
fn main() {
    let n = 200_000;
    let mut strat = RhaiStrategy::<NopBroker>::compile(
        r#"fn on_bar(strategy) {
            let f = sma(5); let s = sma(20);
            if f > s { strategy.order("buy", OrderSide::Long).size(1.0); }
        }"#,
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
    println!("vike-rhai on_bar (sma x2 + verb): {per:.0} ns/bar over {n} bars");
    println!("(compiled-Rust twin would be ~single-digit ns; this quantifies the Rhai tier cost)");
}
