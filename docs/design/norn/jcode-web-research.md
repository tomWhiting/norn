# jcode Web Tool Research

## 1. Web Search Tool

Implementation: `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs`

Registration:
- `/Users/tom/Developer/tools/jcode/src/tool/mod.rs:32-33` declares `webfetch` and `websearch`.
- `/Users/tom/Developer/tools/jcode/src/tool/mod.rs:150-160` registers tools named `webfetch` and `websearch`.

Tool shape:
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:7-16` defines `WebSearchTool`, described as DuckDuckGo HTML search with no API key, storing a shared `reqwest::Client`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:20-25` defines request input as `WebSearchInput { query: String, num_results: Option<usize> }`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:27-32` defines internal parsed result type `SearchResult { title, url, snippet }`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:35-60` implements the `Tool` trait: name `websearch`, description `Search the web.`, JSON schema requiring `query` and allowing `num_results`.

How it works:
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:62-70` deserializes input, defaults `num_results` to 8, caps it at 20, and builds `https://html.duckduckgo.com/html/?q=...` using `urlencoding::encode`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:72-80` sends a GET with a browser-ish `User-Agent`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:82-90` rejects non-2xx responses, reads the body as text, and calls `parse_ddg_results`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:92-112` returns either `No results found...` or a markdown-ish numbered list with title, URL, and snippet.

Backend:
- DuckDuckGo HTML endpoint only. I found no Google/Bing/SerpAPI backend in the implementation.

Result parsing:
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:116-152` defines static regex helpers with `OnceLock`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:143-150` parses DuckDuckGo result anchors via `class="result__a"` and snippets via `class="result__snippet"`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:154-195` extracts link/snippet matches, caps to `max_results`, filters non-http and DuckDuckGo URLs, strips tags from snippets, HTML-decodes title/snippet, and returns `Vec<SearchResult>`.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:197-212` unwraps DuckDuckGo redirect URLs by extracting and URL-decoding the `uddg=` parameter.
- `/Users/tom/Developer/tools/jcode/src/tool/websearch.rs:214-225` does simple entity replacement for HTML decoding.

Extraction notes:
- Dependencies are light: `reqwest`, `serde`, `serde_json`, `regex`, `urlencoding`, `async-trait`, `anyhow`.
- Fragile point: DuckDuckGo parsing is regex over HTML class names, not a DOM parser or official API.
- Shared HTTP client comes from `/Users/tom/Developer/tools/jcode/crates/jcode-provider-core/src/lib.rs:370-389`, configured with connect timeout, TCP keepalive, idle pool timeout, and max idle per host.

## 2. Web Fetch Tool

Implementation: `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs`

Tool shape:
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:9-11` sets `MAX_SIZE` to 5 MiB, default timeout to 30s, max timeout to 120s.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:13-22` defines `WebFetchTool` with shared `reqwest::Client`.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:25-32` defines request input as `WebFetchInput { url, format, timeout }`.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:35-65` implements the `Tool` trait: name `webfetch`, schema requiring `url`, with `format` enum `text|markdown|html` and optional `timeout`.

How it fetches URLs:
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:67-76` deserializes input, requires `http://` or `https://`, clamps timeout, and defaults output format to `markdown`.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:78-87` performs `reqwest` GET with `User-Agent: Mozilla/5.0 (compatible; JCode/1.0)` and per-request timeout.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:89-103` rejects non-2xx responses and rejects declared `Content-Length` over 5 MiB.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:105-110` reads the `content-type` header.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:112-132` streams the body via `bytes_stream`, enforces the 5 MiB cap while streaming, decodes bytes with `String::from_utf8_lossy`, and marks truncated output.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:134-159` formats output: `html` returns raw body, `text` calls `html_to_text`, `markdown` calls `html_to_markdown` only when content type contains `text/html`, otherwise returns the raw body.

HTML to text/markdown conversion:
- No `scraper`, `html2md`, readability library, or DOM parser is used.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:163-229` defines regexes for script/style removal, tags, whitespace, links, inline formatting, pre/code, list items, and headings.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:231-266` implements `html_to_text`: removes script/style, maps a few block tags to newlines, strips all remaining tags, replaces a small set of HTML entities, normalizes excessive blank lines.
- `/Users/tom/Developer/tools/jcode/src/tool/webfetch.rs:268-335` implements `html_to_markdown`: removes script/style, maps h1-h6 to markdown headings, anchors to `[text](href)`, strong/em/code/pre/li to markdown forms, maps `<br>`/`</p>`, strips remaining tags, replaces basic entities, and normalizes whitespace.

Extraction notes:
- Dependencies are light: `reqwest` with `stream`, `futures::StreamExt`, `regex`, `serde`, `serde_json`, `async-trait`, `anyhow`.
- Heavy/fragile point: HTML conversion is deliberately simple regex/string replacement. It is easy to extract, but would need replacement for robust article extraction, nested tags, relative URL normalization, malformed HTML, charset handling beyond UTF-8-lossy, or script-rendered pages.

## 3. Browser Automation Tool

Implementation:
- User-facing tool: `/Users/tom/Developer/tools/jcode/src/tool/browser.rs`
- Setup/runtime support: `/Users/tom/Developer/tools/jcode/src/browser.rs`
- Design doc: `/Users/tom/Developer/tools/jcode/docs/BROWSER_PROVIDER_PROTOCOL.md`

Backend:
- Built-in tool currently supports `auto` and `firefox` only.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:10-12` defines `BrowserTool` and a static `FIREFOX_PROVIDER`.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:116-126` implements `FirefoxBridgeProvider` with id `firefox_agent_bridge` and supported browsers `["auto", "firefox"]`.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:335-345` rejects Chrome/Safari/Edge even though they appear in the schema, with message that only auto/firefox are wired.
- It does not embed Playwright, headless Chrome, WebDriver, or direct CDP in Rust. It shells out to a separately installed Firefox Agent Bridge CLI/native host plus Firefox extension.

Tool input/capabilities:
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:24-81` defines `BrowserInput`, including action, browser, provider_action, params, url, tab/frame targeting, selector/text/contains/script/key, coordinates, output format, wait/new_tab/focus/clear/submit/page_world, scroll fields, timeout, upload path, form fields.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:172-273` exposes the JSON schema. Supported actions are `status`, `setup`, `list_tabs`, `new_tab`, `select_tab`, `get_active_tab`, `list_frames`, `open`, `snapshot`, `get_content`, `interactables`, `click`, `type`, `fill_form`, `select`, `wait`, `screenshot`, `eval`, `scroll`, `upload`, `press`, `provider_command`.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:276-291` dispatches `status`/`setup` directly and otherwise ensures the provider is ready before executing.

Action mapping:
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:473-499` maps jcode actions to bridge actions: `listTabs`, `newSession`, `setActiveTab`, `getActiveTab`, `listFrames`, `navigate`, `getContent`, `getInteractables`, `click`, `type`, `fillForm`, `waitFor`, `screenshot`, `evaluate`, `scroll`, `uploadFile`, or raw `provider_command`.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:501-687` builds/validates bridge params for each action.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:689-705` applies shared tab/frame/selector/text targeting.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:707-730` implements `press` by generating an in-page JavaScript snippet and routing it through `evaluate`.

Bridge execution and responses:
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:732-789` locates `crate::browser::browser_binary_path()`, runs the bridge CLI as `browser <action> <json-params>`, optionally sets `BROWSER_SESSION`, captures stdout/stderr, returns JSON stdout or `{ "raw": stdout }`, and upgrades unknown-action errors into compatibility guidance.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:791-829` handles screenshots specially by passing a temp filename, reading the saved PNG, base64-encoding it into a labeled tool image, then deleting the temp file.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:839-855` formats bridge results; `snapshot`, `get_content`, `interactables`, and `eval` get human-readable rendering.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:857-874` formats content result from `content`, `text`, `html`, or title+URL.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:876-887` formats eval result.
- `/Users/tom/Developer/tools/jcode/src/tool/browser.rs:890-927` formats interactable elements as numbered rows with kind, tag, text, and selector.

Setup/runtime support:
- `/Users/tom/Developer/tools/jcode/src/browser.rs:6-12` points setup at GitHub releases for `1jehuang/firefox-agent-bridge`, and defines native host / extension IDs.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:13-23` defines `BrowserStatus` with backend, browser, setup/binary/responding/compatible/missing_actions/ready.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:25-33` probes bridge support for `evaluate`, `listFrames`, `scroll`, and `uploadFile`.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:47-57` defines the installed bridge CLI path under `~/.jcode/browser/browser` or `browser.exe`.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:59-73` defines the native host binary and XPI paths.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:102-152` starts/reuses per-jcode-session bridge sessions via `browser session start <session_name>` and runtime socket/pid files.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:162-192` detects shell commands beginning with `browser` and rewrites them to the installed full path.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:194-361` implements setup: create browser dir, download CLI if needed, install native messaging host manifest, check Firefox extension connectivity, open/install extension when needed, wait for ping/ready, and mark setup complete.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:364-453` downloads the browser CLI, XPI, and host binary from the latest GitHub release assets.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:532-579` installs the Firefox native messaging host manifest pointing at the host binary.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:648-665` checks bridge connectivity with `browser ping` expecting `pong`.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:667-700` probes required action support.
- `/Users/tom/Developer/tools/jcode/src/browser.rs:702-737` computes readiness and auto-marks setup complete when ready.

Extraction notes:
- The Rust wrapper is extractable, but the actual browser automation engine is external: Firefox, Firefox extension, native messaging host, and downloaded bridge binaries from GitHub releases.
- Heavy/replacement candidates for a different Rust project: replace Firefox Agent Bridge if you need Chrome/Playwright/CDP/WebDriver, remove jcode storage/platform paths, remove jcode `ToolOutput` image plumbing, and replace setup/download/install logic if you do not want runtime installation from GitHub.
- Dependency-wise the Rust layer is moderate (`tokio::process`, `serde_json`, `base64`, `reqwest` for setup downloads), but operationally the browser tool has the heaviest external runtime dependency of the three tools.

## Dependency References

- `/Users/tom/Developer/tools/jcode/Cargo.toml:102-106` includes `futures`, `async-trait`, and `reqwest` with `json`, `stream`, and `blocking`.
- `/Users/tom/Developer/tools/jcode/Cargo.toml:126-132` includes `dirs`, `anyhow`, `chrono`, `regex`, and `urlencoding`.
- `/Users/tom/Developer/tools/jcode/Cargo.toml:142-147` includes `base64`, `url`, and `open`.
- `/Users/tom/Developer/tools/jcode/crates/jcode-provider-core/src/lib.rs:370-389` provides the shared `reqwest::Client` used by `websearch` and `webfetch`.
