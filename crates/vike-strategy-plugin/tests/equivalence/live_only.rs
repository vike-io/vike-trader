//! The narrower witness for the six live-only hooks no backtest engine emits.

use std::path::Path;

use vike_model::{QuoteTick, Strategy};
use vike_strategy_plugin::abi;
use vike_strategy_plugin::host::PluginStrategy;

use super::common::RecordingBroker;
use super::drive::load_or_panic;
use super::{PARAMS_TOML, SYMBOL};

// ---------------------------------------------------------------------------------------------
// The narrower witnesses — the six hooks no backtest engine emits
// ---------------------------------------------------------------------------------------------

/// The six hooks the equivalence comparison CANNOT prove, and what is done about that instead.
///
/// ⚠ **Say which, and why, rather than leaving them quietly untested.** `on_feed_status`,
/// `on_mark`, `on_reference_quote`, `on_flow`, `on_order_event` and `on_params_updated` are
/// LIVE-ONLY: every one of their doc comments in `crates/vike-model/src/strategy/mod.rs` states
/// that the backtest engines never fire them, and neither `StrategyEngine::run` nor `run_ticks`
/// contains a call site for any of them. So there is no engine behaviour for two mechanisms to
/// agree or disagree about, and folding them into the bit-for-bit comparison would have meant
/// comparing two runs in which they never happened — an agreement that says nothing, which is
/// exactly the failure mode this file exists to avoid producing.
///
/// What IS honest is narrower and stated as such: drive each hook through the real
/// `PluginStrategy` host wrapper against a real `dlopen`ed artifact built by the real builder, and
/// read back what reached the user strategy. Everything the equivalence test proves about the
/// PIPELINE is still in play — the template's export, the loader's bind, the host's thunks, the
/// guest's decode, the `catch_unwind` on both sides. What is NOT proven is that an engine would
/// route them the same way, because no engine routes them at all.
///
/// The evidence channel is the fixture's own: each of these hooks submits a SELF-DESCRIBING order
/// (`"<hook>:<payload>"`), so this reads back not merely that the hook fired but which payload
/// crossed — the venue string, the absent-vs-empty tag, the reason text, the `None` barrier.
///
/// ⚠ Called from inside the one `build_fixture_plugin` caller rather than from a `#[test]` of its
/// own, for the reason that test's own doc gives: under `nextest` a second test is a second
/// PROCESS, and two processes racing one cold `build_plugin` share a scratch target directory.
pub(super) fn assert_live_only_hooks_reach_the_plugin(so: &Path) {
    use abi::{BrokerRef, COptStrRef, COrderLifecycle, CStrRef, PluginStatus};

    let plugin = load_or_panic(so);
    let mut broker = RecordingBroker::default();
    {
        let mut strategy: Box<dyn Strategy<RecordingBroker>> =
            Box::new(PluginStrategy::<RecordingBroker>::new(plugin.vtable, PARAMS_TOML));

        strategy.on_feed_status(&mut broker, vike_model::FeedStatus::Stale);
        strategy.on_mark(
            &mut broker,
            &vike_model::MarkTick {
                symbol: "btcusdt".to_string(),
                price: 64_321.5,
                ts: 1_700_000_000_005,
            },
        );
        strategy.on_reference_quote(
            &mut broker,
            "binance",
            &QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 99.0,
                ask: 101.0,
                bid_size: 0.0,
                ask_size: 0.0,
                symbol: SYMBOL.to_string(),
            },
        );
        strategy.on_flow(&mut broker, vike_model::FlowToxicity { bid: 0.25, ask: 0.75, ts: 7 });
        strategy.on_order_event(
            &mut broker,
            &vike_model::OrderLifecycle {
                client_order_id: "vike-77".to_string(),
                tag: Some("bid-1".to_string()),
                kind: vike_model::OrderEventKind::Canceled { reason: "replaced".to_string() },
            },
        );
        // ...and the ABSENT tag, which is a different fact from an empty one and the whole reason
        // `abi::COptStrRef` carries a `present` flag rather than leaning on a null pointer.
        strategy.on_order_event(
            &mut broker,
            &vike_model::OrderLifecycle {
                client_order_id: "vike-78".to_string(),
                tag: None,
                kind: vike_model::OrderEventKind::Accepted,
            },
        );
        strategy.on_params_updated(
            &mut broker,
            &vike_model::StrategyParams::PositionController(vike_model::ControllerParams::new(
                5_000,
                1.5,
                // Every leg UNARMED — four `None`s. This is the value a TOML hop could not have
                // carried AT ALL (`toml` refuses `serialize_none`), so seeing `none` come back is
                // what makes this a witness for the ENCODING and not only for the dispatch.
                vike_model::TripleBarrier::default(),
                0.75,
            )),
        );
    }

    let expected: Vec<(String, i32, f64)> = vec![
        ("feed:stale".to_string(), 1, 2.0),
        ("mark:btcusdt:1700000000005".to_string(), 1, 64_321.5),
        (format!("refq:binance:{SYMBOL}"), 1, 100.0),
        ("flow:7".to_string(), 1, 0.25),
        ("flow:7".to_string(), -1, 0.75),
        ("ord:vike-77:some(bid-1):canceled:replaced".to_string(), 1, 1.0),
        ("ord:vike-78:none:accepted:".to_string(), 1, 1.0),
        ("params:controller:5000:0.75:none".to_string(), 1, 1.5),
    ];
    assert_eq!(
        broker.submits, expected,
        "a live-only hook did not reach the plugin, or reached it with a mangled payload. Each \
         entry is `(<hook>:<payload>, side, scalar)` written by the fixture itself — read the \
         first differing row: a MISSING row means the hook was never delivered, a row with the \
         wrong payload means the mirror lost a field on the way across."
    );

    // ...and the REFUSAL half of the exhaustive mapping, driven through the same real artifact.
    // Without this, `abi::order_event_from_code`'s `None` arm is proven only by a unit test that
    // never crosses a `.so` boundary — and the arm exists precisely so a kind this build does not
    // know is never delivered as a guessed transition.
    let handle = (plugin.vtable.create)(PARAMS_TOML.as_ptr(), PARAMS_TOML.len());
    assert!(!handle.is_null(), "create must succeed for the BadParams probe");
    let mut probe_broker = RecordingBroker::default();
    let broker_ref = BrokerRef {
        ctx: std::ptr::from_mut(&mut probe_broker).cast(),
        vtable: vike_strategy_plugin::host::broker_vtable::<RecordingBroker>(),
    };
    let coid = "vike-99";
    let bad = COrderLifecycle {
        client_order_id: CStrRef::of(coid),
        tag: COptStrRef::of(None),
        // A code no `ORDER_EVENT_*` constant names. If a seventh `OrderEventKind` variant is ever
        // added AND given this code, this probe starts testing delivery instead of refusal —
        // which is why it picks a number far above the six, and why the assertion below names
        // what it is really claiming.
        kind: 9_999,
        reason: CStrRef::empty(),
    };
    let status = (plugin.vtable.on_order_event)(handle, broker_ref, &bad as *const COrderLifecycle);
    (plugin.vtable.destroy)(handle);
    assert_eq!(
        status,
        PluginStatus::BadParams,
        "an order-event kind this build does not know must be REFUSED whole. Delivering a guessed \
         transition would make a live order look dead to a strategy's retry machine and free a \
         slot that is still occupied."
    );
    assert!(
        probe_broker.submits.is_empty(),
        "a refused order event must not have reached user code at all, and it did: {:?}",
        probe_broker.submits
    );
}
