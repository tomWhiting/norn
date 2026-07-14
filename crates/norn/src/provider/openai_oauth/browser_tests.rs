use super::*;
#[cfg(unix)]
use std::path::PathBuf;

#[test]
fn rejects_non_https_authorization_targets() -> Result<(), Box<dyn std::error::Error>> {
    let target = url::Url::parse("http://example.invalid/not-secure")?;
    let error = open_authorization_url(&target)
        .err()
        .ok_or_else(|| io::Error::other("non-HTTPS target was accepted"))?;
    assert!(matches!(error, BrowserLaunchError::Structural(_)));
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn macos_launcher_keeps_authorization_url_out_of_argv_and_environment()
-> Result<(), Box<dyn std::error::Error>> {
    const SECRET: &str = "browser-target-secret-must-not-escape";
    let target = url::Url::parse(&format!("https://example.invalid/authorize?state={SECRET}"))?;
    let spec = launch_spec(&target)?;
    let arguments = spec
        .command
        .get_args()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!arguments.contains(SECRET));
    assert!(
        spec.command
            .get_envs()
            .all(|(_name, value)| value.is_none())
    );
    assert_eq!(spec.stdin.as_deref(), Some(target.as_str().as_bytes()));
    assert_eq!(spec.descriptor_weight, STDIN_PIPE_NULL_OUTPUT_SPAWN_PEAK);
    assert!(MACOS_JXA_SCRIPT.contains("const text = ObjC.unwrap(textObject)"));
    assert!(
        MACOS_JXA_SCRIPT
            .contains("if (!$.NSWorkspace.sharedWorkspace.openURL(target)) throw new Error")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn desktop_launcher_discovery_uses_direct_absolute_execution()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    const SECRET: &str = "desktop-launcher-secret-must-not-escape-via-env";
    let directory = tempfile::tempdir()?;
    let launcher = directory.path().join("xdg-open");
    std::fs::write(&launcher, b"")?;
    let mut permissions = launcher.metadata()?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&launcher, permissions)?;
    let target = url::Url::parse(&format!("https://example.invalid/authorize?state={SECRET}"))?;
    let spec = desktop::launch_spec_from(&target, &[directory.path().to_owned()])?;

    assert_eq!(spec.command.get_program(), launcher.as_os_str());
    assert!(
        spec.command
            .get_args()
            .any(|argument| argument == target.as_str())
    );
    assert!(
        spec.command
            .get_envs()
            .all(|(name, _value)| name != "OPENAI_API_KEY")
    );
    assert!(!spec.ownership.terminate_on_drop);
    Ok(())
}

#[cfg(unix)]
#[test]
fn desktop_launcher_search_uses_only_fixed_system_directories() {
    let directories = desktop::trusted_launcher_directories();
    assert!(directories.iter().all(|entry| entry.is_absolute()));
    assert!(!directories.contains(&PathBuf::from("relative")));
    assert!(!directories.contains(&PathBuf::from("/custom/bin")));
    assert_eq!(directories.len(), 4);
    assert!(directories.contains(&PathBuf::from("/usr/bin")));
    assert!(directories.contains(&PathBuf::from("/run/current-system/sw/bin")));
}

#[cfg(unix)]
#[test]
fn dropping_launcher_reaps_child_and_releases_spawn_peak() -> Result<(), Box<dyn std::error::Error>>
{
    let governor = DescriptorGovernor::with_capacity(10);
    let mut command = isolated_command(Path::new("/bin/sleep"), Path::new("/"));
    command.arg("30").stdin(Stdio::piped());
    let spec = LaunchSpec {
        command,
        stdin: Some(vec![b'x'; 512 * 1024]),
        descriptor_weight: STDIN_PIPE_NULL_OUTPUT_SPAWN_PEAK,
        ownership: LaunchOwnership {
            terminate_on_drop: true,
        },
    };
    let launch = spawn_supervised_with_governor(spec, &governor)?;
    assert_eq!(governor.available(), 10);
    drop(launch);
    assert_eq!(governor.available(), 10);
    Ok(())
}

#[cfg(unix)]
#[test]
fn dropping_delegated_launcher_neither_waits_for_nor_terminates_child()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let sentinel = directory.path().join("completed");
    let governor = DescriptorGovernor::with_capacity(10);
    let mut command = isolated_command(Path::new("/bin/sh"), Path::new("/"));
    command
        .arg("-c")
        .arg("sleep 1; printf complete > \"$1\"")
        .arg("norn-oauth-delegated-launcher")
        .arg(&sentinel)
        .stdin(Stdio::null());
    let launch = spawn_supervised_with_governor(
        LaunchSpec {
            command,
            stdin: None,
            descriptor_weight: crate::resource::NULL_STDIO_SUBPROCESS_PEAK,
            ownership: LaunchOwnership {
                terminate_on_drop: false,
            },
        },
        &governor,
    )?;
    assert_eq!(governor.available(), 10);

    let drop_started = std::time::Instant::now();
    drop(launch);
    assert!(drop_started.elapsed() < Duration::from_millis(500));

    let wait_started = std::time::Instant::now();
    while !sentinel.is_file() {
        if wait_started.elapsed() > Duration::from_secs(3) {
            return Err(io::Error::other("delegated launcher did not complete").into());
        }
        std::thread::sleep(REAPER_POLL_INTERVAL);
    }
    assert_eq!(std::fs::read_to_string(sentinel)?, "complete");
    assert_eq!(governor.available(), 10);
    Ok(())
}

#[cfg(unix)]
#[test]
fn delegated_launcher_with_stdin_is_rejected_before_spawn() -> Result<(), Box<dyn std::error::Error>>
{
    let governor = DescriptorGovernor::with_capacity(10);
    let command = isolated_command(Path::new("/does/not/exist"), Path::new("/"));
    let error = spawn_supervised_with_governor(
        LaunchSpec {
            command,
            stdin: Some(Vec::new()),
            descriptor_weight: STDIN_PIPE_NULL_OUTPUT_SPAWN_PEAK,
            ownership: LaunchOwnership {
                terminate_on_drop: false,
            },
        },
        &governor,
    )
    .err()
    .ok_or_else(|| io::Error::other("delegated launcher retained stdin"))?;

    assert!(matches!(error, BrowserLaunchError::Structural(_)));
    assert_eq!(governor.available(), 10);
    Ok(())
}

#[cfg(unix)]
#[test]
fn launcher_descriptor_admission_fails_before_spawn() -> Result<(), Box<dyn std::error::Error>> {
    let governor = DescriptorGovernor::with_capacity(4);
    let mut command = isolated_command(Path::new("/bin/sleep"), Path::new("/"));
    command.arg("30").stdin(Stdio::null());
    let spec = LaunchSpec {
        command,
        stdin: None,
        descriptor_weight: crate::resource::NULL_STDIO_SUBPROCESS_PEAK,
        ownership: LaunchOwnership {
            terminate_on_drop: true,
        },
    };
    let error = spawn_supervised_with_governor(spec, &governor)
        .err()
        .ok_or_else(|| io::Error::other("descriptor admission unexpectedly succeeded"))?;
    assert!(matches!(error, BrowserLaunchError::DescriptorAdmission(_)));
    assert_eq!(governor.available(), 4);
    Ok(())
}

#[cfg(unix)]
#[test]
fn background_stdin_delivery_releases_its_retained_permit() -> Result<(), Box<dyn std::error::Error>>
{
    let governor = DescriptorGovernor::with_capacity(10);
    let mut command = isolated_command(Path::new("/bin/cat"), Path::new("/"));
    command.stdin(Stdio::piped());
    let spec = LaunchSpec {
        command,
        stdin: Some(vec![b'x'; 512 * 1024]),
        descriptor_weight: STDIN_PIPE_NULL_OUTPUT_SPAWN_PEAK,
        ownership: LaunchOwnership {
            terminate_on_drop: true,
        },
    };
    let mut launch = spawn_supervised_with_governor(spec, &governor)?;
    let started = std::time::Instant::now();
    while !launch.complete {
        launch.check()?;
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::other("browser stdin delivery timed out").into());
        }
        std::thread::sleep(REAPER_POLL_INTERVAL);
    }
    assert_eq!(governor.available(), 10);
    Ok(())
}

#[cfg(unix)]
#[test]
fn launcher_errors_do_not_disclose_authorization_url() -> Result<(), Box<dyn std::error::Error>> {
    const SECRET: &str = "browser-target-secret-must-not-escape";
    let mut command = isolated_command(Path::new("/usr/bin/false"), Path::new("/"));
    command.arg(SECRET).stdin(Stdio::null());
    let mut launch = spawn_supervised(LaunchSpec {
        command,
        stdin: None,
        descriptor_weight: crate::resource::NULL_STDIO_SUBPROCESS_PEAK,
        ownership: LaunchOwnership {
            terminate_on_drop: true,
        },
    })?;
    let started = std::time::Instant::now();
    let error = loop {
        match launch.check() {
            Err(error) => break error,
            Ok(()) if launch.complete => {
                return Err(io::Error::other("failing launcher unexpectedly succeeded").into());
            }
            Ok(()) => {}
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::other("launcher status timed out").into());
        }
        std::thread::sleep(REAPER_POLL_INTERVAL);
    };
    assert!(!error.to_string().contains(SECRET));
    Ok(())
}
