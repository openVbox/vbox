pub(crate) fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}
