//! `ivac` — headless converter and JSON-API surface for the Rust core.
//!
//! Subcommands:
//!   * `ivac import <file>`            — emit /import-shaped JSON to stdout
//!   * `ivac generate <file> [--post]` — emit /generate-shaped JSON
//!     (gcode + 3D preview toolpath) to stdout
//!   * `ivac stream-gcode <project.json> [--output FILE]` — stream gcode
//!     from a full project (a /generate request body) straight to a file or
//!     stdout, never holding the whole program in memory (ivac-3j1p).
//!
//! Mirrors the JSON contract in `schema/openapi.yaml`.

mod i18n;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use ivac_core::cam::chaining::{classify_containment, segments_to_objects};
use ivac_core::cam::offsets::{
    apply_overcut_to_offsets, pocket_for_object, PocketEmit, PolylineOffset,
};
use ivac_core::cam::setup::Setup;
use ivac_core::cam::VcObject;
use ivac_core::gcode::{emit_polylines, grbl, hpgl, linuxcnc, preview};
use ivac_core::project::ToolOffset;
use ivac_core::{ImportOptions, ImportOutput};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct GenerateResponseJson<'a> {
    gcode: &'a str,
    toolpath: &'a [preview::ToolpathSegment],
    stats: GenerateStats,
}

#[derive(Serialize, Default)]
struct GenerateStats {
    object_count: usize,
    closed_object_count: usize,
    offset_count: usize,
    cut_distance: f64,
    travel_distance: f64,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    // Resolve the UI language once, before anything user-facing is printed:
    // a `--lang` override (pulled out of argv here) beats the LANG/LC_ALL
    // environment. Everything below goes through `i18n::t`/`tp`.
    let (lang_override, rest) = i18n::extract_lang(std::env::args().skip(1).collect());
    i18n::set_locale(i18n::detect_locale(lang_override.as_deref(), |k| {
        std::env::var(k).ok()
    }));

    let mut args = rest.into_iter();
    let cmd = args.next().unwrap_or_default();
    match cmd.as_str() {
        "import" => cmd_import(args),
        "generate" => cmd_generate(args),
        "stream-gcode" => cmd_stream(args),
        "posts" => cmd_posts(args),
        "" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            bail!(
                "{}",
                i18n::tp("cli.err.unknown_subcommand", &[("name", other)])
            );
        }
    }
}

fn print_help() {
    // The command skeletons (`ivac import <path>`, flag names) are literal and
    // language-agnostic; only the descriptions and labels go through the
    // catalog. Help prints to stderr so stdout stays clean JSON for pipes.
    eprintln!("{}\n", i18n::t("cli.help.tagline"));
    eprintln!("{}", i18n::t("cli.help.usage"));
    eprintln!("  ivac import <path>");
    eprintln!("      {}", i18n::t("cli.help.import"));
    eprintln!(
        "  ivac generate <path> [--post linuxcnc|grbl|hpgl|cps] [--diameter MM] [--depth MM]"
    );
    eprintln!("                       [--inside|--outside|--on] [--overcut]");
    eprintln!("                       [--post-id ID | --post-file FILE.cps] [--post-prop k=v]...");
    eprintln!("      {}", i18n::t("cli.help.generate"));
    eprintln!("  ivac stream-gcode <project.json> [--output FILE] [--two-sided]");
    eprintln!("      {}", i18n::t("cli.help.stream"));
    eprintln!("  ivac posts [inspect <file.cps>]");
    eprintln!("      {}", i18n::t("cli.help.posts"));
    eprintln!("  ivac --help");
    eprintln!("      {}", i18n::t("cli.help.help"));
    eprintln!("\n  --lang <en|de|ko>   {}", i18n::t("cli.help.lang"));
}

fn cmd_import(mut args: impl Iterator<Item = String>) -> Result<()> {
    let path = args.next().context(i18n::t("cli.err.missing_path"))?;
    let path = PathBuf::from(path);
    let opts = ImportOptions::default();
    let out = ivac_core::input::import_path(&path, &opts).with_context(|| {
        i18n::tp(
            "cli.err.importing",
            &[("path", &path.display().to_string())],
        )
    })?;
    // Serialize the full ImportOutput — it already derives the /import
    // contract (snake_case field names matching the TS ImportResponse and
    // the wasm/server output), so emitting it directly keeps `objects`,
    // `object_meta`, and `text_entities`. The previous hand-rolled subset
    // dropped those, producing "degraded" sample fixtures the 2D canvas
    // couldn't select features from.
    serde_json::to_writer_pretty(std::io::stdout(), &out)?;
    println!();
    Ok(())
}

// One flag-parsing loop plus the per-dialect emit switch; splitting it
// would separate a flag from the value it sets.
#[allow(clippy::too_many_lines)]
fn cmd_generate(args: impl Iterator<Item = String>) -> Result<()> {
    let mut path: Option<PathBuf> = None;
    let mut post_kind = "linuxcnc".to_string();
    let mut diameter = 3.0_f64;
    let mut depth = -2.0_f64;
    let mut step = -1.0_f64;
    let mut tool_offset = ToolOffset::Outside;

    let mut overcut = false;
    // CPS post selection (only meaningful with --post cps).
    let mut post_file: Option<PathBuf> = None;
    let mut post_id: Option<String> = None;
    let mut post_props: Vec<(String, String)> = Vec::new();
    let mut iter = args.peekable();
    while let Some(arg) = iter.next() {
        let needs_value =
            |opt: &'static str| move || i18n::tp("cli.err.opt_needs_value", &[("opt", opt)]);
        match arg.as_str() {
            "--post" => post_kind = iter.next().with_context(needs_value("--post"))?,
            "--post-file" => {
                post_file = Some(PathBuf::from(
                    iter.next().with_context(needs_value("--post-file"))?,
                ));
            }
            "--post-id" => post_id = Some(iter.next().with_context(needs_value("--post-id"))?),
            "--post-prop" => {
                let raw = iter.next().with_context(needs_value("--post-prop"))?;
                let (key, value) = raw
                    .split_once('=')
                    .context("--post-prop expects key=value")?;
                post_props.push((key.to_string(), value.to_string()));
            }
            "--diameter" => {
                diameter = iter
                    .next()
                    .with_context(needs_value("--diameter"))?
                    .parse()?;
            }
            "--depth" => depth = iter.next().with_context(needs_value("--depth"))?.parse()?,
            "--step" => step = iter.next().with_context(needs_value("--step"))?.parse()?,
            "--inside" => tool_offset = ToolOffset::Inside,
            "--outside" => tool_offset = ToolOffset::Outside,
            "--on" => tool_offset = ToolOffset::On,
            "--overcut" => overcut = true,
            other if path.is_none() => path = Some(PathBuf::from(other)),
            other => bail!("{}", i18n::tp("cli.err.unexpected_arg", &[("arg", other)])),
        }
    }
    let path = path.context(i18n::t("cli.err.missing_input_path"))?;

    let import =
        ivac_core::input::import_path(&path, &ImportOptions::default()).with_context(|| {
            i18n::tp(
                "cli.err.importing",
                &[("path", &path.display().to_string())],
            )
        })?;

    let (offsets, stats) = build_offsets(&import, diameter, depth, step, tool_offset, overcut);

    let mut setup = Setup::default();
    setup.tool.diameter = diameter;
    setup.mill.depth = depth;
    setup.mill.step = step;
    setup.mill.offset = tool_offset;
    setup.mill.overcut = overcut;
    setup.machine.comments = true;

    #[cfg(not(feature = "cps"))]
    let _ = (&post_file, &post_id, &post_props);
    let (gcode, cps_toolpath) = match post_kind.as_str() {
        "linuxcnc" | "" => {
            let mut p = linuxcnc::Post::new();
            (emit_polylines(&setup, &offsets, &mut p), None)
        }
        "grbl" => {
            let mut p = grbl::Post::new();
            (emit_polylines(&setup, &offsets, &mut p), None)
        }
        "hpgl" => {
            let mut p = hpgl::Post::new();
            (emit_polylines(&setup, &offsets, &mut p), None)
        }
        #[cfg(feature = "cps")]
        "cps" => {
            let response = generate_cps(
                &import,
                diameter,
                depth,
                step,
                tool_offset,
                post_file.as_deref(),
                post_id.as_deref(),
                &post_props,
            )?;
            (response.gcode, Some(response.toolpath))
        }
        other => bail!("{}", i18n::tp("cli.err.unknown_post", &[("name", other)])),
    };

    let toolpath = cps_toolpath.unwrap_or_else(|| preview::interpret(&gcode));

    let body = GenerateResponseJson {
        gcode: &gcode,
        toolpath: &toolpath,
        stats,
    };
    serde_json::to_writer_pretty(std::io::stdout(), &body)?;
    println!();
    Ok(())
}

/// `--post cps`: build a one-op Profile project from the import and run
/// the REAL pipeline (recorder → JS post), so the CLI exercises the
/// same path the interactive transports use. The post comes from
/// `--post-id <bundled>` or `--post-file <path.cps>`; `--post-prop
/// key=value` (repeatable) sets property overrides (bool/number
/// auto-detected, everything else a string).
#[cfg(feature = "cps")]
#[allow(clippy::too_many_arguments)]
fn generate_cps(
    import: &ivac_core::input::ImportOutput,
    diameter: f64,
    depth: f64,
    step: f64,
    tool_offset: ToolOffset,
    post_file: Option<&std::path::Path>,
    post_id: Option<&str>,
    post_props: &[(String, String)],
) -> Result<ivac_core::pipeline::PipelineResponse> {
    use ivac_core::pipeline::{
        run_pipeline, CpsParamValue, CpsPostSelection, CpsPostSource, PipelineRequest,
        PostProcessorKind,
    };
    use ivac_core::project::{Op, OpKind, Project, ToolEntry};

    let source = match (post_file, post_id) {
        (Some(path), _) => CpsPostSource::Path {
            path: path.display().to_string(),
        },
        (None, Some(id)) => CpsPostSource::Bundled { id: id.to_string() },
        (None, None) => {
            bail!("--post cps needs --post-id <bundled id> or --post-file <file.cps>")
        }
    };
    let mut properties = std::collections::BTreeMap::new();
    for (key, raw) in post_props {
        let value = if raw == "true" || raw == "false" {
            CpsParamValue::Bool(raw == "true")
        } else if let Ok(number) = raw.parse::<f64>() {
            CpsParamValue::Number(number)
        } else {
            CpsParamValue::Text(raw.clone())
        };
        properties.insert(key.clone(), value);
    }

    let mut tool = ToolEntry::default();
    tool.id = 1;
    tool.diameter = diameter;
    tool.default_step = Some(step);
    let mut op = Op::default();
    op.params.depth = depth;
    if let OpKind::Profile { offset, .. } = &mut op.kind {
        *offset = tool_offset;
    }
    let project = Project {
        segments: import.segments.clone(),
        tools: vec![tool],
        operations: vec![op],
        ..Project::default()
    };
    run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Cps),
            cps_post: Some(CpsPostSelection { source, properties }),
        },
        |_, _, _| {},
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// `ivac posts` — table of the bundled `.cps` posts;
/// `ivac posts inspect <file.cps>` — `PostMeta` JSON for a script.
#[cfg(feature = "cps")]
fn cmd_posts(mut args: impl Iterator<Item = String>) -> Result<()> {
    match args.next().as_deref() {
        None => {
            println!("{:<12} {:<28} {:<8}", "ID", "DESCRIPTION", "EXT");
            for post in ivac_cps::library::BUNDLED {
                let meta = ivac_cps::inspect_post(post.source, &format!("{}.cps", post.id))
                    .map_err(|e| {
                        anyhow::anyhow!("bundled post {} failed inspection: {e}", post.id)
                    })?;
                println!(
                    "{:<12} {:<28} {:<8}",
                    post.id, meta.description, meta.extension
                );
            }
            Ok(())
        }
        Some("inspect") => {
            let path = args
                .next()
                .context("posts inspect needs a .cps file path")?;
            let script =
                std::fs::read_to_string(&path).with_context(|| format!("cannot read {path}"))?;
            let name = std::path::Path::new(&path)
                .file_name()
                .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            let meta =
                ivac_cps::inspect_post(&script, &name).map_err(|e| anyhow::anyhow!("{e}"))?;
            serde_json::to_writer_pretty(std::io::stdout(), &meta)?;
            println!();
            Ok(())
        }
        Some(other) => bail!("unknown posts subcommand: {other}"),
    }
}

#[cfg(not(feature = "cps"))]
fn cmd_posts(_args: impl Iterator<Item = String>) -> Result<()> {
    bail!("this build lacks CPS support")
}

/// Stream g-code from a full project JSON (a serialized `PipelineRequest` —
/// the same `/generate` request body the server + tauri accept) straight to
/// `--output FILE`, or to stdout when omitted. Runs the real CAM pipeline
/// through the write-through post, so peak memory is O(largest single op)
/// rather than O(whole program) (ivac-3j1p). The preview toolpath + time
/// estimate are skipped (they'd need a second pass over the program); a
/// short stats + warnings summary prints to stderr so stdout stays pure
/// g-code.
fn cmd_stream(args: impl Iterator<Item = String>) -> Result<()> {
    let mut path: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut two_sided = false;
    let mut iter = args.peekable();
    while let Some(arg) = iter.next() {
        let needs_value =
            |opt: &'static str| move || i18n::tp("cli.err.opt_needs_value", &[("opt", opt)]);
        match arg.as_str() {
            "--output" | "-o" => {
                output = Some(PathBuf::from(
                    iter.next().with_context(needs_value("--output"))?,
                ));
            }
            "--two-sided" => two_sided = true,
            other if path.is_none() => path = Some(PathBuf::from(other)),
            other => bail!("{}", i18n::tp("cli.err.unexpected_arg", &[("arg", other)])),
        }
    }
    let path = path.context(i18n::t("cli.err.missing_input_path"))?;

    // The input is a serialized PipelineRequest (project + optional post).
    let file = std::fs::File::open(&path)
        .with_context(|| i18n::tp("cli.err.reading", &[("path", &path.display().to_string())]))?;
    let request: ivac_core::pipeline::PipelineRequest =
        serde_json::from_reader(std::io::BufReader::new(file)).with_context(|| {
            i18n::tp(
                "cli.err.parse_project",
                &[("path", &path.display().to_string())],
            )
        })?;

    // Two-sided (flip-stock) jobs emit two programs, so they can't stream to a
    // single sink — they write `<base>.front.<ext>` / `<base>.back.<ext>` and
    // require an explicit --output base.
    if two_sided {
        return cmd_stream_two_sided(request, output);
    }

    // Destination: a file if --output was given, else stdout. Both buffered —
    // the streaming sink issues one write per emitted line.
    let (writer, dest): (Box<dyn std::io::Write + Send>, String) = match &output {
        Some(out) => {
            let f = std::fs::File::create(out).with_context(|| {
                i18n::tp("cli.err.output", &[("path", &out.display().to_string())])
            })?;
            (
                Box::new(std::io::BufWriter::new(f)),
                out.display().to_string(),
            )
        }
        None => (
            Box::new(std::io::BufWriter::new(std::io::stdout())),
            "stdout".to_string(),
        ),
    };

    let outcome = ivac_core::pipeline::stream_gcode_to_writer(request, writer).map_err(|e| {
        anyhow::anyhow!(i18n::tp("cli.err.streaming", &[("detail", &e.to_string())]))
    })?;

    // Summary + warnings to stderr — stdout stays pure g-code.
    eprintln!(
        "{}",
        i18n::tp(
            "cli.stream.done",
            &[
                ("dest", &dest),
                ("objects", &outcome.stats.object_count.to_string()),
                ("offsets", &outcome.stats.offset_count.to_string()),
            ],
        )
    );
    for w in &outcome.warnings {
        eprintln!("  ! {}", w.message);
    }
    Ok(())
}

/// Two-sided (flip-stock) emission: write `<base>.front.<ext>` and, for a
/// genuine two-sided job, `<base>.back.<ext>`. Buffered (not streamed) because
/// two programs can't share one sink; two-sided jobs are small relative to the
/// unbounded-raster case the streaming path targets.
fn cmd_stream_two_sided(
    request: ivac_core::pipeline::PipelineRequest,
    output: Option<PathBuf>,
) -> Result<()> {
    let base = output.context(i18n::t("cli.err.two_sided_needs_output"))?;
    let two =
        ivac_core::pipeline::run_pipeline_two_sided(request, |_p, _f, _m| {}).map_err(|e| {
            anyhow::anyhow!(i18n::tp("cli.err.streaming", &[("detail", &e.to_string())]))
        })?;

    write_program(&side_path(&base, "front"), &two.front)?;
    if let Some(back) = &two.back {
        write_program(&side_path(&base, "back"), back)?;
    }
    Ok(())
}

/// Insert `.<side>` before the extension of `base` (`out.gcode` + `front` →
/// `out.front.gcode`; an extensionless `out` → `out.front`).
fn side_path(base: &std::path::Path, side: &str) -> PathBuf {
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match base.extension() {
        Some(ext) => format!("{stem}.{side}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{side}"),
    };
    base.with_file_name(name)
}

/// Write one program's gcode to `path` and print the standard done summary.
fn write_program(
    path: &std::path::Path,
    resp: &ivac_core::pipeline::PipelineResponse,
) -> Result<()> {
    std::fs::write(path, &resp.gcode)
        .with_context(|| i18n::tp("cli.err.output", &[("path", &path.display().to_string())]))?;
    eprintln!(
        "{}",
        i18n::tp(
            "cli.stream.done",
            &[
                ("dest", &path.display().to_string()),
                ("objects", &resp.stats.object_count.to_string()),
                ("offsets", &resp.stats.offset_count.to_string()),
            ],
        )
    );
    for w in &resp.warnings {
        eprintln!("  ! {}", w.message);
    }
    Ok(())
}

/// Build per-object offsets from imported segments.
fn build_offsets(
    import: &ImportOutput,
    diameter: f64,
    _depth: f64,
    _step: f64,
    tool_offset: ToolOffset,
    overcut: bool,
) -> (Vec<PolylineOffset>, GenerateStats) {
    let mut objects = segments_to_objects(&import.segments);
    classify_containment(&mut objects);
    for obj in &mut objects {
        obj.tool_offset = tool_offset;
    }
    let radius = diameter * 0.5;
    let mut offsets = Vec::new();
    let mut closed = 0usize;
    for (idx, obj) in objects.iter().enumerate() {
        if obj.closed {
            closed += 1;
        }
        let pocket = obj.setup.pockets.active && obj.closed;
        if pocket {
            for mut o in pocket_for_object(
                obj,
                radius,
                false,
                6,
                PocketEmit::Cascade,
                &[],
                radius,
                0.0,
                None,
                ivac_core::project::tool::SpindleDirection::Cw,
            ) {
                o.source_object_idx = idx;
                offsets.push(o);
            }
            continue;
        }
        // Otherwise emit a single contour pass — the Rust core derives
        // direction from the user's tool_offset choice once the chain is
        // CCW-oriented (the importer ensures this for closed contours via
        // dxf-rs's CCW convention for ARC / CIRCLE).
        let delta = match tool_offset {
            ToolOffset::None | ToolOffset::On => 0.0,
            ToolOffset::Outside => -radius,
            ToolOffset::Inside => radius,
        };
        if delta.abs() < 1e-9 {
            offsets.push(PolylineOffset {
                segments: obj.segments.clone(),
                closed: obj.closed,
                level: 0,
                is_pocket: 0,
                layer: obj.layer.clone(),
                color: obj.color,
                source_object_idx: idx,
                tabs: Vec::new(),
                is_finish: false,
            });
        } else {
            for mut o in ivac_core::cam::offsets::parallel_offset_object(obj, delta) {
                o.source_object_idx = idx;
                offsets.push(o);
            }
        }
    }
    if overcut {
        apply_overcut_to_offsets(&mut offsets, &objects, radius);
    }

    let stats = GenerateStats {
        object_count: objects.len(),
        closed_object_count: closed,
        offset_count: offsets.len(),
        ..Default::default()
    };
    let _ = VcObject::new(Vec::new(), false); // exercise the constructor
    (offsets, stats)
}
