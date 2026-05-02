//! Gemini OAuth state wrapper.

use crate::proxy::providers::gemini_oauth_auth::GeminiOAuthManager;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Gemini OAuth 认证状态
pub struct GeminiOAuthState(pub Arc<RwLock<GeminiOAuthManager>>);
