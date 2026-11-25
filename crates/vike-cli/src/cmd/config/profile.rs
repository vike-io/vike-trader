//! `vike-cli config profile …`'s verbs, one file each: [`ls`], [`activate`] (with `deactivate`),
//! [`rm`], [`create`] (`daemon`, `recorder`, `run`) and the two stored-mount verbs,
//! [`set_mount_param`] and [`set_mount_max_leverage`]. The family's grammar (`ProfileVerb`,
//! `CreateKind`, `MountVerb`) and its router stay in the root, `crate::cmd::config`.

pub(super) mod activate;
pub(super) mod create;
pub(super) mod ls;
pub(super) mod rm;
pub(super) mod set_mount_max_leverage;
pub(super) mod set_mount_param;
