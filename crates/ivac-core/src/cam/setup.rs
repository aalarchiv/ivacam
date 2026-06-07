//! Setup tree — port of viaConstructor's `setupdefaults.py`.
//!
//! This module now holds ONLY the runtime-resolved [`Setup`] bundle. The
//! JsonSchema wire/config types it embeds live under `crate::project`:
//! machine capabilities in [`crate::project::machine`], cut/tool op config
//! in [`crate::project::config`], and the per-op contour params (tabs /
//! leads) in [`crate::project::params`]. The pipeline `setup_resolver`
//! assembles a `Setup` from those project-level types.

// `is_default_wcs` takes `&Wcs` because that's the signature serde's
// `skip_serializing_if` requires.
#![allow(clippy::trivially_copy_pass_by_ref)]

use crate::project::{
    LeadsConfig, MachineConfig, MillConfig, PocketConfig, TabsConfig, ToolConfig,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Setup {
    pub machine: MachineConfig,
    pub tool: ToolConfig,
    pub mill: MillConfig,
    pub pockets: PocketConfig,
    pub tabs: TabsConfig,
    pub leads: LeadsConfig,
    /// e2mq: program-active work coordinate system. Threaded in from
    /// `Project.work_offset.wcs` by the pipeline `setup_resolver` /
    /// `header_setup_for` builders. The post's `program_begin`
    /// emits the explicit `G54..G59` from this and pins the same
    /// value into `PostState.wcs` so `tool_z_shift` writes its
    /// `G10 L20 P<n>` against the *active* WCS (P1=G54, …, P6=G59),
    /// not a hardcoded P1. Defaults to G54 — back-compat for
    /// projects that don't set `work_offset.wcs`.
    #[serde(default, skip_serializing_if = "is_default_wcs")]
    pub wcs: crate::project::Wcs,
}

fn is_default_wcs(v: &crate::project::Wcs) -> bool {
    matches!(v, crate::project::Wcs::G54)
}
