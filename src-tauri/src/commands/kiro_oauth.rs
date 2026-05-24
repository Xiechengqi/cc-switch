//! Kiro OAuth state wrapper.

use crate::proxy::providers::kiro_oauth_auth::KiroOAuthManager;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Kiro OAuth 认证状态
pub struct KiroOAuthState(pub Arc<RwLock<KiroOAuthManager>>);
