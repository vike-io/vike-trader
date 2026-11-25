//! The resolved datahub address turned into the dial the desktop's datahub clients share; the
//! adoption rule that resolves the address is the parent module's.

use vike_datahub_client::dial::DatahubDial;

/// **The desktop's datahub dial** ([`DatahubDial`], the one dial its datahub clients take), built
/// from the facts a COMPOSITION ROOT owns — the resolved address ([`super::resolve_datahub_address`]'s
/// answer) and the settings directory — signed with this desktop's device key alone (decision 0288).
///
/// The exact shape of [`crate::data::stored::chart_seed::ChartSeedDial::resolve`], including the notice handling:
/// key-resolution notices are LOGGED rather than returned, because this is the fourth caller of
/// the same resolution in one process and a notice the operator has already seen three times is not
/// worth a fourth surface. It stays in this crate (the dial itself is `vike-datahub-client`'s)
/// because it reads this crate's [`crate::backend::registry::device_key_epoch`] and
/// `datahub_dial_keys`. No key is named since the cutover: the epoch is what moves when the key does.
pub fn resolve_datahub_dial(address: String, settings_dir: Option<&str>) -> DatahubDial {
    let epoch = crate::backend::registry::device_key_epoch();
    let (keys, notices) = crate::backend::registry::datahub_dial_keys(settings_dir);
    for n in notices {
        tracing::info!("{n}");
    }
    DatahubDial { address, keys, epoch }
}
