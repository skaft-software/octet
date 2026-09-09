//! Credential-free generic Markdown API benchmark (not octet shell/frame latency).
//! Build: `cargo build --profile profiling --locked -p sexy-tui-rs --example render_bench --features benchmarks`
//! Run: `target/profiling/examples/render_bench --help`
//! JSON goes to stdout; no fixture text, terminal writes, provider, or network is used.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sexy_tui_rs::{
    parse_markdown, CodeOverflow, ColorDepth, Document, RenderOptions, RenderedDocument,
    RichRenderer, StreamingLineUpdate, StreamingMarkdown, StreamingRenderCache, StreamingStats,
    TerminalCapabilities, Theme,
};

struct CountingAllocator;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static REQUESTED_BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

fn record_allocation(size: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    REQUESTED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, Default)]
struct Measurement {
    calls: u64,
    elapsed_ns: u64,
    allocation_calls: u64,
    allocation_requested_bytes: u64,
}

impl Measurement {
    fn add(&mut self, other: Self) {
        self.calls += other.calls;
        self.elapsed_ns += other.elapsed_ns;
        self.allocation_calls += other.allocation_calls;
        self.allocation_requested_bytes += other.allocation_requested_bytes;
    }

    fn json(self) -> String {
        format!(
            "{{\"calls\":{},\"elapsed_ns\":{},\"allocation_calls\":{},\"allocation_requested_bytes\":{}}}",
            self.calls, self.elapsed_ns, self.allocation_calls, self.allocation_requested_bytes
        )
    }
}

// API return values are consumed/dropped after measurement. Counter snapshots,
// fixture construction, renderer setup, correctness checks and JSON are excluded.
fn measure<T>(action: impl FnOnce() -> T) -> (T, Measurement) {
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = REQUESTED_BYTES.load(Ordering::Relaxed);
    let start = Instant::now();
    let result = black_box(action());
    let elapsed_ns = start.elapsed().as_nanos() as u64;
    let metric = Measurement {
        calls: 1,
        elapsed_ns,
        allocation_calls: ALLOCATIONS.load(Ordering::Relaxed) - allocations,
        allocation_requested_bytes: REQUESTED_BYTES.load(Ordering::Relaxed) - bytes,
    };
    (result, metric)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Static,
    Document,
    Lines,
    Tail,
}

impl Mode {
    const ALL: [Self; 4] = [Self::Static, Self::Document, Self::Lines, Self::Tail];

    fn name(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Document => "document",
            Self::Lines => "lines",
            Self::Tail => "tail",
        }
    }
}

const WORKLOADS: [&str; 6] = [
    "prose",
    "paragraph",
    "small-blocks",
    "open-code",
    "unicode",
    "table",
];
const PHASES: [&str; 5] = ["ingest", "render", "finalize", "final_render", "total"];

#[derive(Debug)]
struct Options {
    bytes: usize,
    chunk_bytes: usize,
    width: u16,
    warmup: usize,
    repetitions: usize,
    workload: String,
    mode: String,
    truecolor: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bytes: 128 * 1024,
            chunk_bytes: 1024,
            width: 80,
            warmup: 1,
            repetitions: 5,
            workload: "all".into(),
            mode: "all".into(),
            truecolor: false,
        }
    }
}

const HELP: &str = "Usage: render_bench [OPTIONS]
  --workload all|prose|paragraph|small-blocks|open-code|unicode|table
  --mode all|static|document|lines|tail
  --bytes N          Minimum fixture bytes, rounded up to whole units (default 131072)
  --chunk-bytes N    Arbitrary byte chunk size; may split UTF-8 (default 1024)
  --width N          Fixed render width, 8..65535 (default 80)
  --warmup N         Discarded fresh-state trials per case, 0..100 (default 1)
  --repetitions N    Measured fresh-state trials per case, 1..100 (default 5)
  --capabilities plain|truecolor (default plain; plain disables syntax)
  --help

Outputs octet.render-bench.v1 JSON with raw trials and interpolated p50/p95.
Independent trials create new renderer, stream and layout caches in one process.
An untimed exhaustive replay checks every live update before measuring each case.
Final raw bytes, static semantics, semantic copy and full output are checked on
all warmup/measured trials. No content is truncated by the harness; code wraps.

Scope: generic Markdown parser/layout API calls, NOT octet shell latency, frame
composition, terminal I/O, provider timing, RSS, live heap, or process startup.
Ingest/render are sums of per-chunk calls, not chunk-latency percentiles. Finalize
is finish(); final_render is one post-finish API call. Static ingest is one full
parse; static render is one full layout (finalize/final_render are zero).
Total sums these four phases, excluding setup, output consumption, verification
and JSON. Clock calls and the counting allocator perturb timings; no overhead is
subtracted. Allocations count alloc/alloc_zeroed/realloc and the entire requested
size (not realloc growth); frees are not subtracted. Use a quiet machine, retain
this source and binary identity, and compare identical options/build profiles.
Whole-process profiler samples also include setup, warmups and the untimed
all-API correctness replay; they are not isolated selected-mode measurements.";

fn options(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut result = Options::default();
    let mut args = args;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        let number = || {
            value
                .parse::<usize>()
                .map_err(|_| format!("invalid integer for {flag}"))
        };
        match flag.as_str() {
            "--bytes" => result.bytes = number()?,
            "--chunk-bytes" => result.chunk_bytes = number()?,
            "--width" => {
                result.width = u16::try_from(number()?).map_err(|_| "width exceeds 65535")?
            }
            "--warmup" => result.warmup = number()?,
            "--repetitions" => result.repetitions = number()?,
            "--workload" => result.workload = value,
            "--mode" => result.mode = value,
            "--capabilities" => match value.as_str() {
                "plain" => result.truecolor = false,
                "truecolor" => result.truecolor = true,
                _ => return Err("capabilities must be plain or truecolor".into()),
            },
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    if !(1..=16 * 1024 * 1024).contains(&result.bytes)
        || !(1..=16 * 1024 * 1024).contains(&result.chunk_bytes)
        || result.width < 8
        || result.warmup > 100
        || !(1..=100).contains(&result.repetitions)
    {
        return Err(
            "bytes/chunk-bytes must be 1..16777216, width >=8, warmup <=100, repetitions 1..100"
                .into(),
        );
    }
    if result.workload != "all" && !WORKLOADS.contains(&result.workload.as_str()) {
        return Err("unknown workload".into());
    }
    if result.mode != "all" && !Mode::ALL.iter().any(|mode| mode.name() == result.mode) {
        return Err("unknown mode".into());
    }
    Ok(result)
}

struct Fixture {
    source: String,
    units: usize,
    unit_bytes: usize,
    prefix_bytes: usize,
}

// Fixture revision is independent of the result schema. Never silently change
// these bodies in a before/after comparison. Whole units avoid truncating syntax.
fn fixture(workload: &str, minimum_bytes: usize) -> Fixture {
    let (prefix, unit) = match workload {
        "prose" => ("", "## Recovery step\n\nThe invalid **final record** is removed before the next append. Preserve the earlier records and verify the new tail with `scan(bytes)`.\n\n- preserve records\n- recover the partial tail\n\n"),
        "paragraph" => ("", "A newline-free paragraph keeps every word while streaming through a growing mutable tail. "),
        "small-blocks" => ("", "a\n\nb\n\n## c\n\nd\n\n"),
        "open-code" => ("```rust\n", "let recovered = scan(bytes); // keep this source line intact through finalization\n"),
        "unicode" => ("", "Unicode café e\u{301} 中文 日本語 👩\u{200d}💻 🇮🇳 👍🏽 stays complete across arbitrary byte chunks.\n\n"),
        "table" => ("| state | action |\n| --- | --- |\n", "| valid | preserve |\n| partial | recover |\n"),
        _ => unreachable!("validated workload"),
    };
    let units = minimum_bytes
        .saturating_sub(prefix.len())
        .div_ceil(unit.len())
        .max(1);
    Fixture {
        source: format!("{prefix}{}", unit.repeat(units)),
        units,
        unit_bytes: unit.len(),
        prefix_bytes: prefix.len(),
    }
}

// A reproducible fixture identifier, not a security digest. A process wrapper
// can retain SHA-256 of the executable/source without another Rust dependency.
fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn renderer(options: &Options) -> RichRenderer {
    let capabilities = if options.truecolor {
        TerminalCapabilities::interactive(ColorDepth::TrueColor, true)
    } else {
        TerminalCapabilities::plain()
    };
    RichRenderer::new(
        Theme::with_capabilities(capabilities),
        capabilities,
        RenderOptions {
            code_overflow: CodeOverflow::Wrap,
            ..RenderOptions::default()
        },
    )
}

enum Output {
    Document(RenderedDocument),
    Lines(Vec<String>),
    Tail(StreamingLineUpdate),
}

fn render(
    stream: &StreamingMarkdown,
    cache: &mut StreamingRenderCache,
    renderer: &RichRenderer,
    options: &Options,
    mode: Mode,
) -> Output {
    match mode {
        Mode::Document => Output::Document(cache.render(stream, renderer, options.width)),
        Mode::Lines => Output::Lines(cache.render_lines(stream, renderer, options.width, true)),
        Mode::Tail => Output::Tail(cache.render_line_update(stream, renderer, options.width, true)),
        Mode::Static => unreachable!("static has no streaming cache"),
    }
}

fn apply(update: StreamingLineUpdate, rows: &mut Vec<String>) {
    assert!(
        update.stable_prefix <= rows.len(),
        "tail update prefix exceeds prior output"
    );
    rows.truncate(update.stable_prefix);
    rows.extend(update.replacement);
}

// Fail tersely: assert_eq! on a huge fixture would leak megabytes into logs.
fn check_final(
    stream: &StreamingMarkdown,
    source: &str,
    expected: &Document,
    expected_render: &RenderedDocument,
    renderer: &RichRenderer,
) {
    assert!(stream.is_finished(), "stream did not finalize");
    assert!(stream.raw_bytes() == source.as_bytes(), "raw bytes changed");
    assert!(
        stream.raw_text() == source,
        "UTF-8 decoding changed fixture"
    );
    assert!(
        stream.committed() == expected,
        "final semantics differ from static parsing"
    );
    assert!(
        renderer.sanitize_copy(&stream.copy_text()) == expected_render.copy_text,
        "final semantic copy differs from static rendering"
    );
    assert!(
        stream.stats().pending_utf8_bytes == 0,
        "incomplete final UTF-8"
    );
}

fn check_output(output: Output, expected: &RenderedDocument) {
    match output {
        Output::Document(document) => {
            assert!(document == *expected, "final document output differs")
        }
        Output::Lines(lines) => assert!(
            lines == expected.styled_lines(),
            "final full-lines output differs"
        ),
        Output::Tail(update) => {
            // finish() invalidates the complete layout, so its update must be
            // authoritative rather than hiding missing prior content.
            assert!(
                update.stable_prefix == 0,
                "final tail update is not authoritative"
            );
            assert!(
                update.replacement == expected.styled_lines(),
                "final tail output differs"
            );
        }
    }
}

// Separate untimed replay: validate every live delta exactly against the legacy
// full-document API, including styled rows, without adding O(history) observer
// work to the measured tail path. Final output is independently static-checked.
fn verify_replay(
    source: &str,
    options: &Options,
    expected: &Document,
    expected_render: &RenderedDocument,
) {
    let renderer = renderer(options);
    let mut stream = StreamingMarkdown::new();
    let mut document_cache = StreamingRenderCache::default();
    let mut lines_cache = StreamingRenderCache::default();
    let mut tail_cache = StreamingRenderCache::default();
    let mut rows = Vec::new();
    for chunk in source.as_bytes().chunks(options.chunk_bytes) {
        stream.push_bytes(chunk);
        let document = document_cache.render(&stream, &renderer, options.width);
        let lines = lines_cache.render_lines(&stream, &renderer, options.width, true);
        apply(
            tail_cache.render_line_update(&stream, &renderer, options.width, true),
            &mut rows,
        );
        assert!(
            lines == document.styled_lines(),
            "live full-lines output differs from document API"
        );
        assert!(
            rows == lines,
            "live tail replay differs from full-lines API"
        );
    }
    stream.finish();
    check_final(&stream, source, expected, expected_render, &renderer);
    check_output(
        Output::Document(document_cache.render(&stream, &renderer, options.width)),
        expected_render,
    );
    check_output(
        Output::Lines(lines_cache.render_lines(&stream, &renderer, options.width, true)),
        expected_render,
    );
    apply(
        tail_cache.render_line_update(&stream, &renderer, options.width, true),
        &mut rows,
    );
    assert!(
        rows == expected_render.styled_lines(),
        "final replay differs from static output"
    );
}

struct Trial {
    phases: [Measurement; 5],
    active_stats: Option<StreamingStats>,
    final_stats: Option<StreamingStats>,
}

fn trial(
    source: &str,
    options: &Options,
    mode: Mode,
    expected: &Document,
    expected_render: &RenderedDocument,
) -> Trial {
    let renderer = renderer(options);
    let mut result = Trial {
        phases: [Measurement::default(); 5],
        active_stats: None,
        final_stats: None,
    };
    if mode == Mode::Static {
        let (document, metric) = measure(|| parse_markdown(black_box(source)));
        result.phases[0] = metric;
        let (output, metric) = measure(|| renderer.render(black_box(&document), options.width));
        result.phases[1] = metric;
        assert!(document == *expected, "static trial semantics differ");
        assert!(output == *expected_render, "static trial output differs");
    } else {
        let mut stream = StreamingMarkdown::new();
        let mut cache = StreamingRenderCache::default();
        for chunk in source.as_bytes().chunks(options.chunk_bytes) {
            let (_, metric) = measure(|| stream.push_bytes(black_box(chunk)));
            result.phases[0].add(metric);
            let (output, metric) =
                measure(|| render(&stream, &mut cache, &renderer, options, mode));
            result.phases[1].add(metric);
            drop(black_box(output));
        }
        result.active_stats = Some(stream.stats());
        let (_, metric) = measure(|| {
            stream.finish();
        });
        result.phases[2] = metric;
        let (output, metric) = measure(|| render(&stream, &mut cache, &renderer, options, mode));
        result.phases[3] = metric;
        result.final_stats = Some(stream.stats());
        check_final(&stream, source, expected, expected_render, &renderer);
        check_output(output, expected_render);
    }
    for index in 0..4 {
        let metric = result.phases[index];
        result.phases[4].add(metric);
    }
    result
}

// Linear interpolation at (n - 1) * q, over independent full-trial totals.
fn percentile(values: impl Iterator<Item = u64>, fraction: f64) -> f64 {
    let mut values: Vec<_> = values.collect();
    values.sort_unstable();
    let rank = (values.len() - 1) as f64 * fraction;
    let low = values[rank.floor() as usize] as f64;
    let high = values[rank.ceil() as usize] as f64;
    low + (high - low) * rank.fract()
}

fn stats_json(stats: Option<StreamingStats>) -> String {
    stats.map_or_else(|| "null".into(), |stats| format!(
        "{{\"parse_passes\":{},\"reparsed_bytes\":{},\"committed_blocks\":{},\"pending_utf8_bytes\":{}}}",
        stats.parse_passes, stats.reparsed_bytes, stats.committed_blocks, stats.pending_utf8_bytes
    ))
}

fn case_json(
    workload: &str,
    fixture: &Fixture,
    mode: Mode,
    options: &Options,
    expected: &Document,
    rendered: &RenderedDocument,
    trials: &[Trial],
) -> String {
    let source = &fixture.source;
    let mut json = format!(
        "{{\"workload\":\"{workload}\",\"fixture_revision\":1,\"mode\":\"{}\",\"minimum_bytes\":{},\"source_bytes\":{},\"source_fnv1a64\":\"{:016x}\",\"source_newlines\":{},\"fixture_units\":{},\"unit_bytes\":{},\"prefix_bytes\":{},\"chunk_bytes\":{},\"chunk_count\":{},\"last_chunk_bytes\":{},\"width\":{},\"final_blocks\":{},\"final_rows\":{},\"final_copy_bytes\":{},\"final_copy_fnv1a64\":\"{:016x}\",\"final_output_fnv1a64\":\"{:016x}\",\"correctness\":{{\"live_exact_replay\":true,\"final_raw_exact\":true,\"final_semantics_exact\":true,\"final_copy_exact\":true,\"final_output_exact\":true}},\"trials\":[",
        mode.name(), options.bytes, source.len(), fingerprint(source.as_bytes()), source.bytes().filter(|byte| *byte == b'\n').count(), fixture.units, fixture.unit_bytes, fixture.prefix_bytes,
        options.chunk_bytes, source.len().div_ceil(options.chunk_bytes), (source.len() - 1) % options.chunk_bytes + 1, options.width, expected.blocks.len(), rendered.lines.len(), rendered.copy_text.len(), fingerprint(rendered.copy_text.as_bytes()), fingerprint(rendered.styled_text().as_bytes())
    );
    for (index, trial) in trials.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        write!(json, "{{\"trial\":{index},\"phases\":{{").unwrap();
        for (phase, name) in PHASES.iter().enumerate() {
            if phase > 0 {
                json.push(',');
            }
            write!(json, "\"{name}\":{}", trial.phases[phase].json()).unwrap();
        }
        write!(
            json,
            "}},\"active_stream_stats\":{},\"final_stream_stats\":{},\"correctness_passed\":true}}",
            stats_json(trial.active_stats),
            stats_json(trial.final_stats)
        )
        .unwrap();
    }
    json.push_str("],\"summary\":{");
    for (phase, name) in PHASES.iter().enumerate() {
        if phase > 0 {
            json.push(',');
        }
        write!(json, "\"{name}\":{{").unwrap();
        for (index, metric) in [
            "elapsed_ns",
            "allocation_calls",
            "allocation_requested_bytes",
        ]
        .iter()
        .enumerate()
        {
            if index > 0 {
                json.push(',');
            }
            let values = || {
                trials.iter().map(|trial| match index {
                    0 => trial.phases[phase].elapsed_ns,
                    1 => trial.phases[phase].allocation_calls,
                    _ => trial.phases[phase].allocation_requested_bytes,
                })
            };
            write!(
                json,
                "\"{metric}\":{{\"count\":{},\"p50\":{:.3},\"p95\":{:.3}}}",
                trials.len(),
                percentile(values(), 0.5),
                percentile(values(), 0.95)
            )
            .unwrap();
        }
        json.push('}');
    }
    json.push_str("}}");
    json
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{HELP}");
        return;
    }
    let options = options(args.into_iter()).unwrap_or_else(|error| {
        eprintln!("render_bench: {error}; use --help");
        std::process::exit(2);
    });
    let reference_renderer = renderer(&options);
    let mut cases = Vec::new();
    for workload in WORKLOADS {
        if options.workload != "all" && options.workload != workload {
            continue;
        }
        let fixture = fixture(workload, options.bytes);
        let expected = parse_markdown(&fixture.source);
        let rendered = reference_renderer.render(&expected, options.width);
        verify_replay(&fixture.source, &options, &expected, &rendered);
        for mode in Mode::ALL {
            if options.mode != "all" && options.mode != mode.name() {
                continue;
            }
            for _ in 0..options.warmup {
                black_box(trial(&fixture.source, &options, mode, &expected, &rendered));
            }
            let trials: Vec<_> = (0..options.repetitions)
                .map(|_| trial(&fixture.source, &options, mode, &expected, &rendered))
                .collect();
            cases.push(case_json(
                workload, &fixture, mode, &options, &expected, &rendered, &trials,
            ));
        }
    }
    println!(
        "{{\"schema\":\"octet.render-bench.v1\",\"scope\":\"generic Markdown parser/layout APIs only; not octet shell latency, frame composition, terminal I/O, provider, or process startup\",\"allocation_scope\":\"process-global alloc/alloc_zeroed/realloc requested sizes during API calls; includes full realloc size; no subtraction for frees; NOT RSS or live/peak heap\",\"timing_scope\":\"sum of per-call elapsed nanoseconds; API return consumption/drop, setup, verification and JSON excluded; clock/counting-allocator overhead not subtracted\",\"external_profiler_scope\":\"whole-process samples also include setup, warmups, and untimed all-API verification; not isolated selected-mode samples\",\"phase_contract\":{{\"ingest\":\"stream push_bytes per chunk, or static full parse\",\"render\":\"one selected API call per chunk, or one static full layout\",\"finalize\":\"stream finish only; zero for static\",\"final_render\":\"one selected API call after finish; zero for static\",\"total\":\"sum of the four phases, not end-to-end wall time\"}},\"trial_isolation\":\"fresh renderer, stream and layout cache per trial in one process; allocator/OS caches may remain warm\",\"percentile_method\":\"linear interpolation at (n-1)*q over independent full-trial totals, not chunks\",\"verification\":\"untimed exhaustive live replay once per workload; exact final checks every warmup and measured trial; reference is the same revision static parser/renderer, not an external CommonMark oracle\",\"warmup\":{},\"repetitions\":{},\"capabilities\":\"{}\",\"syntax_highlighting_compiled\":{},\"code_overflow\":\"wrap\",\"debug_assertions\":{},\"target_os\":\"{}\",\"target_arch\":\"{}\",\"cases\":[{}]}}",
        options.warmup, options.repetitions, if options.truecolor { "truecolor" } else { "plain" }, cfg!(feature = "syntax-highlighting"), cfg!(debug_assertions), std::env::consts::OS, std::env::consts::ARCH, cases.join(",")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_independent_trial_totals() {
        assert_eq!(percentile([40, 10, 30, 20].into_iter(), 0.5), 25.0);
        assert_eq!(percentile([40, 10, 30, 20].into_iter(), 0.95), 38.5);
        assert_eq!(percentile([7].into_iter(), 0.95), 7.0);
    }

    #[test]
    fn fixture_units_are_complete_deterministic_and_adversarial() {
        for name in WORKLOADS {
            let fixture = fixture(name, 128 * 1024);
            assert!(fixture.source.len() >= 128 * 1024);
            assert_eq!(
                fixture.source.len(),
                fixture.prefix_bytes + fixture.units * fixture.unit_bytes
            );
            assert!(fixture
                .source
                .ends_with(if name == "paragraph" { " " } else { "\n" }));
        }
        assert!(!fixture("paragraph", 128 * 1024).source.contains('\n'));
        let code = fixture("open-code", 128 * 1024).source;
        assert_eq!(code.matches("```").count(), 1);
        assert_eq!(fingerprint(b"hello"), 0xa430d84680aabd0b);
    }

    #[test]
    fn arguments_reject_invalid_boundaries_and_names() {
        for args in [
            vec!["--bytes", "0"],
            vec!["--width", "7"],
            vec!["--width", "65536"],
            vec!["--repetitions", "0"],
            vec!["--mode", "missing"],
            vec!["--workload", "missing"],
            vec!["--chunk-bytes"],
        ] {
            assert!(options(args.into_iter().map(str::to_owned)).is_err());
        }
        assert!(options(["--warmup", "0"].into_iter().map(str::to_owned)).is_ok());
    }

    #[test]
    fn all_workloads_preserve_live_and_final_output_at_split_utf8_boundaries() {
        for width in [20, 80] {
            for chunk_bytes in [1, 7, 256] {
                let options = Options {
                    bytes: 256,
                    width,
                    chunk_bytes,
                    ..Options::default()
                };
                let renderer = renderer(&options);
                for workload in WORKLOADS {
                    let source = fixture(workload, options.bytes).source;
                    let expected = parse_markdown(&source);
                    let rendered = renderer.render(&expected, width);
                    verify_replay(&source, &options, &expected, &rendered);
                    for mode in Mode::ALL {
                        let result = trial(&source, &options, mode, &expected, &rendered);
                        let chunks = source.len().div_ceil(chunk_bytes) as u64;
                        assert_eq!(
                            result.phases[0].calls,
                            if mode == Mode::Static { 1 } else { chunks }
                        );
                        assert_eq!(result.phases[2].calls, u64::from(mode != Mode::Static));
                        assert_eq!(
                            result.phases[4].allocation_requested_bytes,
                            result.phases[..4]
                                .iter()
                                .map(|phase| phase.allocation_requested_bytes)
                                .sum()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn styled_replay_is_exact() {
        let options = Options {
            bytes: 128,
            chunk_bytes: 7,
            truecolor: true,
            ..Options::default()
        };
        let renderer = renderer(&options);
        for workload in WORKLOADS {
            let source = fixture(workload, options.bytes).source;
            let expected = parse_markdown(&source);
            let rendered = renderer.render(&expected, options.width);
            verify_replay(&source, &options, &expected, &rendered);
        }
    }
}
