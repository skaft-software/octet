//! Native, draft-revision-fenced autocomplete. Keyboard policy stays host-owned.

use octet_tern::wire::{Kind, Node, Props, Span};
use serde_json::json;

use super::{normal_editor_focused, sanitize_ordinary_surface_cell, ShellState};

pub(super) const MAX_LINES: usize = 8;

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
        // Send every candidate and let Tern bound the viewport above the draft,
        // keeping late entries reachable without pre-rendered ANSI rows.
        let items = self
            .entries
            .iter()
            .enumerate()
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
                    .set("max", json!({"lines":MAX_LINES})),
                items,
            )],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::{PromptTemplateDescriptor, PromptTrust};

    fn assert_complete_list(shell: &ShellState, completion: &Completion) {
        let overlay = completion.node(shell);
        let list = &overlay.c.as_ref().unwrap()[0];
        assert_eq!(list.k, Kind::List);
        let props = list.p.as_ref().unwrap().as_map();
        assert_eq!(props["max"], json!({"lines": MAX_LINES}));
        assert_eq!(props["selected"], completion.item_id(completion.selected));
        let items = list.c.as_ref().unwrap();
        assert_eq!(items.len(), completion.entries.len());
        for (index, (item, (label, detail))) in items.iter().zip(&completion.entries).enumerate() {
            assert_eq!(item.id, completion.item_id(index));
            assert_eq!(completion.index(&item.id), Some(index));
            let props = item.p.as_ref().unwrap().as_map();
            assert_eq!(props["label"][0]["t"], *label);
            assert_eq!(props["detail"], *detail);
        }
    }

    #[test]
    fn native_slash_list_keeps_every_builtin_and_dynamic_candidate() {
        let mut shell = ShellState::default();
        shell.editor.set_text("/");
        shell.prompt_templates = vec![PromptTemplateDescriptor {
            name: "late-prompt".into(),
            description: "Prompt template".into(),
            argument_hint: Some("[topic]".into()),
            path: "late-prompt.md".into(),
            trust: PromptTrust::UserInstalled,
            content_hash: "fixture".into(),
        }]
        .into();
        shell.skill_commands = vec![("skill:late".into(), "Skill".into())].into();
        shell.extension_commands = vec![("late-extension".into(), "Extension".into())].into();
        let suggestions = super::super::input_overlays::input_slash_suggestions(&shell);
        let builtin_count = crate::commands::slash_suggestions("/").len();
        assert!(builtin_count > MAX_LINES);
        assert_eq!(suggestions.len(), builtin_count + 3);
        for selected in [0, builtin_count - 1, suggestions.len() - 1] {
            shell.slash_selection = selected;
            let completion = Completion::capture(&shell).unwrap();
            assert!(matches!(completion.source, Source::Slash));
            assert_eq!(completion.selected, selected);
            assert_eq!(completion.entries.len(), suggestions.len());
            for (entry, suggestion) in completion.entries.iter().zip(&suggestions) {
                assert!(entry.0.starts_with(&format!("/{}", suggestion.name)));
                assert_eq!(entry.1, suggestion.description);
            }
            assert_eq!(completion.entries[builtin_count].0, "/late-prompt [topic]");
            assert_eq!(completion.entries[builtin_count + 1].0, "/skill:late");
            assert_eq!(completion.entries[builtin_count + 2].0, "/late-extension");
            assert_complete_list(&shell, &completion);
        }
    }

    #[test]
    fn native_filtered_slash_list_keeps_matches_beyond_its_viewport() {
        let mut shell = ShellState::default();
        shell.editor.set_text("/native-");
        shell.extension_commands = (0..12)
            .map(|index| (format!("native-{index}"), format!("Command {index}")))
            .collect::<Vec<_>>()
            .into();
        shell.slash_selection = 10;
        let completion = Completion::capture(&shell).unwrap();
        assert_eq!(completion.entries.len(), 12);
        assert_eq!(completion.entries[10].0, "/native-10");
        assert_complete_list(&shell, &completion);

        shell.editor.set_text("/native-1");
        let filtered = Completion::capture(&shell).unwrap();
        assert_ne!(filtered.id, completion.id);
        assert_eq!(filtered.selected, 2);
        assert_eq!(
            filtered
                .entries
                .iter()
                .map(|entry| entry.0.as_str())
                .collect::<Vec<_>>(),
            ["/native-1", "/native-10", "/native-11"]
        );
        assert_complete_list(&shell, &filtered);
    }
}
