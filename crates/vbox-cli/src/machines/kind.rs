use super::MachineRecord;

/// Where a machine is hosted. The discriminant is encoded in
/// `MachineRecord::uuid` as a `remote:` prefix; this enum lifts that string
/// check out of every call site that branches on it.
#[derive(Debug, Clone)]
pub(crate) enum MachineKind<'a> {
    Parallels { uuid: &'a str },
    Remote { name: &'a str },
}

impl<'a> MachineKind<'a> {
    pub(crate) fn from_record(record: &'a MachineRecord) -> Self {
        if record.uuid.starts_with("remote:") {
            Self::Remote {
                name: &record.name,
            }
        } else {
            Self::Parallels { uuid: &record.uuid }
        }
    }

    pub(crate) fn keychain_account(&self) -> String {
        match self {
            Self::Remote { name } => crate::remote::keychain_account(name),
            Self::Parallels { uuid } => format!("parallels:{uuid}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(uuid: &str, name: &str) -> MachineRecord {
        MachineRecord {
            uuid: uuid.to_string(),
            name: name.to_string(),
            status: String::new(),
            ip: String::new(),
            os_raw: String::new(),
            os_kind: String::new(),
            ssh_user: String::new(),
            ssh_host: String::new(),
            guest_dir: String::new(),
            identity_file: String::new(),
            has_password: false,
        }
    }

    #[test]
    fn parallels_uuid_classifies_as_parallels() {
        let rec = record("abc-123", "VM");
        match MachineKind::from_record(&rec) {
            MachineKind::Parallels { uuid } => assert_eq!(uuid, "abc-123"),
            other => panic!("expected Parallels, got {other:?}"),
        }
    }

    #[test]
    fn remote_prefix_classifies_as_remote_by_name() {
        let rec = record("remote:lab_one", "Lab One");
        match MachineKind::from_record(&rec) {
            MachineKind::Remote { name } => assert_eq!(name, "Lab One"),
            other => panic!("expected Remote, got {other:?}"),
        }
    }

    #[test]
    fn keychain_account_uses_remote_helper_for_remote_kind() {
        let rec = record("remote:lab_one", "Lab One");
        assert_eq!(
            MachineKind::from_record(&rec).keychain_account(),
            crate::remote::keychain_account("Lab One")
        );
    }

    #[test]
    fn keychain_account_prefixes_parallels_uuid() {
        let rec = record("abc-123", "VM");
        assert_eq!(
            MachineKind::from_record(&rec).keychain_account(),
            "parallels:abc-123"
        );
    }
}
