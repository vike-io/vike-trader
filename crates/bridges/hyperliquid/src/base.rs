//! The foundation every other role stands on: credentials and the live gate (`config`), endpoints and
//! protocol constants (`consts`), the per-IP REST weight gate (`ratelimit`), the `ureq` REST transport
//! (`transport`: keyless `/info` always, signed `/exchange` under `exec`), price/size wire formatting
//! (`px`) and the three symbology index spaces (`symbology`). Nothing here depends on `feeds`, `exec`
//! or `adapter`.

pub mod config;
pub mod consts;
pub mod px;
pub mod ratelimit;
pub mod symbology;
pub mod transport;
