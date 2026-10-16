//! Standalone USER-signed `/exchange` actions (`HyperliquidSignTransaction` scheme), off the exec hot
//! path: `transfer` (`usdClassTransfer`) and `builder_fee` (`approveBuilderFee`). The venue resolves
//! such an action to the RECOVERED signer's own account (see each module's doc for the measurement).

pub mod builder_fee;
pub mod transfer;
