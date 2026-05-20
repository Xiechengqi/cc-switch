pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DISPLAY_VERSION: &str = env!("CC_SWITCH_DISPLAY_VERSION");
pub const BUILD_CHANNEL: &str = env!("CC_SWITCH_BUILD_CHANNEL");
pub const BUILD_SHA: &str = env!("CC_SWITCH_BUILD_SHA");
pub const BUILD_TIME: &str = env!("CC_SWITCH_BUILD_TIME");
pub const RELEASE_VERSION: &str = env!("CC_SWITCH_RELEASE_VERSION");

pub fn display_version() -> &'static str {
    DISPLAY_VERSION
}
