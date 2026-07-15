//! Standard-library and Tokio sinks.

use super::super::{ReceiverConstraint, SinkSpec};
use super::support::{function, method};
use crate::writers::model::{FlowClass, OperationKind as K, SinkOrigin, WriterRole as R};

const KNOWN_WRITER_NAMESPACES: &[&str] = &[
    "std::fs",
    "tokio::fs",
    "std::os::unix::fs",
    "std::os::windows::fs",
    "rustix::fs",
    "tempfile",
];

const REVIEWED_NON_WRITER_FUNCTIONS: &[&str] = &[
    "std::fs::canonicalize",
    "std::fs::exists",
    "std::fs::metadata",
    "std::fs::read",
    "std::fs::read_dir",
    "std::fs::read_link",
    "std::fs::read_to_string",
    "std::fs::symlink_metadata",
    "std::fs::try_exists",
    "tokio::fs::canonicalize",
    "tokio::fs::metadata",
    "tokio::fs::read",
    "tokio::fs::read_dir",
    "tokio::fs::read_link",
    "tokio::fs::read_to_string",
    "tokio::fs::symlink_metadata",
    "tokio::fs::try_exists",
    "std::os::unix::fs::PermissionsExt::from_mode",
];

const REVIEWED_AUTHORITY_FUNCTIONS: &[(&str, FlowClass)] = &[
    ("std::mem::drop", FlowClass::None),
    ("std::boxed::Box::new", FlowClass::FirstArgument),
    ("std::sync::Arc::new", FlowClass::FirstArgument),
    ("std::rc::Rc::new", FlowClass::FirstArgument),
    ("std::io::BufWriter::new", FlowClass::FirstArgument),
    ("std::result::Result::Ok", FlowClass::FirstArgument),
    ("std::option::Option::Some", FlowClass::FirstArgument),
    ("core::result::Result::Ok", FlowClass::FirstArgument),
    ("core::option::Option::Some", FlowClass::FirstArgument),
];

const REVIEWED_AUTHORITY_METHODS: &[(&str, FlowClass, FlowClass)] = &[
    (
        "read",
        FlowClass::StandardOpenBuilder,
        FlowClass::SameReceiver,
    ),
    ("read", FlowClass::TokioOpenBuilder, FlowClass::SameReceiver),
    ("metadata", FlowClass::WritableHandle, FlowClass::None),
    ("metadata", FlowClass::TemporaryHandle, FlowClass::None),
    ("path", FlowClass::TemporaryHandle, FlowClass::None),
    (
        "try_clone",
        FlowClass::WritableHandle,
        FlowClass::WritableHandle,
    ),
    (
        "try_clone",
        FlowClass::TemporaryHandle,
        FlowClass::WritableHandle,
    ),
    (
        "as_file",
        FlowClass::TemporaryHandle,
        FlowClass::WritableHandle,
    ),
    (
        "as_file_mut",
        FlowClass::TemporaryHandle,
        FlowClass::WritableHandle,
    ),
];

pub(super) fn add(specs: &mut Vec<SinkSpec>) {
    add_functions(specs, SinkOrigin::Standard);
    add_functions(specs, SinkOrigin::Tokio);
    add_handle_methods(specs);
    add_builder_methods(specs, SinkOrigin::Standard);
    add_builder_methods(specs, SinkOrigin::Tokio);
}

pub(super) fn is_reviewed_non_writer_function(path: &str) -> bool {
    REVIEWED_NON_WRITER_FUNCTIONS.contains(&path)
}

pub(super) fn reviewed_authority_function(path: &str) -> Option<FlowClass> {
    REVIEWED_AUTHORITY_FUNCTIONS
        .iter()
        .find(|(registered, _)| *registered == path)
        .map(|(_, returns)| *returns)
}

pub(super) fn reviewed_authority_method(name: &str, flow: FlowClass) -> Option<FlowClass> {
    REVIEWED_AUTHORITY_METHODS
        .iter()
        .find(|(registered, receiver, _)| *registered == name && *receiver == flow)
        .map(|(_, _, returns)| *returns)
}

pub(super) const fn known_writer_namespaces() -> &'static [&'static str] {
    KNOWN_WRITER_NAMESPACES
}

pub(super) const fn reviewed_non_writer_functions() -> &'static [&'static str] {
    REVIEWED_NON_WRITER_FUNCTIONS
}

pub(super) const fn reviewed_authority_functions() -> &'static [(&'static str, FlowClass)] {
    REVIEWED_AUTHORITY_FUNCTIONS
}

pub(super) const fn reviewed_authority_methods() -> &'static [(&'static str, FlowClass, FlowClass)]
{
    REVIEWED_AUTHORITY_METHODS
}

fn add_functions(specs: &mut Vec<SinkSpec>, origin: SinkOrigin) {
    let (prefix, file, options, id) = match origin {
        SinkOrigin::Standard => ("std::fs", "std::fs::File", "std::fs::OpenOptions", "std"),
        SinkOrigin::Tokio => (
            "tokio::fs",
            "tokio::fs::File",
            "tokio::fs::OpenOptions",
            "tokio",
        ),
        _ => return,
    };
    let definitions = [
        (
            format!("{id}.fs.write"),
            format!("{prefix}::write"),
            K::Write,
            R::HandleMutation,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.copy"),
            format!("{prefix}::copy"),
            K::Write,
            R::Publication,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.create_dir"),
            format!("{prefix}::create_dir"),
            K::Create,
            R::RootOpen,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.create_dir_all"),
            format!("{prefix}::create_dir_all"),
            K::Create,
            R::RootOpen,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.rename"),
            format!("{prefix}::rename"),
            K::Rename,
            R::Publication,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.hard_link"),
            format!("{prefix}::hard_link"),
            K::Link,
            R::Publication,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.remove_file"),
            format!("{prefix}::remove_file"),
            K::Remove,
            R::Cleanup,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.remove_dir"),
            format!("{prefix}::remove_dir"),
            K::Remove,
            R::Cleanup,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.remove_dir_all"),
            format!("{prefix}::remove_dir_all"),
            K::Remove,
            R::Cleanup,
            FlowClass::None,
        ),
        (
            format!("{id}.fs.set_permissions"),
            format!("{prefix}::set_permissions"),
            K::Permissions,
            R::Permissions,
            FlowClass::None,
        ),
        (
            format!("{id}.file.open"),
            format!("{file}::open"),
            K::Open,
            R::RootOpen,
            FlowClass::None,
        ),
        (
            format!("{id}.file.create"),
            format!("{file}::create"),
            K::Create,
            R::RootOpen,
            FlowClass::WritableHandle,
        ),
        (
            format!("{id}.file.create_new"),
            format!("{file}::create_new"),
            K::Create,
            R::RootOpen,
            FlowClass::WritableHandle,
        ),
        (
            format!("{id}.file.options"),
            format!("{file}::options"),
            K::Open,
            R::RootOpen,
            if origin == SinkOrigin::Standard {
                FlowClass::StandardOpenBuilder
            } else {
                FlowClass::TokioOpenBuilder
            },
        ),
    ];
    specs.extend(definitions.into_iter().map(|definition| {
        SinkSpec::builtin(
            definition.0,
            super::super::SinkSelector::Function { path: definition.1 },
            definition.2,
            definition.3,
            definition.4,
            origin,
        )
    }));
    let builder_flow = match origin {
        SinkOrigin::Standard => FlowClass::StandardOpenBuilder,
        SinkOrigin::Tokio => FlowClass::TokioOpenBuilder,
        _ => FlowClass::None,
    };
    specs.push(SinkSpec::builtin(
        if origin == SinkOrigin::Standard {
            "std.open_options.new"
        } else {
            "tokio.open_options.new"
        },
        super::super::SinkSelector::Function {
            path: format!("{options}::new"),
        },
        K::Open,
        R::RootOpen,
        builder_flow,
        origin,
    ));
    if origin == SinkOrigin::Tokio {
        specs.push(function(
            (
                "tokio.file.from_std",
                "tokio::fs::File::from_std",
                K::Open,
                R::RootOpen,
                FlowClass::WritableHandle,
            ),
            origin,
        ));
        specs.push(function(
            (
                "tokio.fs.symlink",
                "tokio::fs::symlink",
                K::Link,
                R::Publication,
                FlowClass::None,
            ),
            origin,
        ));
    } else {
        for (id, path) in [
            ("std.fs.symlink.unix", "std::os::unix::fs::symlink"),
            (
                "std.fs.symlink.windows_file",
                "std::os::windows::fs::symlink_file",
            ),
            (
                "std.fs.symlink.windows_dir",
                "std::os::windows::fs::symlink_dir",
            ),
        ] {
            specs.push(function(
                (id, path, K::Link, R::Publication, FlowClass::None),
                origin,
            ));
        }
    }
}

fn add_handle_methods(specs: &mut Vec<SinkSpec>) {
    let methods = [
        ("io.handle.write", "write", K::Write, R::HandleMutation),
        (
            "io.handle.write_all",
            "write_all",
            K::Write,
            R::HandleMutation,
        ),
        (
            "io.handle.write_vectored",
            "write_vectored",
            K::Write,
            R::HandleMutation,
        ),
        (
            "io.handle.write_fmt",
            "write_fmt",
            K::Write,
            R::HandleMutation,
        ),
        (
            "io.handle.write_at",
            "write_at",
            K::Write,
            R::HandleMutation,
        ),
        (
            "io.handle.write_all_at",
            "write_all_at",
            K::Write,
            R::HandleMutation,
        ),
        (
            "io.handle.seek_write",
            "seek_write",
            K::Write,
            R::HandleMutation,
        ),
        (
            "io.handle.set_len",
            "set_len",
            K::SetLength,
            R::HandleMutation,
        ),
        (
            "io.handle.set_permissions",
            "set_permissions",
            K::Permissions,
            R::Permissions,
        ),
        (
            "io.handle.set_times",
            "set_times",
            K::Permissions,
            R::Permissions,
        ),
        (
            "io.handle.set_modified",
            "set_modified",
            K::Permissions,
            R::Permissions,
        ),
        ("io.handle.flush", "flush", K::Flush, R::Durability),
        ("io.handle.sync_all", "sync_all", K::Sync, R::Durability),
        ("io.handle.sync_data", "sync_data", K::Sync, R::Durability),
    ];
    specs.extend(methods.map(|definition| {
        method(
            definition,
            ReceiverConstraint::WritableHandle,
            FlowClass::None,
            SinkOrigin::Standard,
        )
    }));
}

fn add_builder_methods(specs: &mut Vec<SinkSpec>, origin: SinkOrigin) {
    let (id, receiver) = if origin == SinkOrigin::Standard {
        ("std", ReceiverConstraint::StandardOpenBuilder)
    } else {
        ("tokio", ReceiverConstraint::TokioOpenBuilder)
    };
    for (name, kind) in [
        ("create", K::Create),
        ("create_new", K::Create),
        ("truncate", K::Truncate),
        ("append", K::Append),
        ("write", K::Write),
        ("open", K::Open),
    ] {
        let sink_id = format!("{id}.open_options.{name}");
        let returns = if name == "open" {
            FlowClass::WritableHandle
        } else {
            FlowClass::SameReceiver
        };
        specs.push(SinkSpec::builtin(
            sink_id,
            super::super::SinkSelector::Method {
                name: name.to_owned(),
                receiver,
            },
            kind,
            R::RootOpen,
            returns,
            origin,
        ));
    }
    for (name, kind, role) in [
        ("mode", K::Permissions, R::Permissions),
        ("custom_flags", K::Open, R::RootOpen),
    ] {
        specs.push(SinkSpec::builtin(
            format!("{id}.open_options.{name}"),
            super::super::SinkSelector::Method {
                name: name.to_owned(),
                receiver,
            },
            kind,
            role,
            FlowClass::SameReceiver,
            origin,
        ));
    }
}
