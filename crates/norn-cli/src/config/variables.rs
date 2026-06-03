//! Session-variable wiring for the Norn CLI (NC-004 R5).
//!
//! Turns `--variables KEY=VALUE` flag values into a
//! [`VariableStore`](norn::integration::variables::VariableStore) populated
//! with [`SessionVariable`](norn::integration::variables::SessionVariable)s
//! whose source is a [`Static`](norn::integration::variables::VariableSource::Static)
//! literal value. The brief's acceptance requires that without any
//! `--variables`, the store is not constructed and `LoopContext::variables`
//! remains `None`; with one or more pairs, the store is wrapped in an
//! `Arc` and threaded onto the loop context.

use std::sync::Arc;

use norn::integration::variables::{SessionVariable, VariableSource, VariableStore};

use crate::cli::BuildError;
use crate::config::parse_kv;

/// Build a [`VariableStore`] from a list of `KEY=VALUE` flag values.
///
/// Returns [`None`] when `pairs` is empty so the caller can leave
/// `LoopContext::variables` as `None` (matching the brief's NC-004 R5
/// acceptance: "Without --variables, `LoopContext::variables` is None").
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when any pair fails [`parse_kv`].
pub fn build_variable_store(
    pairs: &[String],
    working_dir: norn::tool::context::SharedWorkingDir,
) -> Result<Option<Arc<VariableStore>>, BuildError> {
    if pairs.is_empty() {
        return Ok(None);
    }
    let store = VariableStore::new().with_working_dir(working_dir);
    for pair in pairs {
        let (name, value) = parse_kv(pair)?;
        store.set(SessionVariable {
            name,
            source: VariableSource::Static { value },
        });
    }
    Ok(Some(Arc::new(store)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_pairs_returns_none() {
        let store =
            build_variable_store(&[], norn::tool::context::SharedWorkingDir::default()).unwrap();
        assert!(store.is_none());
    }

    #[tokio::test]
    async fn single_pair_populates_store() {
        let store = build_variable_store(
            &["project=yggdrasil".to_owned()],
            norn::tool::context::SharedWorkingDir::default(),
        )
        .unwrap()
        .expect("store constructed");
        assert_eq!(store.resolve("project").await.unwrap(), "yggdrasil");
    }

    #[tokio::test]
    async fn multiple_pairs_populate_store_with_each_entry() {
        let store = build_variable_store(
            &["project=yggdrasil".to_owned(), "env=staging".to_owned()],
            norn::tool::context::SharedWorkingDir::default(),
        )
        .unwrap()
        .expect("store constructed");
        assert_eq!(store.resolve("project").await.unwrap(), "yggdrasil");
        assert_eq!(store.resolve("env").await.unwrap(), "staging");
        assert_eq!(store.len(), 2);
    }

    #[tokio::test]
    async fn malformed_pair_propagates_argument_error() {
        match build_variable_store(
            &["bad-no-equals".to_owned()],
            norn::tool::context::SharedWorkingDir::default(),
        ) {
            Err(BuildError::Argument(_)) => {}
            Err(other) => panic!("expected Argument error, got {other:?}"),
            Ok(_) => panic!("expected error for malformed pair"),
        }
    }

    #[tokio::test]
    async fn value_can_contain_equals_signs() {
        let store = build_variable_store(
            &["query=a=b=c".to_owned()],
            norn::tool::context::SharedWorkingDir::default(),
        )
        .unwrap()
        .expect("store constructed");
        assert_eq!(store.resolve("query").await.unwrap(), "a=b=c");
    }
}
