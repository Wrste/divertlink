//! TCP helpers: happy-eyeballs-free connect with keepalive and nodelay.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, TcpKeepalive};
use tokio::net::TcpStream;

use crate::error::{Error, Result};

/// Whether a non-blocking `connect` error means "still in progress".
///
/// The raw errno values differ across platforms (`EINPROGRESS` is 115 on Linux,
/// 36 on macOS/BSD, `WSAEWOULDBLOCK` is 10035 on Windows), and some platforms map
/// them to `WouldBlock` while others leave them uncategorized.
fn is_connect_in_progress(err: &io::Error) -> bool {
    if matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::AlreadyExists
    ) {
        return true;
    }
    #[cfg(unix)]
    const IN_PROGRESS: &[i32] = &[115, 36, 114, 150]; // EINPROGRESS, EALREADY (Linux/macOS/BSD)
    #[cfg(windows)]
    const IN_PROGRESS: &[i32] = &[10035, 10036, 10037]; // WSAEWOULDBLOCK/WSAEINPROGRESS/WSAEALREADY
    #[cfg(not(any(unix, windows)))]
    const IN_PROGRESS: &[i32] = &[];
    matches!(err.raw_os_error(), Some(code) if IN_PROGRESS.contains(&code))
}

/// Connect with a timeout, enabling `TCP_NODELAY` and optional keepalive.
///
/// `keepalive` enables OS-level probes so that dead NAT mappings are detected
/// even when application heartbeats are disabled.
pub async fn tcp_connect(
    addr: SocketAddr,
    timeout: Duration,
    keepalive: Option<Duration>,
) -> Result<TcpStream> {
    let socket = Socket::new(
        Domain::for_address(addr),
        socket2::Type::STREAM,
        Some(Protocol::TCP),
    )?;
    socket.set_nonblocking(true)?;
    if let Some(idle) = keepalive {
        let keepalive = TcpKeepalive::new()
            .with_time(idle)
            .with_interval(idle.max(Duration::from_secs(1)));
        socket.set_tcp_keepalive(&keepalive)?;
        #[cfg(target_os = "linux")]
        {
            socket.set_tcp_user_timeout(Some(idle * 3))?;
        }
    }
    match socket.connect(&addr.into()) {
        Ok(()) => {}
        Err(err) if is_connect_in_progress(&err) => {}
        Err(err) => return Err(Error::Io(err)),
    }
    let std_stream: std::net::TcpStream = socket.into();
    let stream = TcpStream::from_std(std_stream)?;
    stream.set_nodelay(true)?;
    // Wait for the non-blocking connect to complete, bounded by `timeout`.
    tokio::time::timeout(timeout, stream.writable())
        .await
        .map_err(|_| Error::Timeout(format!("TCP connect to {addr} timed out")))??;
    if let Some(err) = stream.take_error()? {
        return Err(Error::Io(io::Error::new(err.kind(), err.to_string())));
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connects_to_local_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream
        });
        let client = tcp_connect(addr, Duration::from_secs(2), Some(Duration::from_secs(30)))
            .await
            .unwrap();
        assert!(client.peer_addr().is_ok());
        let _server = accept.await.unwrap();
    }

    #[tokio::test]
    async fn refuses_unbound_port() {
        // Bind then drop to get a very likely free port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let res = tcp_connect(addr, Duration::from_millis(200), None).await;
        assert!(res.is_err());
    }
}
