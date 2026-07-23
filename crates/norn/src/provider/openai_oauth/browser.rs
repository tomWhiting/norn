//! Isolated browser launcher for OAuth authorization targets.

use std::io::{self, Write as _};
#[cfg(any(test, target_os = "macos"))]
use std::path::Path;
#[cfg(any(test, target_os = "macos"))]
use std::process::Stdio;
use std::process::{Child, ChildStdin, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(any(all(test, unix), target_os = "macos"))]
use crate::resource::STDIN_PIPE_NULL_OUTPUT_SPAWN_PEAK;
use crate::resource::{DescriptorAdmissionError, DescriptorGovernor, DescriptorPermit};

#[cfg(any(
    all(test, unix),
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
))]
#[path = "browser_desktop.rs"]
mod desktop;

const REAPER_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(target_os = "macos")]
const MACOS_JXA_SCRIPT: &str = r"
ObjC.import('AppKit');
ObjC.import('Foundation');
const input = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
const textObject = $.NSString.alloc.initWithDataEncoding(input, $.NSUTF8StringEncoding);
if (!textObject) throw new Error('invalid authorization target encoding');
const text = ObjC.unwrap(textObject);
if (text.length === 0) throw new Error('missing authorization target');
const target = $.NSURL.URLWithString(text);
if (!target || ObjC.unwrap(target.scheme) !== 'https') throw new Error('invalid authorization target');
if (!$.NSWorkspace.sharedWorkspace.openURL(target)) throw new Error('authorization target was not opened');
true;
";

/// A fixed, non-disclosing browser-launch failure.
#[derive(Debug, thiserror::Error)]
pub(super) enum BrowserLaunchError {
    /// Safe descriptor capacity could not admit the launcher.
    #[error(transparent)]
    DescriptorAdmission(#[from] DescriptorAdmissionError),
    /// The local launcher boundary failed structurally.
    #[error("{0}")]
    Structural(&'static str),
}

struct LaunchSpec {
    command: Command,
    stdin: Option<Vec<u8>>,
    descriptor_weight: u32,
    ownership: LaunchOwnership,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LaunchOwnership {
    terminate_on_drop: bool,
}

struct StdinDelivery {
    input: Vec<u8>,
    _permit: DescriptorPermit,
}

struct Reaper {
    spec: LaunchSpec,
    permit: DescriptorPermit,
    startup_tx: mpsc::SyncSender<Result<(), BrowserLaunchError>>,
    status_tx: mpsc::SyncSender<Result<(), BrowserLaunchError>>,
    cancel: Arc<AtomicBool>,
}

impl Reaper {
    fn run(self) {
        let Self {
            mut spec,
            mut permit,
            startup_tx,
            status_tx,
            cancel,
        } = self;
        let ownership = spec.ownership;
        let mut child = match spec.command.spawn() {
            Ok(child) => child,
            Err(_error) => {
                let _ignored = startup_tx.send(Err(BrowserLaunchError::Structural(
                    "failed to spawn default browser launcher",
                )));
                return;
            }
        };
        let stdin = if let Some(input) = spec.stdin.take() {
            let Some(retained) = permit.split(1) else {
                finish_after_supervision_failure(&mut child, ownership);
                let _ignored = startup_tx.send(Err(BrowserLaunchError::Structural(
                    "browser launcher stdin admission split failed",
                )));
                return;
            };
            Some(StdinDelivery {
                input,
                _permit: retained,
            })
        } else {
            None
        };
        drop(permit);
        let mut writer = match stdin {
            Some(delivery) => match start_stdin_writer(&mut child, delivery) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    finish_after_supervision_failure(&mut child, ownership);
                    let _ignored = startup_tx.send(Err(error));
                    return;
                }
            },
            None => None,
        };
        if startup_tx.send(Ok(())).is_err() {
            finish_after_supervision_failure(&mut child, ownership);
            let _ignored = join_stdin_writer(writer.take());
            return;
        }
        if !ownership.terminate_on_drop {
            let result = child.wait().map_or_else(
                |_error| {
                    Err(BrowserLaunchError::Structural(
                        "failed to observe default browser launcher",
                    ))
                },
                classify_status,
            );
            let _ignored = status_tx.send(result);
            return;
        }
        let result = loop {
            if cancel.load(Ordering::Acquire) {
                finish_after_supervision_failure(&mut child, ownership);
                let _ignored = join_stdin_writer(writer.take());
                return;
            }
            if writer.as_ref().is_some_and(JoinHandle::is_finished)
                && let Err(error) = join_stdin_writer(writer.take())
            {
                finish_after_supervision_failure(&mut child, ownership);
                break Err(error);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if let Err(error) = join_stdin_writer(writer.take()) {
                        break Err(error);
                    }
                    break classify_status(status);
                }
                Ok(None) => std::thread::sleep(REAPER_POLL_INTERVAL),
                Err(_error) => {
                    finish_after_supervision_failure(&mut child, ownership);
                    let _ignored = join_stdin_writer(writer.take());
                    break Err(BrowserLaunchError::Structural(
                        "failed to observe default browser launcher",
                    ));
                }
            }
        };
        let _ignored = status_tx.send(result);
    }
}

/// Active launcher supervision for an owned helper or delegated desktop opener.
pub(super) struct BrowserLaunch {
    cancel: Arc<AtomicBool>,
    status: mpsc::Receiver<Result<(), BrowserLaunchError>>,
    reaper: Option<JoinHandle<()>>,
    complete: bool,
    ownership: LaunchOwnership,
}

impl BrowserLaunch {
    /// Surfaces a completed launcher failure without waiting for the process.
    pub(super) fn check(&mut self) -> Result<(), BrowserLaunchError> {
        if self.complete {
            return Ok(());
        }
        match self.status.try_recv() {
            Ok(result) => {
                self.complete = true;
                self.join_reaper();
                result
            }
            Err(mpsc::TryRecvError::Empty) => Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.complete = true;
                self.join_reaper();
                Err(BrowserLaunchError::Structural(
                    "browser launcher supervisor ended unexpectedly",
                ))
            }
        }
    }

    fn join_reaper(&mut self) {
        if let Some(reaper) = self.reaper.take() {
            let _join_result = reaper.join();
        }
    }
}

impl Drop for BrowserLaunch {
    fn drop(&mut self) {
        if self.ownership.terminate_on_drop {
            self.cancel.store(true, Ordering::Release);
            self.join_reaper();
        } else {
            let _detached = self.reaper.take();
        }
    }
}

/// Starts an HTTPS authorization target without logging it.
pub(super) fn open_authorization_url(
    target: &url::Url,
) -> Result<BrowserLaunch, BrowserLaunchError> {
    if target.scheme() != "https" {
        return Err(BrowserLaunchError::Structural(
            "OAuth authorization targets must use HTTPS",
        ));
    }
    let spec = launch_spec(target)?;
    spawn_supervised(spec)
}

fn spawn_supervised(spec: LaunchSpec) -> Result<BrowserLaunch, BrowserLaunchError> {
    let governor = DescriptorGovernor::global()?;
    spawn_supervised_with_governor(spec, &governor)
}

fn spawn_supervised_with_governor(
    spec: LaunchSpec,
    governor: &DescriptorGovernor,
) -> Result<BrowserLaunch, BrowserLaunchError> {
    if spec.stdin.is_some() && !spec.ownership.terminate_on_drop {
        return Err(BrowserLaunchError::Structural(
            "delegated browser launchers cannot retain stdin",
        ));
    }
    let permit = governor.try_acquire(spec.descriptor_weight)?;
    let ownership = spec.ownership;
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    let (status_tx, status_rx) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let reaper = Reaper {
        spec,
        permit,
        startup_tx,
        status_tx,
        cancel: Arc::clone(&cancel),
    };
    let reaper = std::thread::Builder::new()
        .name("norn-oauth-browser-reaper".to_owned())
        .spawn(move || reaper.run())
        .map_err(|_error| {
            BrowserLaunchError::Structural("failed to start browser launcher supervisor")
        })?;

    let startup = startup_rx.recv().map_err(|_disconnected| {
        BrowserLaunchError::Structural("browser launcher supervisor ended before startup")
    });
    if let Err(error) = startup.and_then(|result| result) {
        let _join_result = reaper.join();
        return Err(error);
    }
    Ok(BrowserLaunch {
        cancel,
        status: status_rx,
        reaper: Some(reaper),
        complete: false,
        ownership,
    })
}

fn start_stdin_writer(
    child: &mut Child,
    delivery: StdinDelivery,
) -> Result<JoinHandle<io::Result<()>>, BrowserLaunchError> {
    let stdin = child.stdin.take().ok_or(BrowserLaunchError::Structural(
        "browser launcher stdin was not captured",
    ))?;
    std::thread::Builder::new()
        .name("norn-oauth-browser-stdin".to_owned())
        .spawn(move || write_child_stdin(stdin, &delivery.input))
        .map_err(|_error| {
            BrowserLaunchError::Structural("failed to start browser launcher input delivery")
        })
}

fn write_child_stdin(mut stdin: ChildStdin, input: &[u8]) -> io::Result<()> {
    stdin.write_all(input)
}

fn join_stdin_writer(writer: Option<JoinHandle<io::Result<()>>>) -> Result<(), BrowserLaunchError> {
    let Some(writer) = writer else {
        return Ok(());
    };
    match writer.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_error)) => Err(BrowserLaunchError::Structural(
            "failed to send authorization target to browser launcher",
        )),
        Err(_panic) => Err(BrowserLaunchError::Structural(
            "failed to send authorization target to browser launcher",
        )),
    }
}

fn classify_status(status: ExitStatus) -> Result<(), BrowserLaunchError> {
    if status.success() {
        Ok(())
    } else {
        Err(BrowserLaunchError::Structural(
            "default browser launcher exited unsuccessfully",
        ))
    }
}

fn terminate_and_reap(child: &mut Child) {
    let _kill_result = child.kill();
    let _wait_result = child.wait();
}

fn finish_after_supervision_failure(child: &mut Child, ownership: LaunchOwnership) {
    if ownership.terminate_on_drop {
        terminate_and_reap(child);
    } else {
        let _wait_result = child.wait();
    }
}

#[cfg(any(test, target_os = "macos"))]
fn isolated_command(program: &Path, current_dir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .current_dir(current_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(target_os = "macos")]
fn launch_spec(target: &url::Url) -> Result<LaunchSpec, BrowserLaunchError> {
    let program = Path::new("/usr/bin/osascript");
    if !program.is_file() {
        return Err(BrowserLaunchError::Structural(
            "the macOS browser launcher is unavailable",
        ));
    }
    let mut command = isolated_command(program, Path::new("/"));
    command
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(MACOS_JXA_SCRIPT)
        .stdin(Stdio::piped());
    Ok(LaunchSpec {
        command,
        stdin: Some(target.as_str().as_bytes().to_vec()),
        descriptor_weight: STDIN_PIPE_NULL_OUTPUT_SPAWN_PEAK,
        ownership: LaunchOwnership {
            terminate_on_drop: true,
        },
    })
}

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn launch_spec(target: &url::Url) -> Result<LaunchSpec, BrowserLaunchError> {
    desktop::launch_spec(target)
}

#[cfg(not(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn launch_spec(_target: &url::Url) -> Result<LaunchSpec, BrowserLaunchError> {
    Err(BrowserLaunchError::Structural(
        "opening a browser is unsupported on this platform",
    ))
}

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;
