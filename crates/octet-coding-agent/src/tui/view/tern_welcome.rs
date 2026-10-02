//! Octet identity in Tern's native welcome layout; immutable, addressed SVG bytes.

use base64::{engine::general_purpose::STANDARD, Engine};
use octet_tern::{
    client::TernClient,
    wire::{Kind, Node, Props, Span},
};
use sha2::{Digest, Sha256};
use std::io;

use super::{terminal_text::sanitize_for_terminal as safe, ShellState};

#[derive(Default)]
pub(super) struct Brand {
    address: Option<String>,
}

impl Brand {
    pub(super) fn prepare(
        &mut self,
        shell: &ShellState,
        client: &mut TernClient,
    ) -> io::Result<()> {
        if !client.supports(Kind::Image) || shell.startup_card_started_at.is_none() {
            self.address = None;
            return Ok(());
        }
        let adaptive = shell.theme.is_compiled_default()
            || shell
                .theme
                .resolve::<bool>("splash_model_adaptive")
                .unwrap_or(false);
        let accent = adaptive
            .then(|| shell.theme.model_rgb(shell.model_lab))
            .flatten();
        let solid = (!adaptive)
            .then(|| shell.theme.role_rgb("splash"))
            .flatten();
        let svg = crate::tui::splash::native_svg(accent, solid);
        let address = format!("{:x}", Sha256::digest(svg.as_bytes()));
        if self.address.as_ref() != Some(&address) {
            // Hash and upload the very same in-memory bytes. No mutable asset path.
            client.blob(&address, "image/svg+xml", &STANDARD.encode(svg.as_bytes()))?;
            self.address = Some(address);
        }
        Ok(())
    }

    fn mark(&self) -> Node {
        if let Some(address) = &self.address {
            Node::new(
                "welcome.byte",
                Kind::Image,
                Props::new()
                    .role("omp.welcome.logo")
                    .set("blob", address)
                    .set("alt", "octet byte mark: 01101111")
                    .set("w", 128)
                    .set("h", 32),
            )
        } else {
            Node::new(
                "welcome.byte",
                Kind::Text,
                Props::new().text("spans", vec![Span::styled("01101111", "accent mono")]),
            )
        }
    }
}

pub(super) fn node(shell: &ShellState, brand: &Brand) -> Node {
    let text = |id: &str, text: String, token: &str| {
        Node::new(
            id,
            Kind::Text,
            Props::new().text("spans", vec![Span::styled(text, token)]),
        )
    };
    let session = vec![
        text(
            "welcome.model",
            safe(crate::presentation::model::footer_model_name(
                &shell.model_display,
                &shell.model,
            )),
            "accent",
        ),
        Node::with_children(
            "welcome.effort",
            Kind::Row,
            Props::new().set("gap", "sm").set("align", "center"),
            vec![
                Node::new(
                    "welcome.effort.glyph",
                    Kind::Effort,
                    Props::new().set("level", &shell.reasoning),
                ),
                text(
                    "welcome.effort.label",
                    format!(
                        "{} effort",
                        if shell.reasoning.is_empty() {
                            "off"
                        } else {
                            &shell.reasoning
                        }
                    ),
                    "muted",
                ),
            ],
        ),
        text(
            "welcome.access",
            if shell.safe_mode {
                "safe mode · approvals required"
            } else {
                "full access · no sandbox"
            }
            .into(),
            if shell.safe_mode {
                "success"
            } else {
                "warning"
            },
        ),
    ];
    let tips = [
        ("/", "commands"),
        ("@", "files"),
        ("!", "shell"),
        ("ctrl+o", "details"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (key, label))| {
        Node::with_children(
            format!("welcome.tip{index}"),
            Kind::Row,
            Props::new().set("gap", "sm"),
            vec![
                Node::new(
                    format!("welcome.tip{index}.key"),
                    Kind::Kbd,
                    Props::new().set("keys", [key]),
                ),
                text(&format!("welcome.tip{index}.label"), label.into(), "muted"),
            ],
        )
    })
    .collect();
    Node::with_children("welcome", Kind::Card, Props::new().role("omp.welcome"), vec![
        Node::with_children("welcome.grid", Kind::Row, Props::new().role("omp.welcome.grid").set("align", "start").set("wrap", true), vec![
            Node::with_children("welcome.brand", Kind::Col, Props::new().role("omp.welcome.brand").set("align", "center"), vec![
                Node::new("welcome.name", Kind::Text, Props::new().role("omp.welcome.greeting").set("text", "octet")),
                brand.mark(),
            ]),
            Node::with_children("welcome.info", Kind::Col, Props::new().role("omp.welcome.info").set("gap", "md"), vec![
                Node::new("welcome.version", Kind::Text, Props::new().role("omp.welcome.version").text("spans", vec![Span::styled(format!("v{}", env!("CARGO_PKG_VERSION")), "dim mono")])),
                Node::with_children("welcome.session", Kind::Col, Props::new().role("omp.welcome.tips").set("gap", "xs"), session),
                Node::with_children("welcome.tips", Kind::Col, Props::new().role("omp.welcome.tips").set("gap", "xs"), tips),
            ]),
        ]),
        Node::with_children("welcome.hint", Kind::Row, Props::new().role("omp.welcome.tip").set("gap", "sm"), vec![
            Node::new("welcome.hint.icon", Kind::Icon, Props::new().role("omp.welcome.tip-icon").set("name", "lightbulb")),
            Node::new("welcome.hint.text", Kind::Text, Props::new().role("omp.welcome.tip-text").set("text", "Ask octet about its features or how to extend it. /changelog shows what's new.")),
        ]),
    ])
}
