use std::collections::BTreeSet;

use super::{RegistryError, validate_identifier, validate_rust_path};
use crate::writers::model::FlowClass;

pub(super) fn validate_auxiliary_authorities(
    namespaces: &[&str],
    non_writers: &[&str],
    functions: &[(&str, FlowClass)],
    methods: &[(&str, FlowClass, FlowClass)],
) -> Result<(), RegistryError> {
    let mut namespace_keys = BTreeSet::new();
    for namespace in namespaces {
        if validate_rust_path(namespace).is_err() {
            return Err(RegistryError::AuxiliaryAuthority);
        }
        if !namespace_keys.insert(*namespace) {
            return Err(RegistryError::AuxiliaryAuthority);
        }
    }

    let mut function_keys = BTreeSet::new();
    for path in non_writers {
        if validate_rust_path(path).is_err() {
            return Err(RegistryError::AuxiliaryAuthority);
        }
        if !function_keys.insert(*path) {
            return Err(RegistryError::AuxiliaryAuthority);
        }
    }
    for (path, returns) in functions {
        if validate_rust_path(path).is_err() {
            return Err(RegistryError::AuxiliaryAuthority);
        }
        if *returns == FlowClass::SameReceiver || !function_keys.insert(*path) {
            return Err(RegistryError::AuxiliaryAuthority);
        }
    }

    let mut method_keys = BTreeSet::new();
    for (name, receiver, returns) in methods {
        if validate_identifier(name).is_err() {
            return Err(RegistryError::AuxiliaryAuthority);
        }
        if matches!(
            receiver,
            FlowClass::None | FlowClass::SameReceiver | FlowClass::FirstArgument
        ) || *returns == FlowClass::FirstArgument
            || !method_keys.insert((*name, *receiver))
        {
            return Err(RegistryError::AuxiliaryAuthority);
        }
    }
    Ok(())
}
