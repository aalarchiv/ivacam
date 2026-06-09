//! Schema export: builds an `OpenAPI` 3.0 document from the Rust types so
//! `schema/openapi.yaml` stays a derived artifact, not hand-maintained.
//!
//! Used by `cargo xtask schema` (writes the YAML) and `xtask schema --check`
//! (asserts the checked-in file is up to date).
//!
//! schemars produces JSON Schema; we wrap the relevant component schemas
//! into an `OpenAPI` envelope by hand because no good "JSON Schema → `OpenAPI`
//! components" crate exists at our pinned rust version. The path
//! definitions are still authored in `schema/openapi.yaml`'s static
//! header — only the `components/schemas` section is regenerated.

use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};


// ─── HTTP envelope types ──────────────────────────────────────────────────
//
// These mirror the JSON the ivac-server crate sends. Keeping them here
// means the OpenAPI YAML is 100% derived from Rust.

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VersionResponse {
    pub version: String,
    pub transport: TransportKind,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TransportKind {
    PythonBridge,
    RustServer,
    Tauri,
    Wasm,
}

// GenerateRequest / Response live in crate::pipeline; we publish them
// under the same OpenAPI component names so the wire contract stays
// stable.
pub use crate::pipeline::{
    PipelineRequest as GenerateRequest, PipelineResponse as GenerateResponse,
    PipelineStats as GenerateStats, PostProcessorKind,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Build the components/schemas object from the Rust types. Returns an
/// `serde_yaml`-compatible JSON value so callers can splice it into the
/// hand-authored `OpenAPI` envelope. All `$ref`s are rewritten to `OpenAPI`'s
/// `#/components/schemas/X` form (schemars defaults to `#/definitions/X`).
#[must_use]
pub fn components_schemas() -> Value {
    let mut schemas = serde_json::Map::new();
    // kb1y: each module registers its own wire types next to their
    // definitions; this function just composes them. Adding a type is a
    // same-file edit in the module that owns it. Output order is
    // independent of call order (serde_json::Map is sorted), and the
    // izup collision assert + schema drift guard still protect renames.
    crate::geometry::register_schemas(&mut schemas);
    crate::input::register_schemas(&mut schemas);
    crate::cam::register_schemas(&mut schemas);
    crate::gcode::preview::register_schemas(&mut schemas);
    crate::project::register_schemas(&mut schemas);
    crate::pipeline::register_schemas(&mut schemas);
    crate::sim::register_schemas(&mut schemas);
    crate::errors::register_schemas(&mut schemas);
    crate::register_schemas(&mut schemas);

    // HTTP envelope types defined in this module.
    insert::<HealthResponse>(&mut schemas, "HealthResponse");
    insert::<VersionResponse>(&mut schemas, "VersionResponse");
    insert::<ErrorResponse>(&mut schemas, "Error");

    let mut value = Value::Object(schemas);
    rewrite_refs(&mut value);
    value
}

/// The components/schemas accumulator each module's `register_schemas`
/// writes into (kb1y).
pub(crate) type SchemaMap = serde_json::Map<String, Value>;

pub(crate) fn insert<T: schemars::JsonSchema>(map: &mut SchemaMap, name: &str) {
    let s = schema_for!(T);
    let mut v = serde_json::to_value(s).unwrap();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
        if let Some(Value::Object(inner)) = obj.remove("definitions") {
            for (k, vv) in inner {
                map.entry(k).or_insert(vv);
            }
        }
    }
    // izup: catch silent registry overwrites. A previous entry under
    // this name with a DIFFERENT schema shape means two distinct Rust
    // types both want to publish as `<name>` — only the second would
    // appear in the output, and the frontend codegen would silently
    // model one of them as the other.
    if let Some(prev) = map.get(name) {
        assert_eq!(
            prev, &v,
            "schema registry conflict: `{name}` is being registered twice with \
             different shapes — rename one of them or split insert::<…> calls."
        );
    }
    map.insert(name.into(), v);
}

fn rewrite_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                if k == "$ref" {
                    if let Some(Value::String(s)) = map.get_mut(&k) {
                        if let Some(rest) = s.strip_prefix("#/definitions/") {
                            *s = format!("#/components/schemas/{rest}");
                        }
                    }
                } else if let Some(child) = map.get_mut(&k) {
                    rewrite_refs(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}

/// Returns a list of TS type names that the frontend currently consumes.
/// `pnpm run codegen` reads `schema/openapi.yaml` directly — this is just
/// a sanity check that we're exporting what we need.
#[must_use]
pub fn frontend_types() -> &'static [&'static str] {
    &[
        "Point2",
        "BBox",
        "Layer",
        "Segment",
        "ImportResponse",
        "Project",
        "Op",
        "ToolEntry",
        "PolylineOffset",
        "ToolpathSegment",
    ]
}

/// Build a flat `OpenAPI` 3.0 document with hand-written paths + the
/// auto-generated component schemas merged in.
#[must_use]
pub fn openapi_document() -> Value {
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "ivaCAM API",
            "version": env!("CARGO_PKG_VERSION"),
            "license": { "name": "GPL-3.0-or-later" },
            "description":
                "JSON contract for ivaCAM core operations. Components below are auto-derived \
                 from Rust types (schemars). Paths are authored in schema/openapi.yaml — re-export \
                 from `ivac-core::schema::openapi_document` if you need them programmatically."
        },
        "components": { "schemas": components_schemas() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_round_trip_to_json() {
        let doc = openapi_document();
        let s = serde_json::to_string(&doc).unwrap();
        assert!(s.contains("\"Segment\""));
        assert!(s.contains("\"ImportResponse\""));
        assert!(s.contains("\"ToolpathSegment\""));
    }

    #[test]
    fn frontend_types_are_all_present() {
        let v = components_schemas();
        let obj = v.as_object().unwrap();
        for t in frontend_types() {
            assert!(obj.contains_key(*t), "{t} missing from components/schemas");
        }
    }

    /// izup: every `$ref` in the generated schema tree must resolve to a
    /// registered type name in `components/schemas`. The common silent
    /// failure mode is "added a new pub `JsonSchema`-deriving type,
    /// inlined it in some other type's field, forgot to call `insert::<T>`
    /// in `components_schemas()`" — schemars then emits a `$ref` to a
    /// definition that isn't in the `OpenAPI` output, and the frontend
    /// codegen produces an `unknown` typed field that no compile-time
    /// check catches.
    ///
    /// This test walks every `$ref` and asserts the referenced name is a
    /// key in the schemas map. New types whose `$ref` lands in someone
    /// else's schema will fail this test until they're registered.
    #[test]
    fn every_ref_resolves_to_a_registered_type() {
        let v = components_schemas();
        let obj = v
            .as_object()
            .expect("components_schemas() must be an object");
        let names: std::collections::HashSet<String> = obj.keys().cloned().collect();
        let mut missing: Vec<(String, String)> = Vec::new();
        for (parent, child) in obj {
            collect_unresolved_refs(parent, child, &names, &mut missing);
        }
        if !missing.is_empty() {
            let summary: Vec<String> = missing
                .iter()
                .map(|(parent, name)| format!("    {parent}.* → {name}"))
                .collect();
            panic!(
                "{} dangling $ref{} in components/schemas — register the missing \
                 types in `components_schemas()` so the frontend codegen sees \
                 them:\n{}",
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                summary.join("\n"),
            );
        }
    }

    /// Walk `value`, append every `#/components/schemas/X` whose `X` is
    /// not in `known` to `out` as `(parent_type, missing_name)`.
    fn collect_unresolved_refs(
        parent: &str,
        value: &Value,
        known: &std::collections::HashSet<String>,
        out: &mut Vec<(String, String)>,
    ) {
        match value {
            Value::Object(map) => {
                if let Some(Value::String(s)) = map.get("$ref") {
                    if let Some(name) = s.strip_prefix("#/components/schemas/") {
                        if !known.contains(name) {
                            out.push((parent.to_string(), name.to_string()));
                        }
                    }
                }
                for child in map.values() {
                    collect_unresolved_refs(parent, child, known, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_unresolved_refs(parent, item, known, out);
                }
            }
            _ => {}
        }
    }
}
