//! Barrier-observed shared writer locking and publication in actual core subprocesses.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};

use super::{
    McpConfigState, McpPersistentMutation, McpPersistentScope, McpServerSettings,
    SettingsPublication, TuiPreferenceScope, TuiPreferencesSnapshot,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
// Fixture liveness guard only; named pipe messages determine every ordering edge.
const DEADLINE: Duration = Duration::from_secs(20);
const CONTROL: &str = "NFP_WRITER_TEST_CONTROL";
const PLAN: &str = "NFP_WRITER_TEST_PLAN";
const CHILD_TEST: &str = "config::settings_write_process_tests::writer_process_entrypoint";

thread_local! {
    static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum Scope {
    User,
    Local,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum Writer {
    Mcp,
    Frontend,
}

#[derive(Debug, Deserialize, Serialize)]
struct Plan {
    root: PathBuf,
    home: PathBuf,
    scope: Scope,
    writer: Writer,
    second: bool,
}

impl Plan {
    fn target(&self) -> PathBuf {
        match self.scope {
            Scope::User => self.home.join("settings.json"),
            Scope::Local => self.root.join(".norn/settings.local.json"),
        }
    }
    fn lock_path(&self) -> PathBuf {
        match self.scope {
            Scope::User => self.home.join(".mcp-settings.lock"),
            Scope::Local => self.root.join(".norn"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Identity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Deserialize, Serialize)]
enum Event {
    Ready { pid: u32, target: PathBuf },
    Contended { identity: Identity },
    Read { identity: Identity, document: Value },
    Published,
    Done,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
enum Barrier {
    Start,
    Publish,
    Release,
}

struct Observer {
    socket: io::BufReader<std::net::TcpStream>,
    path: PathBuf,
    second: bool,
    identity: Option<Identity>,
}

/// Test-only callback on the exact descriptor used by the following ordinary lock call.
pub(super) fn before_lock(file: &File, path: &Path) -> io::Result<()> {
    observe(|observer| {
        observer.check_path(path)?;
        let metadata = file.metadata()?;
        let identity = Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        observer.identity = Some(identity);
        if observer.second {
            match rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    observer.emit(&Event::Contended { identity })?;
                }
                Err(error) => return Err(io::Error::from(error)),
                Ok(()) => {
                    file.unlock()?;
                    return Err(io::Error::other(
                        "second writer acquired the first writer's held lock",
                    ));
                }
            }
        }
        Ok(())
    })
}

/// Stop with the real guard held, after the ordinary fresh document read.
pub(super) fn after_read(path: &Path, content: Option<&str>) -> io::Result<()> {
    observe(|observer| {
        observer.check_path(path)?;
        let identity = observer
            .identity
            .ok_or_else(|| io::Error::other("read preceded descriptor observation"))?;
        let document = serde_json::from_str(
            content.ok_or_else(|| io::Error::other("fixture document missing"))?,
        )?;
        observer.emit(&Event::Read { identity, document })?;
        observer.wait(Barrier::Publish)
    })
}

/// Stop after real replacement/directory-sync while the same guard remains held.
pub(super) fn after_publish(path: &Path, publication: &SettingsPublication) -> io::Result<()> {
    observe(|observer| {
        observer.check_path(path)?;
        if !matches!(publication, SettingsPublication::PublishedDurable) {
            return Err(io::Error::other(format!(
                "fixture expected durable publication: {publication:?}"
            )));
        }
        observer.emit(&Event::Published)?;
        observer.wait(Barrier::Release)
    })
}

fn observe(action: impl FnOnce(&mut Observer) -> io::Result<()>) -> io::Result<()> {
    OBSERVER.with(|slot| {
        let mut slot = slot
            .try_borrow_mut()
            .map_err(|error| io::Error::other(format!("writer observer reentry: {error}")))?;
        slot.as_mut().map_or(Ok(()), action)
    })
}

impl Observer {
    fn check_path(&self, path: &Path) -> io::Result<()> {
        if path != self.path {
            return Err(io::Error::other(format!(
                "writer observed unexpected target {}",
                path.display()
            )));
        }
        Ok(())
    }
    fn emit(&mut self, event: &Event) -> io::Result<()> {
        serde_json::to_writer(self.socket.get_mut(), event)?;
        self.socket.get_mut().write_all(b"\n")?;
        self.socket.get_mut().flush()
    }
    fn wait(&mut self, expected: Barrier) -> io::Result<()> {
        let mut line = String::new();
        if self.socket.read_line(&mut line)? == 0 {
            return Err(io::Error::other("writer barrier closed"));
        }
        let actual: Barrier = serde_json::from_str(&line)?;
        if actual != expected {
            return Err(io::Error::other(format!(
                "writer expected {expected:?}, received {actual:?}"
            )));
        }
        Ok(())
    }
}

#[test]
fn writer_process_entrypoint() -> TestResult {
    let Some(address) = std::env::var_os(CONTROL) else {
        tracing::info!("writer subprocess is launched by its barrier coordinator");
        return Ok(());
    };
    let plan: Plan = serde_json::from_str(&std::env::var(PLAN)?)?;
    let socket =
        std::net::TcpStream::connect(address.to_str().ok_or("writer address is not UTF-8")?)?;
    socket.set_read_timeout(Some(DEADLINE))?;
    socket.set_write_timeout(Some(DEADLINE))?;
    let mut observer = Observer {
        socket: io::BufReader::new(socket),
        path: plan.target(),
        second: plan.second,
        identity: None,
    };
    let original: Value = serde_json::from_slice(&std::fs::read(plan.target())?)?;
    let mut mcp = McpConfigState::load(&plan.root, BTreeMap::new())?;
    let scope = match plan.scope {
        Scope::User => TuiPreferenceScope::User,
        Scope::Local => TuiPreferenceScope::WorkspaceLocal,
    };
    let snapshot =
        TuiPreferencesSnapshot::from_layer(scope, &plan.root, original.get("tui").cloned())?;
    observer.emit(&Event::Ready {
        pid: std::process::id(),
        target: plan.target(),
    })?;
    observer.wait(Barrier::Start)?;
    OBSERVER.with(|slot| -> TestResult {
        let mut slot = slot.try_borrow_mut()?;
        if slot.is_some() {
            return Err("writer observer already installed".into());
        }
        *slot = Some(observer);
        Ok(())
    })?;
    let mutation_result: TestResult = match plan.writer {
        Writer::Mcp => {
            let scope = match plan.scope {
                Scope::User => McpPersistentScope::User,
                Scope::Local => McpPersistentScope::WorkspaceLocal,
            };
            mcp.persist(scope, &mcp_mutation())
                .map(|change| {
                    assert!(change.changed());
                })
                .map_err(Into::into)
        }
        Writer::Frontend => snapshot
            .patch(
                &["view"],
                &serde_json::Map::from_iter([("view".to_owned(), json!({"changes_open":true}))]),
            )
            .map(|change| {
                assert!(matches!(
                    change.publication,
                    SettingsPublication::PublishedDurable
                ));
            })
            .map_err(Into::into),
    };
    let mut observer = OBSERVER.with(|slot| -> TestResult<Observer> {
        slot.try_borrow_mut()?
            .take()
            .ok_or_else(|| "writer observer missing after publication".into())
    })?;
    mutation_result?;
    observer.emit(&Event::Done)?;
    Ok(())
}

#[tokio::test]
async fn user_writers_hold_one_physical_lock_through_read_and_publication() -> TestResult {
    both_orders(Scope::User).await
}

#[tokio::test]
async fn local_writers_hold_one_physical_lock_through_read_and_publication() -> TestResult {
    both_orders(Scope::Local).await
}

struct Process {
    child: Child,
    socket: tokio::io::BufReader<TcpStream>,
}

impl Process {
    async fn spawn(plan: &Plan) -> TestResult<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg(CHILD_TEST)
            .arg("--nocapture")
            .env("NORN_HOME", &plan.home)
            .env("HOME", &plan.home)
            .env(CONTROL, listener.local_addr()?.to_string())
            .env(PLAN, serde_json::to_string(plan)?)
            .current_dir(&plan.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let socket = tokio::time::timeout(DEADLINE, async {
            tokio::select! {
                connection = listener.accept() => Ok(connection?.0),
                status = child.wait() => Err(io::Error::other(format!("writer child exited before connecting: {}", status?))),
            }
        }).await??;
        Ok(Self {
            child,
            socket: tokio::io::BufReader::new(socket),
        })
    }
    async fn send(&mut self, barrier: Barrier) -> TestResult {
        let mut bytes = serde_json::to_vec(&barrier)?;
        bytes.push(b'\n');
        tokio::time::timeout(DEADLINE, self.socket.get_mut().write_all(&bytes)).await??;
        Ok(())
    }
    async fn receive(&mut self) -> TestResult<Event> {
        let mut line = String::new();
        if tokio::time::timeout(DEADLINE, self.socket.read_line(&mut line)).await?? == 0 {
            return Err("writer child closed the observation stream".into());
        }
        Ok(serde_json::from_str(&line)?)
    }
    async fn finish(mut self) -> TestResult {
        assert!(matches!(self.receive().await?, Event::Done));
        let output = tokio::time::timeout(DEADLINE, self.child.wait_with_output()).await??;
        assert!(
            output.status.success(),
            "writer child {}; stdout:{}; stderr:{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

async fn both_orders(scope: Scope) -> TestResult {
    for mcp_first in [true, false] {
        let directory = tempfile::tempdir()?;
        let base = directory.path().canonicalize()?;
        let home = base.join("home");
        let root = base.join("workspace");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&root)?;
        std::fs::create_dir(root.join(".norn"))?;
        let first_writer = if mcp_first {
            Writer::Mcp
        } else {
            Writer::Frontend
        };
        let second_writer = if mcp_first {
            Writer::Frontend
        } else {
            Writer::Mcp
        };
        let first_plan = Plan {
            home,
            root,
            scope,
            writer: first_writer,
            second: false,
        };
        let second_plan = Plan {
            home: first_plan.home.clone(),
            root: first_plan.root.clone(),
            scope,
            writer: second_writer,
            second: true,
        };
        let original = json!({"future":{"kept":true},"tui":{"view":{"changes_open":false},"composer":{"untouched":"yes"}}});
        std::fs::write(first_plan.target(), serde_json::to_vec(&original)?)?;
        let mut first = Process::spawn(&first_plan).await?;
        let mut second = Process::spawn(&second_plan).await?;
        let first_pid = ready(&mut first, &first_plan).await?;
        let second_pid = ready(&mut second, &second_plan).await?;
        assert_ne!(first_pid, second_pid);
        assert_ne!(first_pid, std::process::id());
        assert_ne!(second_pid, std::process::id());
        first.send(Barrier::Start).await?;
        let identity = read_event(&mut first, &original).await?;
        second.send(Barrier::Start).await?;
        match second.receive().await? {
            Event::Contended {
                identity: attempted,
            } => assert_eq!(attempted, identity),
            event => {
                return Err(format!(
                    "second writer did not contend on first descriptor: {event:?}"
                )
                .into());
            }
        }
        assert_eq!(read_file(&first_plan.target())?, original);
        first.send(Barrier::Publish).await?;
        assert!(matches!(first.receive().await?, Event::Published));
        let mut after_first = original.clone();
        apply_expected(&mut after_first, first_writer)?;
        assert_eq!(read_file(&first_plan.target())?, after_first);
        still_locked(&first_plan, identity)?;
        first.send(Barrier::Release).await?;
        assert_eq!(read_event(&mut second, &after_first).await?, identity);
        second.send(Barrier::Publish).await?;
        assert!(matches!(second.receive().await?, Event::Published));
        let mut complete = after_first;
        apply_expected(&mut complete, second_writer)?;
        assert_eq!(read_file(&first_plan.target())?, complete);
        still_locked(&second_plan, identity)?;
        second.send(Barrier::Release).await?;
        first.finish().await?;
        second.finish().await?;
    }
    Ok(())
}

async fn ready(process: &mut Process, plan: &Plan) -> TestResult<u32> {
    match process.receive().await? {
        Event::Ready { pid, target } => {
            assert_eq!(target, plan.target());
            Ok(pid)
        }
        event => Err(format!("writer snapshot barrier missing: {event:?}").into()),
    }
}

async fn read_event(process: &mut Process, expected: &Value) -> TestResult<Identity> {
    match process.receive().await? {
        Event::Read { identity, document } => {
            assert_eq!(&document, expected);
            Ok(identity)
        }
        event => Err(format!("actual locked fresh read missing: {event:?}").into()),
    }
}

fn still_locked(plan: &Plan, expected: Identity) -> TestResult {
    let file = File::open(plan.lock_path())?;
    let metadata = file.metadata()?;
    assert_eq!(
        Identity {
            device: metadata.dev(),
            inode: metadata.ino()
        },
        expected
    );
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(()),
        Err(error) => Err(io::Error::from(error).into()),
        Ok(()) => {
            file.unlock()?;
            Err("publication released its document guard too early".into())
        }
    }
}

fn read_file(path: &Path) -> TestResult<Value> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn mcp_mutation() -> McpPersistentMutation {
    McpPersistentMutation::Upsert {
        name: "overlapping".to_owned(),
        definition: McpServerSettings {
            command: Some("fixture-not-started".to_owned()),
            enabled: Some(false),
            ..McpServerSettings::default()
        },
    }
}

fn apply_expected(document: &mut Value, writer: Writer) -> TestResult {
    match writer {
        Writer::Mcp => {
            let McpPersistentMutation::Upsert { name, definition } = mcp_mutation() else {
                return Err("fixture mutation changed kind".into());
            };
            document["mcp_servers"] = json!({name: serde_json::to_value(definition)?});
        }
        Writer::Frontend => document["tui"]["view"] = json!({"changes_open":true}),
    }
    Ok(())
}
