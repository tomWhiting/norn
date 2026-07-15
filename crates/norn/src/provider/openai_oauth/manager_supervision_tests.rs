use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::auth_root::NornAuthRoot;
use super::super::options::OAuthHttpOptions;
use super::super::storage::{AuthCredentialsStoreMode, save_auth_dot_json};
use super::super::types::AuthDotJson;
use super::attempt::{RefreshAttempt, supervise_refresh_worker};
use super::{AuthManager, CachedAuthState, RefreshTokenError};

type TestResult = Result<(), Box<dyn Error>>;
type RefreshResult = Result<(), RefreshTokenError>;

#[tokio::test]
async fn aborted_worker_wakes_every_waiter_with_indeterminate_outcome() -> TestResult {
    let (attempt, completion) = RefreshAttempt::new();
    let worker = tokio::spawn(std::future::pending::<RefreshResult>());
    let abort_handle = worker.abort_handle();
    let supervisor = tokio::spawn(supervise_refresh_worker(worker, completion));
    let first = spawn_waiter(&attempt);
    let second = spawn_waiter(&attempt);

    tokio::task::yield_now().await;
    abort_handle.abort();

    assert_indeterminate(&join_refresh(first).await?);
    assert_indeterminate(&join_refresh(second).await?);
    join_unit(supervisor).await?;
    Ok(())
}

#[tokio::test]
async fn aborted_supervisor_closes_the_attempt_for_waiters() -> TestResult {
    let (attempt, completion) = RefreshAttempt::new();
    let worker = tokio::spawn(std::future::pending::<RefreshResult>());
    let abort_worker = worker.abort_handle();
    let supervisor = tokio::spawn(supervise_refresh_worker(worker, completion));

    supervisor.abort();
    let supervisor_error = supervisor
        .await
        .err()
        .ok_or_else(|| std::io::Error::other("supervisor did not abort"))?;
    assert!(supervisor_error.is_cancelled());
    assert_indeterminate(&attempt.wait().await);

    abort_worker.abort();
    Ok(())
}

#[tokio::test]
async fn abort_after_dispatch_blocks_replay_until_lineage_changes() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(OAuthHttpOptions::DEFAULT_REQUEST_TIMEOUT)
                .set_body_json(refresh_response_body()),
        )
        .mount(&server)
        .await;

    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    save_auth_dot_json(&auth_root_path, &seed_auth("seed-refresh")?)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let manager =
        AuthManager::shared_for_tests(root, AuthCredentialsStoreMode::File, server.uri()).await?;

    let (attempt, completion) = RefreshAttempt::new();
    *manager.refresh_attempt.lock().await = Some(Arc::clone(&attempt));
    let worker_manager = Arc::clone(&manager);
    let worker = tokio::spawn(async move { worker_manager.perform_refresh_attempt().await });
    let abort_worker = worker.abort_handle();
    let supervisor_manager = Arc::clone(&manager);
    let supervised_attempt = Arc::clone(&attempt);
    let supervisor = tokio::spawn(async move {
        supervise_refresh_worker(worker, completion).await;
        supervisor_manager
            .clear_attempt_if_current(&supervised_attempt)
            .await;
    });
    let first = spawn_manager_refresh(&manager);

    wait_until_dispatched(&server, &manager).await?;
    let second = spawn_manager_refresh(&manager);
    tokio::task::yield_now().await;
    abort_worker.abort();

    assert_indeterminate(&join_refresh(first).await?);
    assert_indeterminate(&join_refresh(second).await?);
    join_unit(supervisor).await?;
    assert_indeterminate(&manager.refresh_token_from_authority().await);
    assert_eq!(received_request_count(&server).await?, 1);

    let mut recovered = seed_auth("externally-rotated-refresh")?;
    if let Some(tokens) = recovered.tokens.as_mut() {
        tokens.access_token = "externally-rotated-access".to_owned();
    }
    save_auth_dot_json(&auth_root_path, &recovered)?;

    manager.refresh_token_from_authority().await?;
    assert_eq!(access_token(&manager).await?, "externally-rotated-access");
    assert_eq!(received_request_count(&server).await?, 1);
    Ok(())
}

#[tokio::test]
async fn ambiguous_disconnect_blocks_replay_in_the_current_manager() -> TestResult {
    let server = DisconnectServer::start().await?;
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    save_auth_dot_json(&auth_root_path, &seed_auth("seed-refresh")?)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let manager =
        AuthManager::shared_for_tests(root, AuthCredentialsStoreMode::File, server.uri()).await?;

    assert_indeterminate(&manager.refresh_token_from_authority().await);
    assert_indeterminate(&manager.refresh_token_from_authority().await);

    assert_eq!(
        server.stop().await?,
        1,
        "the live manager must not replay a possibly consumed refresh lineage",
    );
    Ok(())
}

fn spawn_waiter(attempt: &Arc<RefreshAttempt>) -> JoinHandle<RefreshResult> {
    let attempt = Arc::clone(attempt);
    tokio::spawn(async move { attempt.wait().await })
}

fn spawn_manager_refresh(manager: &Arc<AuthManager>) -> JoinHandle<RefreshResult> {
    let manager = Arc::clone(manager);
    tokio::spawn(async move { manager.refresh_token_from_authority().await })
}

async fn join_refresh(handle: JoinHandle<RefreshResult>) -> Result<RefreshResult, std::io::Error> {
    handle.await.map_err(std::io::Error::other)
}

async fn join_unit(handle: JoinHandle<()>) -> Result<(), std::io::Error> {
    handle.await.map_err(std::io::Error::other)
}

fn assert_indeterminate(result: &RefreshResult) {
    assert!(
        matches!(result, Err(RefreshTokenError::Indeterminate(_))),
        "expected indeterminate refresh result, got {result:?}",
    );
}

async fn wait_until_dispatched(
    server: &MockServer,
    manager: &Arc<AuthManager>,
) -> Result<(), Box<dyn Error>> {
    tokio::time::timeout(OAuthHttpOptions::DEFAULT_REQUEST_TIMEOUT, async {
        loop {
            let dispatched = received_request_count(server).await? == 1;
            let provisional = matches!(
                &*manager.auth.lock().await,
                CachedAuthState::Indeterminate { .. }
            );
            if dispatched && provisional {
                return Ok::<(), std::io::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(())
}

async fn received_request_count(server: &MockServer) -> Result<usize, std::io::Error> {
    server
        .received_requests()
        .await
        .map(|requests| requests.len())
        .ok_or_else(|| std::io::Error::other("request recording is unavailable"))
}

async fn access_token(manager: &Arc<AuthManager>) -> Result<String, RefreshTokenError> {
    let auth = manager
        .auth()
        .await?
        .ok_or_else(|| RefreshTokenError::Permanent("test credential missing".to_owned()))?;
    auth.get_token()
        .map(str::to_owned)
        .map_err(|error| RefreshTokenError::Permanent(error.to_string()))
}

fn seed_auth(refresh_token: &str) -> Result<AuthDotJson, serde_json::Error> {
    serde_json::from_value(serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token_fixture(),
            "access_token": "seed-access-token",
            "refresh_token": refresh_token,
            "account_id": "seed-account",
        }
    }))
}

fn id_token_fixture() -> String {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "seed-account"
            }
        })
        .to_string(),
    );
    format!("{header}.{claims}.")
}

fn refresh_response_body() -> serde_json::Value {
    serde_json::json!({
        "id_token": id_token_fixture(),
        "access_token": "new-access-token",
        "refresh_token": "rotated-refresh-token",
        "account_id": "seed-account",
    })
}

struct DisconnectServer {
    address: SocketAddr,
    task: JoinHandle<Result<usize, std::io::Error>>,
}

impl DisconnectServer {
    async fn start() -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let mut posts = 0;
            loop {
                let (stream, _) = listener.accept().await?;
                match read_request(stream).await? {
                    RequestKind::Post => posts += 1,
                    RequestKind::Stop => return Ok(posts),
                    RequestKind::Other => {}
                }
            }
        });
        Ok(Self { address, task })
    }

    fn uri(&self) -> String {
        format!("http://{}", self.address)
    }

    async fn stop(self) -> Result<usize, std::io::Error> {
        let mut stream = TcpStream::connect(self.address).await?;
        stream
            .write_all(b"GET /__stop__ HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        stream.shutdown().await?;
        self.task.await.map_err(std::io::Error::other)?
    }
}

enum RequestKind {
    Post,
    Stop,
    Other,
}

async fn read_request(mut stream: TcpStream) -> Result<RequestKind, std::io::Error> {
    let mut request = Vec::new();
    loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "test authority received a truncated request",
            ));
        }
        request.extend_from_slice(&chunk[..read]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&request[..header_end])
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let body_start = header_end + 4;
        if request.len() < body_start + content_length(headers)? {
            continue;
        }
        return Ok(if headers.starts_with("POST ") {
            RequestKind::Post
        } else if headers.starts_with("GET /__stop__ ") {
            RequestKind::Stop
        } else {
            RequestKind::Other
        });
    }
}

fn content_length(headers: &str) -> Result<usize, std::io::Error> {
    for header in headers.lines() {
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
    }
    Ok(0)
}

const _: fn() = || {
    fn check<T: Send + Sync>() {}
    check::<RefreshAttempt>();
};
