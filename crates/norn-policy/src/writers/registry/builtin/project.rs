//! Definition-backed Norn private-root and wrapper sinks.

mod digests;

use super::super::{DefinitionSpec, ReceiverConstraint, RegistryError, SinkSelector, SinkSpec};
use crate::digest::Digest;
use crate::writers::model::{FlowClass, OperationKind as K, SinkOrigin, WriterRole as R};

const PRIVATE_ROOT_SOURCE: &str = "crates/norn/src/util/private_fs.rs";
const SESSION_IO_SOURCE: &str = "crates/norn/src/session/persistence/io.rs";
const SESSION_INDEX_SOURCE: &str = "crates/norn/src/session/persistence/index.rs";
const TASK_STORAGE_SOURCE: &str = "crates/norn/src/tools/task/disk/storage.rs";

pub(super) fn add(specs: &mut Vec<SinkSpec>) -> Result<(), RegistryError> {
    add_private_root(specs)?;
    add_wrappers(specs)?;
    add_macros(specs);
    Ok(())
}

fn add_private_root(specs: &mut Vec<SinkSpec>) -> Result<(), RegistryError> {
    for (id, name, kind, role, returns, signature, implementation) in [
        (
            "project.private_root.create",
            "create",
            K::Open,
            R::SharedPrimitive,
            FlowClass::RootAuthority,
            "pub(crate) fn create(path: &Path) -> io::Result<Self>",
            digests::CREATE,
        ),
        (
            "project.private_root.open",
            "open",
            K::Open,
            R::SharedPrimitive,
            FlowClass::RootAuthority,
            "pub(crate) fn open(path: &Path) -> io::Result<Self>",
            digests::OPEN,
        ),
    ] {
        let item = format!("PrivateRoot::{name}");
        specs.push(SinkSpec::project_function(
            id,
            &format!("crate::util::{item}"),
            DefinitionSpec::reviewed_function(
                PRIVATE_ROOT_SOURCE,
                &item,
                signature,
                implementation,
            )?,
            kind,
            role,
            returns,
        )?);
    }
    for (id, name, kind, role, returns, signature, implementation) in private_root_methods() {
        let item = format!("PrivateRoot::{name}");
        specs.push(SinkSpec::project_method(
            id,
            name,
            ReceiverConstraint::RootAuthority,
            DefinitionSpec::reviewed_function(
                PRIVATE_ROOT_SOURCE,
                &item,
                signature,
                implementation,
            )?,
            kind,
            role,
            returns,
        )?);
    }
    Ok(())
}

type PrivateRootMethod = (
    &'static str,
    &'static str,
    K,
    R,
    FlowClass,
    &'static str,
    Digest,
);

fn private_root_methods() -> [PrivateRootMethod; 11] {
    [
        (
            "project.private_root.create_dir_all",
            "create_dir_all",
            K::Create,
            R::SharedPrimitive,
            FlowClass::None,
            "pub(crate) fn create_dir_all(&self, relative: &Path) -> io::Result<()>",
            digests::CREATE_DIR_ALL,
        ),
        (
            "project.private_root.open_read",
            "open_read",
            K::Open,
            R::SharedPrimitive,
            FlowClass::None,
            "pub(crate) fn open_read(&self, relative: &Path) -> io::Result<File>",
            digests::OPEN_READ,
        ),
        (
            "project.private_root.open_read_append",
            "open_read_append",
            K::Append,
            R::SharedPrimitive,
            FlowClass::WritableHandle,
            "pub(crate) fn open_read_append(&self, relative: &Path) -> io::Result<File>",
            digests::OPEN_READ_APPEND,
        ),
        (
            "project.private_root.open_append_create",
            "open_append_create",
            K::Append,
            R::SharedPrimitive,
            FlowClass::WritableHandle,
            "pub(crate) fn open_append_create(&self, relative: &Path) -> io::Result<File>",
            digests::OPEN_APPEND_CREATE,
        ),
        (
            "project.private_root.open_lock",
            "open_lock",
            K::Create,
            R::SharedPrimitive,
            FlowClass::WritableHandle,
            "pub(crate) fn open_lock(&self, relative: &Path) -> io::Result<File>",
            digests::OPEN_LOCK,
        ),
        (
            "project.private_root.create_new",
            "create_new",
            K::Create,
            R::SharedPrimitive,
            FlowClass::WritableHandle,
            "pub(crate) fn create_new(&self, relative: &Path) -> io::Result<File>",
            digests::CREATE_NEW,
        ),
        (
            "project.private_root.remove_file",
            "remove_file",
            K::Remove,
            R::Cleanup,
            FlowClass::None,
            "pub(crate) fn remove_file(&self, relative: &Path) -> io::Result<()>",
            digests::REMOVE_FILE,
        ),
        (
            "project.private_root.remove_dir_all",
            "remove_dir_all",
            K::Remove,
            R::Cleanup,
            FlowClass::None,
            "pub(crate) fn remove_dir_all(&self, relative: &Path) -> io::Result<()>",
            digests::REMOVE_DIR_ALL,
        ),
        (
            "project.private_root.rename",
            "rename",
            K::Rename,
            R::SharedPrimitive,
            FlowClass::None,
            "pub(crate) fn rename(&self, from: &Path, to: &Path) -> io::Result<()>",
            digests::RENAME,
        ),
        (
            "project.private_root.publish_new",
            "publish_new",
            K::Link,
            R::SharedPrimitive,
            FlowClass::None,
            "pub(crate) fn publish_new(&self, from: &Path, to: &Path) -> io::Result<()>",
            digests::PUBLISH_NEW,
        ),
        (
            "project.private_root.sync_dir",
            "sync_dir",
            K::Sync,
            R::SharedPrimitive,
            FlowClass::None,
            "pub(crate) fn sync_dir(&self, relative: &Path) -> io::Result<()>",
            digests::SYNC_DIR,
        ),
    ]
}

fn add_wrappers(specs: &mut Vec<SinkSpec>) -> Result<(), RegistryError> {
    add_session_io_wrappers(specs)?;
    for definition in [
        (
            "project.write_index_atomic",
            "crate::session::persistence::write_index_atomic",
            SESSION_INDEX_SOURCE,
            "write_index_atomic",
            "pub fn write_index_atomic(data_dir: &Path, entries: &[SessionIndexEntry],) -> Result<(), SessionPersistError>",
            digests::WRITE_INDEX_ATOMIC,
            K::Write,
            R::Publication,
            FlowClass::None,
        ),
        (
            "project.append_index_entry",
            "crate::session::persistence::append_index_entry",
            SESSION_INDEX_SOURCE,
            "append_index_entry",
            "pub fn append_index_entry(data_dir: &Path, entry: &SessionIndexEntry, lock_deadline: Option<Duration>,) -> Result<(), SessionPersistError>",
            digests::APPEND_INDEX_ENTRY,
            K::Append,
            R::HandleMutation,
            FlowClass::None,
        ),
        (
            "project.write_json_atomic",
            "write_json_atomic",
            TASK_STORAGE_SOURCE,
            "write_json_atomic",
            "fn write_json_atomic(root: &PrivateRoot, tmp_path: &Path, final_path: &Path, entry: &TaskEntry, replace: bool,) -> Result<(), ToolError>",
            digests::WRITE_JSON_ATOMIC,
            K::Write,
            R::Publication,
            FlowClass::None,
        ),
    ] {
        specs.push(wrapper(definition)?);
    }
    Ok(())
}

type WrapperDefinition = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Digest,
    K,
    R,
    FlowClass,
);

fn wrapper(definition: WrapperDefinition) -> Result<SinkSpec, RegistryError> {
    SinkSpec::project_function(
        definition.0,
        definition.1,
        DefinitionSpec::reviewed_function(definition.2, definition.3, definition.4, definition.5)?,
        definition.6,
        definition.7,
        definition.8,
    )
}

fn add_session_io_wrappers(specs: &mut Vec<SinkSpec>) -> Result<(), RegistryError> {
    for definition in [
        (
            "project.open_session_append",
            "crate::session::persistence::io::open_session_append",
            SESSION_IO_SOURCE,
            "open_session_append",
            "pub(crate) fn open_session_append(path: &Path) -> Result<AdmittedSessionFile, SessionPersistError>",
            digests::OPEN_SESSION_APPEND,
            K::Append,
            R::RootOpen,
            FlowClass::WritableHandle,
        ),
        (
            "project.open_session_append_for_entry",
            "crate::session::persistence::io::open_session_append_for_entry",
            SESSION_IO_SOURCE,
            "open_session_append_for_entry",
            "pub(crate) fn open_session_append_for_entry(data_dir: &Path, entry: &SessionIndexEntry,) -> Result<AdmittedSessionFile, SessionPersistError>",
            digests::OPEN_SESSION_APPEND_FOR_ENTRY,
            K::Append,
            R::RootOpen,
            FlowClass::WritableHandle,
        ),
        (
            "project.open_session_append_bound",
            "crate::session::persistence::io::open_session_append_bound",
            SESSION_IO_SOURCE,
            "open_session_append_bound",
            "pub(crate) fn open_session_append_bound(path: &Path, identity: PrivateFileIdentity,) -> Result<AdmittedSessionFile, SessionPersistError>",
            digests::OPEN_SESSION_APPEND_BOUND,
            K::Append,
            R::RootOpen,
            FlowClass::WritableHandle,
        ),
        (
            "project.open_session_append_for_entry_bound",
            "crate::session::persistence::io::open_session_append_for_entry_bound",
            SESSION_IO_SOURCE,
            "open_session_append_for_entry_bound",
            "pub(crate) fn open_session_append_for_entry_bound(data_dir: &Path, entry: &SessionIndexEntry, identity: PrivateFileIdentity,) -> Result<AdmittedSessionFile, SessionPersistError>",
            digests::OPEN_SESSION_APPEND_FOR_ENTRY_BOUND,
            K::Append,
            R::RootOpen,
            FlowClass::WritableHandle,
        ),
        (
            "project.append_events",
            "crate::session::persistence::append_events",
            SESSION_IO_SOURCE,
            "append_events",
            "pub fn append_events(data_dir: &Path, session_id: &str, events: &[SessionEvent], disabled: bool,) -> Result<(), SessionPersistError>",
            digests::APPEND_EVENTS,
            K::Append,
            R::HandleMutation,
            FlowClass::None,
        ),
    ] {
        specs.push(wrapper(definition)?);
    }
    Ok(())
}

fn add_macros(specs: &mut Vec<SinkSpec>) {
    for (id, path) in [
        ("std.macro.write", "write"),
        ("std.macro.writeln", "writeln"),
        ("std.macro.write.qualified", "std::write"),
        ("std.macro.writeln.qualified", "std::writeln"),
    ] {
        specs.push(SinkSpec::builtin(
            id,
            SinkSelector::Macro {
                path: path.to_owned(),
            },
            K::Write,
            R::HandleMutation,
            FlowClass::None,
            SinkOrigin::Standard,
        ));
    }
}
