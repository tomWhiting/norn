use std::error::Error;

use norn_policy::path::RepositoryPath;
use norn_policy::writers::{
    SinkRegistry, WriterInventory, WriterOperationId, WriterSource, WriterToken, analyze_writers,
    builtin_sink_registry,
};

pub(super) type TestResult = Result<(), Box<dyn Error>>;

pub(super) fn source(path: &str, text: &str) -> Result<WriterSource, Box<dyn Error>> {
    Ok(WriterSource::new(
        RepositoryPath::parse(path)?,
        text.as_bytes(),
    ))
}

pub(super) fn analyze(path: &str, text: &str) -> Result<WriterInventory, Box<dyn Error>> {
    let source = source(path, text)?;
    let registry = builtin_sink_registry()?;
    Ok(analyze_writers(&[source], &registry)?)
}

pub(super) fn analyze_with(
    path: &str,
    text: &str,
    registry: &SinkRegistry,
) -> Result<WriterInventory, Box<dyn Error>> {
    let source = source(path, text)?;
    Ok(analyze_writers(&[source], registry)?)
}

pub(super) fn token(value: &str) -> Result<WriterToken, Box<dyn Error>> {
    Ok(WriterToken::parse(value)?)
}

pub(super) fn operation_id(
    inventory: &WriterInventory,
    sink: &str,
) -> Result<WriterOperationId, Box<dyn Error>> {
    inventory
        .operations()
        .iter()
        .find(|operation| operation.sink().as_str() == sink)
        .map(norn_policy::writers::WriterOperation::id)
        .ok_or_else(|| std::io::Error::other("test sink was not inventoried").into())
}
