use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};

use crate::{Digest, RepositoryPath};

const AUTHORITY_IDENTITY_DOMAIN: &[u8] = b"norn-policy-responses-contract-authority-1";

pub(super) fn authority_identity(files: &BTreeMap<RepositoryPath, &[u8]>) -> Digest {
    let mut hasher = Sha256::new();
    append_field(&mut hasher, AUTHORITY_IDENTITY_DOMAIN);
    append_length(&mut hasher, files.len());
    for (path, bytes) in files {
        append_field(&mut hasher, path.as_str().as_bytes());
        append_field(&mut hasher, bytes);
    }
    Digest::from_bytes(hasher.finalize().into())
}

fn append_field(hasher: &mut Sha256, value: &[u8]) {
    append_length(hasher, value.len());
    hasher.update(value);
}

fn append_length(hasher: &mut Sha256, value: usize) {
    let native = value.to_be_bytes();
    hasher.update(&[0_u8; 16][native.len()..]);
    hasher.update(native);
}
