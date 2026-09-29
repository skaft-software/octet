//! `/extensions` options menus: every extension is configured and operated
//! from here. Selecting an extension shows its menu (its own `menu/collect`
//! answer, or entries generated from its declared commands); running an item
//! shows live progress and the extension's confirm and input dialogs.

use super::*;

/// Most recent progress lines kept in the running-action view.
const ACTION_LOG_LINES: usize = 12;

/// Live view of one running options-menu action: a spinner, elapsed time and
/// the latest progress lines. It opens on first use and yields the screen to
/// confirm and input dialogs, reopening afterwards.
struct ExtensionActionConsole<'a> {
    shell: &'a mut InteractiveShell,
    input: &'a mut EventStream,
    dialogs: &'a crate::extensions::ExtensionLifecycleSnapshot,
    title: String,
    started: std::time::Instant,
    lines: Vec<String>,
    open: bool,
    frame: usize,
}

impl<'a> ExtensionActionConsole<'a> {
    fn new(
        shell: &'a mut InteractiveShell,
        input: &'a mut EventStream,
        dialogs: &'a crate::extensions::ExtensionLifecycleSnapshot,
        title: String,
    ) -> Self {
        Self {
            shell,
            input,
            dialogs,
            title,
            started: std::time::Instant::now(),
            lines: Vec::new(),
            open: false,
            frame: 0,
        }
    }

    fn document(&self) -> String {
        const UNICODE: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        const ASCII: [&str; 4] = ["|", "/", "-", "\\"];
        let spinner = if self.shell.theme().unicode() {
            UNICODE[self.frame % UNICODE.len()]
        } else {
            ASCII[self.frame % ASCII.len()]
        };
        let mut text = format!("{spinner} Working… {}s", self.started.elapsed().as_secs());
        if !self.lines.is_empty() {
            text.push('\n');
        }
        for line in &self.lines {
            text.push('\n');
            text.push_str(line);
        }
        text.push_str("\n\nEsc or Ctrl+C cancels");
        text
    }

    fn show(&mut self) {
        let text = crate::tui::view::sanitize_for_terminal(&self.document());
        if self.open {
            self.shell.update_read_only_document(text);
        } else {
            self.shell.open_panel(Panel::ReadOnlyDocument {
                title: self.title.clone(),
                text: text.into(),
                styled: false,
                scroll_from_bottom: 0,
            });
            self.open = true;
        }
        self.shell.render();
    }

    fn record(&mut self, line: String) {
        let line = line.trim().to_owned();
        if line.is_empty() || self.lines.last() == Some(&line) {
            return;
        }
        self.lines.push(line);
        if self.lines.len() > ACTION_LOG_LINES {
            self.lines.remove(0);
        }
        self.show();
    }

    fn close(&mut self) {
        if self.open {
            self.shell.close_panel();
            self.open = false;
            self.shell.render();
        }
    }
}

impl crate::extensions::ExtensionConfirmationHandler for ExtensionActionConsole<'_> {
    fn wait_for_cancel<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
        Box::pin(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                let event = tokio::select! {
                    biased;
                    _ = crate::tui::terminal::wait_for_shutdown_signal() => return Ok(()),
                    _ = tick.tick() => {
                        self.frame = self.frame.wrapping_add(1);
                        self.show();
                        continue;
                    }
                    event = self.input.next() => event,
                };
                match event {
                    Some(Ok(Event::Key(key))) if keymap::is_close_key(&key) => {
                        self.shell.request_close();
                        return Ok(());
                    }
                    Some(Ok(Event::Key(key)))
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                            && (key.code == KeyCode::Esc
                                || (key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL))) =>
                    {
                        return Ok(());
                    }
                    Some(Ok(Event::Resize(columns, rows))) => {
                        self.shell.set_size(columns, rows);
                        self.show();
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.into()),
                    None => return Ok(()),
                }
            }
        })
    }

    fn progress(&mut self, _extension: &str, progress: &ToolProgress) {
        match progress {
            ToolProgress::Status(message) => self.record(message.clone()),
            ToolProgress::Decoration(decoration) => {
                let detail = decoration
                    .detail()
                    .map(|detail| format!(" · {detail}"))
                    .unwrap_or_default();
                self.record(format!("{}{detail}", decoration.label()));
            }
            _ => {}
        }
    }

    fn confirm<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a octet_agent::extension_process::ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        let dialogs = self.dialogs;
        Box::pin(async move {
            self.open = false;
            let outcome = present_host_dialog(dialogs, "confirm", async {
                tokio::select! {
                    biased;
                    _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                        anyhow::bail!("shutdown requested while awaiting extension confirmation")
                    }
                    result = extension_confirmation_picker(
                        self.shell,
                        self.input,
                        extension,
                        request,
                    ) => result,
                }
            })
            .await;
            outcome
        })
    }

    fn input<'a>(
        &'a mut self,
        _extension: &'a str,
        request: &'a octet_agent::extension_process::ExtensionInputRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + 'a>> {
        let dialogs = self.dialogs;
        Box::pin(async move {
            // The input prompt owns the editor row; the progress panel stays
            // hidden until the next progress line or tick reopens it.
            self.close();
            present_host_dialog(dialogs, "input", async {
                tokio::select! {
                    biased;
                    _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                        anyhow::bail!("shutdown requested while awaiting extension input")
                    }
                    result = extension_input_picker(self.shell, self.input, request) => result,
                }
            })
            .await
        })
    }
}

/// How the user left one extension's options menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExtensionMenuOutcome {
    Back,
    Disable,
}

enum MenuEntry {
    Item(usize),
    Disable,
}

/// Shows one extension's options menu until the user goes back or disables it.
pub(super) async fn extension_options_menu(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    extension: &str,
    allow_disable: bool,
) -> anyhow::Result<ExtensionMenuOutcome> {
    // Submenu ids from the top level, and the last selected id per level, so
    // a refreshed menu keeps the user's place.
    let mut path: Vec<String> = Vec::new();
    let mut remembered: std::collections::HashMap<Vec<String>, String> =
        std::collections::HashMap::new();
    loop {
        let options = match app.executable_extensions.options_menu(extension).await {
            Ok(Some(options)) => options,
            Ok(None) => not_running_options(),
            Err(error) => {
                shell.error(format!("{error:#}"));
                app.executable_extensions
                    .generated_options_menu(extension)
                    .unwrap_or_else(not_running_options)
            }
        };
        let menu = &options.menu;
        let mut title = menu.title.clone().unwrap_or_else(|| extension.to_owned());
        let mut detail = menu.detail.clone();
        let mut items = &menu.items;
        let mut resolved = Vec::new();
        for id in &path {
            let Some(submenu) = items
                .iter()
                .find(|item| &item.id == id && item.items.is_some())
            else {
                break;
            };
            title = format!("{title} › {}", submenu.label);
            detail = submenu.detail.clone();
            items = submenu.items.as_ref().expect("submenu checked above");
            resolved.push(id.clone());
        }
        path = resolved;

        let mut labels = Vec::new();
        let mut descriptions = Vec::new();
        let mut entries = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let mut label = item.label.clone();
            if item.recommended {
                label.push_str(" (recommended)");
            }
            if item.items.is_some() {
                label.push_str(" ›");
            }
            labels.push(label);
            descriptions.push(item.description.clone());
            entries.push(MenuEntry::Item(index));
        }
        if path.is_empty() && allow_disable {
            labels.push(format!("Disable {extension}"));
            descriptions.push(Some("Stop the extension and remove its tools".to_owned()));
            entries.push(MenuEntry::Disable);
        }
        if entries.is_empty() {
            read_only_document(
                shell,
                input,
                title,
                menu_purpose(menu, detail.as_deref())
                    .unwrap_or_else(|| "This extension has no options.".to_owned()),
            )
            .await?;
            if path.pop().is_none() {
                return Ok(ExtensionMenuOutcome::Back);
            }
            continue;
        }

        let preselected = remembered
            .get(&path)
            .and_then(|id| items.iter().position(|item| &item.id == id))
            .or_else(|| items.iter().position(|item| item.recommended))
            .unwrap_or(0);
        let surface = match menu_purpose(menu, detail.as_deref()) {
            Some(purpose) => OrdinarySurfaceMetadata::with_purpose(title, purpose),
            None => OrdinarySurfaceMetadata::new(title),
        };
        let Some(index) =
            extension_picker(shell, input, surface, labels, descriptions, preselected).await?
        else {
            if path.pop().is_none() {
                return Ok(ExtensionMenuOutcome::Back);
            }
            continue;
        };
        let item = match entries[index] {
            MenuEntry::Disable => return Ok(ExtensionMenuOutcome::Disable),
            MenuEntry::Item(index) => items[index].clone(),
        };
        remembered.insert(path.clone(), item.id.clone());
        if item.items.is_some() {
            path.push(item.id);
            continue;
        }
        run_extension_menu_action(app, shell, input, extension, &item, options.generated).await?;
        if shell.close_requested() {
            return Ok(ExtensionMenuOutcome::Back);
        }
    }
}

/// Status first, then the first line of any detail, for the picker subtitle.
fn menu_purpose(menu: &octet_agent::ExtensionMenu, detail: Option<&str>) -> Option<String> {
    let status = menu.status.as_ref().map(|status| status.label.clone());
    let detail = detail
        .and_then(|detail| detail.lines().find(|line| !line.trim().is_empty()))
        .map(str::to_owned);
    match (status, detail) {
        (Some(status), Some(detail)) => Some(format!("{status} · {detail}")),
        (Some(status), None) => Some(status),
        (None, detail) => detail,
    }
}

fn not_running_options() -> crate::extensions::ExtensionOptions {
    crate::extensions::ExtensionOptions {
        menu: octet_agent::ExtensionMenu {
            status: Some(octet_agent::ExtensionPresentationStatus {
                state: octet_agent::ExtensionPresentationState::Unavailable,
                label: "Not running".to_owned(),
                detail: None,
            }),
            detail: Some(
                "Executable extensions need full access, and safe mode keeps them stopped. \
                 See /extensions status for why this one is not running."
                    .to_owned(),
            ),
            ..octet_agent::ExtensionMenu::default()
        },
        generated: true,
    }
}

/// Runs one menu action with live progress, then shows what it reported.
async fn run_extension_menu_action(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    extension: &str,
    item: &octet_agent::ExtensionMenuItem,
    generated: bool,
) -> anyhow::Result<()> {
    let Some(command) = item.command.clone() else {
        return Ok(());
    };
    let mut arguments = item.arguments.clone();
    if generated {
        // Generated entries route to a bare declared command; its arguments
        // are the only thing the user can add.
        let request = octet_agent::extension_process::ExtensionInputRequest {
            parent_request_id: 0,
            prompt: format!("Arguments for {} (Enter for none)", item.label),
            secret: false,
        };
        let Some(text) = extension_input_picker(shell, input, &request).await? else {
            return Ok(());
        };
        arguments.extend(text.split_whitespace().map(str::to_owned));
    }
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    let result = {
        let mut console = ExtensionActionConsole::new(shell, input, &dialogs, item.label.clone());
        let result = app
            .executable_extensions
            .execute_menu_action_with_confirmation(
                extension,
                &item.label,
                &command,
                arguments,
                item.destructive,
                &mut console,
            )
            .await;
        console.close();
        result
    };
    request_extension_ui(shell, app);
    // The octet-subagents worker list is the host's live browser (refreshing,
    // with worker transcripts), not a static result document.
    let worker_browser =
        extension == "octet-subagents" && command == "subagents" && item.arguments.is_empty();
    match result {
        Ok(output) if worker_browser => super::subagents_view(app, shell, input, output).await?,
        Ok(output) if output.trim().is_empty() => {
            shell.notice(format!("{} finished", item.label));
        }
        Ok(output) => read_only_document(shell, input, item.label.clone(), output).await?,
        Err(error) => shell.error(format!("{}: {error:#}", item.label)),
    }
    Ok(())
}

/// Persists and applies one activation change. Returns whether it applied.
pub(super) async fn set_extension_enabled(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    name: &str,
    currently_enabled: bool,
) -> anyhow::Result<(App, bool)> {
    let authoritative = match crate::cli::extension_activation_menu_authoritative(&app.config) {
        Ok(authoritative) => authoritative,
        Err(error) => {
            shell.error(format!(
                "{name} was not changed: could not revalidate activation precedence: {error}"
            ));
            return Ok((app, false));
        }
    };
    if !authoritative {
        shell.error(format!(
            "{name} was not changed: project, environment, or CLI activation now makes the user config read-only"
        ));
        return Ok((app, false));
    }
    if refuse_resource_reload(&app, shell) {
        return Ok((app, false));
    }
    let enabled = !currently_enabled;
    let config_path = crate::cli::global_config_path();
    let before_config = config_path.as_deref().and_then(configuration_snapshot);
    let persisted = match crate::cli::persist_extension_enabled(name, enabled) {
        Ok(persisted) => persisted,
        Err(error) => {
            shell.error(format!(
                "{name} was not changed: could not update user configuration: {error}"
            ));
            return Ok((app, false));
        }
    };
    app.config.enabled_extensions = persisted;
    app = match reload_resources(app, shell, input).await {
        Ok((app, _)) => app,
        Err(error) => {
            let rollback = crate::cli::persist_extension_enabled(name, currently_enabled);
            return match rollback {
                Ok(_) => Err(error.context(format!(
                    "{name} runtime rebuild failed; the user-config activation change was rolled back"
                ))),
                Err(rollback_error) => Err(error.context(format!(
                    "{name} runtime rebuild failed and user-config rollback also failed: {rollback_error}"
                ))),
            };
        }
    };
    observe_configuration_commit(
        &mut app.executable_extensions,
        before_config,
        config_path.as_deref(),
    )
    .await;
    request_extension_ui(shell, &mut app);
    let summary = app
        .executable_extensions
        .summaries()
        .into_iter()
        .find(|summary| summary.name == name);
    let detail = if enabled && summary.as_ref().is_some_and(|summary| !summary.trusted) {
        "; executable extensions require full access; safe mode keeps them stopped"
    } else {
        ""
    };
    shell.notice(format!(
        "{name} {}{detail}",
        if enabled { "enabled" } else { "disabled" }
    ));
    shell.clear_error();
    Ok((app, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(label: &str) -> octet_agent::ExtensionPresentationStatus {
        octet_agent::ExtensionPresentationStatus {
            state: octet_agent::ExtensionPresentationState::Active,
            label: label.to_owned(),
            detail: None,
        }
    }

    #[test]
    fn the_menu_subtitle_leads_with_status_then_the_first_detail_line() {
        let mut menu = octet_agent::ExtensionMenu {
            status: Some(status("Ready · cua-driver 0.30.4")),
            ..octet_agent::ExtensionMenu::default()
        };
        assert_eq!(
            menu_purpose(
                &menu,
                Some("\nAccessibility granted\nScreen Recording granted")
            ),
            Some("Ready · cua-driver 0.30.4 · Accessibility granted".to_owned())
        );
        assert_eq!(
            menu_purpose(&menu, None),
            Some("Ready · cua-driver 0.30.4".to_owned())
        );
        menu.status = None;
        assert_eq!(
            menu_purpose(&menu, Some("Only detail")),
            Some("Only detail".to_owned())
        );
        assert_eq!(menu_purpose(&menu, None), None);
    }

    #[test]
    fn a_stopped_extension_offers_its_state_instead_of_actions() {
        let options = not_running_options();
        assert!(options.generated);
        assert!(options.menu.items.is_empty());
        assert_eq!(options.menu.status.unwrap().label, "Not running");
    }
}
