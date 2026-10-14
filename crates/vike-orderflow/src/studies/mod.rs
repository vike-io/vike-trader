//! Book- and volume-fed microstructure studies, one file per study: `vpin` (`Vpin`), `bvc_vpin`
//! (`BvcVpin`), `imbalance`, `otr` and `p2`. Their API is re-exported at the crate root.
pub mod bvc_vpin;
pub mod imbalance;
pub mod otr;
pub mod p2;
pub mod vpin;
