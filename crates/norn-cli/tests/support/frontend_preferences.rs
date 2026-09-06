//! Isolated process barriers for public frontend and MCP settings mutations.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use norn::config::{
    McpConfigState, McpPersistentMutation, McpPersistentScope, McpServerSettings,
    SettingsPublication, TuiPreferenceScope, TuiPreferencesError, TuiPreferencesSnapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
/// Test liveness bound only; barriers, never elapsed time, determine write ordering.
pub const DEADLINE: Duration = Duration::from_secs(20);
const CHILD_CONTROL: &str = "NFP_FIXTURE_CONTROL";
const CHILD_PLAN: &str = "NFP_FIXTURE_PLAN";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum Scope {
    User,
    Local,
}

impl Scope {
    pub fn path(self, home: &Path, root: &Path) -> PathBuf {
        match self {
            Self::User => home.join("settings.json"),
            Self::Local => root.join(".norn/settings.local.json"),
        }
    }

    pub fn lock_path(self, home: &Path, root: &Path) -> PathBuf {
        match self {
            Self::User => home.join(".mcp-settings.lock"),
            Self::Local => root.join(".norn"),
        }
    }

    fn preference(self) -> TuiPreferenceScope {
        match self {
            Self::User => TuiPreferenceScope::User,
            Self::Local => TuiPreferenceScope::WorkspaceLocal,
        }
    }

    fn mcp(self) -> McpPersistentScope {
        match self {
            Self::User => McpPersistentScope::User,
            Self::Local => McpPersistentScope::WorkspaceLocal,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Mutation {
    Mcp,
    Frontend {
        owned: Vec<String>,
        replacement: Map<String, Value>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct Plan {
    scope: Scope,
    home: PathBuf,
    root: PathBuf,
    mutation: Mutation,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum Barrier {
    ObserveContendedLock,
    Publish,
    Finish,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Observation {
    Ready {
        pid: u32,
        path: PathBuf,
        original: Option<Value>,
    },
    Contended {
        path: PathBuf,
    },
    Published {
        path: PathBuf,
        changed: bool,
        original: Option<Value>,
    },
    Conflict {
        path: PathBuf,
        key: String,
    },
}

pub struct Worker {
    child: Child,
    control: tokio::io::BufReader<TcpStream>,
}

impl Worker {
    pub async fn spawn(
        home: &Path,
        root: &Path,
        scope: Scope,
        mutation: Mutation,
    ) -> TestResult<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let plan = Plan {
            home: home.to_path_buf(),
            root: root.to_path_buf(),
            scope,
            mutation,
        };
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("frontend_preferences_child_entrypoint")
            .arg("--nocapture")
            .env("NORN_HOME", home)
            .env("HOME", home)
            .env(CHILD_CONTROL, listener.local_addr()?.to_string())
            .env(CHILD_PLAN, serde_json::to_string(&plan)?)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let control = tokio::time::timeout(DEADLINE, async {
            tokio::select! {
                accepted = listener.accept() => Ok(accepted?.0),
                exited = child.wait() => Err(io::Error::other(format!(
                    "settings helper exited before barrier connection: {}", exited?
                ))),
            }
        })
        .await??;
        Ok(Self {
            child,
            control: tokio::io::BufReader::new(control),
        })
    }

    pub async fn send(&mut self, barrier: Barrier) -> TestResult {
        let mut bytes = serde_json::to_vec(&barrier)?;
        bytes.push(b'\n');
        tokio::time::timeout(DEADLINE, self.control.get_mut().write_all(&bytes)).await??;
        Ok(())
    }

    pub async fn receive(&mut self) -> TestResult<Observation> {
        let mut line = String::new();
        let count = tokio::time::timeout(DEADLINE, self.control.read_line(&mut line)).await??;
        if count == 0 {
            return Err(child_failure(
                &mut self.child,
                "settings helper closed its barrier without an observation",
            )
            .await?
            .into());
        }
        Ok(serde_json::from_str(&line)?)
    }

    pub async fn finish(mut self) -> TestResult {
        self.send(Barrier::Finish).await?;
        let output = tokio::time::timeout(DEADLINE, self.child.wait_with_output()).await??;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "settings helper failed: {}; stdout: {}; stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        Ok(())
    }
}

async fn child_failure(child: &mut Child, context: &str) -> io::Result<io::Error> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other(format!("{context}; helper stdout pipe is unavailable")))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other(format!("{context}; helper stderr pipe is unavailable")))?;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let captured = tokio::time::timeout(DEADLINE, async {
        tokio::try_join!(
            child.wait(),
            stdout.read_to_end(&mut output),
            stderr.read_to_end(&mut errors)
        )
    })
    .await;
    let outcome = match captured {
        Ok(Ok((status, ..))) => status.to_string(),
        failure => {
            if child.try_wait()?.is_none() {
                child.kill().await.map_err(|error| {
                    io::Error::other(format!("{context}; helper termination failed: {error}"))
                })?;
            }
            format!("output collection failed; helper reaped: {failure:?}")
        }
    };
    Ok(io::Error::other(format!(
        "{context}; {outcome}; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output),
        String::from_utf8_lossy(&errors)
    )))
}

pub fn child_entrypoint() -> TestResult {
    let Some(control) = std::env::var_os(CHILD_CONTROL) else {
        tracing::info!("frontend settings helper is launched by the process tests");
        return Ok(());
    };
    let address = control
        .to_str()
        .ok_or("settings helper address is not UTF-8")?;
    let plan: Plan = serde_json::from_str(&std::env::var(CHILD_PLAN)?)?;
    let stream = std::net::TcpStream::connect(address)?;
    stream.set_read_timeout(Some(DEADLINE))?;
    stream.set_write_timeout(Some(DEADLINE))?;
    let mut control = io::BufReader::new(stream);
    let path = plan.scope.path(&plan.home, &plan.root);
    let document: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let original = document.get("tui").cloned();
    let mut prepared = Prepared::new(&plan, original.clone())?;
    emit(
        &mut control,
        &Observation::Ready {
            pid: std::process::id(),
            path,
            original,
        },
    )?;
    loop {
        let mut line = String::new();
        if control.read_line(&mut line)? == 0 {
            return Err(
                io::Error::other("parent closed the settings barrier before Finish").into(),
            );
        }
        match serde_json::from_str::<Barrier>(&line)? {
            Barrier::ObserveContendedLock => {
                let path = plan.scope.lock_path(&plan.home, &plan.root);
                let file = File::open(&path)?;
                match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                {
                    Err(rustix::io::Errno::WOULDBLOCK) => {}
                    Err(error) => return Err(io::Error::from(error).into()),
                    Ok(()) => {
                        file.unlock()?;
                        return Err(io::Error::other(format!(
                            "expected settings lock contention at {}",
                            path.display()
                        ))
                        .into());
                    }
                }
                emit(&mut control, &Observation::Contended { path })?;
            }
            Barrier::Publish => emit(&mut control, &prepared.publish(plan.scope)?)?,
            Barrier::Finish => return Ok(()),
        }
    }
}

enum Prepared {
    Mcp(McpConfigState),
    Frontend {
        snapshot: TuiPreferencesSnapshot,
        owned: Vec<String>,
        replacement: Map<String, Value>,
    },
}

impl Prepared {
    fn new(plan: &Plan, original: Option<Value>) -> TestResult<Self> {
        match &plan.mutation {
            Mutation::Mcp => Ok(Self::Mcp(McpConfigState::load(
                &plan.root,
                BTreeMap::new(),
            )?)),
            Mutation::Frontend { owned, replacement } => Ok(Self::Frontend {
                snapshot: TuiPreferencesSnapshot::from_layer(
                    plan.scope.preference(),
                    &plan.root,
                    original,
                )?,
                owned: owned.clone(),
                replacement: replacement.clone(),
            }),
        }
    }

    fn publish(&mut self, scope: Scope) -> TestResult<Observation> {
        match self {
            Self::Mcp(state) => {
                let change = state.persist(
                    scope.mcp(),
                    &McpPersistentMutation::Upsert {
                        name: "parallel-fixture".to_owned(),
                        definition: McpServerSettings {
                            command: Some("fixture-server-not-launched".to_owned()),
                            enabled: Some(false),
                            ..McpServerSettings::default()
                        },
                    },
                )?;
                Ok(Observation::Published {
                    path: change.path().to_path_buf(),
                    changed: change.changed(),
                    original: None,
                })
            }
            Self::Frontend {
                snapshot,
                owned,
                replacement,
            } => {
                let keys: Vec<_> = owned.iter().map(String::as_str).collect();
                match snapshot.patch(&keys, replacement) {
                    Ok(change) => {
                        let changed = match change.publication {
                            SettingsPublication::Unchanged => false,
                            SettingsPublication::PublishedDurable => true,
                            SettingsPublication::PublishedDurabilityUncertain(error) => {
                                return Err(error.into());
                            }
                        };
                        *snapshot = change.snapshot;
                        Ok(Observation::Published {
                            path: snapshot.path().to_path_buf(),
                            changed,
                            original: snapshot.original().cloned(),
                        })
                    }
                    Err(TuiPreferencesError::Conflict { path, key }) => {
                        Ok(Observation::Conflict { path, key })
                    }
                    Err(error) => Err(error.into()),
                }
            }
        }
    }
}

fn emit(control: &mut io::BufReader<std::net::TcpStream>, observation: &Observation) -> TestResult {
    serde_json::to_writer(control.get_mut(), observation)?;
    control.get_mut().write_all(b"\n")?;
    control.get_mut().flush()?;
    Ok(())
}
