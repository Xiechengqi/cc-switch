//! Cursor OAuth state wrapper.

use crate::proxy::providers::cursor_oauth_auth::CursorOAuthManager;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cursor OAuth 认证状态
pub struct CursorOAuthState(pub Arc<RwLock<CursorOAuthManager>>);
