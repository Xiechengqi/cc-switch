use crate::database::Database;
use crate::services::ProxyService;
use crate::tunnel::TunnelManager;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 全局应用状态
pub struct AppState {
    pub db: Arc<Database>,
    pub proxy_service: ProxyService,
    pub tunnel_manager: Arc<RwLock<TunnelManager>>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(db: Arc<Database>) -> Self {
        let proxy_service = ProxyService::new(db.clone());
        let tunnel_manager = Arc::new(RwLock::new(TunnelManager::new()));

        Self {
            db,
            proxy_service,
            tunnel_manager,
        }
    }
}
