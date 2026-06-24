pub const NO_DESKTOP_ENV: &str = "CC_SWITCH_NO_DESKTOP";

pub fn is_no_desktop() -> bool {
    std::env::var(NO_DESKTOP_ENV)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}
