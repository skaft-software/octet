//! Native, draft-revision-fenced autocomplete. Keyboard policy stays host-owned.

use octet_tern::wire::{Kind, Node, Props, Span};
use serde_json::json;

use super::{normal_editor_focused, sanitize_ordinary_surface_cell, ShellState};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Slash,
    Path,
    Extension,
}

pub(super) struct Completion {
    pub(super) id: String,
    pub(super) source: Source,
    pub(super) selected: usize,
    pub(super) entries: Vec<(String, String)>,
}

impl Completion {
    pub(super) fn capture(shell: &ShellState) -> Option<Self> {
        Self::capture_revision(shell, shell.editor.revision())
    }

    // The retained renderer has a private editor with its own mutation count.
    // Gesture IDs and extension fences must use the accepted frontend revision.
    pub(super) fn capture_revision(shell: &ShellState, revision: u64) -> Option<Self> {
        if !normal_editor_focused(shell) || shell.startup_pending {
            return None;
        }
        let (source, selected, entries) = if let Some(overlay) =
            shell.extension_autocomplete.as_ref().filter(|overlay| {
                overlay.revision == revision
                    && overlay.text == shell.editor.text()
                    && overlay.cursor == shell.editor.cursor()
                    && !overlay.items.is_empty()
            }) {
            let selected = shell
                .extension_autocomplete_selection
                .filter(|(revision, _)| *revision == overlay.revision)
                .map_or(0, |(_, index)| index);
            (
                Source::Extension,
                selected,
                overlay
                    .items
                    .iter()
                    .map(|item| {
                        (
                            item.label.clone(),
                            item.description.clone().unwrap_or_default(),
                        )
                    })
                    .collect(),
            )
        } else {
            let slash = super::input_overlays::input_slash_suggestions(shell);
            if shell.slash_popup_dismissed && !slash.is_empty() {
                return None;
            }
            if !slash.is_empty() {
                (
                    Source::Slash,
                    shell.slash_selection,
                    slash
                        .into_iter()
                        .map(|entry| {
                            let label = format!(
                                "/{}{}",
                                entry.name,
                                entry
                                    .argument_hint
                                    .map(|hint| format!(" {hint}"))
                                    .unwrap_or_default()
                            );
                            (label, entry.description)
                        })
                        .collect(),
                )
            } else {
                (
                    Source::Path,
                    shell.path_selection,
                    super::input_overlays::input_path_suggestions(shell)
                        .into_iter()
                        .map(|entry| {
                            (
                                entry.completion,
                                if entry.is_dir {
                                    "directory".into()
                                } else {
                                    String::new()
                                },
                            )
                        })
                        .collect(),
                )
            }
        };
        let entries: Vec<(String, String)> = entries;
        if entries.is_empty() {
            return None;
        }
        let kind = match source {
            Source::Slash => "slash",
            Source::Path => "path",
            Source::Extension => "extension",
        };
        Some(Self {
            id: format!("completion.{revision}.{kind}"),
            source,
            selected: selected.min(entries.len() - 1),
            entries,
        })
    }

    pub(super) fn item_id(&self, index: usize) -> String {
        format!("{}.item.{index}", self.id)
    }

    pub(super) fn index(&self, item: &str) -> Option<usize> {
        let index = item
            .strip_prefix(&format!("{}.item.", self.id))?
            .parse()
            .ok()?;
        (index < self.entries.len()).then_some(index)
    }

    pub(super) fn node(&self, shell: &ShellState) -> Node {
        // A bounded native list, anchored above the draft rather than consuming
        // dock height with pane-wide, pre-rendered ANSI migration rows.
        let start = self.selected.saturating_sub(7);
        let items = self
            .entries
            .iter()
            .enumerate()
            .skip(start)
            .take(8)
            .map(|(index, (label, detail))| {
                Node::new(
                    self.item_id(index),
                    Kind::Item,
                    Props::new()
                        .text(
                            "label",
                            vec![Span::styled(
                                sanitize_ordinary_surface_cell(label, shell.theme.unicode()),
                                "mono",
                            )],
                        )
                        .text(
                            "detail",
                            sanitize_ordinary_surface_cell(detail, shell.theme.unicode()),
                        ),
                )
            })
            .collect();
        Node::with_children(
            format!("{}.overlay", self.id),
            Kind::Overlay,
            Props::new()
                .role("octet.autocomplete")
                .set("anchor", json!({"node":"composer", "side":"above"}))
                .set("size", "md")
                .set("modal", false),
            vec![Node::with_children(
                &self.id,
                Kind::List,
                Props::new()
                    .set("selected", self.item_id(self.selected))
                    .set("max", json!({"lines":8})),
                items,
            )],
        )
    }
}
