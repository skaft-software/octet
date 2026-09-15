use sexy_tui_rs::rich_text::latex::{render_latex, RenderLatexOptions};

#[test]
fn deep_braces() {
    let depth = std::env::var("DEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let source = "{".repeat(depth);
    println!("braces depth={depth} => {:?}", render_latex(&source, RenderLatexOptions::default()).map(|v| v.len()));
}

#[test]
fn deep_fractions() {
    let depth = std::env::var("DEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let mut source = String::new();
    for _ in 0..depth {
        source.push_str(r"\frac{");
    }
    source.push('1');
    for _ in 0..depth {
        source.push_str("}{2}");
    }
    println!("frac depth={depth} => {:?}", render_latex(&source, RenderLatexOptions::default()).map(|v| v.len()));
}

#[test]
fn deep_environments() {
    let depth = std::env::var("DEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(2_000);
    let source = r"\begin{cases}".repeat(depth);
    let start = std::time::Instant::now();
    let rendered = render_latex(&source, RenderLatexOptions::default());
    println!("env depth={depth} in {:?} => {:?}", start.elapsed(), rendered.map(|v| v.len()));
}

#[test]
fn wide_input_is_linearish() {
    let source = "x+y ".repeat(4_000);
    let start = std::time::Instant::now();
    let rendered = render_latex(&source, RenderLatexOptions::default());
    println!("wide {} bytes in {:?} => {:?}", source.len(), start.elapsed(), rendered.map(|v| v.len()));
}

#[test]
fn repeated_fractions_layout_scale() {
    let source = r"\frac{1}{2}".repeat(2_000);
    let start = std::time::Instant::now();
    let rendered = render_latex(&source, RenderLatexOptions { display: true });
    println!("2000 fractions in {:?} => {:?}", start.elapsed(), rendered.map(|v| v.len()));
}
