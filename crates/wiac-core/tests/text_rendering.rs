//! Integration test: render a DXF TEXT entity with the bundled fonts and
//! make sure single-line vs outline modes produce structurally different
//! output.

use std::path::PathBuf;

use wiac_core::input::text::{is_single_line_font, render_text};
use wiac_core::Point2;

fn font_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fonts")
}

#[test]
fn rhss_engraving_produces_fewer_segments_than_outline_font() {
    let rhss = std::fs::read(font_dir().join("RhSS.ttf")).expect("RhSS.ttf");
    let dejavu = std::fs::read(font_dir().join("DejaVuSans.ttf")).expect("DejaVuSans.ttf");

    let rhss_segs =
        render_text(&rhss, "AB", Point2::new(0.0, 0.0), 10.0, "0", 7).expect("rhss");
    let dejavu_segs =
        render_text(&dejavu, "AB", Point2::new(0.0, 0.0), 10.0, "0", 7).expect("dejavu");

    assert!(rhss_segs.len() < dejavu_segs.len(),
        "single-line should produce fewer segments: rhss={}, dejavu={}",
        rhss_segs.len(), dejavu_segs.len());
}

#[test]
fn detection_round_trip_via_face_parse() {
    let rhss = std::fs::read(font_dir().join("RhSS.ttf")).unwrap();
    let dejavu = std::fs::read(font_dir().join("DejaVuSans.ttf")).unwrap();
    let rhss_face = ttf_parser::Face::parse(&rhss, 0).unwrap();
    let dejavu_face = ttf_parser::Face::parse(&dejavu, 0).unwrap();
    assert!(is_single_line_font(&rhss_face));
    assert!(!is_single_line_font(&dejavu_face));
}
