use thiserror::Error;

#[derive(Debug, Error)]
pub enum TunnelError {
    #[error("SSH connection failed: {0}")]
    SshConnect(String),
    #[error("SSH authentication failed")]
    SshAuth,
    #[error("Remote port forward failed: {0}")]
    PortForward(String),
    #[error("Connection API failed: {0}")]
    Api(String),
    #[error("Tunnel not found: {0}")]
    NotFound(String),
    #[error("Tunnel already exists: {0}")]
    AlreadyExists(String),
    #[error("Not configured: {0}")]
    NotConfigured(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl From<TunnelError> for String {
    fn from(e: TunnelError) -> String {
        e.to_string()
    }
}
