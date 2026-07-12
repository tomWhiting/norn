//! Fetch argument parsing, bounded body reads, and content conversion.

use std::fmt::Write as _;

use futures_util::StreamExt;

use crate::error::ToolError;
use crate::internal::extraction::DetailLevel;
use crate::tool::failure::ToolErrorKind;

use super::MAX_SIZE;

/// Output format selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format {
    Markdown,
    Text,
    Html,
}

pub(super) fn parse_format(raw: Option<&str>) -> Result<Format, ToolError> {
    match raw {
        None | Some("markdown") => Ok(Format::Markdown),
        Some("text") => Ok(Format::Text),
        Some("html") => Ok(Format::Html),
        Some(other) => Err(ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!("unknown format `{other}`; expected markdown|text|html"),
        )),
    }
}

pub(super) fn parse_detail(raw: Option<&str>) -> Result<DetailLevel, ToolError> {
    match raw {
        None | Some("normal") => Ok(DetailLevel::Normal),
        Some("brief") => Ok(DetailLevel::Brief),
        Some("detailed") => Ok(DetailLevel::Detailed),
        Some(other) => Err(ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!("unknown detail `{other}`; expected brief|normal|detailed"),
        )),
    }
}

pub(super) async fn read_body_with_cap(
    response: reqwest::Response,
) -> Result<(Vec<u8>, bool), ToolError> {
    let mut body_bytes = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ToolError::ExecutionFailed {
            reason: format!("response body read failed: {e}"),
        })?;
        let remaining = MAX_SIZE.saturating_sub(body_bytes.len());
        if chunk.len() > remaining {
            body_bytes.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        body_bytes.extend_from_slice(&chunk);
    }
    Ok((body_bytes, truncated))
}

pub(super) fn convert(body: &str, format: Format, content_type: &str) -> String {
    match format {
        Format::Html => body.to_owned(),
        Format::Text => html_to_text(body),
        Format::Markdown => {
            if content_type.contains("text/html") {
                html_to_markdown(body)
            } else {
                body.to_owned()
            }
        }
    }
}

fn html_conversion_options(
    output_format: html_to_markdown_rs::OutputFormat,
) -> html_to_markdown_rs::ConversionOptions {
    html_to_markdown_rs::ConversionOptions::builder()
        .preprocessing(html_to_markdown_rs::PreprocessingOptions {
            enabled: true,
            preset: html_to_markdown_rs::PreprocessingPreset::Aggressive,
            remove_navigation: true,
            remove_forms: true,
        })
        .compact_tables(true)
        .output_format(output_format)
        .build()
}

pub(super) fn html_to_text(html: &str) -> String {
    convert_html(html, html_to_markdown_rs::OutputFormat::Plain, "text")
}

pub(super) fn html_to_markdown(html: &str) -> String {
    convert_html(
        html,
        html_to_markdown_rs::OutputFormat::Markdown,
        "markdown",
    )
}

fn convert_html(html: &str, format: html_to_markdown_rs::OutputFormat, label: &str) -> String {
    match html_to_markdown_rs::convert(html, Some(html_conversion_options(format))) {
        Ok(result) => {
            let Some(content) = result.content else {
                tracing::warn!(
                    target_format = label,
                    "HTML conversion produced no content; returning raw HTML",
                );
                return html.to_owned();
            };
            content
        }
        Err(error) => {
            tracing::warn!(
                target_format = label,
                error = ?error,
                "HTML conversion failed; returning raw HTML",
            );
            html.to_owned()
        }
    }
}

pub(super) fn prepend_line_numbers(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + content.lines().count() * 6);
    for (i, line) in content.lines().enumerate() {
        let _ = writeln!(out, "{}\t{}", i + 1, line);
    }
    out
}
