//! Compact conversation-local identity in Tern; immutable, addressed PNG bytes.

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
        let png = crate::tui::splash::native_png(accent, solid)?;
        let address = format!("{:x}", Sha256::digest(&png));
        if self.address.as_ref() != Some(&address) {
            // Hash and upload the very same bounded in-memory bytes. No asset path.
            client.blob(&address, "image/png", &STANDARD.encode(&png))?;
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
                    .role("octet.welcome.logo")
                    .set("blob", address)
                    .set("alt", "octet byte mark: 01101111")
                    .set("w", 40)
                    .set("h", 20),
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
    let text = |id: &str, spans: Vec<Span>| {
        Node::new(
            id,
            Kind::Text,
            Props::new().text("spans", spans).set("wrap", "word"),
        )
    };
    let release = if let Some(version) = &shell.available_update {
        vec![
            Span::styled(safe(&format!("v{version} available · run ")), "accent"),
            Span::styled("octet update", "accent mono"),
        ]
    } else {
        vec![
            Span::styled("/changelog", "dim mono"),
            Span::styled(" · what's new", "dim"),
        ]
    };
    // RAIL is one wrapping row in the conversation, not a centered startup card.
    // OMP's private welcome hooks impose their own logo sizing and animation.
    // Model, effort, context and input remain exclusively in the native composer.
    Node::with_children(
        "welcome",
        Kind::Row,
        Props::new()
            .role("octet.welcome")
            .set("align", "center")
            .set("gap", "sm")
            .set("wrap", true),
        vec![
            brand.mark(),
            text("welcome.name", vec![Span::styled("octet", "strong")]),
            text(
                "welcome.version",
                vec![Span::styled(
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                    "dim mono",
                )],
            ),
            text("welcome.release", release),
            text(
                "welcome.access",
                vec![Span::styled(
                    if shell.safe_mode {
                        "safe mode · approvals required"
                    } else {
                        "full access · no sandbox"
                    },
                    if shell.safe_mode {
                        "success"
                    } else {
                        "warning"
                    },
                )],
            ),
            text(
                "welcome.tips",
                vec![Span::styled(
                    "/ commands · @ files · ! shell · ctrl+o details",
                    "muted",
                )],
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{
        terminal::{ColorDepth, TerminalCapabilities},
        theme::{test_theme_for, test_theme_from_source, ModelLab, TerminalBackground},
        view::InteractiveShell,
    };
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };

    #[derive(Clone, Default)]
    struct Output(Arc<Mutex<Vec<String>>>);
    impl io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push(String::from_utf8(bytes.to_vec()).unwrap());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Output {
        fn blobs(&self) -> Vec<octet_tern::frame::Raw> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter_map(|wire| octet_tern::frame::split(wire).filter(|raw| raw.verb == "b"))
                .collect()
        }
    }

    fn child<'a>(node: &'a Node, id: &str) -> &'a Node {
        node.c
            .as_ref()
            .unwrap()
            .iter()
            .find(|node| node.id == id)
            .unwrap()
    }

    fn plain(node: &Node) -> String {
        node.p.as_ref().unwrap().as_map()["spans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|span| span["t"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn rail_is_one_compact_wrapping_row_without_composer_controls_or_startup_whitespace() {
        let shell = InteractiveShell::test_shell();
        let brand = Brand {
            address: Some("a".repeat(64)),
        };
        for cols in [12, 20, 46, 120, 240] {
            shell.state.borrow_mut().size = (cols, 40);
            let rail = node(&shell.state.borrow(), &brand);
            assert_eq!(rail.k, Kind::Row);
            let props = rail.p.as_ref().unwrap().as_map();
            assert_eq!(props["role"], "octet.welcome");
            assert_eq!(props["gap"], "sm");
            assert_eq!(props["wrap"], true);
            assert_eq!(props["align"], "center");
            let children = rail.c.as_ref().unwrap();
            assert_eq!(children.len(), 6);
            assert!(children.iter().all(|node| node.c.is_none()));
            assert!(children
                .iter()
                .all(|node| matches!(node.k, Kind::Text | Kind::Image)));
            let mark = child(&rail, "welcome.byte");
            assert_eq!(mark.k, Kind::Image);
            let props = mark.p.as_ref().unwrap().as_map();
            assert_eq!(props["w"], 40);
            assert_eq!(props["h"], 20);
            assert_eq!(props["alt"], "octet byte mark: 01101111");
            assert_eq!(plain(child(&rail, "welcome.name")), "octet");
            assert_eq!(
                plain(child(&rail, "welcome.version")),
                format!("v{}", env!("CARGO_PKG_VERSION"))
            );
            for text in children.iter().filter(|node| node.k == Kind::Text) {
                assert_eq!(text.p.as_ref().unwrap().as_map()["wrap"], "word");
                assert!(!plain(text).contains('\n'));
            }
            let json = serde_json::to_string(&rail).unwrap();
            for removed in [
                "omp.",
                "welcome.model",
                "welcome.effort",
                "composer",
                "fontSize",
                "padding",
                "spacer",
            ] {
                assert!(
                    !json.contains(removed),
                    "unexpected startup duplication: {removed}"
                );
            }
        }
    }

    #[test]
    fn rail_retains_permission_and_discovery_hints_and_replaces_changelog_with_real_update() {
        let shell = InteractiveShell::test_shell();
        let brand = Brand::default();
        for safe_mode in [false, true] {
            shell.state.borrow_mut().safe_mode = safe_mode;
            let rail = node(&shell.state.borrow(), &brand);
            let access = child(&rail, "welcome.access");
            assert_eq!(
                plain(access),
                if safe_mode {
                    "safe mode · approvals required"
                } else {
                    "full access · no sandbox"
                }
            );
            assert_eq!(
                access.p.as_ref().unwrap().as_map()["spans"][0]["s"],
                if safe_mode { "success" } else { "warning" }
            );
            assert!(plain(child(&rail, "welcome.release")).contains("/changelog"));
            let tips = plain(child(&rail, "welcome.tips"));
            for hint in ["/ commands", "@ files", "! shell", "ctrl+o details"] {
                assert!(tips.contains(hint));
            }
            shell.state.borrow_mut().available_update = Some(semver::Version::new(9, 8, 7));
            let updated = node(&shell.state.borrow(), &brand);
            assert_eq!(
                plain(child(&updated, "welcome.release")),
                "v9.8.7 available · run octet update"
            );
            assert!(!serde_json::to_string(&updated)
                .unwrap()
                .contains("/changelog"));
            assert_eq!(
                rail.c.as_ref().unwrap().len(),
                updated.c.as_ref().unwrap().len()
            );
            assert_eq!(
                child(&rail, "welcome.byte"),
                child(&updated, "welcome.byte")
            );
            shell.state.borrow_mut().available_update = None;
        }
    }

    #[test]
    fn brand_uploads_identical_bounded_png_bytes_once_and_preserves_text_fallback() {
        let shell = InteractiveShell::test_shell();
        shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
        let output = Output::default();
        let mut client = TernClient::with_writer("test", None, &[], output.clone()).unwrap();
        let mut brand = Brand::default();
        brand.prepare(&shell.state.borrow(), &mut client).unwrap();
        brand.prepare(&shell.state.borrow(), &mut client).unwrap();
        let blobs = output.blobs();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].params["mime"], "image/png");
        let bytes = STANDARD.decode(&blobs[0].body).unwrap();
        assert!(bytes.len() < 4096);
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            blobs[0].params["id"],
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert_eq!(
            brand.mark().p.as_ref().unwrap().as_map()["blob"],
            blobs[0].params["id"]
        );
        // A freshly reopened native surface needs the exact same blob again.
        let mut reopened = Brand::default();
        reopened
            .prepare(&shell.state.borrow(), &mut client)
            .unwrap();
        assert_eq!(output.blobs()[1], blobs[0]);
        let hello =
            serde_json::from_value(serde_json::json!({"v":1,"term":"test","kinds":["row","text"]}))
                .unwrap();
        client.apply_hello(&hello);
        brand.prepare(&shell.state.borrow(), &mut client).unwrap();
        let fallback = brand.mark();
        assert_eq!(fallback.k, Kind::Text);
        assert_eq!(plain(&fallback), "01101111");
        assert_eq!(output.blobs().len(), 2);
    }

    #[test]
    fn brand_uses_background_balanced_model_colors_and_custom_theme_precedence() {
        let output = Output::default();
        let mut client = TernClient::with_writer("test", None, &[], output.clone()).unwrap();
        let mut brand = Brand::default();
        let mut addresses = Vec::new();
        for background in [TerminalBackground::Light, TerminalBackground::Dark] {
            let theme = test_theme_for(
                background,
                TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            );
            let shell = InteractiveShell::test_shell_with_theme(theme);
            shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
            for lab in [ModelLab::OpenAi, ModelLab::Anthropic] {
                shell.state.borrow_mut().model_lab = Some(lab);
                let state = shell.state.borrow();
                brand.prepare(&state, &mut client).unwrap();
                let expected =
                    crate::tui::splash::native_png(state.theme.model_rgb(Some(lab)), None).unwrap();
                assert_eq!(
                    brand.address.as_ref().unwrap(),
                    &format!("{:x}", Sha256::digest(expected))
                );
                addresses.push(brand.address.clone().unwrap());
            }
        }
        let distinct = addresses.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(
            distinct.len(),
            4,
            "appearance and model must both adapt the mark"
        );
        for adaptive in [false, true] {
            let theme = test_theme_from_source(&format!("[metadata]\nname = 'Rail fixture'\n[colors]\nsplash = '#d97757'\nsplash_model_adaptive = {adaptive}\n"));
            let shell = InteractiveShell::test_shell_with_theme(theme);
            shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
            shell.state.borrow_mut().model_lab = Some(ModelLab::OpenAi);
            let state = shell.state.borrow();
            brand.prepare(&state, &mut client).unwrap();
            let expected = if adaptive {
                crate::tui::splash::native_png(state.theme.model_rgb(state.model_lab), None)
            } else {
                crate::tui::splash::native_png(None, Some((217, 119, 87)))
            }
            .unwrap();
            assert_eq!(
                brand.address.as_ref().unwrap(),
                &format!("{:x}", Sha256::digest(expected))
            );
        }
    }
}
