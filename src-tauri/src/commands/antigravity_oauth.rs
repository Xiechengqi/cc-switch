//! Antigravity OAuth state wrapper.

use crate::proxy::providers::antigravity_oauth_auth::AntigravityOAuthManager;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Antigravity OAuth 认证状态
pub struct AntigravityOAuthState(pub Arc<RwLock<AntigravityOAuthManager>>);
