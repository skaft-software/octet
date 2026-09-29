//! Wrapping and overlay painting, markdown and rich-text transcript shapes, the slash menu and
//! inline autocomplete, model switching, and mention/path completion in the composer. Separate
//! because they all assert which text reaches which composer cell.

use super::support::*;

use super::*;

#[test]
fn plain_wrapping_is_nonempty_for_empty_text() {
    assert_eq!(wrap_plain("", 10), vec![String::new()]);
}

#[test]
fn wrapped_truecolor_never_reopens_rgb_components_as_backgrounds() {
    let mut theme = crate::tui::theme::test_theme();
    theme.override_token("accent", "#16846b");
    let styled = theme.fg("accent", "alpha beta gamma");
    assert!(styled.contains(";107m"));

    let wrapped = wrap_text_with_ansi(&styled, 6);
    assert!(wrapped.len() > 1);
    assert!(!wrapped.iter().any(|line| line.contains("\x1b[107m")));
    assert!(!wrapped.iter().any(|line| line.contains("\x1b[38;2m")));
    assert!(wrapped.join("").contains("\x1b[38;2;22;132;107m"));
}

#[test]
fn styled_overlay_wraps_by_visible_width_without_splitting_ansi() {
    let theme = sexy_tui_rs::theme::Theme::load(
        None,
        sexy_tui_rs::theme::capability::CapabilityTier::Baseline,
    );
    let selected = format!(
        "{} — {}",
        theme.bold(&theme.fg("accent", "gpt-audio-1.5")),
        theme.fg("muted", "gpt-audio-1.5")
    );
    // This is 29 visible cells but 82 raw characters. At an 80-column
    // terminal the old raw-character wrapper split off the final reset as
    // a literal `[39m` line.
    assert_eq!(visible_width(&selected), 29);
    let wrapped = wrap_text_with_ansi(&selected, 78);
    assert_eq!(wrapped, vec![selected.clone()]);

    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 20);
    shell.show_styled_overlay_text(selected);
    let rendered = render_shell(&shell.state.borrow(), 80);
    assert_eq!(
        rendered
            .iter()
            .filter(|line| line.contains("gpt-audio-1.5"))
            .count(),
        1,
        "one styled item must occupy one overlay row at 80 columns"
    );
    assert!(rendered.iter().any(|line| line.contains(CURSOR_MARKER)));
    assert!(!rendered.iter().any(|line| line == "[39m"));
}

#[test]
fn markdown_transcript_renders_common_headings_lists_code_and_rules() {
    let theme = crate::tui::theme::test_theme();
    let rendered = markdown_lines(
        "### 🔍 **Read & Search**\n- **`read`** — inspect a file\n\n---",
        &theme,
        80,
    )
    .join("\n");
    for marker in ["###", "**", "`", "---"] {
        assert!(!rendered.contains(marker), "marker {marker:?} leaked");
    }
    assert!(rendered.contains("Read & Search"));
    assert!(rendered.contains("read"));
    assert!(rendered.contains('—'));
    assert!(rendered.contains('─'));
}

#[test]
fn rich_text_renders_gfm_tables_tasks_links_and_fenced_code() {
    let theme = crate::tui::theme::test_theme();
    let rendered = markdown_lines(
            "- [x] migrated\n\n| Name | State |\n| --- | --- |\n| TUI | ready |\n\n[docs](https://example.com)\n\n```rust\nfn main() {}\n```",
            &theme,
            80,
        )
        .join("\n");
    assert!(rendered.contains("[x]"), "{rendered}");
    assert!(rendered.contains("migrated"), "{rendered}");
    assert!(rendered.contains("Name"));
    assert!(rendered.contains("ready"));
    assert!(rendered.contains("https://example.com"));
    assert!(rendered.contains("fn"));
    assert!(!rendered.contains("```"));
}

#[test]
fn slash_popup_event_path_keeps_arrow_navigation_active() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Char('/'));
    let total = commands::slash_suggestions("/").len();
    for expected in 1..total {
        let pending = shell.pending();
        let action = crate::tui::keymap::translate_with_popup(
            Some(crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Down,
                    crossterm::event::KeyModifiers::NONE,
                ),
            )),
            false,
            &pending,
            shell.slash_popup_open(),
        );
        assert_eq!(
            action,
            crate::tui::keymap::InputAction::SlashMenu(SlashMenuAction::Next)
        );
        shell.slash_menu(SlashMenuAction::Next);
        assert_eq!(shell.state.borrow().slash_selection, expected);
    }
}

#[test]
fn login_and_setup_are_discoverable_from_either_partial_query() {
    for query in ["/logi", "/setu"] {
        let mut shell = InteractiveShell::test_shell();
        for character in query.chars() {
            shell.apply_edit(EditAction::Char(character));
        }
        let popup = render_slash_suggestions(&shell.state.borrow(), 120, 100).join("\n");
        assert!(
            popup.contains("/login") && popup.contains("/setup"),
            "{query}: {popup}"
        );
        shell.complete_slash_command();
        assert_eq!(
            shell.pending(),
            query,
            "ambiguous completion must not choose for the user"
        );
    }
}

#[test]
fn slash_command_menu_lists_commands_and_tab_completes_a_unique_prefix() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Char('/'));
    let rendered = render_slash_suggestions(&shell.state.borrow(), 120, 100);
    for command in ["/new", "/model", "/login", "/cost"] {
        assert!(rendered.iter().any(|line| line.contains(command)));
    }
    let popup = rendered.join("\n");
    assert!(popup.contains("commands"));
    assert!(!popup.contains("Session"));
    assert!(!popup.contains("opens picker"));
    assert!(popup.contains("/help"));
    for removed in ["/tool", "/docs", "/sessions", "/cycle-model"] {
        assert!(!popup.contains(removed), "{removed} remained in {popup}");
    }
    assert!(popup.contains("› /new"));
    assert_input_suggestions_replace_status_footer(&mut shell, "commands");

    shell.slash_menu(SlashMenuAction::Last);
    let scrolled = render_slash_suggestions(&shell.state.borrow(), 80, 7).join("\n");
    assert!(scrolled.contains("/exit"), "{scrolled}");
    assert!(scrolled.contains('/'), "{scrolled}");

    shell.slash_menu(SlashMenuAction::First);
    shell.slash_menu(SlashMenuAction::Next);
    let selected = shell.slash_menu(SlashMenuAction::Select);
    assert!(selected);
    assert_eq!(shell.pending(), "/resume ");
    assert!(!shell.slash_popup_open());
    let restored = shell_chrome(&shell.state.borrow(), 120, Instant::now());
    assert!(restored.suggestions.is_empty());
    assert!(restored
        .composer
        .iter()
        .any(|line| strip_terminal_sequences(line).contains("0%/272K")));

    shell.drain_editor();
    shell.apply_edit(EditAction::Char('/'));
    for character in "mod".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    shell.complete_slash_command();
    assert_eq!(shell.pending(), "/model ");
}

#[test]
fn inline_autocomplete_uses_compact_footers_and_the_model_accent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), b"x").unwrap();

    let mut shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        crate::tui::theme::apply_model_lab(&mut state.theme, ModelLab::Anthropic);
        state.model_lab = Some(ModelLab::Anthropic);
    }
    let model_accent = {
        let state = shell.state.borrow();
        let (red, green, blue) = state
            .theme
            .role_rgb("model_accent")
            .expect("active model accent");
        format!("\x1b[38;2;{red};{green};{blue}m")
    };
    let ui_accent = {
        let state = shell.state.borrow();
        let (red, green, blue) = state.theme.role_rgb("accent").expect("UI accent");
        format!("\x1b[38;2;{red};{green};{blue}m")
    };
    assert_ne!(model_accent, ui_accent);

    shell.apply_edit(EditAction::Char('/'));
    let slash = render_slash_suggestions(&shell.state.borrow(), 120, 6);
    assert_eq!(slash.len(), 6, "{slash:?}");
    let selected = slash.first().expect("selected slash suggestion");
    assert!(
        strip_terminal_sequences(selected).starts_with("› /new"),
        "{slash:?}"
    );
    assert!(selected.contains(&model_accent), "{selected:?}");
    assert!(!selected.contains(&ui_accent), "{selected:?}");
    let unselected = slash
        .iter()
        .find(|line| line.contains("/resume"))
        .expect("unselected slash suggestion");
    assert!(!unselected.contains(&model_accent), "{unselected:?}");
    let footer = slash.last().expect("slash suggestion footer");
    let plain_footer = strip_terminal_sequences(footer);
    assert!(
        plain_footer.contains("commands 1–5/")
            && plain_footer.contains("↑↓ navigate · ↵ select · esc close"),
        "{plain_footer:?}"
    );
    assert!(footer.contains(&model_accent), "{footer:?}");
    assert!(!footer.contains(&ui_accent), "{footer:?}");

    shell.drain_editor();
    shell.set_workspace(dir.path().to_path_buf());
    for character in "see @main".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    // The index is walked off-thread; the popup needs the finished walk.
    shell.settle_file_index();
    let paths = shell_chrome(&shell.state.borrow(), 120, Instant::now()).suggestions;
    // Rendered separators are platform-native (`/` on Unix, `\` on Windows).
    let expected_mention = format!("› src{}main.rs", std::path::MAIN_SEPARATOR);
    let selected = paths
        .iter()
        .find(|line| {
            let plain = strip_terminal_sequences(line);
            plain.contains("src/main.rs") || plain.contains("src\\main.rs")
        })
        .expect("selected mention suggestion");
    assert_eq!(strip_terminal_sequences(selected).trim(), expected_mention);
    assert!(selected.contains(&model_accent), "{selected:?}");
    assert!(!selected.contains(&ui_accent), "{selected:?}");
    let footer = paths.last().expect("mention suggestion footer");
    assert_eq!(
        strip_terminal_sequences(footer).trim(),
        "project files · tab complete · ↑↓ navigate"
    );
    assert!(footer.contains(&model_accent), "{footer:?}");
    assert!(!footer.contains(&ui_accent), "{footer:?}");
}

#[test]
fn slash_palette_shares_the_composer_grid_at_narrow_and_wide_widths() {
    for width in [32_u16, 80, 120] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(width, 20);
        shell.apply_edit(EditAction::Char('/'));

        let state = shell.state.borrow();
        let plan = crate::tui::layout::PresentationLayout::new(&state.theme, width);
        let slash = render_slash_suggestions(&state, width, 6);
        let selected = slash.first().expect("selected slash command");
        let selected = strip_terminal_sequences(selected);
        let composer =
            crate::tui::composer_surface::render_composer_surface(&state, width, Instant::now());
        let composer_prompt = composer
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .find(|line| line.contains("› /"))
            .expect("composer prompt row");

        assert_eq!(
            plan.inset, 0,
            "default surfaces must reach the terminal edge at width {width}"
        );
        assert_eq!(
            selected.find('›'),
            Some(usize::from(plan.inset)),
            "slash palette width {width}: {selected:?}"
        );
        assert_eq!(
            composer_prompt.find('›'),
            Some(usize::from(plan.inset)),
            "composer width {width}: {composer_prompt:?}"
        );
        let command_byte = selected.find('/').expect("slash command name");
        assert_eq!(
            visible_width(&selected[..command_byte]),
            2,
            "slash command names belong on the shared primary text column: {selected:?}"
        );
        assert!(
            visible_width(&selected) <= usize::from(plan.inset + plan.content_width),
            "slash palette exceeded composer right edge at width {width}: {selected:?}"
        );
    }
}

#[test]
fn composer_always_uses_the_model_selected_for_the_next_prompt() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("anthropic", "claude-sonnet-4", "high");
    let now = Instant::now();
    let idle =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 80, now);
    shell.apply_edit(EditAction::Char('x'));
    let focused =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 80, now);
    let run_id = shell.begin_run("anthropic");
    let active =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 80, now);

    let accent = shell
        .state
        .borrow()
        .theme
        .model_rgb(Some(ModelLab::Anthropic))
        .expect("Anthropic model accent");
    let encoded = format!("38;2;{};{};{}", accent.0, accent.1, accent.2);
    for surface in [&idle, &focused, &active] {
        assert!(surface.join("\n").contains(&encoded), "{surface:?}");
    }
    assert_eq!(idle[0], focused[0]);
    assert_eq!(focused[0], active[0]);
    shell.interrupt_run(run_id);
}

#[test]
fn model_switch_recolors_only_the_composer_and_future_prompt() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_theme(crate::tui::theme::test_theme_for(
        TerminalBackground::Dark,
        crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::TrueColor,
        ),
    ));
    shell.set_identity("anthropic", "claude-sonnet-4", "high");
    shell.on_prompt_submitted("prompt for Claude");
    shell.set_identity("local", "qwen3.6-27b", "high");
    shell.on_prompt_submitted("prompt for Qwen");

    let before_switch = shell
        .state
        .borrow()
        .transcript
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::User {
                text, prompt_color, ..
            } => Some((text.clone(), prompt_color.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_ne!(before_switch[0].1, before_switch[1].1);

    shell.set_identity("openai", "gpt-5.6", "high");
    let after_switch = shell
        .state
        .borrow()
        .transcript
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::User {
                text, prompt_color, ..
            } => Some((text.clone(), prompt_color.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(after_switch, before_switch);

    let rendered = shell.state.borrow().rendered_transcript(80).join("\n");
    for (_, color) in &before_switch {
        let color = color.as_deref().expect("model prompt colour");
        let sample = shell
            .state
            .borrow()
            .theme
            .prompt_provenance_card(Some(color), "x");
        let paint = emulate_rows(&[sample], 2)
            .screen()
            .cell(0, 0)
            .expect("sample highlight cell")
            .bgcolor();
        let vt100::Color::Rgb(red, green, blue) = paint else {
            panic!("known dark profile must paint prompt text: {paint:?}");
        };
        assert!(
            rendered.contains(&format!("48;2;{red};{green};{blue}")),
            "stored prompt highlight lost {color}: {rendered:?}"
        );
    }

    let state = shell.state.borrow();
    assert_eq!(state.model_lab, Some(ModelLab::OpenAi));
    let openai = state
        .theme
        .model_rgb(Some(ModelLab::OpenAi))
        .expect("OpenAI model accent");
    let composer =
        crate::tui::composer_surface::render_composer_surface(&state, 80, Instant::now())
            .join("\n");
    assert!(composer.contains(&format!("38;2;{};{};{}", openai.0, openai.1, openai.2)));
}

#[test]
fn discovered_prompt_templates_join_slash_autocomplete() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_prompt_templates(Arc::from(vec![crate::prompts::PromptTemplateDescriptor {
        name: "local-review".into(),
        description: "Focused local review".into(),
        argument_hint: Some("[focus]".into()),
        path: PathBuf::from("/tmp/local-review.md"),
        trust: crate::prompts::PromptTrust::UserInstalled,
        content_hash: "hash".into(),
    }]));
    for character in "/loc".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    let rendered = render_slash_suggestions(&shell.state.borrow(), 100, 10).join("\n");
    assert!(rendered.contains("/local-review [focus]"), "{rendered}");
    assert!(
        rendered.contains("prompt · Focused local review"),
        "{rendered}"
    );
    let narrow = render_slash_suggestions(&shell.state.borrow(), 32, 10).join("\n");
    assert!(narrow.contains("/local-review [focus]"), "{narrow}");
    shell.complete_slash_command();
    assert_eq!(shell.pending(), "/local-review ");
}

#[test]
fn dynamic_slash_discovery_contains_only_registered_executable_names() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_prompt_templates(Arc::from(vec![crate::prompts::PromptTemplateDescriptor {
        name: "local-review".into(),
        description: "Focused local review".into(),
        argument_hint: None,
        path: PathBuf::from("/tmp/local-review.md"),
        trust: crate::prompts::PromptTrust::UserInstalled,
        content_hash: "hash".into(),
    }]));
    shell.set_skill_commands(Arc::from(vec![(
        "workspace-review".into(),
        "Review workspace changes".into(),
    )]));
    shell.set_skill_commands(Arc::from(vec![
        ("workspace-review".into(), "Review workspace changes".into()),
        // A dynamic command cannot shadow a working built-in.
        ("status".into(), "Shadow status".into()),
    ]));
    shell.apply_edit(EditAction::Char('/'));

    let state = shell.state.borrow();
    let suggestions = input_slash_suggestions(&state);
    let prompt_names = state
        .prompt_templates
        .iter()
        .map(|template| template.name.as_str())
        .collect::<HashSet<_>>();
    let skill_names = state
        .skill_commands
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<HashSet<_>>();
    for suggestion in suggestions.iter().filter(|suggestion| {
        matches!(
            suggestion.provenance,
            super::input_overlays::SlashSuggestionProvenance::Prompt
                | super::input_overlays::SlashSuggestionProvenance::Skill
        )
    }) {
        let registered = match suggestion.provenance {
            super::input_overlays::SlashSuggestionProvenance::Prompt => {
                prompt_names.contains(suggestion.name.as_str())
            }
            super::input_overlays::SlashSuggestionProvenance::Skill => {
                skill_names.contains(suggestion.name.as_str())
            }
            super::input_overlays::SlashSuggestionProvenance::Builtin => {
                unreachable!("only dynamic slash suggestions should reach this registration check")
            }
        };
        assert!(registered, "unregistered suggestion: {suggestion:?}");
    }
    assert_eq!(
        suggestions
            .iter()
            .filter(|suggestion| suggestion.name == "status")
            .count(),
        1,
        "dynamic command shadowed the built-in route"
    );
    assert!(suggestions.iter().any(|suggestion| {
        suggestion.name == "local-review"
            && suggestion.provenance == super::input_overlays::SlashSuggestionProvenance::Prompt
    }));
    assert!(suggestions.iter().any(|suggestion| {
        suggestion.name == "workspace-review"
            && suggestion.provenance == super::input_overlays::SlashSuggestionProvenance::Skill
    }));
}

#[test]
fn mention_completion_inserts_path_reference_for_text_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), b"x").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(dir.path().to_path_buf());
    for character in "see @main".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    // The index is walked off-thread; completion needs the finished walk.
    shell.settle_file_index();
    let rendered = render_shell(&shell.state.borrow(), 120);
    assert!(rendered
        .iter()
        .any(|line| { strip_terminal_sequences(line).contains("project files · tab complete") }));
    // Rendered separators are platform-native.
    assert!(rendered
        .iter()
        .any(|line| { line.contains("src/main.rs") || line.contains("src\\main.rs") }));
    assert_input_suggestions_replace_status_footer(&mut shell, "project files");
    shell.complete_path();
    // The inserted reference keeps the platform-native spelling.
    #[cfg(windows)]
    assert_eq!(shell.pending(), "see @src\\main.rs ");
    #[cfg(not(windows))]
    assert_eq!(shell.pending(), "see @src/main.rs ");
}

#[test]
fn literal_path_completion_descends_through_directories() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), b"x").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(dir.path().to_path_buf());
    for character in "inspect ./sr".chars() {
        shell.apply_edit(EditAction::Char(character));
    }

    let rendered = render_shell(&shell.state.borrow(), 120);
    assert!(rendered
        .iter()
        .any(|line| strip_terminal_sequences(line).contains("paths · tab complete")));
    assert!(rendered.iter().any(|line| line.contains("./src/")));
    assert_input_suggestions_replace_status_footer(&mut shell, "paths");

    shell.complete_path();
    assert_eq!(shell.pending(), "inspect ./src/");
    shell.complete_path();
    assert_eq!(shell.pending(), "inspect ./src/main.rs ");
}

#[test]
fn literal_path_completion_escapes_spaces_and_stays_active() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("My Folder")).unwrap();
    std::fs::write(dir.path().join("My Folder/draft note.md"), b"text").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(dir.path().to_path_buf());
    for character in "inspect ./My".chars() {
        shell.apply_edit(EditAction::Char(character));
    }

    shell.complete_path();
    assert_eq!(shell.pending(), r"inspect ./My\ Folder/");
    shell.complete_path();
    assert_eq!(shell.pending(), r"inspect ./My\ Folder/draft\ note.md ");
}

#[test]
fn mention_path_completion_keeps_directories_active() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), b"x").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(dir.path().to_path_buf());
    for character in "@./sr".chars() {
        shell.apply_edit(EditAction::Char(character));
    }

    shell.complete_path();
    assert_eq!(shell.pending(), "@./src/");
    shell.complete_path();
    assert_eq!(shell.pending(), "@./src/lib.rs ");
}

#[test]
fn mention_completion_attaches_media_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("shot.png"), b"png").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(dir.path().to_path_buf());
    shell.set_input_modalities(octet_ai::ModalitySet::none().with(octet_ai::Modality::Image));
    for character in "@shot".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    // The index is walked off-thread; completion needs the finished walk.
    shell.settle_file_index();
    shell.complete_path();
    assert_eq!(shell.pending(), "[Image #1]");
    let composed = shell.drain_composed();
    assert!(composed
        .parts
        .iter()
        .any(|part| matches!(part, octet_agent::InputPart::Media(_))));
}

#[test]
fn set_workspace_keeps_file_index_and_layout_when_the_root_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), b"x").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(dir.path().to_path_buf());
    for character in "@a".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    shell.settle_file_index();
    let generation = {
        let state = shell.state.borrow();
        drop(state.rendered_transcript(80));
        let cache = state.transcript_cache.borrow();
        assert_eq!(cache.width, Some(80));
        assert!(!cache.dirty);
        cache.generation
    };
    assert!(shell.state.borrow().file_index.is_some());

    // Re-asserting the same root (update_status runs after every turn) must
    // preserve both the lazily built mention index and historic layout.
    shell.set_workspace(dir.path().to_path_buf());
    let state = shell.state.borrow();
    assert!(state.file_index.is_some());
    let cache = state.transcript_cache.borrow();
    assert_eq!(cache.width, Some(80));
    assert!(!cache.dirty);
    assert_eq!(cache.generation, generation);
    drop(cache);
    drop(state);

    // A genuinely different root invalidates both workspace-derived caches.
    let other = tempfile::tempdir().unwrap();
    shell.set_workspace(other.path().to_path_buf());
    let state = shell.state.borrow();
    assert!(state.file_index.is_none());
    let cache = state.transcript_cache.borrow();
    assert_eq!(cache.width, None);
    assert!(cache.dirty);
}

#[test]
fn invalidate_file_index_forces_a_fresh_walk_for_new_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), b"x").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(dir.path().to_path_buf());
    for character in "@a".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    shell.settle_file_index();
    assert!(shell.state.borrow().file_index.is_some());

    // A run may have created files; invalidation makes the next mention
    // pick them up.
    std::fs::write(dir.path().join("brand_new.rs"), b"x").unwrap();
    shell.invalidate_file_index();
    assert!(shell.state.borrow().file_index.is_none());
    shell.apply_edit(EditAction::Char('_'));
    shell.settle_file_index();
    let state = shell.state.borrow();
    let files = state.file_index.as_ref().unwrap();
    assert!(files.paths().iter().any(|file| file == "brand_new.rs"));
}

#[test]
fn unsupported_media_mention_falls_back_to_a_path_and_notice() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("shot.png"), b"png").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(dir.path().to_path_buf());
    for character in "@shot".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    // The index is walked off-thread; completion needs the finished walk.
    shell.settle_file_index();
    shell.complete_path();

    assert_eq!(shell.pending(), "@shot.png ");
    assert!(shell
        .debug_snapshot()
        .contains("does not accept image input"));
}
