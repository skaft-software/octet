use sexy_tui_rs::rich_text::latex::{render_latex, RenderLatexOptions};

#[test]
fn debug() {
    let parts = [
        r"\det",
        r"\det\!",
        r"\det\!\left(",
        r"\det\!\left(\frac{a}{b}\right)",
        r"\det\!\left(\frac{\partial(F_1,F_2,F_3)}{\partial(x,y,z)}\right)",
        r"\det\!\left(\frac{\partial(F_1,F_2,F_3)}{\partial(x,y,z)}\right)=-2.",
        r"\left(\frac{a}{b}\right)",
        r"\frac{\partial(A)}{\partial(B)}",
        r"\partial(F_1,F_2,F_3)",
        r"\partial(x,y,z)",
        r"\!",
        r"a\!b",
    ];
    for part in parts {
        println!("{part:?} => {:?}", render_latex(part, RenderLatexOptions::default()));
    }
}
