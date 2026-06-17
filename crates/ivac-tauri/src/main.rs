//! ivaCAM desktop binary — a thin wrapper over the shared Tauri shell in
//! `lib.rs`.
//!
//! Mobile (Android / iOS) does not build this binary; its entry point is
//! [`ivac_tauri_lib::run`] invoked by the Tauri-generated host glue. All
//! the actual app setup lives in the library so both targets share it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ivac_tauri_lib::run();
}
