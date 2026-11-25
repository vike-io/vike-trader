//! The gate's session-wide sliding-window order-rate throttle and its journal-snapshot state.

use super::RiskGate;

impl RiskGate {
    /// Sliding-window throttle: evict expired stamps, then admit (and record) one order.
    /// `true` = admitted. Unarmed (`max_orders_per_window == None`) always admits and records
    /// nothing.
    pub(super) fn admit_throttle(&mut self, now_ms: i64) -> bool {
        let Some(max_orders) = self.limits.max_orders_per_window else { return true };
        let cutoff = now_ms - self.limits.window_ms;
        while self.order_times.front().is_some_and(|&t| t <= cutoff) {
            self.order_times.pop_front();
        }
        if self.order_times.len() >= max_orders {
            return false;
        }
        self.order_times.push_back(now_ms);
        true
    }

    /// Throttle-window state for the journal snapshot (replay determinism on throttle denials).
    pub fn throttle_times(&self) -> Vec<i64> {
        self.order_times.iter().copied().collect()
    }
    pub fn set_throttle_times(&mut self, times: Vec<i64>) {
        self.order_times = times.into();
    }
}
