use std::fs;
use std::path::Path;

use tempfile::tempdir;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

fn write_skill(dir: &Path, name: &str, description: &str) -> std::io::Result<()> {
    write_file(
        &dir.join(name).join("SKILL.md"),
        &format!("---\ndescription: {description}\n---\nbody\n"),
    )
}

mod basics;
mod diagnostics;
mod prompt_listing;
mod slash_commands;
