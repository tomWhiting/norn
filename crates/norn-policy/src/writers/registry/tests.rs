use super::RegistryError;
use super::auxiliary::validate_auxiliary_authorities;
use crate::writers::model::FlowClass;

#[test]
fn auxiliary_authorities_reject_invalid_or_duplicate_entries() {
    let cases = [
        validate_auxiliary_authorities(&["std::fs", "std::fs"], &[], &[], &[]),
        validate_auxiliary_authorities(&["not a path"], &[], &[], &[]),
        validate_auxiliary_authorities(&[], &["std::fs::read", "std::fs::read"], &[], &[]),
        validate_auxiliary_authorities(
            &[],
            &["std::fs::read"],
            &[("std::fs::read", FlowClass::None)],
            &[],
        ),
        validate_auxiliary_authorities(
            &[],
            &[],
            &[("std::boxed::Box::new", FlowClass::SameReceiver)],
            &[],
        ),
        validate_auxiliary_authorities(
            &[],
            &[],
            &[],
            &[("as_ref", FlowClass::None, FlowClass::None)],
        ),
        validate_auxiliary_authorities(
            &[],
            &[],
            &[],
            &[(
                "as_ref",
                FlowClass::WritableHandle,
                FlowClass::FirstArgument,
            )],
        ),
    ];
    assert!(
        cases
            .iter()
            .all(|result| matches!(result, Err(RegistryError::AuxiliaryAuthority)))
    );
}

#[test]
fn auxiliary_authorities_accept_distinct_typed_entries() {
    assert!(
        validate_auxiliary_authorities(
            &["std::fs"],
            &["std::fs::read"],
            &[("std::boxed::Box::new", FlowClass::FirstArgument)],
            &[
                ("metadata", FlowClass::WritableHandle, FlowClass::None,),
                ("metadata", FlowClass::TemporaryHandle, FlowClass::None,),
            ],
        )
        .is_ok()
    );
}
