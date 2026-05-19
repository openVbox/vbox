use std::ffi::OsString;

pub(crate) fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub(crate) fn env_os(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env;

    #[test]
    fn env_var_reads_set_value_and_ignores_missing_key() {
        let _guard = test_env::lock();
        test_env::set_var("VBOX_CLIENT_BRAND_TEST", "value");
        assert_eq!(env_var("VBOX_CLIENT_BRAND_TEST").as_deref(), Some("value"));
        test_env::remove_var("VBOX_CLIENT_BRAND_TEST");
        assert!(env_var("VBOX_CLIENT_BRAND_TEST").is_none());
    }

    #[test]
    fn env_os_reads_os_string_values() {
        let _guard = test_env::lock();
        test_env::set_var("VBOX_CLIENT_BRAND_OS_TEST", "os-value");
        assert_eq!(
            env_os("VBOX_CLIENT_BRAND_OS_TEST"),
            Some(OsString::from("os-value"))
        );
        test_env::remove_var("VBOX_CLIENT_BRAND_OS_TEST");
    }
}
