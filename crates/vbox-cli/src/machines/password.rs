use anyhow::{Result, bail};
use serde_json::{Map, Value};

use super::kind::MachineKind;
use super::{MachineRecord, load_overrides, save_overrides};
use crate::context::AppContext;
use crate::{keychain, remote};

/// Store `password` in the macOS Keychain for `record` and mark the matching
/// store (remote-hosts.json for remote machines, machines.json overrides for
/// Parallels machines) so future reads can tell that a password exists.
///
/// An empty password clears any existing entry.
pub(crate) fn set(ctx: &AppContext, record: &MachineRecord, password: &str) -> Result<()> {
    if password.is_empty() {
        return clear(ctx, record);
    }
    let kind = MachineKind::from_record(record);
    keychain::set_password(&kind.keychain_account(), password)?;
    mark_has_password(ctx, &kind, true)
}

pub(crate) fn clear(ctx: &AppContext, record: &MachineRecord) -> Result<()> {
    let kind = MachineKind::from_record(record);
    let _ = keychain::delete_password(&kind.keychain_account());
    mark_has_password(ctx, &kind, false)
}

fn mark_has_password(ctx: &AppContext, kind: &MachineKind<'_>, present: bool) -> Result<()> {
    match kind {
        MachineKind::Remote { name } => remote::set_has_password(ctx, name, present),
        MachineKind::Parallels { uuid } => mark_override_password(ctx, uuid, present),
    }
}

fn mark_override_password(ctx: &AppContext, uuid: &str, present: bool) -> Result<()> {
    let mut overrides = load_overrides(ctx)?;
    let entry = overrides
        .entry(uuid.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(map) = entry.as_object_mut() else {
        bail!("override record is not an object");
    };
    if present {
        map.insert("has_password".to_string(), Value::Bool(true));
    } else {
        map.remove("has_password");
    }
    save_overrides(ctx, &overrides)
}
