pub(crate) const CODEX_CLI_VERSION: &str = "0.139.0";

pub(crate) fn codex_cli_user_agent(os_name: &str, arch_name: &str, suffix: &str) -> String {
    let suffix = suffix.trim();
    if suffix.is_empty() {
        format!("codex_cli_rs/{CODEX_CLI_VERSION} ({os_name}; {arch_name})")
    } else {
        format!("codex_cli_rs/{CODEX_CLI_VERSION} ({os_name}; {arch_name}) {suffix}")
    }
}

pub(crate) fn codex_cli_terminal_user_agent(os_name: &str, arch_name: &str) -> String {
    // Keep the same CLI-like shape used by Codex stream checks; only the version
    // is centralized so ChatGPT feature gates do not drift behind the real CLI.
    format!("codex_cli_rs/{CODEX_CLI_VERSION} ({os_name} 15.7.2; {arch_name}) Terminal")
}
