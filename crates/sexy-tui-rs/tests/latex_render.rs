//! Behavioral goldens for the LaTeX renderer (parity card 2c.3).
//!
//! Every expected value in this file was captured by running the upstream
//! reference implementation `packages/tui/src/latex.ts`
//! (pi @ `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`) on the same input:
//!
//! - [`UPSTREAM_SUITE`] is the upstream `packages/tui/test/latex.test.ts`
//!   corpus (each entry reproduces the upstream suite's literal expectation).
//! - [`UPSTREAM_OBSERVED`], [`DISPLAY_LAYOUT_CORPUS`] and
//!   [`INLINE_LAYOUT_CORPUS`] are real-LaTeX inputs whose box-drawing output was
//!   captured from that reference implementation.
//! - [`UNSUPPORTED_COMMANDS`] are inputs the reference implementation refuses
//!   (`undefined`); the port must fail closed.
//!
//! The differential harness that produced these values ran the reference
//! renderer under Node (with its real `visibleWidth`, so wide/combining-glyph
//! alignment is measured the same way) against this port over the upstream
//! suite, the tables below, 232 curated real-LaTeX cases and 2626 randomized
//! token-soup cases, with zero divergences outside JavaScript's UTF-16
//! surrogate handling of non-BMP code points (see the module docs).

use sexy_tui_rs::rich_text::latex::{render_latex, RenderLatexOptions};

/// `(source, expected, display)` — upstream `packages/tui/test/latex.test.ts`.
const UPSTREAM_SUITE: &[(&str, &str, bool)] = &[
    ("\\mathbb{C}^3 \\to \\mathbb{C}^3", "ℂ³ → ℂ³", false),
    ("\\{3x+2y,\\; 27x^2-4z-1,\\; x(x-1)(x+1)\\} \\quad\\Rightarrow\\quad x \\in \\{0, \\pm 1\\},", "{3x+2y, 27x²-4z-1, x(x-1)(x+1)} ⇒ x ∈ {0, ± 1},", false),
    ("F_1 = -\\frac{1}{4x^2}.", "F₁ = -1/(4x²).", false),
    ("-2", "-2", false),
    ("(0,0,-1/4)", "(0,0,-1/4)", false),
    ("(1,-3/2,13/2)", "(1,-3/2,13/2)", false),
    ("(1,1,1)", "(1,1,1)", false),
    ("(2,1,0)", "(2,1,0)", false),
    ("(-1/4, 0, 0)", "(-1/4, 0, 0)", false),
    ("\\{(0,0,-1/4), (1,-3/2,13/2), (-1,3/2,13/2)\\}", "{(0,0,-1/4), (1,-3/2,13/2), (-1,3/2,13/2)}", false),
    ("(2,1,1)", "(2,1,1)", false),
    ("(7/3,-2/5,11/7)", "(7/3,-2/5,11/7)", false),
    ("\\{y - p(x),\\; q(x)\\}", "{y - p(x), q(x)}", false),
    ("\\deg q = 3", "deg q = 3", false),
    ("[\\mathbb{C}(x,y,z):\\mathbb{C}(F_1,F_2,F_3)] = 3", "[ℂ(x,y,z):ℂ(F₁,F₂,F₃)] = 3", false),
    ("u = 1+xy", "u = 1+xy", false),
    ("G = u^2 z + y^2(4+3xy)", "G = u² z + y²(4+3xy)", false),
    ("F_1 = uG", "F₁ = uG", false),
    ("F_2 = y + 3xG", "F₂ = y + 3xG", false),
    ("x=0", "x = 0", false),
    ("F_2 = F_3 = 0", "F₂ = F₃ = 0", false),
    ("xy = -3/2", "xy = -3/2", false),
    ("x^2 z = 13/2", "x² z = 13/2", false),
    ("\\mathbb{C}^*", "ℂ^*", false),
    ("s \\mapsto (s,\\, -\\tfrac{3}{2s},\\, \\tfrac{13}{2s^2})", "s ↦ (s, -3/(2s), 13/(2s²))", false),
    ("X", "X", false),
    ("p_\\pm", "p_±", false),
    ("F(-x,-y,z) = (F_1, -F_2, -F_3)", "F(-x,-y,z) = (F₁, -F₂, -F₃)", false),
    ("p_0", "p₀", false),
    ("s \\to \\infty", "s → ∞", false),
    ("(0,0,0)", "(0,0,0)", false),
    ("\\Rightarrow", "⇒", false),
    ("\\ge 2", "≥ 2", false),
    ("\\ge 3", "≥ 3", false),
    ("1", "1", false),
    ("\\mathrm{diag}(-1/2,1,1)", "diag(-1/2,1,1)", false),
    ("4+3xy", "4+3xy", false),
    ("E \\approx \\frac{0.1\\ \\text{lux}}{100\\ \\text{lm/W}} = 0.001\\ \\text{W/m}^2", "E ≈ (0.1 lux)/(100 lm/W) = 0.001 W/m²", false),
    ("\\boxed{1\\ \\text{milliwatt per square metre}}", "[1 milliwatt per square metre]", false),
    ("5\\ \\text{km}^2 = 5{,}000{,}000\\ \\text{m}^2", "5 km² = 5,000,000 m²", false),
    ("P_{\\text{light}} = 0.001 \\times 5{,}000{,}000\n= \\boxed{5{,}000\\ \\text{W}}", "P_light = 0.001 × 5,000,000 = [5,000 W]", false),
    ("P_{\\text{electric}} = 5\\ \\text{kW} \\times 0.2\n= \\boxed{1\\ \\text{kW}}", "P_electric = 5 kW × 0.2 = [1 kW]", false),
    ("\\pi(2.5\\ \\text{km})^2 = 19.6\\ \\text{km}^2", "π(2.5 km)² = 19.6 km²", false),
    ("0.001\\ \\text{W/m}^2 \\times 19.6 \\times 10^6\\ \\text{m}^2\n\\approx \\boxed{20\\ \\text{kW optical}}", "0.001 W/m² × 19.6 × 10⁶ m² ≈ [20 kW optical]", false),
    ("1\\ \\text{kW} \\times \\frac{1}{3600}\\ \\text{hour}\n= \\boxed{0.28\\ \\text{Wh}}", "1 kW × 1/3600 hour = [0.28 Wh]", false),
    ("\\det\\!\\left(\\frac{\\partial(F_1,F_2,F_3)}{\\partial(x,y,z)}\\right)=-2.", "det((∂(F₁,F₂,F₃))/(∂(x,y,z))) = -2.", false),
    ("\\begin{aligned}\nF(0,0,-\\tfrac14)&=(-\\tfrac14,0,0),\\\\\nF(1,-\\tfrac32,\\tfrac{13}2)&=(-\\tfrac14,0,0),\\\\\nF(-1,\\tfrac32,\\tfrac{13}2)&=(-\\tfrac14,0,0).\n\\end{aligned}", "F(0,0,-1/4) = (-1/4,0,0),\nF(1,-3/2,13/2) = (-1/4,0,0),\nF(-1,3/2,13/2) = (-1/4,0,0).", false),
    ("F=(F_1,F_2,F_3)", "F = (F₁,F₂,F₃)", false),
    ("F", "F", false),
    ("3", "3", false),
    ("J = \\begin{pmatrix}\n\\frac{\\partial f_1}{\\partial x} & \\frac{\\partial f_1}{\\partial y} & \\frac{\\partial f_1}{\\partial z} \\\\\n\\frac{\\partial f_2}{\\partial x} & \\frac{\\partial f_2}{\\partial y} & \\frac{\\partial f_2}{\\partial z} \\\\\n\\frac{\\partial f_3}{\\partial x} & \\frac{\\partial f_3}{\\partial y} & \\frac{\\partial f_3}{\\partial z}\n\\end{pmatrix}", "J = ⎛ (∂ f₁)/(∂ x) │ (∂ f₁)/(∂ y) │ (∂ f₁)/(∂ z) ⎞\n    ⎜ (∂ f₂)/(∂ x) │ (∂ f₂)/(∂ y) │ (∂ f₂)/(∂ z) ⎟\n    ⎝ (∂ f₃)/(∂ x) │ (∂ f₃)/(∂ y) │ (∂ f₃)/(∂ z) ⎠", false),
    ("\\begin{aligned}\nf_1 &= (1+xy)^3 z + y^2(1+xy)(4+3xy) \\\\\nf_2 &= y + 3x(1+xy)^2 z + 3xy^2(4+3xy) \\\\\nf_3 &= 2x - 3x^2y - x^3z\n\\end{aligned}", "f₁ = (1+xy)³ z + y²(1+xy)(4+3xy)\nf₂ = y + 3x(1+xy)² z + 3xy²(4+3xy)\nf₃ = 2x - 3x²y - x³z", false),
    ("x, y, z", "x, y, z", false),
    ("(x, y, z)", "(x, y, z)", false),
    ("(0,\\; 0,\\; -\\tfrac14)", "(0, 0, -1/4)", false),
    ("(-\\tfrac14,\\; 0,\\; 0)", "(-1/4, 0, 0)", false),
    ("(1,\\; -\\tfrac32,\\; \\tfrac{13}{2})", "(1, -3/2, 13/2)", false),
    ("(-1,\\; \\tfrac32,\\; \\tfrac{13}{2})", "(-1, 3/2, 13/2)", false),
    ("(-\\frac14, 0, 0)", "(-1/4, 0, 0)", false),
    ("F: \\mathbb{C}^3 \\to \\mathbb{C}^3", "F: ℂ³ → ℂ³", false),
    ("F(0,0,-\\tfrac14) = F(1,-\\tfrac32,\\tfrac{13}{2}) = F(-1,\\tfrac32,\\tfrac{13}{2}) = (-\\tfrac14, 0, 0)", "F(0,0,-1/4) = F(1,-3/2,13/2) = F(-1,3/2,13/2) = (-1/4, 0, 0)", false),
    ("\\mathbb{C}^3", "ℂ³", false),
    ("\\begin{aligned}\nf_1 &= \\frac{f_1^{\\text{ut}}(u,t)}{x^2}, \\quad\nf_2 = \\frac{f_2^{\\text{ut}}(u,t)}{x}, \\quad\nf_3 = x\\,(2 - 3u - t)\n\\end{aligned}", "f₁ = (f₁ᵘᵗ(u,t))/(x²), f₂ = (f₂ᵘᵗ(u,t))/x, f₃ = x (2 - 3u - t)", false),
    ("\\det J_F", "det J_F", false),
    ("(-\\tfrac14, 0, 0)", "(-1/4, 0, 0)", false),
    ("u = xy", "u = xy", false),
    ("t = x^2z", "t = x²z", false),
    ("x \\neq 0", "x ≠ 0", false),
    ("f_1^{\\text{ut}}, f_2^{\\text{ut}}", "f₁ᵘᵗ, f₂ᵘᵗ", false),
    ("u,t", "u,t", false),
    ("x", "x", false),
    ("x, x^2", "x, x²", false),
    ("\\mathbb{C}^n \\to \\mathbb{C}^n", "ℂⁿ → ℂⁿ", false),
    ("n \\geq 2", "n ≥ 2", false),
    ("\\mathbb{P}^3", "ℙ³", false),
    ("e^{i\\pi}+1=0", "e^(iπ)+1 = 0", false),
    ("\\boxed{\n\\mathcal{Z}(\\beta)\n=\n\\int_{\\mathcal M}\n\\exp\\!\\left(\n-\\beta\\left[\n\\frac12 g^{ij}(x)\\,\\partial_i\\phi\\,\\partial_j\\phi\n+V(\\phi)\n\\right]\\right)\n\\mathcal D\\phi\n}", "[Z(β) = ∫_M exp( -β[ 1/2 gⁱʲ(x) ∂ᵢϕ ∂ⱼϕ +V(ϕ) ]) Dϕ]", false),
    ("\\begin{aligned}\n\\nabla_\\mu T^{\\mu\\nu}\n&=\n\\frac{1}{\\sqrt{-g}}\n\\partial_\\mu\\!\\left(\\sqrt{-g}\\,T^{\\mu\\nu}\\right)\n+\\Gamma^\\nu_{\\mu\\lambda}T^{\\mu\\lambda}\n=0, \\\\[4pt]\nR_{\\mu\\nu}-\\frac12 Rg_{\\mu\\nu}+\\Lambda g_{\\mu\\nu}\n&=\n\\frac{8\\pi G}{c^4}T_{\\mu\\nu}.\n\\end{aligned}", "∇_μ T^(μν) = 1/(√(-g)) ∂_μ(√(-g) T^(μν)) +Γ^ν_(μλ)T^(μλ) = 0,\nR_(μν)-1/2 Rg_(μν)+Λ g_(μν) = (8π G)/(c⁴)T_(μν).", false),
    ("f(z)\n=\n\\frac{1}{2\\pi i}\n\\oint_{\\gamma}\n\\frac{f(\\zeta)}{\\zeta-z}\\,d\\zeta,\n\\qquad\n\\det\\!\\begin{pmatrix}\n\\lambda-a & -b & 0\\\\\n-c & \\lambda-d & -e\\\\\n0 & -f & \\lambda-g\n\\end{pmatrix}\n=0.", "f(z) = 1/(2π i) ∮_γ (f(ζ))/(ζ-z) dζ, det⎛ λ-a │ -b  │ 0   ⎞ = 0.\n                                        ⎜ -c  │ λ-d │ -e  ⎟\n                                        ⎝ 0   │ -f  │ λ-g ⎠", false),
    ("\\Psi(x,t)=\n\\sum_{n=1}^{\\infty}\n\\underbrace{\nc_n\n\\sqrt{\\frac{2}{L}}\n\\sin\\!\\left(\\frac{n\\pi x}{L}\\right)\n}_{\\text{spatial eigenmode}}\n\\exp\\!\\left(-\\frac{i\\hbar n^2\\pi^2}{2mL^2}t\\right),\n\\qquad\n|\\Psi(x,t)|^2\n=\n\\begin{cases}\n\\Psi^\\ast\\Psi, & 0<x<L,\\\\\n0, & \\text{otherwise}.\n\\end{cases}", "Ψ(x,t) = ∑ₙ₌₁^∞ cₙ √(2/L) sin((nπ x)/L)_(spatial eigenmode) exp(-(iℏ n²π²)/(2mL²)t), |Ψ(x,t)|² = ⎧ Ψ^∗Ψ if 0 < x < L,\n⎩ 0 otherwise.", false),
    ("x=\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a}", "x = (-b±√(b²-4ac))/(2a)", false),
    ("\\int_0^\\infty e^{-x^2}\\,dx=\\frac{\\sqrt{\\pi}}{2}", "∫₀^∞ e^(-x²) dx = (√π)/2", false),
    ("e^{i\\theta}=\\cos\\theta+i\\sin\\theta", "e^(iθ) = cos θ+i sin θ", false),
    ("\\sum_{n=1}^{\\infty}\\frac{1}{n^2}=\\frac{\\pi^2}{6}", "∑ₙ₌₁^∞1/(n²) = π²/6", false),
    ("\\lim_{x\\to 0}\\frac{\\sin x}{x}=1", "lim[x→0] (sin x)/x = 1", false),
    ("\\lim_{n\\to\\infty}\n\\left(1+\\frac{1}{n}\\right)^n=e", "lim[n→∞] (1+1/n)ⁿ = e", false),
    ("\\int_0^1 \\frac{x^2}{1+x^3}\\,dx\n=\\frac{1}{3}\\ln 2", "∫₀¹ x²/(1+x³) dx = 1/3 ln 2", false),
    ("\\sum_{k=1}^{n}\\frac{k}{k+1}\n=n+1-H_{n+1}", "∑ₖ₌₁ⁿk/(k+1) = n+1-Hₙ₊₁", false),
    ("\\frac{\n  \\displaystyle \\frac{x^2+1}{x-1}\n  -\n  \\displaystyle \\frac{2x}{x+1}\n}{\n  \\displaystyle \\frac{x}{x^2-1}\n}", "((x²+1)/(x-1) - 2x/(x+1))/(x/(x²-1))", false),
    ("\\lim_{x\\to 0}\n\\frac{\n  \\displaystyle \\frac{\\sin x}{x}-1\n}{\n  \\displaystyle \\frac{e^x-1}{x}-1\n}\n=0", "lim[x→0] ((sin x)/x-1)/((eˣ-1)/x-1) = 0", false),
    ("\\frac{\n  1+\\displaystyle\\frac{1}{1+\\frac{1}{x}}\n}{\n  1-\\displaystyle\\frac{1}{1-\\frac{1}{x}}\n}", "(1+1/(1+1/x))/(1-1/(1-1/x))", false),
    ("\\sum_{n=1}^{\\infty}\n\\frac{\n  \\displaystyle \\frac{1}{n}-\\frac{1}{n+1}\n}{\n  \\displaystyle 1+\\frac{1}{n^2}\n}", "∑ₙ₌₁^∞ (1/n-1/(n+1))/(1+1/(n²))", false),
    ("\\sum_{i=0}^n \\alpha_i + \\int_0^\\infty e^{-x^2}\\,dx = \\sqrt{\\pi}", "∑ᵢ₌₀ⁿ αᵢ + ∫₀^∞ e^(-x²) dx = √π", false),
    ("\\binom{n}{k}+\\vec{x}+\\hat{y}+\\overline{AB}", "(n choose k)+x⃗+ŷ+overline(AB)", false),
    ("\\epsilon+\\varepsilon+\\varsigma+\\varkappa+\\oplus+\\otimes+\\therefore+\\because", "ϵ+ε+ς+ϰ+⊕+⊗+∴+∵", false),
    ("A\\not\\subseteq B,\\quad x\\not\\in X", "A ⊈ B, x ∉ X", false),
    ("R\\bowtie S,\\quad R\\Join S", "R ⋈ S, R ⋈ S", false),
    ("R\\ltimes S,\\quad R\\rtimes S", "R ⋉ S, R ⋊ S", false),
    ("R\\leftouterjoin S,\\quad R\\rightouterjoin S,\\quad R\\fullouterjoin S", "R ⟕ S, R ⟖ S, R ⟗ S", false),
    ("\\lvert{x}\\rvert+\\lVert{v}\\rVert+\\left.\\frac{dy}{dx}\\right|_{x=0}", "|x|+‖v‖+dy/(dx)|ₓ₌₀", false),
    ("\\left\\lbrace x \\middle| x>0 \\right\\rbrace", "{ x | x > 0 }", false),
    ("\\operatorname*{arg\\,max}_{x\\in X} f(x)", "arg max[x∈X] f(x)", false),
    ("a\\bmod n,\\quad a\\equiv b\\pmod n", "a mod n, a ≡ b (mod n)", false),
    ("\\overset{!}{=}+\\underset{n}{x}+\\stackrel{def}{=}", "=^!+xₙ+=ᵈᵉᶠ", false),
    ("\\sqrt[2]{x}+\\sqrt[3]{x}+\\sqrt[4]{x}+\\sqrt[n]{x}+\\sqrt[k]{x+1}", "√x+∛x+∜x+ⁿ√x+ᵏ√(x+1)", false),
    ("\\acute{x}+\\grave{y}+\\widehat{xyz}+\\overrightarrow{AB}", "x́+ỳ+widehat(xyz)+overrightarrow(AB)", false),
    ("\\textnormal{hello}+\\mbox{world}+\\boldsymbol{x}", "hello+world+x", false),
    ("\\begin{equation}\\begin{split}a&=b\\\\&=c\\end{split}\\end{equation}", "a = b\n= c", false),
    ("\\begin{alignedat}{2}a&=b&\\quad c&=d\\\\e&=f&g&=h\\end{alignedat}", "a = b c = d\ne = f g = h", false),
    ("\\begin{cases}a & x<0 \\\\ b & \\text{if }x=0 \\\\ c & \\text{otherwise}\\end{cases}", "⎧ a if x < 0\n⎨ b if x = 0\n⎩ c otherwise", false),
    ("\\begin{pmatrix}1&200\\\\3000&4\\end{pmatrix}", "⎛ 1    │ 200 ⎞\n⎝ 3000 │ 4   ⎠", false),
    ("R\\left(\\frac{\\pi}{4}\\right)\n=\n\\begin{pmatrix}\n\\frac{\\sqrt{2}}{2} & -\\frac{\\sqrt{2}}{2}\\\\\n\\frac{\\sqrt{2}}{2} & \\frac{\\sqrt{2}}{2}\n\\end{pmatrix}.", "   π\nR( ─ ) = ⎛ (√2)/2 │ -(√2)/2 ⎞\n   4     ⎝ (√2)/2 │ (√2)/2  ⎠.", true),
    ("\\mathbf w\n=\nR\\left(\\frac{\\pi}{4}\\right)\n\\begin{pmatrix}1\\\\0\\end{pmatrix}\n=\n\\begin{pmatrix}\\frac{\\sqrt{2}}{2}\\\\\\frac{\\sqrt{2}}{2}\\end{pmatrix}.", "       π\nw = R( ─ ) ⎛ 1 ⎞ = ⎛ (√2)/2 ⎞\n       4   ⎝ 0 ⎠   ⎝ (√2)/2 ⎠.", true),
    ("A\\mathbf e_1=\\begin{pmatrix}\\pi\\\\0\\end{pmatrix},\\qquad A\\mathbf e_2=\\begin{pmatrix}0\\\\\\frac{1}{\\pi}\\end{pmatrix}.", "Ae₁ = ⎛ π ⎞, Ae₂ = ⎛ 0   ⎞\n      ⎝ 0 ⎠        ⎝ 1/π ⎠.", true),
    ("\\sum_{i=0}^n x_i=\\begin{pmatrix}a&b\\\\c&d\\end{pmatrix}.", " n\n ∑  xᵢ = ⎛ a │ b ⎞\ni=0      ⎝ c │ d ⎠.", true),
    ("x=y", "x = y", false),
    ("x =y", "x = y", false),
    ("x=\ny", "x = y", false),
    ("x\n=\ny", "x = y", false),
    ("x_{i=0}", "xᵢ₌₀", false),
    ("x\\neq0", "x ≠ 0", false),
    ("A\\to B", "A → B", false),
    ("\\pi\\cdot\\frac{1}{\\pi}", "π · 1/π", false),
    ("\\sin\\theta", "sin θ", false),
    ("\\sin^2 x", "sin² x", false),
    ("-\\sin\\theta", "-sin θ", false),
    ("i\\sin\\theta", "i sin θ", false),
    ("\\det(A)", "det(A)", false),
    ("\\boxed{\n(1,1,1),\\ (1,1,2),\\ (1,2,5),\\ (1,5,13),\\ (2,5,29),\\\n(1,13,34),\\ (1,34,89)\n}.", "[(1,1,1), (1,1,2), (1,2,5), (1,5,13), (2,5,29), (1,13,34), (1,34,89)].", true),
    ("a\\\r\nb", "a b", false),
    ("\\sum_{i=0}^n x_i", " n\n ∑  xᵢ\ni=0", true),
    ("\\min_{x\\in X} f(x)", "min f(x)\nx∈X", true),
    ("\\operatorname*{arg\\,max}_{x\\in X} f(x)", "arg max f(x)\n  x∈X", true),
    ("\\int\\nolimits_0^1 f(x)\\,dx", "∫₀¹ f(x) dx", true),
    ("\\int\\limits_0^1 f(x)\\,dx", "1\n∫ f(x) dx\n0", true),
    ("\\begin{cases}a & x<0 \\\\ b & x=0 \\\\ c & x>0\\end{cases}", "⎧ a if x < 0\n⎨ b if x = 0\n⎩ c if x > 0", false),
    ("x=\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a}", "    -b±√(b²-4ac)\nx = ────────────\n         2a", true),
    ("\\frac{x^2+1}{x-1}", "x²+1\n────\nx-1", true),
    ("\\frac{1}\n{2}", "1\n─\n2", true),
    ("\\frac{\\frac{x^2+1}{x-1}-\\frac{2x}{x+1}}{\\frac{x}{x^2-1}}", "(x²+1)/(x-1)-2x/(x+1)\n─────────────────────\n      x/(x²-1)", true),
    ("\\lim_{x\\to 0}\\frac{\\frac{\\sin x}{x}-1}{\\frac{e^x-1}{x}-1}=0", "     (sin x)/x-1\nlim  ─────────── = 0\nx→0  (eˣ-1)/x-1", true),
    ("\\frac{1+\\frac{1}{1+\\frac{1}{x}}}{1-\\frac{1}{1-\\frac{1}{x}}}", "1+1/(1+1/x)\n───────────\n1-1/(1-1/x)", true),
    ("e^{\\frac{1}{2}}", "e^(1/2)", true),
    ("\\tfrac{1}{2}", "1/2", true),
];

/// Sources the upstream suite expects to fail closed.
const UPSTREAM_FAILURES: &[&str] = &[
    "x + \\unknown{y}",
    "\\frac{1}{x",
    "x}",
    "\\begin{matrix}1 & 2",
    "x\\",
];

/// `(source, expected, display)` — captured from the upstream implementation.
const UPSTREAM_OBSERVED: &[(&str, Option<&str>, bool)] = &[
    ("\\begin{pmatrix}a & b \\\\ c & d\\end{pmatrix}", Some("⎛ a │ b ⎞\n⎝ c │ d ⎠"), true),
    ("\\int_0^\\infty e^{-x^2}\\,dx = \\frac{\\sqrt{\\pi}}{2}", Some("∞              √π\n∫ e^(-x²) dx = ──\n0              2"), true),
    ("x = \\frac{-b \\pm \\sqrt{b^2-4ac}}{2a}", Some("    -b ± √(b²-4ac)\nx = ──────────────\n          2a"), true),
    ("\\begin{cases} x & x > 0 \\\\ -x & x \\le 0 \\end{cases}", Some("⎧ x if x > 0\n⎩ -x if x ≤ 0"), false),
    ("\\lim_{n\\to\\infty}\\left(1+\\frac{1}{n}\\right)^n = e", Some("        1\nlim (1+ ─ )ⁿ = e\nn→∞     n"), true),
    ("\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}", Some(" n      n(n+1)\n ∑  i = ──────\ni=1       2"), true),
    ("\\begin{aligned} a &= b + c \\\\ d &= e \\end{aligned}", Some("a = b + c\nd = e"), false),
    ("\\hat{H}\\psi = E\\psi", Some("Ĥψ = Eψ"), false),
    ("\\vec{F} = m\\vec{a}", Some("F⃗ = ma⃗"), false),
    ("\\sqrt[3]{x+1}", Some("∛(x+1)"), false),
    ("\\nabla \\cdot \\vec{E} = \\frac{\\rho}{\\varepsilon_0}", Some("        ρ\n∇ · E⃗ = ──\n        ε₀"), true),
    ("\\mathcal{L} = \\bar{\\psi}(i\\gamma^\\mu \\partial_\\mu - m)\\psi", Some("L = ψ̅(iγ^μ ∂_μ - m)ψ"), true),
    ("\\begin{tikzpicture}\\draw (0,0);\\end{tikzpicture}", None, false),
    ("\\frac{1}{", None, false),
    ("\\unknown{x}", None, false),
    ("\\text{if } x > 0", Some("if x > 0"), false),
    ("\\frac{\\hat{x}}{\\vec{y}}", Some("x̂\n─\ny⃗"), true),
    ("\\begin{pmatrix}界&a\\\\b&c\\end{pmatrix}", Some("⎛ 界 │ a ⎞\n⎝ b  │ c ⎠"), true),
    ("\\underbrace{c_n\\sqrt{2}}_{\\text{mode}}", Some("cₙ√2_mode"), true),
];


/// `(source, expected)` in display mode: operator limits, stacked fractions and
/// every matrix environment the port supports. Captured from the upstream
/// reference implementation.
const DISPLAY_LAYOUT_CORPUS: &[(&str, &str)] = &[
    (r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}", " n      n(n+1)\n ∑  i = ──────\ni=1       2"),
    (r"\prod_{i=1}^{n} a_i", " n\n ∏  aᵢ\ni=1"),
    (r"\prod_{k=0}^{\infty} \frac{1}{k!}", " ∞   1\n ∏   ──\nk=0  k!"),
    (r"\oint_C \frac{dz}{z}", "   dz\n∮  ──\nC  z"),
    (r"\bigcup_{i=1}^{n} A_i", " n\n ⋃  Aᵢ\ni=1"),
    (r"\bigcap_{i \in I} B_i", " ⋂  Bᵢ\ni∈I"),
    (r"\lim_{x\to\infty} \frac{1}{x} = 0", "     1\nlim  ─ = 0\nx→∞  x"),
    (r"\max_{1 \le i \le n} x_i", " max  xᵢ\n1≤i≤n"),
    (r"\operatorname*{arg\,max}_{x \in X} f(x)", "arg max f(x)\n  x∈X"),
    (r"\frac{\partial f}{\partial x}", "∂ f\n───\n∂ x"),
    (r"\frac{\partial^2 f}{\partial x \partial y}", " ∂² f\n───────\n∂ x ∂ y"),
    (r"\frac{1+\frac{1}{x}}{1-\frac{1}{x}}", "1+1/x\n─────\n1-1/x"),
    (r"\frac{\frac{a}{b}}{\frac{c}{d}}", "a/b\n───\nc/d"),
    (r"\begin{matrix}a&b\\c&d\end{matrix}", "a │ b\nc │ d"),
    (r"\begin{bmatrix}a&b\\c&d\end{bmatrix}", "⎡ a │ b ⎤\n⎣ c │ d ⎦"),
    (r"\begin{Bmatrix}a&b\\c&d\end{Bmatrix}", "⎧ a │ b ⎫\n⎩ c │ d ⎭"),
    (r"\begin{vmatrix}a&b\\c&d\end{vmatrix}", "│ a │ b │\n│ c │ d │"),
    (r"\begin{Vmatrix}a&b\\c&d\end{Vmatrix}", "║ a │ b ║\n║ c │ d ║"),
    (r"\begin{smallmatrix}a&b\\c&d\end{smallmatrix}", "a │ b\nc │ d"),
    (r"\begin{array}{c|c}a&b\\c&d\end{array}", "a │ b\nc │ d"),
    (r"\begin{array}{ll}a&b\\c&d\end{array}", "a │ b\nc │ d"),
    (r"\begin{pmatrix}1&2&3\\4&5&6\\7&8&9\end{pmatrix}", "⎛ 1 │ 2 │ 3 ⎞\n⎜ 4 │ 5 │ 6 ⎟\n⎝ 7 │ 8 │ 9 ⎠"),
    (r"\begin{pmatrix}a\\b\end{pmatrix}", "⎛ a ⎞\n⎝ b ⎠"),
    (r"\begin{pmatrix}a&b\end{pmatrix}", "⎛ a │ b ⎞"),
    (r"\left(\begin{matrix}a&b\\c&d\end{matrix}\right)", "(a │ b)\n c │ d"),
    (r"\left[\begin{matrix}a&b\\c&d\end{matrix}\right]", "[a │ b]\n c │ d"),
    (r"\begin{pmatrix}\frac{1}{2}&\sqrt{2}\\\pi&e\end{pmatrix}", "⎛ 1/2 │ √2 ⎞\n⎝ π   │ e  ⎠"),
    (r"\begin{pmatrix}\frac{\partial f_1}{\partial x}&\frac{\partial f_1}{\partial y}\\\frac{\partial f_2}{\partial x}&\frac{\partial f_2}{\partial y}\end{pmatrix}", "⎛ (∂ f₁)/(∂ x) │ (∂ f₁)/(∂ y) ⎞\n⎝ (∂ f₂)/(∂ x) │ (∂ f₂)/(∂ y) ⎠"),
    (r"\begin{pmatrix}界&a\\b&c\end{pmatrix}", "⎛ 界 │ a ⎞\n⎝ b  │ c ⎠"),
    (r"\begin{alignedat}{2}a&=b&\quad c&=d\\e&=f&g&=h\end{alignedat}", "a = b c = d\ne = f g = h"),
    (r"\begin{equation}\begin{split}a&=b\\&=c\end{split}\end{equation}", "a = b\n= c"),
    (r"\int_{-\infty}^{+\infty} e^{-x^2}\,dx = \sqrt{\pi}", "+∞\n∫  e^(-x²) dx = √π\n-∞"),
    (r"\int\limits_0^1 f(x)\,dx", "1\n∫ f(x) dx\n0"),
    (r"\int\nolimits_0^1 f(x)\,dx", "∫₀¹ f(x) dx"),
];

/// `(source, expected)` in inline mode: environments, scripts and delimiters.
/// Captured from the upstream reference implementation.
const INLINE_LAYOUT_CORPUS: &[(&str, &str)] = &[
    (r"\begin{cases}a&x<0\\b&x=0\\c&x>0\end{cases}", "⎧ a if x < 0\n⎨ b if x = 0\n⎩ c if x > 0"),
    (r"\begin{pmatrix}a&b\\c&d\end{pmatrix}", "⎛ a │ b ⎞\n⎝ c │ d ⎠"),
    (r"\begin{cases}x^2 & x \ge 0 \\ -x^2 & x < 0\end{cases}", "⎧ x² if x ≥ 0\n⎩ -x² if x < 0"),
    (r"\begin{aligned} a &= b + c \\ d &= e \end{aligned}", "a = b + c\nd = e"),
    (r"\overbrace{a+b}^{n}", "a+bⁿ"),
    (r"\underbrace{a+b}_{n}", "a+bₙ"),
    (r"\binom{n}{k}", "(n choose k)"),
    (r"\left\{x \middle| x>0\right\}", "{x | x > 0}"),
    (r"\operatorname{rank} A = n", "rank A = n"),
    (r"\substack{i=1\\j=2}", "i = 1\nj = 2"),
    (r"\text{a \textbf{b}}", "a b"),
];

/// Commands the reference renderer returns `undefined` for. The port must fail
/// closed (return `None`) instead of guessing or recursing.
const UNSUPPORTED_COMMANDS: &[&str] = &[
    r"\cfrac{1}{1+x}",
    r"\hspace{1em}",
    r"\phantom{x}",
    r"\cancel{x}",
    r"\genfrac{}{}{}{}{a}{b}",
    r"\verb|x|",
    r"\begin{tikzpicture}\draw (0,0);\end{tikzpicture}",
    r"\def\foo{}",
    r"\usepackage{amsmath}",
    r"\xrightarrow{f}",
];

#[test]
fn upstream_suite_corpus_matches_reference_renderer() {
    for (source, expected, display) in UPSTREAM_SUITE {
        let options = RenderLatexOptions { display: *display };
        assert_eq!(
            render_latex(source, options).as_deref(),
            Some(*expected),
            "source = {source:?} (display = {display})"
        );
    }
}

#[test]
fn observed_upstream_output_matches_reference_renderer() {
    for (source, expected, display) in UPSTREAM_OBSERVED {
        let options = RenderLatexOptions { display: *display };
        assert_eq!(
            render_latex(source, options).as_deref(),
            *expected,
            "source = {source:?} (display = {display})"
        );
    }
}

#[test]
fn display_layout_corpus_matches_reference_renderer() {
    for (source, expected) in DISPLAY_LAYOUT_CORPUS {
        assert_eq!(
            render_latex(source, RenderLatexOptions { display: true }).as_deref(),
            Some(*expected),
            "source = {source:?}"
        );
    }
}

#[test]
fn inline_layout_corpus_matches_reference_renderer() {
    for (source, expected) in INLINE_LAYOUT_CORPUS {
        assert_eq!(
            render_latex(source, RenderLatexOptions::default()).as_deref(),
            Some(*expected),
            "source = {source:?}"
        );
    }
}

#[test]
fn operator_limits_stack_over_their_operator() {
    // Every limit-taking operator composes the same way: the operator glyph in
    // the middle row, the upper bound above it and the lower bound below it.
    assert_eq!(
        render_latex(r"\prod_{i=1}^{n} a_i", RenderLatexOptions { display: true }).as_deref(),
        Some(" n\n ∏  aᵢ\ni=1")
    );
    assert_eq!(
        render_latex(r"\oint_C \frac{dz}{z}", RenderLatexOptions { display: true }).as_deref(),
        Some("   dz\n∮  ──\nC  z")
    );
    assert_eq!(
        render_latex(r"\lim_{x\to\infty} \frac{1}{x} = 0", RenderLatexOptions { display: true })
            .as_deref(),
        Some("     1\nlim  ─ = 0\nx→∞  x")
    );
    // `\nolimits` keeps the bounds as scripts; `\limits` forces the stacked
    // form even for operators that default to inline limits.
    assert_eq!(
        render_latex(r"\int\nolimits_0^1 f(x)\,dx", RenderLatexOptions { display: true }).as_deref(),
        Some("∫₀¹ f(x) dx")
    );
    assert_eq!(
        render_latex(r"\int\limits_0^1 f(x)\,dx", RenderLatexOptions { display: true }).as_deref(),
        Some("1\n∫ f(x) dx\n0")
    );
}

#[test]
fn matrix_environments_draw_their_delimiters() {
    let cases = [
        (r"\begin{pmatrix}a&b\\c&d\end{pmatrix}", "⎛ a │ b ⎞\n⎝ c │ d ⎠"),
        (r"\begin{bmatrix}a&b\\c&d\end{bmatrix}", "⎡ a │ b ⎤\n⎣ c │ d ⎦"),
        (r"\begin{Bmatrix}a&b\\c&d\end{Bmatrix}", "⎧ a │ b ⎫\n⎩ c │ d ⎭"),
        (r"\begin{vmatrix}a&b\\c&d\end{vmatrix}", "│ a │ b │\n│ c │ d │"),
        (r"\begin{Vmatrix}a&b\\c&d\end{Vmatrix}", "║ a │ b ║\n║ c │ d ║"),
        (r"\begin{matrix}a&b\\c&d\end{matrix}", "a │ b\nc │ d"),
    ];
    for (source, expected) in cases {
        assert_eq!(
            render_latex(source, RenderLatexOptions { display: true }).as_deref(),
            Some(expected),
            "source = {source:?}"
        );
    }
}

#[test]
fn unsupported_commands_fail_closed_without_panicking() {
    for source in UNSUPPORTED_COMMANDS {
        assert_eq!(
            render_latex(source, RenderLatexOptions::default()),
            None,
            "source = {source:?}"
        );
    }
}

#[test]
fn upstream_suite_failures_render_nothing() {
    for source in UPSTREAM_FAILURES {
        assert_eq!(
            render_latex(source, RenderLatexOptions::default()),
            None,
            "source = {source:?}"
        );
    }
}

#[test]
fn display_mode_draws_stacked_fractions() {
    assert_eq!(
        render_latex(
            r"x = \frac{-b \pm \sqrt{b^2-4ac}}{2a}",
            RenderLatexOptions { display: true }
        )
        .as_deref(),
        Some("    -b ± √(b²-4ac)\nx = ──────────────\n          2a")
    );
    assert_eq!(
        render_latex(
            r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
            RenderLatexOptions { display: true }
        )
        .as_deref(),
        Some("∞              √π\n∫ e^(-x²) dx = ──\n0              2")
    );
    // Scripts and text-style fractions stay linear even in display mode.
    assert_eq!(
        render_latex(r"\tfrac{1}{2}", RenderLatexOptions { display: true }).as_deref(),
        Some("1/2")
    );
    assert_eq!(
        render_latex(r"e^{\frac{1}{2}}", RenderLatexOptions { display: true }).as_deref(),
        Some("e^(1/2)")
    );
}

#[test]
fn display_mode_stacks_operator_limits() {
    assert_eq!(
        render_latex(
            r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}",
            RenderLatexOptions { display: true }
        )
        .as_deref(),
        Some(" n      n(n+1)\n ∑  i = ──────\ni=1       2")
    );
    assert_eq!(
        render_latex(
            r"\lim_{n\to\infty}\left(1+\frac{1}{n}\right)^n = e",
            RenderLatexOptions { display: true }
        )
        .as_deref(),
        Some("        1\nlim (1+ ─ )ⁿ = e\nn→∞     n")
    );
}

#[test]
fn matrices_align_columns_and_draw_delimiters() {
    assert_eq!(
        render_latex(
            r"\begin{pmatrix}a & b \\ c & d\end{pmatrix}",
            RenderLatexOptions { display: true }
        )
        .as_deref(),
        Some("⎛ a │ b ⎞\n⎝ c │ d ⎠")
    );
}

#[test]
fn matrix_column_width_is_cell_correct_for_wide_and_combining_glyphs() {
    assert_eq!(
        render_latex(
            r"\begin{pmatrix}界&a\\b&c\end{pmatrix}",
            RenderLatexOptions { display: true }
        )
        .as_deref(),
        Some("⎛ 界 │ a ⎞\n⎝ b  │ c ⎠")
    );
    assert_eq!(
        render_latex(r"\frac{\hat{x}}{\vec{y}}", RenderLatexOptions { display: true }).as_deref(),
        Some("x̂\n─\ny⃗")
    );
}

#[test]
fn case_environments_use_brace_delimiters() {
    assert_eq!(
        render_latex(
            r"\begin{cases} x & x > 0 \\ -x & x \le 0 \end{cases}",
            RenderLatexOptions::default()
        )
        .as_deref(),
        Some("⎧ x if x > 0\n⎩ -x if x ≤ 0")
    );
}

#[test]
fn unsupported_and_malformed_input_fails_closed() {
    for source in [
        r"\unknown{x}",
        r"\begin{tikzpicture}\draw (0,0);\end{tikzpicture}",
        r"\frac{1}{",
        r"x}",
        r"\begin{matrix}1 & 2",
        "x\\",
    ] {
        assert_eq!(render_latex(source, RenderLatexOptions::default()), None, "source = {source:?}");
    }
}

#[test]
fn nesting_is_bounded_and_never_panics() {
    use sexy_tui_rs::rich_text::latex::MAX_LATEX_NESTING_DEPTH;

    // Well inside the bound: groups and fraction chains still render.
    let mut source = String::from("x");
    for _ in 0..32 {
        source = format!("{{{source}}}");
    }
    assert_eq!(render_latex(&source, RenderLatexOptions::default()).as_deref(), Some("x"));

    let mut nested = String::new();
    for _ in 0..32 {
        nested.push_str(r"\frac{");
    }
    nested.push('1');
    for _ in 0..32 {
        nested.push_str("}{2}");
    }
    assert!(render_latex(&nested, RenderLatexOptions { display: true }).is_some());

    // Past the bound the parser stops descending and fails closed. Before the
    // bound existed these inputs exhausted the thread stack and aborted the
    // process, which is why the assertion is "no panic, `None`" rather than a
    // rendered value.
    let deep_braces = "{".repeat(MAX_LATEX_NESTING_DEPTH + 1);
    assert_eq!(render_latex(&deep_braces, RenderLatexOptions::default()), None);

    let mut deep_fractions = String::new();
    for _ in 0..10_000 {
        deep_fractions.push_str(r"\frac{");
    }
    deep_fractions.push('1');
    for _ in 0..10_000 {
        deep_fractions.push_str("}{2}");
    }
    assert_eq!(render_latex(&deep_fractions, RenderLatexOptions::default()), None);
    assert_eq!(
        render_latex(&deep_fractions, RenderLatexOptions { display: true }),
        None
    );

    let deep_environments = r"\begin{cases}".repeat(2_000);
    assert_eq!(render_latex(&deep_environments, RenderLatexOptions::default()), None);

    // Unbalanced input never panics either.
    assert_eq!(render_latex("}".repeat(1_000).as_str(), RenderLatexOptions::default()), None);
}

#[test]
fn default_options_are_inline() {
    assert_eq!(RenderLatexOptions::default(), RenderLatexOptions { display: false });
    assert_eq!(
        render_latex(r"\frac{1}{2}", RenderLatexOptions::default()).as_deref(),
        Some("1/2")
    );
}
