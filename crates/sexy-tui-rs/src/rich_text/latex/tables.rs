//! Symbol tables ported mechanically from the upstream reference
//! `packages/tui/src/latex.ts` (pi @ 8a7b0c03dfb702663acafb6dc29f8acaa4ffe391).
//!
//! The tables are data only; every parser and layout rule lives in [`super`].
//! Card `2c.3` of `docs/parity/editor.md`; do not hand-edit entries without
//! re-checking the upstream file.



pub(crate) fn symbol(value: &str) -> Option<&'static str> {
    Some(match value {
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ϵ",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "varkappa" => "ϰ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "varpi" => "ϖ",
        "rho" => "ρ",
        "varrho" => "ϱ",
        "sigma" => "σ",
        "varsigma" => "ς",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "ϕ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        "pm" => "±",
        "mp" => "∓",
        "times" => "×",
        "div" => "÷",
        "cdot" => "·",
        "ast" => "∗",
        "star" => "⋆",
        "circ" => "∘",
        "bullet" => "•",
        "oplus" => "⊕",
        "ominus" => "⊖",
        "otimes" => "⊗",
        "oslash" => "⊘",
        "odot" => "⊙",
        "bigcirc" => "○",
        "dagger" => "†",
        "ddagger" => "‡",
        "amalg" => "⨿",
        "uplus" => "⊎",
        "sqcap" => "⊓",
        "sqcup" => "⊔",
        "bowtie" => "⋈",
        "Join" => "⋈",
        "ltimes" => "⋉",
        "rtimes" => "⋊",
        "leftouterjoin" => "⟕",
        "rightouterjoin" => "⟖",
        "fullouterjoin" => "⟗",
        "triangleleft" => "◁",
        "triangleright" => "▷",
        "wr" => "≀",
        "cap" => "∩",
        "cup" => "∪",
        "bigcap" => "⋂",
        "bigcup" => "⋃",
        "bigwedge" => "⋀",
        "bigvee" => "⋁",
        "bigsqcup" => "⨆",
        "biguplus" => "⨄",
        "bigoplus" => "⨁",
        "bigotimes" => "⨂",
        "bigodot" => "⨀",
        "setminus" => "∖",
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "subset" => "⊂",
        "supset" => "⊃",
        "subseteq" => "⊆",
        "supseteq" => "⊇",
        "sqsubset" => "⊏",
        "sqsupset" => "⊐",
        "sqsubseteq" => "⊑",
        "sqsupseteq" => "⊒",
        "prec" => "≺",
        "preceq" => "≼",
        "succ" => "≻",
        "succeq" => "≽",
        "ll" => "≪",
        "gg" => "≫",
        "le" => "≤",
        "leq" => "≤",
        "leqslant" => "≤",
        "ge" => "≥",
        "geq" => "≥",
        "geqslant" => "≥",
        "ne" => "≠",
        "neq" => "≠",
        "equiv" => "≡",
        "approx" => "≈",
        "sim" => "∼",
        "simeq" => "≃",
        "cong" => "≅",
        "asymp" => "≍",
        "doteq" => "≐",
        "propto" => "∝",
        "parallel" => "∥",
        "perp" => "⊥",
        "mid" => "∣",
        "vdash" => "⊢",
        "dashv" => "⊣",
        "models" => "⊨",
        "Vdash" => "⊩",
        "Vvdash" => "⊪",
        "nvdash" => "⊬",
        "nvDash" => "⊭",
        "forall" => "∀",
        "exists" => "∃",
        "nexists" => "∄",
        "neg" => "¬",
        "land" => "∧",
        "wedge" => "∧",
        "lor" => "∨",
        "vee" => "∨",
        "to" => "→",
        "rightarrow" => "→",
        "longrightarrow" => "→",
        "leftarrow" => "←",
        "longleftarrow" => "←",
        "gets" => "←",
        "leftrightarrow" => "↔",
        "longleftrightarrow" => "↔",
        "hookleftarrow" => "↩",
        "hookrightarrow" => "↪",
        "twoheadleftarrow" => "↞",
        "twoheadrightarrow" => "↠",
        "leftharpoonup" => "↼",
        "leftharpoondown" => "↽",
        "rightharpoonup" => "⇀",
        "rightharpoondown" => "⇁",
        "rightleftharpoons" => "⇌",
        "leftrightharpoons" => "⇋",
        "nearrow" => "↗",
        "searrow" => "↘",
        "swarrow" => "↙",
        "nwarrow" => "↖",
        "rightsquigarrow" => "⇝",
        "leadsto" => "⇝",
        "Rightarrow" => "⇒",
        "Longrightarrow" => "⇒",
        "Leftarrow" => "⇐",
        "Longleftarrow" => "⇐",
        "Leftrightarrow" => "⇔",
        "Longleftrightarrow" => "⇔",
        "implies" => "⇒",
        "iff" => "⇔",
        "mapsto" => "↦",
        "longmapsto" => "↦",
        "uparrow" => "↑",
        "downarrow" => "↓",
        "partial" => "∂",
        "nabla" => "∇",
        "int" => "∫",
        "iint" => "∬",
        "iiint" => "∭",
        "oint" => "∮",
        "sum" => "∑",
        "prod" => "∏",
        "coprod" => "∐",
        "infty" => "∞",
        "emptyset" => "∅",
        "varnothing" => "∅",
        "angle" => "∠",
        "therefore" => "∴",
        "because" => "∵",
        "aleph" => "ℵ",
        "beth" => "ℶ",
        "gimel" => "ℷ",
        "daleth" => "ℸ",
        "top" => "⊤",
        "bot" => "⊥",
        "triangle" => "△",
        "square" => "□",
        "lozenge" => "◊",
        "checkmark" => "✓",
        "complement" => "∁",
        "wp" => "℘",
        "prime" => "′",
        "ldots" => "…",
        "dots" => "…",
        "cdots" => "⋯",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "ell" => "ℓ",
        "hbar" => "ℏ",
        "Im" => "ℑ",
        "Re" => "ℜ",
        "langle" => "⟨",
        "rangle" => "⟩",
        "vert" => "|",
        "lvert" => "|",
        "rvert" => "|",
        "Vert" => "‖",
        "lVert" => "‖",
        "rVert" => "‖",
        "lbrace" => "{",
        "rbrace" => "}",
        "backslash" => "\\",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "lceil" => "⌈",
        "rceil" => "⌉",
        "colon" => ":",
        _ => return None,
    })
}

pub(crate) fn negated_symbol(value: &str) -> Option<&'static str> {
    Some(match value {
        "<" => "≮",
        ">" => "≯",
        "=" => "≠",
        "∈" => "∉",
        "∋" => "∌",
        "∣" => "∤",
        "∥" => "∦",
        "∼" => "≁",
        "≃" => "≄",
        "≅" => "≇",
        "≈" => "≉",
        "≡" => "≢",
        "≤" => "≰",
        "≥" => "≱",
        "≺" => "⊀",
        "≻" => "⊁",
        "⊂" => "⊄",
        "⊃" => "⊅",
        "⊆" => "⊈",
        "⊇" => "⊉",
        "⊢" => "⊬",
        "⊨" => "⊭",
        "↔" => "↮",
        "←" => "↚",
        "→" => "↛",
        "⇒" => "⇏",
        "⇐" => "⇍",
        "⇔" => "⇎",
        "≼" => "⋠",
        "≽" => "⋡",
        _ => return None,
    })
}

pub(crate) fn blackboard(value: char) -> Option<&'static str> {
    Some(match value {
        'C' => "ℂ",
        'H' => "ℍ",
        'N' => "ℕ",
        'P' => "ℙ",
        'Q' => "ℚ",
        'R' => "ℝ",
        'Z' => "ℤ",
        _ => return None,
    })
}

pub(crate) fn superscript(value: char) -> Option<&'static str> {
    Some(match value {
        '0' => "⁰",
        '1' => "¹",
        '2' => "²",
        '3' => "³",
        '4' => "⁴",
        '5' => "⁵",
        '6' => "⁶",
        '7' => "⁷",
        '8' => "⁸",
        '9' => "⁹",
        '+' => "⁺",
        '-' => "⁻",
        '=' => "⁼",
        '(' => "⁽",
        ')' => "⁾",
        'a' => "ᵃ",
        'b' => "ᵇ",
        'c' => "ᶜ",
        'd' => "ᵈ",
        'e' => "ᵉ",
        'f' => "ᶠ",
        'g' => "ᵍ",
        'h' => "ʰ",
        'i' => "ⁱ",
        'j' => "ʲ",
        'k' => "ᵏ",
        'l' => "ˡ",
        'm' => "ᵐ",
        'n' => "ⁿ",
        'o' => "ᵒ",
        'p' => "ᵖ",
        'r' => "ʳ",
        's' => "ˢ",
        't' => "ᵗ",
        'u' => "ᵘ",
        'v' => "ᵛ",
        'w' => "ʷ",
        'x' => "ˣ",
        'y' => "ʸ",
        'z' => "ᶻ",
        _ => return None,
    })
}

pub(crate) fn subscript(value: char) -> Option<&'static str> {
    Some(match value {
        '0' => "₀",
        '1' => "₁",
        '2' => "₂",
        '3' => "₃",
        '4' => "₄",
        '5' => "₅",
        '6' => "₆",
        '7' => "₇",
        '8' => "₈",
        '9' => "₉",
        '+' => "₊",
        '-' => "₋",
        '=' => "₌",
        '(' => "₍",
        ')' => "₎",
        'a' => "ₐ",
        'e' => "ₑ",
        'h' => "ₕ",
        'i' => "ᵢ",
        'j' => "ⱼ",
        'k' => "ₖ",
        'l' => "ₗ",
        'm' => "ₘ",
        'n' => "ₙ",
        'o' => "ₒ",
        'p' => "ₚ",
        'r' => "ᵣ",
        's' => "ₛ",
        't' => "ₜ",
        'u' => "ᵤ",
        'v' => "ᵥ",
        'x' => "ₓ",
        _ => return None,
    })
}

pub(crate) fn accent(value: &str) -> Option<&'static str> {
    Some(match value {
        "acute" => "́",
        "bar" => "̅",
        "breve" => "̆",
        "check" => "̌",
        "ddot" => "̈",
        "dot" => "̇",
        "grave" => "̀",
        "hat" => "̂",
        "mathring" => "̊",
        "overleftarrow" => "⃖",
        "overleftrightarrow" => "⃡",
        "overline" => "̅",
        "overrightarrow" => "⃗",
        "tilde" => "̃",
        "underline" => "̲",
        "vec" => "⃗",
        "widehat" => "̂",
        "widetilde" => "̃",
        _ => return None,
    })
}

pub(crate) fn is_named_operator(value: &str) -> bool {
    matches!(
        value,
        "arccos" | "arcsin" | "arctan" | "arg" | "cos" | "cosh"
            | "cot" | "coth" | "csc" | "deg" | "det" | "dim"
            | "exp" | "gcd" | "hom" | "inf" | "ker" | "lg"
            | "lim" | "liminf" | "limsup" | "ln" | "log" | "max"
            | "min" | "Pr" | "sec" | "sin" | "sinh" | "sup"
            | "tan" | "tanh"
    )
}

pub(crate) fn is_limit_operator(value: &str) -> bool {
    matches!(
        value,
        "argmax" | "argmin" | "inf" | "injlim" | "lim" | "liminf"
            | "limsup" | "max" | "min" | "projlim" | "sup"
    )
}

pub(crate) fn is_display_limit_symbol(value: &str) -> bool {
    matches!(
        value,
        "bigcap" | "bigcup" | "bigodot" | "bigoplus" | "bigotimes" | "bigsqcup"
            | "biguplus" | "bigvee" | "bigwedge" | "coprod" | "int" | "iint"
            | "iiint" | "oint" | "prod" | "sum"
    )
}

pub(crate) fn is_relation_command(value: &str) -> bool {
    matches!(
        value,
        "Leftarrow" | "Leftrightarrow" | "Longleftarrow" | "Longleftrightarrow" | "Longrightarrow" | "Rightarrow"
            | "Join" | "Vdash" | "Vvdash" | "approx" | "asymp" | "bowtie"
            | "cong" | "dashv" | "fullouterjoin" | "doteq" | "downarrow" | "equiv"
            | "ge" | "geq" | "geqslant" | "gets" | "gg" | "hookleftarrow"
            | "hookrightarrow" | "iff" | "implies" | "in" | "leadsto" | "le"
            | "leftarrow" | "leftharpoondown" | "leftharpoonup" | "leftrightarrow" | "leftrightharpoons" | "leftouterjoin"
            | "leq" | "leqslant" | "ll" | "longleftarrow" | "longleftrightarrow" | "longmapsto"
            | "longrightarrow" | "ltimes" | "mapsto" | "mid" | "models" | "ne"
            | "nearrow" | "neq" | "ni" | "notin" | "nvdash" | "nvDash"
            | "nwarrow" | "parallel" | "perp" | "prec" | "preceq" | "propto"
            | "rightharpoondown" | "rightharpoonup" | "rightleftharpoons" | "rightouterjoin" | "rightarrow" | "rightsquigarrow"
            | "rtimes" | "searrow" | "sim" | "simeq" | "sqsubset" | "sqsubseteq"
            | "sqsupset" | "sqsupseteq" | "subset" | "subseteq" | "succ" | "succeq"
            | "supset" | "supseteq" | "swarrow" | "to" | "triangleleft" | "triangleright"
            | "twoheadleftarrow" | "twoheadrightarrow" | "uparrow" | "vdash"
    )
}

pub(crate) fn is_spacing_command(value: &str) -> bool {
    matches!(
        value,
        "," | ":" | ";" | " " | ">" | "enspace"
            | "enskip" | "medspace" | "quad" | "qquad" | "thickspace" | "thinspace"
    )
}

pub(crate) fn is_negative_spacing_command(value: &str) -> bool {
    matches!(
        value,
        "!" | "negmedspace" | "negthickspace" | "negthinspace"
    )
}

pub(crate) fn is_ignored_command(value: &str) -> bool {
    matches!(
        value,
        "displaystyle" | "limits" | "nolimits" | "scriptstyle" | "scriptscriptstyle" | "textstyle"
    )
}

pub(crate) fn is_size_command(value: &str) -> bool {
    matches!(
        value,
        "big" | "Big" | "bigg" | "Bigg" | "bigl" | "Bigl"
            | "biggl" | "Biggl" | "bigr" | "Bigr" | "biggr" | "Biggr"
    )
}

pub(crate) fn is_plain_wrapper(value: &str) -> bool {
    matches!(
        value,
        "emph" | "mathcal" | "mathbf" | "mathfrak" | "mathit" | "mathrm"
            | "mathnormal" | "mathscr" | "mathsf" | "mathtt" | "mathup" | "mbox"
            | "overbrace" | "pmb" | "smash" | "substack" | "text" | "textbf"
            | "textit" | "textmd" | "textnormal" | "textrm" | "textsc" | "textsf"
            | "textsl" | "texttt" | "textup" | "underbrace" | "bm" | "boldsymbol"
    )
}
