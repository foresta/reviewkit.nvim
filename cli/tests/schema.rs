use std::path::PathBuf;

use reviewkit::model::{Contract, SCHEMA_REVISION, SCHEMA_VERSION};
use serde_json::Value;

fn committed_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/schema.json")
}

fn current() -> Value {
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "schema_revision": SCHEMA_REVISION,
        "schema": schemars::schema_for!(Contract),
    })
}

/// Descriptions and titles are documentation, not contract.
fn shape(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != "description" && k.as_str() != "title")
                .map(|(k, v)| (k.clone(), shape(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(shape).collect()),
        other => other.clone(),
    }
}

fn types(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(ts)) => ts.iter().filter_map(|t| t.as_str().map(str::to_string)).collect(),
        _ => vec![],
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().map(|v| v.to_string()).collect())
        .unwrap_or_default()
}

/// What a client written against `old` relies on and `new` changes. The
/// contract has inputs and outputs, so an existing field is invariant both
/// ways (type, requiredness, limits); only new fields and enum values grow.
fn breaks(path: &str, old: &Value, new: &Value, out: &mut Vec<String>) {
    if old.get("$ref") != new.get("$ref") {
        out.push(format!("{path}: reference changed"));
        return;
    }
    let (old_types, new_types) = (types(old), types(new));
    for t in &old_types {
        if !new_types.contains(t) {
            out.push(format!("{path}: no longer accepts type {t}"));
        }
    }
    for t in &new_types {
        if !old_types.is_empty() && !old_types.contains(t) {
            out.push(format!("{path}: now also accepts type {t}"));
        }
    }
    let new_values = strings(new.get("enum"));
    for v in strings(old.get("enum")) {
        if !new_values.contains(&v) {
            out.push(format!("{path}: enum value {v} removed"));
        }
    }
    for key in [
        "const",
        "format",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minItems",
        "maxItems",
        "minLength",
        "maxLength",
    ] {
        if old.get(key) != new.get(key) {
            out.push(format!("{path}: {key} changed"));
        }
    }
    match (old.get("additionalProperties"), new.get("additionalProperties")) {
        (Some(o @ Value::Object(_)), Some(n @ Value::Object(_))) => breaks(&format!("{path}{{}}"), o, n, out),
        (o, n) if o != n => out.push(format!("{path}: additionalProperties changed")),
        _ => {}
    }
    let old_props = old.get("properties");
    let (old_required, new_required) = (strings(old.get("required")), strings(new.get("required")));
    let existed = |r: &String| old_props.and_then(|p| p.get(r.trim_matches('"'))).is_some();
    let exists_now = |r: &String| new.get("properties").and_then(|p| p.get(r.trim_matches('"'))).is_some();
    for r in &old_required {
        if exists_now(r) && !new_required.contains(r) {
            out.push(format!("{path}: {r} is no longer required"));
        }
    }
    // A new field is optional (`#[serde(default)]` or Option): an old client
    // does not send it, and an old session file does not have it.
    for r in &new_required {
        if !old_required.contains(r) {
            let how = if existed(r) {
                "became required"
            } else {
                "was added as required"
            };
            out.push(format!("{path}: {r} {how}"));
        }
    }
    if let Some(Value::Object(props)) = old_props {
        for (key, old_prop) in props {
            match new.get("properties").and_then(|p| p.get(key)) {
                Some(new_prop) => breaks(&format!("{path}.{key}"), old_prop, new_prop, out),
                None => out.push(format!("{path}.{key}: removed")),
            }
        }
    }
    if let Some(old_items) = old.get("items") {
        match new.get("items") {
            Some(new_items) => breaks(&format!("{path}[]"), old_items, new_items, out),
            None => out.push(format!("{path}[]: items removed")),
        }
    }
    // Variants are matched by content: inserting or reordering one is
    // additive, losing or changing one is not.
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(Value::Array(old_variants)) = old.get(key) {
            let new_variants = new.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
            for (i, o) in old_variants.iter().enumerate() {
                let compatible = new_variants.iter().any(|n| {
                    let mut found = vec![];
                    breaks(path, o, n, &mut found);
                    found.is_empty()
                });
                if !compatible {
                    out.push(format!("{path}: {key} variant {i} has no compatible match"));
                }
            }
        }
    }
}

fn contract_breaks(old: &Value, new: &Value) -> Vec<String> {
    let mut out = vec![];
    // The root only gathers the types into one schema; nobody sends or
    // receives a Contract, so adding a type there is not a new required field.
    let root = |schema: &Value| {
        let mut root = schema.clone();
        if let Some(map) = root.as_object_mut() {
            map.remove("required");
        }
        root
    };
    breaks("Contract", &root(old), &root(new), &mut out);
    if let Some(Value::Object(defs)) = old.get("$defs") {
        for (name, old_def) in defs {
            match new.get("$defs").and_then(|d| d.get(name)) {
                Some(new_def) => breaks(name, old_def, new_def, &mut out),
                None => out.push(format!("{name}: type removed")),
            }
        }
    }
    out
}

/// The committed schema is what released clients were built against.
/// Within one SCHEMA_VERSION it may only grow; anything else needs the
/// version bumped and a session migration (store::MIGRATIONS).
#[test]
fn the_contract_only_grows_within_a_schema_version() {
    let current = current();
    let path = committed_path();
    let update = std::env::var_os("REVIEWKIT_UPDATE_SCHEMA").is_some();
    let committed: Option<Value> = match std::fs::read(&path) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes).unwrap()),
        Err(_) if update => None,
        Err(err) => panic!("{}: {err}; run `make gen-schema`", path.display()),
    };
    if let Some(committed) = &committed
        && committed["schema_version"] == current["schema_version"]
    {
        let found = contract_breaks(&committed["schema"], &current["schema"]);
        assert!(
            found.is_empty(),
            "the contract lost something without a SCHEMA_VERSION bump:\n  {}",
            found.join("\n  ")
        );
    }
    if let Some(committed) = &committed {
        let changed = shape(&committed["schema"]) != shape(&current["schema"]);
        let (before, now) = (
            committed["schema_revision"].as_u64().unwrap_or(0),
            current["schema_revision"].as_u64().unwrap_or(0),
        );
        if changed {
            assert!(
                now > before,
                "the contract changed but SCHEMA_REVISION is still {now}: bump it"
            );
        } else {
            assert_eq!(now, before, "SCHEMA_REVISION changed without a contract change");
        }
    }
    if committed.as_ref() != Some(&current) {
        assert!(update, "docs/schema.json is out of date; run `make gen-schema`");
        let mut text = serde_json::to_string_pretty(&current).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
    }
}

#[test]
fn removals_renames_and_narrowing_are_breaks() {
    let old = serde_json::json!({
        "properties": {
            "a": { "type": "string" },
            "b": { "type": ["string", "null"] },
            "e": { "enum": ["x", "y"] },
            "o": { "type": "string" }
        },
        "required": ["a"]
    });
    let new = serde_json::json!({
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "string" },
            "e": { "enum": ["x", "z"] },
            "o": { "type": "string" },
            "added": { "type": "string" }
        },
        "required": ["a", "o", "added"]
    });
    let mut found = vec![];
    breaks("T", &old, &new, &mut found);
    assert_eq!(
        found,
        vec![
            "T: \"o\" became required",
            "T: \"added\" was added as required",
            "T.b: no longer accepts type null",
            "T.e: enum value \"y\" removed",
        ]
    );
    let mut grown = vec![];
    breaks("T", &old, &old, &mut grown);
    assert!(grown.is_empty());
    assert_eq!(
        contract_breaks(&serde_json::json!({ "$defs": { "Gone": {} } }), &serde_json::json!({})),
        vec!["Gone: type removed"]
    );
}

#[test]
fn existing_fields_keep_their_type_requiredness_and_limits() {
    let old = serde_json::json!({
        "properties": {
            "decision": { "type": "string" },
            "count": { "type": "integer", "format": "uint32", "minimum": 0 },
            "opts": { "type": "object", "additionalProperties": true }
        },
        "required": ["decision", "count"]
    });
    let new = serde_json::json!({
        "properties": {
            "decision": { "type": ["string", "null"] },
            "count": { "type": "integer", "format": "uint8", "minimum": 0 },
            "opts": { "type": "object", "additionalProperties": false }
        },
        "required": ["count"]
    });
    let mut found = vec![];
    breaks("T", &old, &new, &mut found);
    assert_eq!(
        found,
        vec![
            "T: \"decision\" is no longer required",
            "T.count: format changed",
            "T.decision: now also accepts type null",
            "T.opts: additionalProperties changed",
        ]
    );
}

#[test]
fn variants_are_matched_by_content_not_position() {
    let a = serde_json::json!({ "properties": { "kind": { "const": "a" } } });
    let b = serde_json::json!({ "properties": { "kind": { "const": "b" } } });
    let c = serde_json::json!({ "properties": { "kind": { "const": "c" } } });
    let old = serde_json::json!({ "oneOf": [a.clone(), b.clone()] });
    let mut found = vec![];
    breaks(
        "S",
        &old,
        &serde_json::json!({ "oneOf": [c, b.clone(), a] }),
        &mut found,
    );
    assert!(found.is_empty(), "{found:?}");
    breaks("S", &old, &serde_json::json!({ "oneOf": [b] }), &mut found);
    assert_eq!(found, vec!["S: oneOf variant 0 has no compatible match"]);
}

#[test]
fn a_new_field_must_not_be_required() {
    let old = serde_json::json!({ "properties": { "body": { "type": "string" } }, "required": ["body"] });
    let new = serde_json::json!({
        "properties": { "body": { "type": "string" }, "priority": { "type": "string" }, "note": { "type": "string" } },
        "required": ["body", "priority"]
    });
    let mut found = vec![];
    breaks("CommentInput", &old, &new, &mut found);
    assert_eq!(found, vec!["CommentInput: \"priority\" was added as required"]);
}

#[test]
fn adding_a_type_to_the_contract_root_is_additive() {
    let old = serde_json::json!({ "properties": { "a": { "$ref": "#/$defs/A" } }, "required": ["a"] });
    let new = serde_json::json!({
        "properties": { "a": { "$ref": "#/$defs/A" }, "b": { "$ref": "#/$defs/B" } },
        "required": ["a", "b"]
    });
    assert!(contract_breaks(&old, &new).is_empty());
}
