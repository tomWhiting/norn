#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::io::Write;

    use crate::runtime::wiring::build_tool_context_with_diagnostics;
    use norn::tool::context::SharedWorkingDir;
    use norn::tools::diagnostics::DiagnosticInfra;

    fn write_file(path: &std::path::Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        let mut file = std::fs::File::create(path).expect("create file");
        file.write_all(contents.as_bytes()).expect("write file");
    }

    fn python_adapter_toml(name: &str, binary: &str) -> String {
        format!(
            r#"
[adapter]
name = "{name}"
language = "python"
file_patterns = ["*.py", "**/*.py"]
output_format = "json-lines"

[adapter.command]
binary = "{binary}"
args = ["{{file}}"]

[adapter.mapping]
severity = "severity"
message = "message"
file = "file"
line = "line"
column = "column"
code = "code"
"#,
        )
    }

    #[test]
    fn build_tool_context_ignores_project_local_adapter_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            &dir.path().join("CONVENTIONS.toml"),
            r#"
[python.diagnostics]
mypy = { target = "file", handling = "advise" }

[python-general]
tools = ["write"]
paths = ["**/*.py"]
mypy = { on = "tool", handling = "advise" }
"#,
        );
        write_file(
            &dir.path().join("adapters/mypy.toml"),
            &python_adapter_toml("mypy", "mypy"),
        );

        let ctx = build_tool_context_with_diagnostics(
            dir.path(),
            SharedWorkingDir::new(dir.path().to_path_buf()),
            None,
            None,
        );
        let infra = ctx
            .get_extension::<DiagnosticInfra>()
            .expect("diagnostic infra extension");

        assert!(infra.adapters.adapter_by_name("mypy").is_none());
    }

    #[test]
    fn build_tool_context_ignores_missing_project_local_adapter_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            &dir.path().join("CONVENTIONS.toml"),
            r#"
[python.diagnostics]
mypy = { target = "file", handling = "advise" }
missing-adapter = { target = "file", handling = "advise" }

[python-general]
tools = ["write"]
paths = ["**/*.py"]
mypy = { on = "tool", handling = "advise" }
missing-adapter = { on = "tool", handling = "advise" }
"#,
        );
        write_file(
            &dir.path().join("adapters/mypy.toml"),
            &python_adapter_toml("mypy", "mypy"),
        );

        let ctx = build_tool_context_with_diagnostics(
            dir.path(),
            SharedWorkingDir::new(dir.path().to_path_buf()),
            None,
            None,
        );
        let infra = ctx
            .get_extension::<DiagnosticInfra>()
            .expect("diagnostic infra extension");

        assert!(infra.adapters.adapter_by_name("mypy").is_none());
        assert!(infra.adapters.adapter_by_name("missing-adapter").is_none());
    }
}
