//! **The labels a core-made order client id carries, and the mint that renders them** (decision 0270).
//!
//! - **Source and strategy come from the OWNER**: `S` and the mount's `Mount::order_client_id_code` on
//!   `EngineRoute::Mount` (a strategy's submits, groups and budget-latch flatten) and for a strategy's
//!   conditional fire, which lowers on its arm's engine (`CoreThread::order_client_id_owner`); `E` and `0000` for
//!   everything else (an operator's order that reached the core with no id, an operator's fired core-held
//!   stop, a margin call, a flatten leg).
//! - **The intent comes from the routed engine's FOLDED state** (`OrderClientIdIntent::of`): the symbol's
//!   `default_margin_mode_of` (`Cash` is spot) and its net `Both` position before the order. A replay core
//!   that folded the same state labels the same order alike.
//! - **The render is the engine's plan** (`CoreThread::order_client_id_plans`, chosen once per engine): one index and
//!   a straight append, no venue read and no table (the HFT rule). A v1 session renders no label at all,
//!   so it mints today's exact bytes.
//!
//! Per ORDER, never per message: every caller is a command arm in `apply/`.

use super::*;
use vike_model::events::PositionSide;
use vike_model::orders::client_id::{
    OrderClientIdIntent, OrderClientIdLabels, OrderClientIdLegRole, OrderClientIdSource,
    OrderClientIdStem, OrderClientIdStrategyCode,
};

impl<C: ExecutionClient> CoreThread<C> {
    /// Who made an order lowered on `route`: a strategy (`S`, its mount's code, `0000` for a mount nobody
    /// gave one) on `EngineRoute::Mount` or while [`Self::order_client_id_owner`] names the mount (its conditional
    /// fire, on its arm's engine); the engine itself (`E`, `0000`) otherwise.
    pub(crate) fn order_client_id_who(
        &self,
        route: EngineRoute,
    ) -> (OrderClientIdSource, OrderClientIdStrategyCode) {
        let mount = match route {
            EngineRoute::Mount(i) => Some(i),
            EngineRoute::Payload | EngineRoute::Engine(_) => self.order_client_id_owner,
        };
        match mount {
            Some(i) => (
                OrderClientIdSource::Strategy,
                self.mounts
                    .get(i)
                    .and_then(Option::as_ref)
                    .and_then(|m| m.order_client_id_code)
                    .unwrap_or(OrderClientIdStrategyCode::NONE),
            ),
            None => (OrderClientIdSource::Engine, OrderClientIdStrategyCode::NONE),
        }
    }

    /// What a `side` order on `symbol` (empty: the engine's own) does to engine `engine_index`'s folded book.
    pub(crate) fn order_client_id_intent(
        &self,
        engine_index: usize,
        side: i32,
        reduce_only: bool,
        symbol: &str,
    ) -> OrderClientIdIntent {
        let engine = self.engine(engine_index);
        let symbol = if symbol.is_empty() { engine.symbol.as_str() } else { symbol };
        let cash = engine.account.default_margin_mode_of(symbol) == vike_model::MarginMode::Cash;
        OrderClientIdIntent::of(
            side,
            reduce_only,
            cash,
            engine.position_size_of(symbol, PositionSide::Both),
        )
    }

    /// ONE plain order's id for `req` on engine `engine_index`, lowered on `route`: one sequence number.
    pub(crate) fn mint_order_client_id(
        &mut self,
        route: EngineRoute,
        engine_index: usize,
        req: &OrderRequest,
    ) -> String {
        let (src, strat) = self.order_client_id_who(route);
        let intent =
            self.order_client_id_intent(engine_index, req.side, req.reduce_only, &req.symbol);
        self.order_client_id_plans[engine_index]
            .mint(&mut self.order_client_id_gen, OrderClientIdLabels::new(src, strat, intent))
    }

    /// ONE group's stem on engine `engine_index`, lowered on `route`: one sequence number on a v2 session, none on
    /// a v1 one (whose legs then mint whole, one each, as every group did before stems).
    pub(crate) fn mint_stem(
        &mut self,
        route: EngineRoute,
        engine_index: usize,
    ) -> OrderClientIdStem {
        let (src, strat) = self.order_client_id_who(route);
        self.order_client_id_plans[engine_index].generate_stem(
            &mut self.order_client_id_gen,
            src,
            strat,
        )
    }

    /// **The first leg id a SENDER's stem would name that this node already holds or remembers**
    /// (`order_id_in_use`), under ANY intent: a resend after an unclear reply must find the group it
    /// placed even when a fill in between moved the position the intent was read from. `None` when
    /// the stem is free. A given stem is v2 text, so the probe draws no sequence number. Cold: per
    /// command, `roles.len()` times six probes.
    pub(crate) fn stem_in_use(
        &mut self,
        engine_index: usize,
        stem: &OrderClientIdStem,
        roles: &[OrderClientIdLegRole],
    ) -> Option<String> {
        for &role in roles {
            for intent in OrderClientIdIntent::ALL {
                let id = stem.leg(&mut self.order_client_id_gen, intent, role);
                if self.order_id_in_use(engine_index, &id) {
                    return Some(id);
                }
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "tests/order_client_id_labels.rs"]
mod tests;
