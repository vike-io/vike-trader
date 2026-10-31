//! A scripted [`ExitVenue`] for the tests of the exit binder, the exit groups and the exec thread's glue over them: the
//! venue's trades and exits as the JSON the shim writes, a placement that answers with fresh order ids unless it is told
//! to fail, and every write it is asked for recorded in order.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::exit::{ExitLeg, ExitVenue};
use crate::sys::{FxcmError, PlacedOrder};

#[derive(Debug)]
pub(crate) struct Script {
    pub(crate) trades: Result<String, FxcmError>,
    pub(crate) exits: Result<String, FxcmError>,
    /// What the next placements answer; once empty, a placement is accepted under a fresh id `P<n>`.
    pub(crate) placements: VecDeque<Result<PlacedOrder, FxcmError>>,
    /// Venue order ids whose delete is refused.
    pub(crate) delete_refused: HashSet<String>,
    /// Venue order ids whose edit is refused.
    pub(crate) edit_refused: HashSet<String>,
    /// Venue order ids whose SECOND edit onward is refused (it moved once and cannot be put back).
    pub(crate) edit_refused_again: HashSet<String>,
    /// `may_start` answers `false` from its n-th call on (1-based); `None` = always true.
    pub(crate) out_of_time_from: Option<usize>,
    /// Every call that wrote, in order: `place <trade> <units> <rate>`, `delete <order>`, `edit <order> <rate>`.
    pub(crate) log: Vec<String>,
    next: u32,
    asked: usize,
    /// How many times each order was edited.
    edits: HashMap<String, usize>,
}

impl Script {
    pub(crate) fn new(trades: &str, exits: &str) -> Self {
        Self {
            trades: Ok(trades.to_string()),
            exits: Ok(exits.to_string()),
            placements: VecDeque::new(),
            delete_refused: HashSet::new(),
            edit_refused: HashSet::new(),
            edit_refused_again: HashSet::new(),
            out_of_time_from: None,
            log: Vec::new(),
            next: 0,
            asked: 0,
            edits: HashMap::new(),
        }
    }
}

/// An accepted placement under `order_id`, the rate it was sent at.
pub(crate) fn accepted(order_id: &str, rate: f64) -> PlacedOrder {
    PlacedOrder { order_id: order_id.into(), offer_id: "1".into(), account_id: "7".into(), rate }
}

impl ExitVenue for Script {
    fn open_trades(&mut self) -> Result<String, FxcmError> {
        self.trades.clone()
    }

    fn open_exits(&mut self) -> Result<String, FxcmError> {
        self.exits.clone()
    }

    fn place(&mut self, trade_id: &str, units: i32, rate: f64) -> Result<PlacedOrder, FxcmError> {
        self.log.push(format!("place {trade_id} {units} {rate}"));
        if let Some(answer) = self.placements.pop_front() {
            return answer;
        }
        self.next += 1;
        Ok(accepted(&format!("P{}", self.next), rate))
    }

    fn delete(&mut self, leg: &ExitLeg) -> Result<(), FxcmError> {
        self.log.push(format!("delete {}", leg.order_id));
        if self.delete_refused.contains(&leg.order_id) {
            return Err(FxcmError::Native { code: -6, message: "ORA-refused".into() });
        }
        Ok(())
    }

    fn edit(&mut self, leg: &ExitLeg, rate: f64) -> Result<(), String> {
        self.log.push(format!("edit {} {rate}", leg.order_id));
        let count = self.edits.entry(leg.order_id.clone()).or_insert(0);
        *count += 1;
        if self.edit_refused.contains(&leg.order_id)
            || (*count >= 2 && self.edit_refused_again.contains(&leg.order_id))
        {
            return Err("ORA-20115: refused".to_string());
        }
        Ok(())
    }

    fn may_start(&mut self) -> bool {
        self.asked += 1;
        self.out_of_time_from.is_none_or(|from| self.asked < from)
    }
}
