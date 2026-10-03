//! Terminal theme selection: the picker, the file-metadata preview, direct name
//! loading, and the restore-on-cancel guarantee.
//! Separate because the theme surface owns its own live-preview lifecycle and would
//! otherwise be buried between reload and session probes.

use super::*;

use super::support::*;

#[tokio::test]
async fn terminal_theme_picker_confirms_compiled_previews_without_changing_config() {
    for choice in TerminalThemeChoice::all() {
        let mut config = terminal_theme_test_config(PathBuf::from("."));
        config.theme = Some("auto".into());
        let mut shell = InteractiveShell::test_shell();
        let mut original = crate::tui::theme::test_theme_for(
            TerminalBackground::Dark,
            shell.theme().capabilities(),
        );
        original.override_token("foreground", "#123456");
        shell.set_theme(original.clone());
        let mut events = vec![
            theme_picker_key(KeyCode::End),
            theme_picker_key(KeyCode::Home),
        ];
        for _ in 0..choice.index() {
            events.push(theme_picker_key(KeyCode::Down));
        }
        // Filtering leaves a single displayed row whose original index
        // is still the choice's index, not necessarily zero.
        events.extend(
            choice
                .key()
                .chars()
                .map(|key| theme_picker_key(KeyCode::Char(key))),
        );
        events.push(theme_picker_key(KeyCode::Enter));
        let mut input = tokio_stream::iter(events);

        assert_eq!(
            pick_terminal_theme(&mut shell, &mut input, &config, false)
                .await
                .unwrap(),
            Some(ThemeSelection::Builtin(choice))
        );
        assert_eq!(
            shell.theme().background(),
            choice
                .explicit_background()
                .unwrap_or(TerminalBackground::Dark),
            "Auto must retain its already-resolved background after visiting other rows"
        );
        if choice == TerminalThemeChoice::Auto {
            assert_eq!(shell.theme().capabilities(), original.capabilities());
            assert_eq!(
                shell.theme().role_rgb("foreground"),
                original.role_rgb("foreground")
            );
        }
        assert_eq!(config.theme.as_deref(), Some("auto"));
        assert!(!shell.has_panel());
    }
}

#[tokio::test]
async fn terminal_theme_picker_filters_file_metadata_and_preserves_active_theme_until_confirmed() {
    let directory = tempfile::tempdir().unwrap();
    let config = terminal_theme_test_config(directory.path().to_owned());
    let themes = config.workspace.join(".octet/themes");
    std::fs::create_dir_all(&themes).unwrap();
    let path = themes.join("picker-amber.toml");
    std::fs::write(
        &path,
        "[metadata]\nname = 'Golden Hour'\ndescription = 'warm orange palette'\n[colors]\naccent = '#aabbcc'\n",
    )
    .unwrap();
    let mut shell = InteractiveShell::test_shell();
    let original = shell.theme();
    let mut events: Vec<_> = "warm orange"
        .chars()
        .map(|key| theme_picker_key(KeyCode::Char(key)))
        .collect();
    events.push(theme_picker_key(KeyCode::Enter));
    let mut input = tokio_stream::iter(events);
    assert_eq!(
        pick_terminal_theme(&mut shell, &mut input, &config, false)
            .await
            .unwrap(),
        Some(ThemeSelection::File("picker-amber".into()))
    );
    assert_eq!(
        shell.theme().source_path(),
        Some(path.canonicalize().unwrap().as_path())
    );
    assert_eq!(
        shell.theme().resolve::<String>("accent").as_deref(),
        Some("#aabbcc")
    );
    assert!(config.theme.is_none(), "preview must not commit config");
    assert!(!shell.has_panel());

    let mut selected_config = config.clone();
    selected_config.theme = Some("picker-amber".into());
    let prior = shell.theme();
    std::fs::write(&path, "accent = '#abcdef'").unwrap();
    let mut input = tokio_stream::iter([theme_picker_key(KeyCode::Enter)]);
    assert_eq!(
        pick_terminal_theme(&mut shell, &mut input, &selected_config, false)
            .await
            .unwrap(),
        Some(ThemeSelection::File("picker-amber".into()))
    );
    assert_eq!(
        shell.theme().resolve::<String>("accent"),
        prior.resolve::<String>("accent")
    );
    assert_eq!(selected_config.theme.as_deref(), Some("picker-amber"));

    let mut input = tokio_stream::iter([
        theme_picker_key(KeyCode::Home),
        theme_picker_key(KeyCode::Enter),
    ]);
    assert_eq!(
        pick_terminal_theme(&mut shell, &mut input, &selected_config, false)
            .await
            .unwrap(),
        Some(ThemeSelection::Builtin(TerminalThemeChoice::Auto))
    );
    assert!(shell.theme().is_compiled_default());
    assert_eq!(shell.theme().background(), original.background());
}

#[tokio::test]
async fn terminal_theme_file_preview_cancel_and_onboarding_keep_the_original() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = terminal_theme_test_config(directory.path().to_owned());
    config.theme = Some("light".into());
    let themes = config.workspace.join(".octet/themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(themes.join("picker-custom.toml"), "accent = '#aabbcc'").unwrap();
    let mut shell = InteractiveShell::test_shell();
    let original = shell.theme();
    let mut events: Vec<_> = "picker-custom"
        .chars()
        .map(|key| theme_picker_key(KeyCode::Char(key)))
        .collect();
    events.push(theme_picker_key(KeyCode::Esc));
    let mut input = EventStream::from_stream(tokio_stream::iter(events));
    assert_eq!(
        configure_terminal_theme(&mut shell, &mut input, &mut config, None, false, None)
            .await
            .unwrap(),
        None
    );
    assert_eq!(shell.theme().source_path(), original.source_path());
    assert_eq!(
        shell.theme().role_rgb("foreground"),
        original.role_rgb("foreground")
    );
    assert_eq!(config.theme.as_deref(), Some("light"));
    assert!(!shell.has_panel());

    let mut events: Vec<_> = "picker-custom"
        .chars()
        .map(|key| theme_picker_key(KeyCode::Char(key)))
        .collect();
    events.push(theme_picker_key(KeyCode::Enter));
    events.push(theme_picker_key(KeyCode::Esc));
    let mut input = tokio_stream::iter(events);
    assert_eq!(
        pick_terminal_theme(&mut shell, &mut input, &config, true)
            .await
            .unwrap(),
        None
    );
    assert_eq!(shell.theme().source_path(), original.source_path());
}

#[test]
fn direct_theme_name_loads_only_valid_selectable_files() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = terminal_theme_test_config(directory.path().to_owned());
    let themes = config.workspace.join(".octet/themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(themes.join("picker-custom.toml"), "accent = '#aabbcc'").unwrap();
    std::fs::write(themes.join("picker-broken.toml"), "[invalid").unwrap();
    std::fs::write(themes.join("dark.toml"), "accent = '#123456'").unwrap();
    for name in ["picker-custom", "picker-custom.toml"] {
        let (key, loaded) = requested_file_theme(name, &config, TerminalBackground::Dark).unwrap();
        assert_eq!(key, "picker-custom");
        assert_eq!(
            loaded.resolve::<String>("accent").as_deref(),
            Some("#aabbcc")
        );
        config.theme = Some(key);
        assert_eq!(
            load_theme_for_background(&config, TerminalBackground::Dark).source_path(),
            loaded.source_path()
        );
    }
    for name in [
        "picker-broken",
        "missing",
        "../picker-custom",
        "dark.toml",
        "default",
    ] {
        assert!(
            requested_file_theme(name, &config, TerminalBackground::Dark).is_none(),
            "accepted {name}"
        );
    }
}

#[tokio::test]
async fn invalid_direct_theme_selection_keeps_current_config_and_appearance() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = terminal_theme_test_config(directory.path().to_owned());
    config.theme = Some("light".into());
    let mut shell = InteractiveShell::test_shell();
    let original = shell.theme();
    let mut input = EventStream::from_stream(tokio_stream::empty());
    assert_eq!(
        configure_terminal_theme(
            &mut shell,
            &mut input,
            &mut config,
            Some("missing-theme".into()),
            false,
            None,
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(config.theme.as_deref(), Some("light"));
    assert_eq!(shell.theme().source_path(), original.source_path());
    assert_eq!(
        shell.theme().role_rgb("foreground"),
        original.role_rgb("foreground")
    );
}

#[tokio::test]
async fn terminal_theme_preview_restores_on_cancel_eof_error_and_close() {
    for exit in ["escape", "eof", "error", "close"] {
        let mut config = terminal_theme_test_config(PathBuf::from("."));
        config.theme = Some("light".into());
        let mut shell = InteractiveShell::test_shell();
        let mut original = crate::tui::theme::test_theme_for(
            TerminalBackground::Light,
            shell.theme().capabilities(),
        );
        original.override_token("foreground", "#123456");
        shell.set_theme(original.clone());
        let mut events = vec![theme_picker_key(KeyCode::End)];
        match exit {
            "escape" => events.push(theme_picker_key(KeyCode::Esc)),
            "eof" => {}
            "error" => events.push(Err(std::io::Error::other("preview input failed"))),
            "close" => events.push(Ok(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL,
            )))),
            _ => unreachable!(),
        }
        let mut input = EventStream::from_stream(tokio_stream::iter(events));
        // Exercise the configuration boundary too: none of these /theme
        // outcomes may reach its persistence branch or mutate the config.
        let result =
            configure_terminal_theme(&mut shell, &mut input, &mut config, None, false, None).await;
        if exit == "error" {
            assert_eq!(result.unwrap_err().to_string(), "preview input failed");
        } else {
            assert_eq!(result.unwrap(), None);
        }
        assert_eq!(shell.theme().background(), original.background(), "{exit}");
        assert_eq!(
            shell.theme().role_rgb("foreground"),
            original.role_rgb("foreground"),
            "{exit}"
        );
        assert_eq!(
            shell.theme().capabilities(),
            original.capabilities(),
            "{exit}"
        );
        assert_eq!(config.theme.as_deref(), Some("light"), "{exit}");
        assert_eq!(shell.close_requested(), exit == "close");
        assert!(!shell.has_panel());
    }
}

#[tokio::test]
async fn terminal_theme_onboarding_dismissal_returns_no_preview_choice() {
    let config = terminal_theme_test_config(PathBuf::from("."));
    let mut shell = InteractiveShell::test_shell();
    let original = shell.theme();
    let mut input = tokio_stream::iter([
        theme_picker_key(KeyCode::End),
        theme_picker_key(KeyCode::Esc),
    ]);
    // configure_terminal_theme retains its dismissal-to-Auto fallback;
    // the highlighted Dark preview must not masquerade as confirmation.
    assert_eq!(
        pick_terminal_theme(&mut shell, &mut input, &config, true)
            .await
            .unwrap(),
        None
    );
    assert_eq!(shell.theme().background(), original.background());
    assert_eq!(shell.theme().capabilities(), original.capabilities());
    assert!(config.theme.is_none());
}

// Requires a POSIX shell plus coreutils (`yes`, `head`): Unix-only.
#[cfg(unix)]
#[test]
fn posix_shell_quote_round_trips_shell_sensitive_selector() {
    let selector = "resume id;$(printf pwned);'\"$HOME\" * 雪";
    let script = format!("set -- {}; printf '%s' \"$1\"", posix_shell_quote(selector));
    let output = std::process::Command::new("sh")
        .args(["-c", &script, "resume-test"])
        .env_clear()
        .env("HOME", "should-not-expand")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run POSIX shell");
    assert!(
        output.status.success(),
        "POSIX shell rejected quoted selector: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, selector.as_bytes());
}
