//! The risk budget the engine-start suites plant: this crate's backend, leverage and hot-start unit tests
//! and the gatewayhub's adapter contract suite each carried an identical copy until 2026-11-01.

/// A risk budget wide enough that no planted start is refused for want of one.
pub fn budget() -> vike_model::ProfileRisk {
    vike_model::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        ..Default::default()
    }
}
