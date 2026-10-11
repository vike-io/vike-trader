//! The LIVE cross-exchange mount: the maker folded into the wired-market node's core.

#[cfg(doc)]
use vike_core::CoreConfig;
use vike_core::StrategyMount;
#[cfg(doc)]
use vike_mm::XemmMaker;

#[cfg(doc)]
use super::build_paper_xemm_core;
use super::{XemmConfigError, XemmMountConfig, build_xemm_maker, xemm_mount_legs};
use crate::node::{Node, NodeConfig, NodeError, build_node};

/// The spawned LIVE cross-exchange mount: the wired-market [`Node`] with the [`XemmMaker`] folded
/// into its [`CoreConfig::strategy`].
pub struct LiveXemmMount {
    pub node: Node,
}

/// Why a live mount failed: a configuration refusal, or the node's own fault.
#[derive(Debug)]
pub enum XemmMountError {
    Config(XemmConfigError),
    Node(NodeError),
}

impl std::fmt::Display for XemmMountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XemmMountError::Config(e) => write!(f, "{e}"),
            XemmMountError::Node(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for XemmMountError {}

/// The LIVE twin of [`build_paper_xemm_core`], on the REAL [`build_node`] core. Validated against
/// `node_cfg.markets`, so an unwired or wrong-symbol taker venue is a STARTUP error. Spawns NO feed
/// (module doc: the caller wires three).
pub fn build_live_xemm_core(
    cfg: &XemmMountConfig,
    mut node_cfg: NodeConfig,
) -> Result<LiveXemmMount, XemmMountError> {
    let total_fee = cfg.validate(Some(node_cfg.markets)).map_err(XemmMountError::Config)?;
    node_cfg.core_config.strategy = Some(StrategyMount {
        account: None,
        venue: cfg.maker_venue.clone(),
        symbol: cfg.maker_symbol.clone(),
        interval: cfg.interval.clone(),
        strategy: Box::new(build_xemm_maker(cfg, total_fee)),
        symbols: xemm_mount_legs(cfg),
        underlying_symbol: None,
        controller_id: None,
    });
    Ok(LiveXemmMount { node: build_node(node_cfg).map_err(XemmMountError::Node)? })
}
