//! Schema export: builds an OpenAPI 3.0 document from the Rust types so
//! `schema/openapi.yaml` stays a derived artifact, not hand-maintained.
//!
//! Used by `cargo xtask schema` (writes the YAML) and `xtask schema --check`
//! (asserts the checked-in file is up to date).
//!
//! schemars produces JSON Schema; we wrap the relevant component schemas
//! into an OpenAPI envelope by hand because no good "JSON Schema → OpenAPI
//! components" crate exists at our pinned rust version. The path
//! definitions are still authored in `schema/openapi.yaml`'s static
//! header — only the `components/schemas` section is regenerated.

use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cam::offsets::PolylineOffset;
use crate::cam::VcObject;
use crate::gcode::preview::ToolpathSegment;
use crate::geometry::{BBox, Layer, Point2, Segment};
use crate::ImportOutput;

// ─── HTTP envelope types ──────────────────────────────────────────────────
//
// These mirror the JSON the wiac-server crate sends. Keeping them here
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
/// hand-authored OpenAPI envelope. All `$ref`s are rewritten to OpenAPI's
/// `#/components/schemas/X` form (schemars defaults to `#/definitions/X`).
pub fn components_schemas() -> Value {
    let mut schemas = serde_json::Map::new();
    insert::<Point2>(&mut schemas, "Point2");
    insert::<BBox>(&mut schemas, "BBox");
    insert::<Layer>(&mut schemas, "Layer");
    insert::<Segment>(&mut schemas, "Segment");
    insert::<ImportOutput>(&mut schemas, "ImportResponse");
    insert::<VcObject>(&mut schemas, "VcObject");
    insert::<PolylineOffset>(&mut schemas, "PolylineOffset");
    insert::<ToolpathSegment>(&mut schemas, "ToolpathSegment");

    // New project / operations / tools shapes — ship them so the frontend
    // can codegen the matching TS types ahead of UX-4..-7.
    insert::<crate::project::Project>(&mut schemas, "Project");
    insert::<crate::project::Operation>(&mut schemas, "Operation");
    insert::<crate::project::OperationKind>(&mut schemas, "OperationKind");
    insert::<crate::project::OperationParams>(&mut schemas, "OperationParams");
    insert::<crate::project::OperationSource>(&mut schemas, "OperationSource");
    insert::<crate::project::SourceCombine>(&mut schemas, "SourceCombine");
    insert::<crate::project::CutDirection>(&mut schemas, "CutDirection");
    insert::<crate::cam::setup::PlungeStrategy>(&mut schemas, "PlungeStrategy");
    insert::<crate::pipeline::RegionPreview>(&mut schemas, "RegionPreview");
    insert::<crate::pipeline::PipelineWarning>(&mut schemas, "PipelineWarning");
    insert::<crate::project::PocketStrategy>(&mut schemas, "PocketStrategy");
    insert::<crate::project::ToolEntry>(&mut schemas, "ToolEntry");
    insert::<crate::project::ToolKind>(&mut schemas, "ToolKind");
    insert::<crate::project::Coolant>(&mut schemas, "Coolant");

    insert::<HealthResponse>(&mut schemas, "HealthResponse");
    insert::<VersionResponse>(&mut schemas, "VersionResponse");
    insert::<GenerateRequest>(&mut schemas, "GenerateRequest");
    insert::<GenerateResponse>(&mut schemas, "GenerateResponse");
    insert::<GenerateStats>(&mut schemas, "GenerateStats");
    insert::<ErrorResponse>(&mut schemas, "Error");

    let mut value = Value::Object(schemas);
    rewrite_refs(&mut value);
    value
}

fn insert<T: schemars::JsonSchema>(map: &mut serde_json::Map<String, Value>, name: &str) {
    let s = schema_for!(T);
    let mut v = serde_json::to_value(s).unwrap();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
        if let Some(defs) = obj.remove("definitions") {
            if let Value::Object(inner) = defs {
                for (k, vv) in inner {
                    map.entry(k).or_insert(vv);
                }
            }
        }
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
pub fn frontend_types() -> &'static [&'static str] {
    &[
        "Point2",
        "BBox",
        "Layer",
        "Segment",
        "ImportResponse",
        "Project",
        "Operation",
        "ToolEntry",
        "PolylineOffset",
        "ToolpathSegment",
    ]
}

/// Build a flat OpenAPI 3.0 document with hand-written paths + the
/// auto-generated component schemas merged in.
pub fn openapi_document() -> Value {
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "wiaConstructor API",
            "version": env!("CARGO_PKG_VERSION"),
            "license": { "name": "GPL-3.0-or-later" },
            "description":
                "JSON contract for wiaConstructor core operations. Components below are auto-derived \
                 from Rust types (schemars). Paths are authored in schema/openapi.yaml — re-export \
                 from `wiac-core::schema::openapi_document` if you need them programmatically."
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
}
