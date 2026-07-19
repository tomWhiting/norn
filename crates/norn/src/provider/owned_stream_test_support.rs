use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

const ASSERTION_DEADLINE: Duration = Duration::from_secs(5);

pub(super) type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy)]
pub(super) enum StallPoint {
    ResponseHeaders,
    SilentSse,
    ErrorBody,
}

pub(super) struct StalledServer {
    pub(super) base_url: String,
    ready: Option<oneshot::Receiver<()>>,
    peer_closed: Option<oneshot::Receiver<()>>,
    task: Option<tokio::task::JoinHandle<io::Result<()>>>,
}

impl StalledServer {
    pub(super) async fn spawn(point: StallPoint) -> TestResult<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let (ready_sender, ready) = oneshot::channel();
        let (closed_sender, peer_closed) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            drain_request(&mut socket).await?;
            match point {
                StallPoint::ResponseHeaders => {}
                StallPoint::SilentSse => {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000000\r\nConnection: keep-alive\r\n\r\n",
                        )
                        .await?;
                    socket.flush().await?;
                }
                StallPoint::ErrorBody => {
                    socket
                        .write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 1000000\r\nConnection: keep-alive\r\n\r\npartial-error-body",
                        )
                        .await?;
                    socket.flush().await?;
                }
            }
            let _ = ready_sender.send(());
            wait_for_peer_close(&mut socket).await?;
            let _ = closed_sender.send(());
            Ok(())
        });
        Ok(Self {
            base_url: format!("http://{address}"),
            ready: Some(ready),
            peer_closed: Some(peer_closed),
            task: Some(task),
        })
    }

    pub(super) async fn wait_ready(&mut self) -> TestResult {
        if let Err(signal_error) =
            await_signal(&mut self.ready, "server did not reach its stall point").await
        {
            let task = self
                .task
                .take()
                .ok_or_else(|| io::Error::other("server task was already consumed"))?;
            let result = task.await.map_err(|error| {
                io::Error::other(format!("server task failed to join: {error}"))
            })?;
            result.map_err(|error| {
                io::Error::other(format!("server failed before stall point: {error}"))
            })?;
            return Err(signal_error);
        }
        Ok(())
    }

    pub(super) async fn wait_peer_closed(&mut self) -> TestResult {
        await_signal(
            &mut self.peer_closed,
            "server did not observe provider socket closure",
        )
        .await?;
        let task = self
            .task
            .take()
            .ok_or_else(|| io::Error::other("server task was already consumed"))?;
        let joined = tokio::time::timeout(ASSERTION_DEADLINE, task)
            .await
            .map_err(|_elapsed| io::Error::other("server did not finish after peer closure"))?
            .map_err(|error| io::Error::other(format!("server task failed: {error}")))?;
        joined?;
        Ok(())
    }
}

impl Drop for StalledServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(super) struct RetryServer {
    pub(super) base_url: String,
    first_closed: Option<oneshot::Receiver<()>>,
    task: Option<tokio::task::JoinHandle<io::Result<()>>>,
}

impl RetryServer {
    pub(super) async fn spawn() -> TestResult<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let (first_closed_sender, first_closed) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await?;
            drain_request(&mut first).await?;
            first
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nRetry-After: 3600\r\nConnection: close\r\n\r\n",
                )
                .await?;
            first.flush().await?;
            first.shutdown().await?;
            wait_for_peer_close(&mut first).await?;
            let _ = first_closed_sender.send(());
            Ok(())
        });
        Ok(Self {
            base_url: format!("http://{address}"),
            first_closed: Some(first_closed),
            task: Some(task),
        })
    }

    pub(super) async fn wait_first_closed(&mut self) -> TestResult {
        let receiver = self
            .first_closed
            .take()
            .ok_or_else(|| io::Error::other("429 close signal was already consumed"))?;
        if let Err(signal_error) = receiver.await {
            let task = self
                .task
                .take()
                .ok_or_else(|| io::Error::other("retry server task was already consumed"))?;
            let result = task.await.map_err(|error| {
                io::Error::other(format!("retry server task failed to join: {error}"))
            })?;
            result.map_err(|error| {
                io::Error::other(format!("retry server failed before backoff: {error}"))
            })?;
            return Err(io::Error::other(format!(
                "429 response was not consumed and released: {signal_error}"
            ))
            .into());
        }
        Ok(())
    }
}

impl Drop for RetryServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn await_signal(receiver: &mut Option<oneshot::Receiver<()>>, message: &str) -> TestResult {
    let receiver = receiver
        .take()
        .ok_or_else(|| io::Error::other("fixture signal was already consumed"))?;
    tokio::time::timeout(ASSERTION_DEADLINE, receiver)
        .await
        .map_err(|_elapsed| io::Error::other(message))?
        .map_err(|error| io::Error::other(format!("fixture signal sender dropped: {error}")))?;
    Ok(())
}

async fn drain_request(socket: &mut TcpStream) -> io::Result<()> {
    let mut request = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    let mut header_end = None;
    let mut content_length = None;
    loop {
        let count = socket.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer closed before request completed",
            ));
        }
        request.extend_from_slice(&chunk[..count]);
        if header_end.is_none()
            && let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
        {
            let end = position + 4;
            header_end = Some(end);
            let headers = String::from_utf8_lossy(&request[..position]);
            content_length = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            });
        }
        match (header_end, content_length) {
            (Some(end), Some(length)) if request.len() >= end.saturating_add(length) => {
                return Ok(());
            }
            (Some(_), None) => return Ok(()),
            _ => {}
        }
    }
}

async fn wait_for_peer_close(socket: &mut TcpStream) -> io::Result<()> {
    let mut buffer = [0_u8; 64];
    loop {
        match socket.read(&mut buffer).await {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
}
