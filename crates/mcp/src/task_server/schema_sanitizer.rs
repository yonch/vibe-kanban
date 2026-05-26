//! Strip `"null"` branches from JSON Schemas before they are sent to MCP clients.
//!
//! Background. `rmcp 1.x` builds tool input schemas with `SchemaSettings::draft2020_12()`
//! and intentionally does NOT install the `AddNullable` transform (rmcp PR #664), so an
//! `Option<T>` field on a request struct serializes as `"type": ["T", "null"]` — the
//! standards-compliant union form for JSON Schema 2020-12.
//!
//! Cursor's MCP client (cursor-agent through at least 2026.05) does not yet accept union
//! `type` arrays containing `"null"` nor `anyOf` branches whose only purpose is to permit
//! `null`. It rejects the call client-side with errors like `Parameter 'X' must be of
//! type null,string, got string`, so the request never reaches the server. Tracked in
//! forum.cursor.com threads "Error Invoking MCP Tools with Optional Parameters" (Nov 2025)
//! and "MCP parameter validation fails for integer in anyOf [integer, null] schemas"
//! (Nov 2025). Cursor acknowledged the bug; no fix has shipped as of cursor-agent
//! 2026.05.
//!
//! Workaround. The MCP server can publish a schema that drops the `null` branch entirely.
//! The field stays optional via the request struct's `required` array (an `Option<T>` is
//! optional at the JSON-RPC level as long as the client doesn't supply it), and every
//! other MCP client we use — Claude Code, Codex, Gemini, Amp — accepts the stripped form.
//! Cursor accepts it too.
//!
//! Scope. This module rewrites only the JSON Schema published to clients via
//! `tools/list`. Server-side deserialization continues to use `serde_json::from_value`
//! against the original `Option<T>` types, so the runtime behavior of every tool is
//! unchanged: a missing field still deserializes to `None`, and an explicit `null`
//! supplied by a permissive client also deserializes to `None`.
//!
//! Exception. A few tool fields are tri-state `Option<Option<T>>` where an explicit
//! `null` is semantically distinct from "field absent" (e.g. `update_issue.parent_issue_id`,
//! where `null` clears the parent and absence leaves it unchanged). Those fields are
//! enumerated in [`preserve_null_fields_for`] and skipped by the sanitizer so the
//! published schema still advertises `null`. Cursor will continue to reject calls
//! that supply `null` to those specific fields until upstream fixes nullable
//! validation, but every other supported client accepts them.

use std::sync::Arc;

use rmcp::{handler::server::tool::ToolRouter, model::JsonObject};
use serde_json::Value;

/// Rewrite every tool's `input_schema` in `router` to remove `null` from
/// type unions and `anyOf`/`oneOf` branches, except for the fields listed in
/// [`preserve_null_fields_for`].
///
/// The rewrite is idempotent: running it twice yields the same schema as
/// running it once.
pub fn sanitize_tool_router<S>(router: &mut ToolRouter<S>) {
    for route in router.map.values_mut() {
        let preserve = preserve_null_fields_for(route.attr.name.as_ref());
        let mut schema: JsonObject = (*route.attr.input_schema).clone();

        // Pull aside any top-level properties whose nullability must survive,
        // sanitize the rest of the schema, then put them back unchanged.
        let mut stashed: Vec<(String, Value)> = Vec::new();
        if !preserve.is_empty()
            && let Some(Value::Object(props)) = schema.get_mut("properties")
        {
            for name in preserve {
                if let Some(value) = props.remove(*name) {
                    stashed.push(((*name).to_string(), value));
                }
            }
        }

        sanitize_schema_object(&mut schema);

        if route.attr.name.as_ref() == "start_workspace" {
            inline_start_workspace_repositories_items(&mut schema);
        }

        if !stashed.is_empty()
            && let Some(Value::Object(props)) = schema.get_mut("properties")
        {
            for (name, value) in stashed {
                props.insert(name, value);
            }
        }

        route.attr.input_schema = Arc::new(schema);
    }
}

/// Top-level properties whose nullability must be preserved in the published
/// schema because the tool relies on an explicit `null` to express a distinct
/// operation. Used by [`sanitize_tool_router`].
pub(crate) fn preserve_null_fields_for(tool_name: &str) -> &'static [&'static str] {
    match tool_name {
        // `parent_issue_id` is `Option<Option<Uuid>>`: absent = no change,
        // explicit `null` = un-nest from parent, UUID = set parent. Stripping
        // `null` from the schema would remove the un-nest operation from the
        // MCP surface.
        "update_issue" => &["parent_issue_id"],
        _ => &[],
    }
}

/// Strip `"null"` from union `type` arrays and from `anyOf`/`oneOf` branches in
/// `obj`, recursing into nested schemas.
///
/// Public for unit tests.
pub(crate) fn sanitize_schema_object(obj: &mut JsonObject) {
    if let Some(type_value) = obj.get_mut("type") {
        strip_null_from_type(type_value);
    }

    for keyword in ["anyOf", "oneOf"] {
        if let Some(Value::Array(branches)) = obj.get_mut(keyword) {
            strip_null_branches(branches);

            // Recurse into the remaining branches.
            for branch in branches.iter_mut() {
                if let Value::Object(map) = branch {
                    sanitize_schema_object(map);
                }
            }

            // If exactly one branch survives, splice its keywords into the parent
            // and drop the union — clients see a simpler, equivalent schema.
            let collapsed_into_parent = if branches.len() == 1 {
                if let Some(Value::Object(only)) = branches.pop() {
                    obj.remove(keyword);
                    for (key, value) in only {
                        obj.entry(key).or_insert(value);
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };

            // If the union became empty, drop the keyword entirely.
            if !collapsed_into_parent
                && obj
                    .get(keyword)
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
            {
                obj.remove(keyword);
            }
        }
    }

    // `allOf` does not affect nullability the same way (every branch must validate),
    // but we still recurse to clean nested unions inside it.
    if let Some(Value::Array(branches)) = obj.get_mut("allOf") {
        for branch in branches.iter_mut() {
            if let Value::Object(map) = branch {
                sanitize_schema_object(map);
            }
        }
    }

    // Recurse into the structural keywords that hold child schemas.
    for keyword in ["properties", "patternProperties", "$defs", "definitions"] {
        if let Some(Value::Object(children)) = obj.get_mut(keyword) {
            for value in children.values_mut() {
                if let Value::Object(child) = value {
                    sanitize_schema_object(child);
                }
            }
        }
    }

    // `items` can be a single schema or (legacy) an array of schemas; `prefixItems`
    // (2020-12) is always an array.
    for keyword in [
        "items",
        "additionalItems",
        "additionalProperties",
        "contains",
        "not",
    ] {
        if let Some(Value::Object(child)) = obj.get_mut(keyword) {
            sanitize_schema_object(child);
        }
    }

    for keyword in ["prefixItems"] {
        if let Some(Value::Array(items)) = obj.get_mut(keyword) {
            for item in items.iter_mut() {
                if let Value::Object(child) = item {
                    sanitize_schema_object(child);
                }
            }
        }
    }
}

/// Inline the `start_workspace.repositories.items` schema so clients that do not
/// resolve local `$ref` definitions still see the required array-of-objects
/// shape directly in the published tool schema.
fn inline_start_workspace_repositories_items(obj: &mut JsonObject) {
    let ref_path = obj
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|props| props.get("repositories"))
        .and_then(Value::as_object)
        .and_then(|repositories| repositories.get("items"))
        .and_then(Value::as_object)
        .and_then(|items| items.get("$ref"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let Some(ref_path) = ref_path else {
        return;
    };

    let schema_value = Value::Object(obj.clone());
    let Some(Value::Object(target)) = schema_value.pointer(ref_path.trim_start_matches('#')) else {
        return;
    };
    let target = target.clone();

    if let Some(Value::Object(props)) = obj.get_mut("properties")
        && let Some(Value::Object(repositories)) = props.get_mut("repositories")
    {
        repositories.insert("items".to_string(), Value::Object(target));
    }
}

/// Collapse a `"type"` value of the form `["T", "null"]` into `"T"` (or
/// `["T", "U"]` when more than one non-null type remained).
fn strip_null_from_type(type_value: &mut Value) {
    let Value::Array(items) = type_value else {
        return;
    };

    items.retain(|item| !matches!(item, Value::String(s) if s == "null"));

    match items.len() {
        // Should not happen for a well-formed schema, but be conservative.
        0 => {}
        1 => {
            if let Some(only) = items.first().cloned() {
                *type_value = only;
            }
        }
        _ => {} // Multiple non-null types — leave as-is.
    }
}

/// Drop branches in an `anyOf`/`oneOf` array that exist solely to permit `null`
/// (i.e. `{"type": "null"}` with no other constraints).
fn strip_null_branches(branches: &mut Vec<Value>) {
    branches.retain(|branch| !is_null_only_branch(branch));
}

/// Return true if `branch` is a schema that matches only `null`.
fn is_null_only_branch(branch: &Value) -> bool {
    let Value::Object(map) = branch else {
        return false;
    };

    // We only collapse the narrow shape `{"type": "null"}` (optionally with a
    // `description`). Anything richer than that (e.g. `const: null` combined
    // with other keywords) is left alone — those carry semantics beyond
    // "allow null".
    let has_only_safe_keys = map
        .keys()
        .all(|key| matches!(key.as_str(), "type" | "description" | "title"));
    if !has_only_safe_keys {
        return false;
    }

    matches!(map.get("type"), Some(Value::String(t)) if t == "null")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sanitize(value: Value) -> Value {
        let mut obj = value.as_object().expect("expected JSON object").clone();
        sanitize_schema_object(&mut obj);
        Value::Object(obj)
    }

    #[test]
    fn preserve_list_lists_known_tri_state_fields() {
        assert_eq!(
            preserve_null_fields_for("update_issue"),
            &["parent_issue_id"]
        );
        assert!(preserve_null_fields_for("list_issues").is_empty());
        assert!(preserve_null_fields_for("unknown_tool").is_empty());
    }

    #[test]
    fn strips_null_from_string_union_type() {
        let input = json!({
            "type": "object",
            "properties": {
                "name": { "type": ["string", "null"], "description": "x" }
            }
        });
        let expected = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "x" }
            }
        });
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn strips_null_from_integer_union_type() {
        let input = json!({
            "type": "object",
            "properties": {
                "limit": { "type": ["integer", "null"], "format": "int32" }
            }
        });
        let expected = json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "format": "int32" }
            }
        });
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn leaves_multi_type_unions_without_null_alone() {
        let input = json!({
            "type": "object",
            "properties": {
                "value": { "type": ["string", "number"] }
            }
        });
        let cloned = input.clone();
        assert_eq!(sanitize(input), cloned);
    }

    #[test]
    fn collapses_three_way_union_with_null() {
        let input = json!({
            "type": "object",
            "properties": {
                "value": { "type": ["string", "number", "null"] }
            }
        });
        let expected = json!({
            "type": "object",
            "properties": {
                "value": { "type": ["string", "number"] }
            }
        });
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn strips_null_branch_from_anyof_and_collapses_single_survivor() {
        let input = json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "null" }
                    ],
                    "description": "an optional count"
                }
            }
        });
        let expected = json!({
            "type": "object",
            "properties": {
                "value": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "an optional count"
                }
            }
        });
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn collapse_does_not_overwrite_existing_parent_keys() {
        // When the parent already has a `description`, splicing the surviving
        // branch must preserve the parent's description.
        let input = json!({
            "anyOf": [
                { "type": "string", "description": "from branch" },
                { "type": "null" }
            ],
            "description": "from parent"
        });
        let expected = json!({
            "type": "string",
            "description": "from parent"
        });
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn strips_null_branch_from_oneof() {
        let input = json!({
            "oneOf": [
                { "type": "string" },
                { "type": "integer" },
                { "type": "null" }
            ]
        });
        let expected = json!({
            "oneOf": [
                { "type": "string" },
                { "type": "integer" }
            ]
        });
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn does_not_strip_richer_null_branches() {
        // `{"type": "null", "const": null, "title": "Disabled"}` carries semantic
        // weight (it documents a sentinel). Leave it alone.
        let input = json!({
            "anyOf": [
                { "type": "string" },
                { "type": "null", "const": null, "title": "Disabled" }
            ]
        });
        let cloned = input.clone();
        assert_eq!(sanitize(input), cloned);
    }

    #[test]
    fn recurses_into_array_items() {
        let input = json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": ["string", "null"], "format": "uuid" }
                }
            }
        });
        let expected = json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string", "format": "uuid" }
                }
            }
        });
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn recurses_into_defs_and_ref_targets() {
        // Mirrors the `start_workspace` schema shape: a nested $ref target
        // under $defs that itself contains nullable fields.
        let input = json!({
            "type": "object",
            "properties": {
                "repositories": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/RepoInput" }
                }
            },
            "$defs": {
                "RepoInput": {
                    "type": "object",
                    "properties": {
                        "repo_id": { "type": ["string", "null"], "format": "uuid" },
                        "branch": { "type": "string" }
                    }
                }
            }
        });
        let expected = json!({
            "type": "object",
            "properties": {
                "repositories": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/RepoInput" }
                }
            },
            "$defs": {
                "RepoInput": {
                    "type": "object",
                    "properties": {
                        "repo_id": { "type": "string", "format": "uuid" },
                        "branch": { "type": "string" }
                    }
                }
            }
        });
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn is_idempotent() {
        let input = json!({
            "type": "object",
            "properties": {
                "a": { "type": ["string", "null"] },
                "b": { "anyOf": [{ "type": "integer" }, { "type": "null" }] }
            }
        });

        let once = sanitize(input.clone());
        let twice = sanitize(once.clone());
        assert_eq!(once, twice);
    }

    #[test]
    fn empty_anyof_after_stripping_is_dropped() {
        // Degenerate but well-defined: an `anyOf` consisting only of null branches.
        // We drop the keyword rather than leaving an empty `anyOf: []`, which
        // would reject every value.
        let input = json!({
            "description": "useless",
            "anyOf": [
                { "type": "null" },
                { "type": "null", "title": "still null" }
            ]
        });
        let expected = json!({
            "description": "useless"
        });
        assert_eq!(sanitize(input), expected);
    }
}
