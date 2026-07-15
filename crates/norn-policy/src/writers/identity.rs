//! Canonical writer-operation identity shared by scanning and origin decoding.

use crate::digest::{Digest, digest_bytes};
use crate::path::RepositoryPath;

use super::model::{
    OperationKind, SinkDiscovery, WRITER_ANALYZER_VERSION, WriterOperationId, WriterRole,
    WriterToken,
};

pub(crate) struct OperationIdentityInput<'a> {
    pub(crate) path: &'a RepositoryPath,
    pub(crate) enclosing_item: Digest,
    pub(crate) normalized_call: Digest,
    pub(crate) sink: &'a WriterToken,
    pub(crate) kind: OperationKind,
    pub(crate) role: WriterRole,
    pub(crate) discovery: SinkDiscovery,
}

pub(crate) fn operation_key(input: &OperationIdentityInput<'_>) -> Vec<u8> {
    let mut key = Vec::new();
    field(&mut key, WRITER_ANALYZER_VERSION.as_bytes());
    field(&mut key, input.path.as_str().as_bytes());
    field(&mut key, input.enclosing_item.as_bytes());
    field(&mut key, input.normalized_call.as_bytes());
    field(&mut key, input.sink.as_str().as_bytes());
    field(&mut key, input.kind.token().as_bytes());
    field(&mut key, input.role.token().as_bytes());
    field(&mut key, input.discovery.token().as_bytes());
    key
}

pub(crate) fn operation_id(input: &OperationIdentityInput<'_>, ordinal: u32) -> WriterOperationId {
    let mut key = operation_key(input);
    field(&mut key, &ordinal.to_be_bytes());
    WriterOperationId::new(digest_bytes(&key))
}

fn field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(digest_bytes(value).as_bytes());
}
