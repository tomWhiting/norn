//! Integration tests for the core `#[derive(ToolArgs)]` schema generation.
//!
//! Each struct derives both `Deserialize` and `ToolArgs` to confirm the two
//! derives coexist (C48 / S5) and to exercise serde attribute handling on a
//! realistic deserialization target.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::option_option,
    dead_code,
    non_snake_case
)]

use std::collections::HashMap;

use norn_macros::ToolArgs;
use serde::Deserialize;

#[derive(Deserialize, ToolArgs)]
struct AllRequired {
    /// Absolute path to the file to edit.
    path: String,
    /// Exact text to find and replace. Must match exactly once.
    old_string: String,
    /// Replacement text.
    new_string: String,
}

#[test]
fn all_required_string_fields() {
    let schema = AllRequired::json_schema();
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["required"],
        serde_json::json!(["path", "old_string", "new_string"])
    );
    assert_eq!(schema["properties"]["path"]["type"], "string");
    assert_eq!(schema["properties"]["old_string"]["type"], "string");
    assert_eq!(schema["properties"]["new_string"]["type"], "string");
}

#[derive(Deserialize, ToolArgs)]
struct WithDefault {
    /// Required path.
    path: String,
    /// Optional content with a serde default.
    #[serde(default)]
    content: String,
}

#[test]
fn default_field_omitted_from_required() {
    let schema = WithDefault::json_schema();
    assert_eq!(schema["required"], serde_json::json!(["path"]));
    // The defaulted field is still a property, just not required.
    assert_eq!(schema["properties"]["content"]["type"], "string");
}

fn default_label() -> String {
    "label".to_string()
}

#[derive(Deserialize, ToolArgs)]
struct WithDefaultFn {
    /// Required path.
    path: String,
    /// Defaulted via a named function.
    #[serde(default = "default_label")]
    label: String,
}

#[test]
fn serde_default_with_function() {
    let schema = WithDefaultFn::json_schema();
    assert_eq!(schema["required"], serde_json::json!(["path"]));
    assert_eq!(schema["properties"]["label"]["type"], "string");
}

#[derive(Deserialize, ToolArgs)]
struct WithSkip {
    /// Required path.
    path: String,
    /// Internal field excluded from the schema.
    #[serde(skip)]
    internal: String,
}

#[test]
fn skipped_field_excluded() {
    let schema = WithSkip::json_schema();
    assert_eq!(schema["required"], serde_json::json!(["path"]));
    assert!(schema["properties"].get("internal").is_none());
    assert!(schema["properties"]["path"].is_object());
}

#[derive(Deserialize, ToolArgs)]
struct WithDoc {
    /// Absolute path to the file to edit.
    path: String,
}

#[test]
fn doc_comment_becomes_description() {
    let schema = WithDoc::json_schema();
    assert_eq!(
        schema["properties"]["path"]["description"],
        "Absolute path to the file to edit."
    );
}

#[derive(Deserialize, ToolArgs)]
struct WithMultilineDoc {
    /// First.
    /// Second.
    /// Third.
    path: String,
}

#[test]
fn multiline_doc_joined_with_space() {
    let schema = WithMultilineDoc::json_schema();
    assert_eq!(
        schema["properties"]["path"]["description"],
        "First. Second. Third."
    );
}

#[derive(Deserialize, ToolArgs)]
struct Empty {}

#[test]
fn empty_struct() {
    let schema = Empty::json_schema();
    assert_eq!(
        schema,
        serde_json::json!({
            "type": "object",
            "required": [],
            "properties": {},
            "additionalProperties": false
        })
    );
}

#[derive(Deserialize, ToolArgs)]
struct WithBool {
    /// Whether to overwrite.
    overwrite: bool,
}

#[test]
fn bool_field_type() {
    let schema = WithBool::json_schema();
    assert_eq!(schema["properties"]["overwrite"]["type"], "boolean");
    assert_eq!(schema["required"], serde_json::json!(["overwrite"]));
}

#[derive(Deserialize, ToolArgs)]
struct FieldOrder {
    /// Zebra field.
    zebra: String,
    /// Apple field.
    apple: String,
    /// Mango field.
    mango: String,
}

#[test]
fn field_order_preserved() {
    let schema = FieldOrder::json_schema();
    let keys: Vec<&String> = schema["properties"]
        .as_object()
        .expect("properties is an object")
        .keys()
        .collect();
    assert_eq!(keys, vec!["zebra", "apple", "mango"]);
}

// ---------------------------------------------------------------------------
// NTM-002: numeric type mapping
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct SignedInt {
    /// Signed offset.
    value: i64,
}

#[test]
fn signed_int_has_no_minimum() {
    let schema = SignedInt::json_schema();
    assert_eq!(schema["properties"]["value"]["type"], "integer");
    assert!(
        schema["properties"]["value"].get("minimum").is_none(),
        "signed integers must not carry a minimum"
    );
}

#[derive(Deserialize, ToolArgs)]
struct AllSignedInts {
    /// i8 field.
    a: i8,
    /// i16 field.
    b: i16,
    /// i32 field.
    c: i32,
    /// i64 field.
    d: i64,
    /// isize field.
    e: isize,
}

#[test]
fn signed_int_uniformity() {
    let schema = AllSignedInts::json_schema();
    for field in ["a", "b", "c", "d", "e"] {
        assert_eq!(schema["properties"][field]["type"], "integer");
        assert!(schema["properties"][field].get("minimum").is_none());
    }
}

#[derive(Deserialize, ToolArgs)]
struct UnsignedInt {
    /// Unsigned count.
    count: u32,
}

#[test]
fn unsigned_int_has_zero_minimum() {
    let schema = UnsignedInt::json_schema();
    assert_eq!(schema["properties"]["count"]["type"], "integer");
    assert_eq!(schema["properties"]["count"]["minimum"], 0);
}

#[derive(Deserialize, ToolArgs)]
struct AllUnsignedInts {
    /// u8 field.
    a: u8,
    /// u16 field.
    b: u16,
    /// u32 field.
    c: u32,
    /// u64 field.
    d: u64,
    /// usize field.
    e: usize,
}

#[test]
fn unsigned_int_uniformity() {
    let schema = AllUnsignedInts::json_schema();
    for field in ["a", "b", "c", "d", "e"] {
        assert_eq!(schema["properties"][field]["type"], "integer");
        assert_eq!(schema["properties"][field]["minimum"], 0);
    }
}

#[derive(Deserialize, ToolArgs)]
struct Floats {
    /// 64-bit float.
    big: f64,
    /// 32-bit float.
    small: f32,
}

#[test]
fn floats_map_to_number() {
    let schema = Floats::json_schema();
    assert_eq!(schema["properties"]["big"]["type"], "number");
    assert_eq!(schema["properties"]["small"]["type"], "number");
}

// ---------------------------------------------------------------------------
// NTM-002: Option<T> unwrapping
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct WithOption {
    /// Required path.
    path: String,
    /// Optional byte offset.
    #[serde(default)]
    offset: Option<u64>,
    /// Optional display name.
    #[serde(default)]
    name: Option<String>,
}

#[test]
fn option_unwraps_inner_schema() {
    let schema = WithOption::json_schema();
    assert_eq!(schema["properties"]["offset"]["type"], "integer");
    assert_eq!(schema["properties"]["offset"]["minimum"], 0);
    assert_eq!(schema["properties"]["name"]["type"], "string");
    assert_eq!(schema["required"], serde_json::json!(["path"]));
}

#[derive(Deserialize, ToolArgs)]
struct WithDoubleOption {
    /// Optional optional flag.
    #[serde(default)]
    flag: Option<Option<bool>>,
}

#[test]
fn option_option_collapses_to_inner_type() {
    let schema = WithDoubleOption::json_schema();
    assert_eq!(schema["properties"]["flag"]["type"], "boolean");
    assert_eq!(schema["required"], serde_json::json!([]));
}

// ---------------------------------------------------------------------------
// NTM-002: Vec<T> and HashMap<String, T>
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct WithVec {
    /// List of tag strings.
    tags: Vec<String>,
    /// List of counts.
    counts: Vec<u64>,
}

#[test]
fn vec_generates_array_schema() {
    let schema = WithVec::json_schema();
    assert_eq!(schema["properties"]["tags"]["type"], "array");
    assert_eq!(schema["properties"]["tags"]["items"]["type"], "string");
    assert_eq!(schema["properties"]["counts"]["type"], "array");
    assert_eq!(schema["properties"]["counts"]["items"]["type"], "integer");
    assert_eq!(schema["properties"]["counts"]["items"]["minimum"], 0);
}

#[derive(Deserialize, ToolArgs)]
struct WithMap {
    /// Free-form string metadata.
    metadata: HashMap<String, String>,
    /// Numeric counters keyed by name.
    counters: HashMap<String, i64>,
}

#[test]
fn hashmap_generates_object_with_additional_properties() {
    let schema = WithMap::json_schema();
    assert_eq!(schema["properties"]["metadata"]["type"], "object");
    assert_eq!(
        schema["properties"]["metadata"]["additionalProperties"]["type"],
        "string"
    );
    assert_eq!(schema["properties"]["counters"]["type"], "object");
    assert_eq!(
        schema["properties"]["counters"]["additionalProperties"]["type"],
        "integer"
    );
}

// ---------------------------------------------------------------------------
// NTM-002: serde_json::Value escape hatch
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct WithValue {
    /// Arbitrary content payload.
    content: serde_json::Value,
}

#[test]
fn serde_json_value_becomes_any_schema() {
    let schema = WithValue::json_schema();
    // Doc-comment description rides on the otherwise-empty schema.
    assert_eq!(
        schema["properties"]["content"]["description"],
        "Arbitrary content payload."
    );
    assert!(schema["properties"]["content"].get("type").is_none());
    // The field is still required — Value carries no #[serde(default)].
    assert_eq!(schema["required"], serde_json::json!(["content"]));
}

// ---------------------------------------------------------------------------
// NTM-002: nested ToolArgs struct
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct InnerArgs {
    /// Inner identifier.
    id: String,
    /// Inner counter.
    count: u32,
}

#[derive(Deserialize, ToolArgs)]
struct OuterArgs {
    /// Outer label.
    label: String,
    /// The nested args payload.
    inner: InnerArgs,
}

#[test]
fn nested_struct_inlines_inner_schema() {
    let schema = OuterArgs::json_schema();
    let inner = &schema["properties"]["inner"];
    assert_eq!(inner["type"], "object");
    assert_eq!(inner["properties"]["id"]["type"], "string");
    assert_eq!(inner["properties"]["count"]["type"], "integer");
    assert_eq!(inner["properties"]["count"]["minimum"], 0);
    assert_eq!(inner["required"], serde_json::json!(["id", "count"]));
    assert_eq!(inner["additionalProperties"], false);
    // The outer description rides on the nested object via runtime patch.
    assert_eq!(inner["description"], "The nested args payload.");
}

// ---------------------------------------------------------------------------
// NTM-002: enum representations
// ---------------------------------------------------------------------------

/// Mode selector used by the search tool.
#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "snake_case")]
enum SearchMode {
    /// Search file contents using regex.
    Content,
    /// Search file names using glob patterns.
    Files,
    /// Search using AST structural queries.
    Ast,
}

#[test]
fn string_enum_with_rename_all_snake_case() {
    let schema = SearchMode::json_schema();
    assert_eq!(schema["type"], "string");
    assert_eq!(
        schema["enum"],
        serde_json::json!(["content", "files", "ast"])
    );
    let description = schema["description"].as_str().expect("has description");
    assert!(description.starts_with("Mode selector used by the search tool."));
    assert!(description.contains("content: Search file contents using regex."));
    assert!(description.contains("files: Search file names using glob patterns."));
    assert!(description.contains("ast: Search using AST structural queries."));
}

#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "snake_case")]
enum MultiWordVariants {
    /// Full text search across all files.
    FullTextSearch,
    /// Search only in recently modified files.
    RecentlyModified,
    /// Use the AST-based structural matcher.
    AstStructuralMatch,
}

#[test]
fn enum_rename_all_splits_pascal_case_variants() {
    let schema = MultiWordVariants::json_schema();
    assert_eq!(schema["type"], "string");
    assert_eq!(
        schema["enum"],
        serde_json::json!([
            "full_text_search",
            "recently_modified",
            "ast_structural_match"
        ])
    );
}

#[derive(Deserialize, ToolArgs)]
enum BareEnum {
    A,
    B,
    C,
}

#[test]
fn string_enum_without_rename_uses_raw_idents() {
    let schema = BareEnum::json_schema();
    assert_eq!(schema["enum"], serde_json::json!(["A", "B", "C"]));
    // No container doc + no variant docs means no description key.
    assert!(schema.get("description").is_none());
}

#[derive(Deserialize, ToolArgs)]
enum EnumWithVariantRename {
    #[serde(rename = "custom")]
    First,
    Second,
}

#[test]
fn variant_rename_overrides_ident() {
    let schema = EnumWithVariantRename::json_schema();
    assert_eq!(schema["enum"], serde_json::json!(["custom", "Second"]));
}

#[derive(Deserialize, ToolArgs)]
#[serde(tag = "type")]
enum TaskAction {
    /// Create a fresh task.
    Create {
        /// Short summary.
        title: String,
    },
    /// Update an existing task's status.
    Update {
        /// Task identifier.
        task_id: String,
        /// New status string.
        status: String,
    },
}

#[test]
fn internally_tagged_enum_uses_one_of_with_const_discriminator() {
    let schema = TaskAction::json_schema();
    let variants = schema["oneOf"].as_array().expect("oneOf is array");
    assert_eq!(variants.len(), 2);
    assert_eq!(variants[0]["type"], "object");
    assert_eq!(variants[0]["properties"]["type"]["const"], "Create");
    assert_eq!(variants[0]["properties"]["title"]["type"], "string");
    assert_eq!(
        variants[0]["required"],
        serde_json::json!(["type", "title"])
    );
    assert_eq!(variants[0]["additionalProperties"], false);
    assert_eq!(variants[1]["properties"]["type"]["const"], "Update");
    assert_eq!(variants[1]["properties"]["task_id"]["type"], "string");
    assert_eq!(variants[1]["properties"]["status"]["type"], "string");
    assert_eq!(
        variants[1]["required"],
        serde_json::json!(["type", "task_id", "status"])
    );
}

/// Per-variant doc comments must surface as `description` on each `oneOf`
/// variant so catalog derivation (and the model) can see what each command
/// does.
#[test]
fn internally_tagged_variants_carry_doc_descriptions() {
    let schema = TaskAction::json_schema();
    let variants = schema["oneOf"].as_array().expect("oneOf is array");
    assert_eq!(variants[0]["description"], "Create a fresh task.");
    assert_eq!(
        variants[1]["description"],
        "Update an existing task's status."
    );
}

#[derive(Deserialize, ToolArgs)]
#[serde(tag = "kind")]
enum UndocumentedAction {
    Ping,
}

#[test]
fn internally_tagged_variant_without_doc_has_no_description_key() {
    let schema = UndocumentedAction::json_schema();
    let variants = schema["oneOf"].as_array().expect("oneOf is array");
    assert!(variants[0].get("description").is_none());
}

#[derive(Deserialize, ToolArgs)]
#[serde(untagged)]
enum UntaggedKind {
    Number {
        /// Numeric value.
        value: i64,
    },
    Label {
        /// Text label.
        label: String,
    },
}

#[test]
fn untagged_enum_omits_discriminator() {
    let schema = UntaggedKind::json_schema();
    let variants = schema["oneOf"].as_array().expect("oneOf is array");
    assert_eq!(variants.len(), 2);
    // No const discriminator anywhere.
    assert!(variants[0]["properties"].get("type").is_none());
    assert!(variants[1]["properties"].get("type").is_none());
    assert_eq!(variants[0]["properties"]["value"]["type"], "integer");
    assert_eq!(variants[1]["properties"]["label"]["type"], "string");
}

#[derive(Deserialize, ToolArgs)]
#[serde(tag = "t", content = "c")]
enum AdjacentKind {
    /// Variant carrying named data.
    Create {
        /// Item title.
        title: String,
    },
}

#[test]
fn adjacent_enum_uses_tag_and_content_fields() {
    let schema = AdjacentKind::json_schema();
    let variants = schema["oneOf"].as_array().expect("oneOf is array");
    assert_eq!(variants.len(), 1);
    let entry = &variants[0];
    assert_eq!(entry["type"], "object");
    assert_eq!(entry["properties"]["t"]["const"], "Create");
    assert_eq!(entry["properties"]["c"]["type"], "object");
    assert_eq!(
        entry["properties"]["c"]["properties"]["title"]["type"],
        "string"
    );
    assert_eq!(entry["required"], serde_json::json!(["t", "c"]));
    assert_eq!(entry["additionalProperties"], false);
}

#[derive(Deserialize, ToolArgs)]
#[serde(untagged)]
enum DocumentedUntagged {
    /// A numeric form.
    Number {
        /// Numeric value.
        value: i64,
    },
}

/// Adjacent and untagged variants carry their doc descriptions too.
#[test]
fn adjacent_and_untagged_variants_carry_doc_descriptions() {
    let adjacent = AdjacentKind::json_schema();
    assert_eq!(
        adjacent["oneOf"][0]["description"],
        "Variant carrying named data."
    );

    let untagged = DocumentedUntagged::json_schema();
    assert_eq!(untagged["oneOf"][0]["description"], "A numeric form.");
}

// ---------------------------------------------------------------------------
// NTM-003: serde field rename
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct FieldRename {
    /// Renamed wire field.
    #[serde(rename = "custom_name")]
    rust_name: String,
}

#[test]
fn serde_rename_changes_property_and_required() {
    let schema = FieldRename::json_schema();
    assert_eq!(schema["properties"]["custom_name"]["type"], "string");
    assert!(schema["properties"].get("rust_name").is_none());
    assert_eq!(schema["required"], serde_json::json!(["custom_name"]));
}

// ---------------------------------------------------------------------------
// NTM-003: container rename_all
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "camelCase")]
struct RenameAllCamel {
    /// First field.
    my_field: String,
    /// Second field.
    another_field: u32,
}

#[test]
fn rename_all_camel_case_transforms_all_names() {
    let schema = RenameAllCamel::json_schema();
    assert_eq!(schema["properties"]["myField"]["type"], "string");
    assert_eq!(schema["properties"]["anotherField"]["type"], "integer");
    assert!(schema["properties"].get("my_field").is_none());
    assert_eq!(
        schema["required"],
        serde_json::json!(["myField", "anotherField"])
    );
}

#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "snake_case")]
struct RenameAllSnake {
    /// A pascal-cased Rust field.
    MyField: String,
}

#[test]
fn rename_all_snake_case_transforms_pascal_ident() {
    let schema = RenameAllSnake::json_schema();
    assert_eq!(schema["properties"]["my_field"]["type"], "string");
    assert_eq!(schema["required"], serde_json::json!(["my_field"]));
}

#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "camelCase")]
struct RenamePrecedence {
    /// Field with an explicit rename overriding `rename_all`.
    #[serde(rename = "explicit")]
    my_field: String,
    /// Field that follows `rename_all`.
    other_field: String,
}

#[test]
fn field_rename_takes_precedence_over_rename_all() {
    let schema = RenamePrecedence::json_schema();
    assert_eq!(schema["properties"]["explicit"]["type"], "string");
    assert_eq!(schema["properties"]["otherField"]["type"], "string");
    assert!(schema["properties"].get("myField").is_none());
    assert_eq!(
        schema["required"],
        serde_json::json!(["explicit", "otherField"])
    );
}

// ---------------------------------------------------------------------------
// NTM-003: skip_serializing vs skip vs skip_deserializing
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct SkipSerializingVisible {
    /// Required path.
    path: String,
    /// Output-only field that is still a valid input.
    #[serde(skip_serializing)]
    computed: String,
}

#[test]
fn skip_serializing_field_stays_in_schema() {
    let schema = SkipSerializingVisible::json_schema();
    assert_eq!(schema["properties"]["computed"]["type"], "string");
    assert_eq!(schema["required"], serde_json::json!(["path", "computed"]));
}

#[derive(Deserialize, ToolArgs)]
struct SkipDeserializingHidden {
    /// Required path.
    path: String,
    /// Field excluded from input.
    #[serde(skip_deserializing)]
    derived: String,
}

#[test]
fn skip_deserializing_field_omitted() {
    let schema = SkipDeserializingHidden::json_schema();
    assert!(schema["properties"].get("derived").is_none());
    assert_eq!(schema["required"], serde_json::json!(["path"]));
}

#[derive(Deserialize, ToolArgs)]
struct SkipBoth {
    /// Required path.
    path: String,
    /// Field skipped in both directions.
    #[serde(skip_serializing, skip_deserializing)]
    internal: String,
}

#[test]
fn skip_serializing_and_deserializing_omitted() {
    let schema = SkipBoth::json_schema();
    assert!(schema["properties"].get("internal").is_none());
    assert_eq!(schema["required"], serde_json::json!(["path"]));
}

// ---------------------------------------------------------------------------
// NTM-003: serde flatten
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct Pagination {
    /// Page offset.
    offset: u64,
    /// Optional page size.
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Deserialize, ToolArgs)]
struct ListQuery {
    /// Search filter.
    filter: String,
    /// Merged pagination parameters.
    #[serde(flatten)]
    page: Pagination,
}

#[test]
fn flatten_merges_inner_properties_into_parent() {
    let schema = ListQuery::json_schema();
    // Inner properties merged into the parent object.
    assert_eq!(schema["properties"]["filter"]["type"], "string");
    assert_eq!(schema["properties"]["offset"]["type"], "integer");
    assert_eq!(schema["properties"]["limit"]["type"], "integer");
    // The flatten field name itself never appears.
    assert!(schema["properties"].get("page").is_none());
    // Inner required names are appended; the defaulted Option is not required.
    assert_eq!(schema["required"], serde_json::json!(["filter", "offset"]));
    // The outer object remains closed.
    assert_eq!(schema["additionalProperties"], false);
}

// ---------------------------------------------------------------------------
// NTM-003: tool_args overrides
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct SchemaOverride {
    /// Mode with a constrained enum schema.
    #[tool_args(schema = {"type": "string", "enum": ["strict", "auto"]})]
    mode: String,
}

#[test]
fn tool_args_schema_replaces_field_schema() {
    let schema = SchemaOverride::json_schema();
    assert_eq!(schema["properties"]["mode"]["type"], "string");
    assert_eq!(
        schema["properties"]["mode"]["enum"],
        serde_json::json!(["strict", "auto"])
    );
    assert_eq!(schema["required"], serde_json::json!(["mode"]));
}

#[derive(Deserialize, ToolArgs)]
struct DescriptionOverride {
    /// Doc comment that should be overridden.
    #[tool_args(description = "Custom desc")]
    field: String,
}

#[test]
fn tool_args_description_overrides_doc_comment() {
    let schema = DescriptionOverride::json_schema();
    assert_eq!(schema["properties"]["field"]["description"], "Custom desc");
}

#[derive(Deserialize, ToolArgs)]
struct RequiredOverride {
    /// Required path.
    path: String,
    /// Defaulted field forced back into required.
    #[serde(default)]
    #[tool_args(required)]
    label: String,
}

#[test]
fn tool_args_required_overrides_serde_default() {
    let schema = RequiredOverride::json_schema();
    assert_eq!(schema["required"], serde_json::json!(["path", "label"]));
    assert_eq!(schema["properties"]["label"]["type"], "string");
}

#[derive(Deserialize, ToolArgs)]
struct SkipOverride {
    /// Required path.
    path: String,
    /// Field excluded from the schema only.
    #[tool_args(skip)]
    internal: String,
}

#[test]
fn tool_args_skip_excludes_field_from_schema() {
    let schema = SkipOverride::json_schema();
    assert!(schema["properties"].get("internal").is_none());
    assert_eq!(schema["required"], serde_json::json!(["path"]));
    // Deserialization still populates the skipped field.
    let value: SkipOverride =
        serde_json::from_value(serde_json::json!({ "path": "p", "internal": "x" })).unwrap();
    assert_eq!(value.internal, "x");
}

#[derive(Deserialize, ToolArgs)]
struct AdditionalPropsOverride {
    /// Free-form nested object.
    #[tool_args(additional_properties)]
    config: InnerArgs,
}

#[test]
fn tool_args_additional_properties_sets_true_on_field() {
    let schema = AdditionalPropsOverride::json_schema();
    assert_eq!(schema["properties"]["config"]["additionalProperties"], true);
    // The outer struct stays closed.
    assert_eq!(schema["additionalProperties"], false);
}

// ---------------------------------------------------------------------------
// NTM-003: Deserialize coexistence with a real round trip
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
struct RoundTrip {
    /// User identifier.
    user_id: String,
    /// Optional retry count.
    #[serde(default)]
    retry_count: u32,
}

#[test]
fn derive_coexists_with_deserialize_round_trip() {
    let schema = RoundTrip::json_schema();
    assert_eq!(schema["properties"]["userId"]["type"], "string");
    assert_eq!(schema["properties"]["retryCount"]["type"], "integer");
    assert_eq!(schema["required"], serde_json::json!(["userId"]));

    // serde uses the same wire names the schema advertises.
    let value: RoundTrip =
        serde_json::from_value(serde_json::json!({ "userId": "u1", "retryCount": 3 })).unwrap();
    assert_eq!(
        value,
        RoundTrip {
            user_id: "u1".to_string(),
            retry_count: 3,
        }
    );

    // The defaulted field may be omitted, exactly as the schema's required list says.
    let defaulted: RoundTrip =
        serde_json::from_value(serde_json::json!({ "userId": "u2" })).unwrap();
    assert_eq!(defaulted.retry_count, 0);
}

#[derive(ToolArgs)]
struct ToolArgsOnly {
    /// A field on a struct that does not derive Deserialize.
    name: String,
}

#[test]
fn tool_args_without_deserialize_compiles() {
    let schema = ToolArgsOnly::json_schema();
    assert_eq!(schema["properties"]["name"]["type"], "string");
}

// ---------------------------------------------------------------------------
// NTM-003 / R8: mixed-feature interaction
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToolArgs)]
struct MixedInner {
    /// Inner required token.
    token: String,
    /// Inner optional note.
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "camelCase")]
struct MixedFeatures {
    /// A renamed field via `rename_all`.
    primary_key: String,
    /// An explicitly renamed field.
    #[serde(rename = "explicitName")]
    secondary_key: String,
    /// A defaulted field forced required.
    #[serde(default)]
    #[tool_args(required)]
    forced: String,
    /// Custom-described field.
    #[tool_args(description = "Overridden description")]
    described: u32,
    /// Flattened inner parameters.
    #[serde(flatten)]
    inner: MixedInner,
}

#[test]
fn mixed_features_interact_correctly() {
    let schema = MixedFeatures::json_schema();

    // rename_all + field rename.
    assert_eq!(schema["properties"]["primaryKey"]["type"], "string");
    assert_eq!(schema["properties"]["explicitName"]["type"], "string");
    assert!(schema["properties"].get("secondaryKey").is_none());

    // description override.
    assert_eq!(
        schema["properties"]["described"]["description"],
        "Overridden description"
    );

    // flatten merge — inner properties hoisted, flatten field name absent.
    assert_eq!(schema["properties"]["token"]["type"], "string");
    assert_eq!(schema["properties"]["note"]["type"], "string");
    assert!(schema["properties"].get("inner").is_none());

    // required, in declaration order: rename_all name, explicit rename name,
    // forced default, the non-defaulted described field, then inner required.
    assert_eq!(
        schema["required"],
        serde_json::json!(["primaryKey", "explicitName", "forced", "described", "token"])
    );

    // Outer object stays closed.
    assert_eq!(schema["additionalProperties"], false);

    // Deserialize honours the same wire names.
    let value: MixedFeatures = serde_json::from_value(serde_json::json!({
        "primaryKey": "pk",
        "explicitName": "ek",
        "forced": "f",
        "described": 7,
        "token": "t"
    }))
    .unwrap();
    assert_eq!(value.primary_key, "pk");
    assert_eq!(value.secondary_key, "ek");
    assert_eq!(value.inner.token, "t");
}
