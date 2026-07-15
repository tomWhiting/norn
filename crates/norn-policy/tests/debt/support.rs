use std::error::Error;

use norn_policy::RepositoryPath;
use norn_policy::debt::{DebtOccurrence, DebtTargetContext, DebtTargetKind, scan_rust_debt};

pub type TestResult = Result<(), Box<dyn Error>>;

pub fn scan(source: &str) -> Result<Vec<DebtOccurrence>, Box<dyn Error>> {
    scan_at("src/lib.rs", source)
}

pub fn scan_at(path: &str, source: &str) -> Result<Vec<DebtOccurrence>, Box<dyn Error>> {
    let path: RepositoryPath = path.parse()?;
    let target = DebtTargetContext::new(DebtTargetKind::Library, "fixture", "fixture")?;
    Ok(scan_rust_debt(&path, &target, source.as_bytes())?)
}

pub fn attribute(name: &str, body: &str) -> String {
    format!("#[{name}({body})]")
}

pub fn method_call(name: &str) -> String {
    format!("value.{name}()")
}

pub fn macro_call(name: &str) -> String {
    format!("{name}!()")
}

pub fn marker(parts: &[&str]) -> String {
    parts.concat()
}
