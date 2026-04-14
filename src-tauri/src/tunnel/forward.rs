use tokio::io::{self, AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// Bidirectional TCP copy between an SSH forwarded channel and a local service.
pub async fn forward_tcp<S>(mut remote: S, local_addr: &str) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut local = TcpStream::connect(local_addr).await?;
    io::copy_bidirectional(&mut remote, &mut local).await?;
    Ok(())
}
