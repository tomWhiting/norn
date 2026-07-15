use std::time::Duration;

use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

pub(super) struct WithholdingAuthority {
    url: String,
    observed: Option<oneshot::Receiver<std::io::Result<()>>>,
    release: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl WithholdingAuthority {
    pub(super) async fn start() -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let (observed_tx, observed) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            match read_complete_http_request(&mut stream).await {
                Ok(()) => {
                    drop(observed_tx.send(Ok(())));
                    drop(release_rx.await);
                    Ok(())
                }
                Err(error) => {
                    let reported = std::io::Error::new(error.kind(), error.to_string());
                    drop(observed_tx.send(Err(reported)));
                    Err(error)
                }
            }
        });
        Ok(Self {
            url: format!("http://{address}/token"),
            observed: Some(observed),
            release: Some(release),
            task,
        })
    }

    pub(super) fn url(&self) -> &str {
        &self.url
    }

    pub(super) async fn wait_for_dispatch(&mut self) -> Result<(), std::io::Error> {
        let observed = self
            .observed
            .take()
            .ok_or_else(|| std::io::Error::other("authority dispatch was already observed"))?;
        tokio::time::timeout(Duration::from_secs(5), observed)
            .await
            .map_err(|elapsed| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("authority did not observe the refresh dispatch: {elapsed}"),
                )
            })?
            .map_err(|error| std::io::Error::other(format!("authority task ended: {error}")))?
    }

    pub(super) fn close_without_response(&mut self) {
        drop(self.release.take());
    }

    pub(super) async fn finish(mut self) -> Result<(), std::io::Error> {
        self.close_without_response();
        self.task
            .await
            .map_err(|error| std::io::Error::other(format!("authority task failed: {error}")))?
    }
}

async fn read_complete_http_request(stream: &mut TcpStream) -> Result<(), std::io::Error> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "refresh request ended before its complete body arrived",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
        let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&request[..headers_end]).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        let content_length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .ok_or_else(|| std::io::Error::other("refresh request omitted Content-Length"))?
            .1
            .trim()
            .parse::<usize>()
            .map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
            })?;
        if request.len() >= headers_end + 4 + content_length {
            return Ok(());
        }
    }
}
