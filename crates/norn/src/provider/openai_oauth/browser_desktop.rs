//! Browser-launch discovery for Unix desktop environments.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{BrowserLaunchError, LaunchOwnership, LaunchSpec, isolated_command};

const TRUSTED_LAUNCHER_DIRECTORIES: &[&str] = &[
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/run/current-system/sw/bin",
];

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub(super) fn launch_spec(target: &url::Url) -> Result<LaunchSpec, BrowserLaunchError> {
    let directories = trusted_launcher_directories();
    launch_spec_from(target, &directories)
}

pub(super) fn launch_spec_from(
    target: &url::Url,
    directories: &[PathBuf],
) -> Result<LaunchSpec, BrowserLaunchError> {
    let Some((program, prefix)) = find_launcher(directories) else {
        return Err(BrowserLaunchError::Structural(
            "no supported default browser launcher is installed",
        ));
    };
    let launcher_path = std::env::join_paths(directories).map_err(|_error| {
        BrowserLaunchError::Structural("default browser launcher path is invalid")
    })?;
    let mut command = isolated_command(&program, Path::new("/"));
    command
        .args(prefix)
        .arg(target.as_str())
        .stdin(Stdio::null());
    copy_environment_allowlist(
        &mut command,
        &[
            "DBUS_SESSION_BUS_ADDRESS",
            "DESKTOP_SESSION",
            "DISPLAY",
            "HOME",
            "WAYLAND_DISPLAY",
            "WSLENV",
            "WSL_DISTRO_NAME",
            "WSL_INTEROP",
            "XAUTHORITY",
            "XDG_CONFIG_DIRS",
            "XDG_CONFIG_HOME",
            "XDG_CURRENT_DESKTOP",
            "XDG_DATA_DIRS",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
            "XDG_SESSION_DESKTOP",
            "XDG_SESSION_TYPE",
        ],
    );
    command.env("PATH", launcher_path);
    Ok(LaunchSpec {
        command,
        stdin: None,
        descriptor_weight: crate::resource::NULL_STDIO_SUBPROCESS_PEAK,
        ownership: LaunchOwnership {
            terminate_on_drop: false,
        },
    })
}

pub(super) fn trusted_launcher_directories() -> Vec<PathBuf> {
    TRUSTED_LAUNCHER_DIRECTORIES
        .iter()
        .map(PathBuf::from)
        .collect()
}

fn copy_environment_allowlist(command: &mut Command, names: &[&str]) {
    for name in names {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn find_launcher(directories: &[PathBuf]) -> Option<(PathBuf, &'static [&'static str])> {
    const CANDIDATES: &[(&str, &[&str])] = &[
        ("xdg-open", &[]),
        ("gio", &["open"]),
        ("wslview", &[]),
        ("gnome-open", &[]),
        ("kde-open5", &[]),
        ("kde-open", &[]),
        ("x-www-browser", &[]),
        ("sensible-browser", &[]),
    ];
    CANDIDATES.iter().find_map(|(name, prefix)| {
        directories.iter().find_map(|directory| {
            let candidate = directory.join(name);
            is_executable_file(&candidate).then_some((candidate, *prefix))
        })
    })
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}
