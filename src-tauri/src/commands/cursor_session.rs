//! Cursor `agent.v1` session-manager state wrapper.
//!
//! Holds the process-wide [`CursorSessionManager`] so handlers in
//! `cursor_claude` / `cursor_codex` / `cursor_apikey` can park / reacquire
//! the h2 stream for an in-flight tool round-trip.

use crate::proxy::providers::cursor_session::CursorSessionManager;

pub struct CursorSessionState(pub CursorSessionManager);
