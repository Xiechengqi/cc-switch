//! Cursor AgentService debug logging (`CC_SWITCH_CURSOR_DEBUG=1`).

pub fn enabled() -> bool {
    matches!(
        std::env::var("CC_SWITCH_CURSOR_DEBUG")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1" | "true" | "yes")
    )
}

macro_rules! cursor_dbg {
    ($($arg:tt)*) => {
        if $crate::proxy::providers::cursor_debug::enabled() {
            log::info!($($arg)*);
        }
    };
}

pub(crate) use cursor_dbg;

pub fn log_protocol_choice(protocol: &str, inbound: &str, reason: &str) {
    cursor_dbg!(
        "[CursorDebug] protocol={protocol} inbound={inbound} reason={reason}"
    );
}

pub fn log_session(event: &str, session_key: &str, detail: &str) {
    cursor_dbg!("[CursorDebug] session {event} key={session_key} {detail}");
}

pub fn log_exec(kind: &str, exec_id: &str, detail: &str) {
    cursor_dbg!("[CursorDebug] exec kind={kind} exec_id={exec_id} {detail}");
}

pub fn log_bridge(from: &str, to: &str) {
    cursor_dbg!("[CursorDebug] bridge {from} → MCP {to}");
}

pub fn log_retry(reason: &str, attempt: usize, max: usize) {
    cursor_dbg!("[CursorDebug] retry reason={reason} {attempt}/{max}");
}

pub fn cold_resume_reject() -> bool {
    matches!(
        std::env::var("CC_SWITCH_CURSOR_COLD_RESUME")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("reject")
    )
}
