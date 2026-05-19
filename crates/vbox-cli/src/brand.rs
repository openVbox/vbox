use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) const PRIMARY_STATE_DIR: &str = ".vbox";

pub(crate) fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub(crate) fn env_os(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

pub(crate) fn state_dir(root: &Path) -> PathBuf {
    env_os("VBOX_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(PRIMARY_STATE_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env;

    #[test]
    fn env_var_reads_primary_name() {
        let _guard = test_env::lock();
        test_env::set_var("VBOX_BRAND_TEST", "primary");
        assert_eq!(env_var("VBOX_BRAND_TEST").as_deref(), Some("primary"));
        test_env::remove_var("VBOX_BRAND_TEST");
    }

    #[test]
    fn env_var_ignores_unrelated_names() {
        let _guard = test_env::lock();
        test_env::remove_var("VBOX_BRAND_TEST");
        test_env::set_var("OTHER_BRAND_TEST", "other");
        assert!(env_var("VBOX_BRAND_TEST").is_none());
        test_env::remove_var("OTHER_BRAND_TEST");
    }

    #[test]
    fn state_dir_defaults_to_primary_name() {
        assert_eq!(state_dir(Path::new("/repo")), PathBuf::from("/repo/.vbox"));
    }
}
