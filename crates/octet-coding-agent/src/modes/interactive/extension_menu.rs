//! `/extensions` options menus: extension activation and configuration live
//! here; subagent runtime controls stay on `/subagents`. Selecting an extension shows its menu (its own `menu/collect`
//! answer, or entries generated from its declared commands); running an item
//! shows live progress and the extension's confirm and input dialogs.

use super::*;
use crate::extensions::ExtensionConfirmationHandler;

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
    fullscreen_before: bool,
    fullscreen_admitted: bool,
}

impl<'a> ExtensionActionConsole<'a> {
    fn new(
        shell: &'a mut InteractiveShell,
        input: &'a mut EventStream,
        dialogs: &'a crate::extensions::ExtensionLifecycleSnapshot,
        title: String,
    ) -> Self {
        let fullscreen_before = shell.has_remote_fullscreen_mount();
        Self {
            shell,
            input,
            dialogs,
            title,
            started: std::time::Instant::now(),
            lines: Vec::new(),
            open: false,
            frame: 0,
            fullscreen_before,
            fullscreen_admitted: false,
        }
    }

    fn observe_fullscreen_mount(&mut self) {
        if !self.fullscreen_before && self.shell.has_remote_fullscreen_mount() {
            self.fullscreen_admitted = true;
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
        self.observe_fullscreen_mount();
        if self.shell.has_remote_fullscreen_mount() {
            self.close();
            return;
        }
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

impl InteractiveCommandFrontend for ExtensionActionConsole<'_> {
    fn apply_lifecycle<'a>(
        &'a mut self,
        app: &'a mut App,
        request: ExtensionSessionLifecycleRequest,
    ) -> Pin<Box<dyn Future<Output = bool> + 'a>> {
        self.close();
        Box::pin(execute_command_session_lifecycle(
            app, self.shell, self.input, request,
        ))
    }
}

impl crate::extensions::ExtensionConfirmationHandler for ExtensionActionConsole<'_> {
    fn command_shell(&mut self) -> Option<&mut InteractiveShell> {
        self.observe_fullscreen_mount();
        Some(self.shell)
    }

    fn wait_for_command_event<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Event>>> + 'a>> {
        Box::pin(async move {
            tokio::select! {
                biased;
                _ = crate::tui::terminal::wait_for_shutdown_signal() => Ok(None),
                event = self.input.next() => match event {
                    Some(Ok(event)) => Ok(Some(event)),
                    Some(Err(error)) => Err(error.into()),
                    None => Ok(None),
                },
            }
        })
    }

    fn command_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if keymap::is_close_key(&key) => {
                self.shell.request_close();
                true
            }
            Event::Key(key) if is_ctrl_c(&key) => true,
            Event::Resize(columns, rows) => {
                self.shell.set_size(columns, rows);
                self.shell.render();
                false
            }
            event => {
                let _ = handle_cancellable_wait_input(self.shell, event);
                self.shell.render();
                false
            }
        }
    }

    fn command_cancellation_event(&mut self, event: &Event) -> bool {
        matches!(event, Event::Key(key) if is_ctrl_c(key))
    }

    fn should_yield_to_fullscreen(&self) -> bool {
        self.fullscreen_admitted
            || (!self.fullscreen_before && self.shell.has_remote_fullscreen_mount())
    }

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
    ReturnToIdle,
    Disable,
    GrantHostAuthority,
    RevokeHostAuthority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExtensionMenuActionDisposition {
    Continue,
    ReturnToIdle,
}

enum MenuEntry {
    SetupRuntime,
    Item(usize),
    Disable,
    HostAuthority(bool),
}

fn authority_grant(summary: &crate::extensions::ExtensionSummary) -> Option<String> {
    (summary.source != octet_agent::extension_process::ExtensionSource::Explicit).then(|| {
        crate::extensions::persistent_host_authority_grant(
            summary.source,
            &summary.name,
            &summary.manifest_path,
        )
    })
}

pub(super) fn authority_label(
    config: &crate::config::Config,
    summary: &crate::extensions::ExtensionSummary,
) -> &'static str {
    if summary.source == octet_agent::extension_process::ExtensionSource::Explicit {
        "granted (--extension-dir)"
    } else if config.effect_policy == octet_agent::EffectPolicy::UnsafeHost {
        "implicit (full access)"
    } else if summary.trusted {
        "granted"
    } else {
        "not granted"
    }
}

fn authority_action(
    config: &crate::config::Config,
    summary: &crate::extensions::ExtensionSummary,
    authoritative: bool,
) -> Option<bool> {
    let grant = authority_grant(summary)?;
    if !authoritative
        || config
            .invocation_trusted_extensions
            .iter()
            .any(|name| name == &summary.name)
    {
        return None;
    }
    Some(!config.trusted_extensions.contains(&grant))
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
        // Host bootstrap is available even when no Python process/factory can
        // start. Creating (but never polling) a cancelled future only validates
        // the selected source, activation and execution policy.
        if path.is_empty() && python_runtime_setup_available(app, extension) {
            labels.push("Set up runtime".to_owned());
            descriptions.push(Some("Provision this trusted extension's private Python runtime; no credentials or OS grants".to_owned()));
            entries.push(MenuEntry::SetupRuntime);
        }
        let host_prefix = entries.len();
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
        if path.is_empty() {
            if let Some(summary) = app
                .executable_extensions
                .summaries()
                .into_iter()
                .find(|summary| summary.name == extension)
            {
                if let Some(grant) = authority_action(
                    &app.config,
                    &summary,
                    crate::cli::extension_host_authority_menu_authoritative(),
                ) {
                    labels.push(
                        if grant {
                            "Grant host authority"
                        } else {
                            "Revoke host authority"
                        }
                        .to_owned(),
                    );
                    descriptions.push(Some("A grant lets this extension run as a host process with your OS permissions, even in safe mode; the tool-effect broker cannot confine its code.".to_owned()));
                    entries.push(MenuEntry::HostAuthority(grant));
                }
            }
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
            .and_then(|id| {
                items
                    .iter()
                    .position(|item| &item.id == id)
                    .map(|index| index + host_prefix)
            })
            .or_else(|| {
                items
                    .iter()
                    .position(|item| item.recommended)
                    .map(|index| index + host_prefix)
            })
            .unwrap_or(0);
        let surface = match menu_purpose(menu, detail.as_deref()) {
            Some(purpose) => OrdinarySurfaceMetadata::with_purpose(title.clone(), purpose),
            None => OrdinarySurfaceMetadata::new(title.clone()),
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
            MenuEntry::SetupRuntime => {
                setup_python_runtime(app, shell, input, extension).await?;
                if shell.close_requested() {
                    return Ok(ExtensionMenuOutcome::Back);
                }
                continue;
            }
            MenuEntry::Disable => return Ok(ExtensionMenuOutcome::Disable),
            MenuEntry::HostAuthority(true) => return Ok(ExtensionMenuOutcome::GrantHostAuthority),
            MenuEntry::HostAuthority(false) => {
                return Ok(ExtensionMenuOutcome::RevokeHostAuthority);
            }
            MenuEntry::Item(index) => items[index].clone(),
        };
        remembered.insert(path.clone(), item.id.clone());
        if item.items.is_some() {
            path.push(item.id);
            continue;
        }
        if run_extension_menu_action(
            app,
            shell,
            input,
            extension,
            &title,
            &item,
            options.generated,
        )
        .await?
            == ExtensionMenuActionDisposition::ReturnToIdle
        {
            return Ok(ExtensionMenuOutcome::ReturnToIdle);
        }
        if shell.close_requested() {
            return Ok(ExtensionMenuOutcome::Back);
        }
    }
}

struct PythonSetupWork {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    future: Pin<Box<dyn Future<Output = anyhow::Result<PathBuf>> + Send>>,
}

impl Drop for PythonSetupWork {
    fn drop(&mut self) {
        // Drop runs before fields, even if the whole owning UI task is dropped.
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(super) fn python_runtime_setup_available(app: &App, extension: &str) -> bool {
    let setup = octet_agent::extension_process::PythonRuntimeSetup {
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        progress: std::sync::Arc::new(|_| {}),
    };
    app.executable_extensions
        .python_runtime_setup(extension, &app.config, setup)
        .is_ok()
}

/// Explicit native setup owns no extension factory and no mutable App borrow in
/// its download future. Dropping any wait path cancels before dropping that future.
pub(super) async fn setup_python_runtime(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    extension: &str,
) -> anyhow::Result<bool> {
    let request = octet_agent::extension_process::ConfirmationRequest {
        parent_request_id: None,
        prompt: format!("Set up {extension}'s private Python runtime?"),
        detail: Some("Download and verify the platform-pinned private Python runtime and this extension's declared Python dependencies. This does not grant host authority, ask for credentials, install a desktop driver or grant OS permissions.".to_owned()),
        destructive: false,
        default: false,
    };
    if !extension_confirmation_picker(shell, input, "octet", &request).await? {
        shell.notice("runtime setup cancelled; no download started");
        return Ok(false);
    }
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let lines = std::sync::Arc::new(std::sync::Mutex::new(VecDeque::<String>::new()));
    let progress_lines = lines.clone();
    let setup = octet_agent::extension_process::PythonRuntimeSetup {
        cancelled: cancelled.clone(),
        progress: std::sync::Arc::new(move |line| {
            let mut lines = progress_lines
                .lock()
                .expect("native runtime setup progress");
            if lines.back().is_some_and(|last| last == line) {
                return;
            }
            if lines.len() == ACTION_LOG_LINES {
                lines.pop_front();
            }
            lines.push_back(
                crate::tui::view::sanitize_for_terminal(line)
                    .chars()
                    .take(1024)
                    .collect(),
            );
        }),
    };
    let owner = app.agent.session().resource_owner_key();
    let mut work =
        match app
            .executable_extensions
            .python_runtime_setup(extension, &app.config, setup)
        {
            Ok(future) => PythonSetupWork { cancelled, future },
            Err(error) => {
                shell.error(format!("runtime setup not admitted: {error:#}"));
                return Ok(false);
            }
        };
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    let result = {
        let mut console = ExtensionActionConsole::new(
            shell,
            input,
            &dialogs,
            format!("{extension} · Set up runtime"),
        );
        console.show();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let result = loop {
            tokio::select! {
                biased;
                _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                    console.shell.request_close();
                    break None;
                }
                event = console.input.next() => match event {
                    Some(Ok(Event::Key(key))) if keymap::is_close_key(&key) => {
                        console.shell.request_close();
                        break None;
                    }
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press && (key.code == KeyCode::Esc || is_ctrl_c(&key)) => break None,
                    Some(Ok(Event::Resize(columns, rows))) => { console.shell.set_size(columns, rows); console.show(); }
                    Some(Ok(_)) => {},
                    Some(Err(error)) => break Some(Err(error.into())),
                    None => break None,
                },
                result = &mut work.future => break Some(result),
                _ = tick.tick() => {
                    let progress = lines.lock().expect("native runtime setup progress").iter().cloned().collect::<Vec<_>>();
                    console.lines = progress;
                    console.frame = console.frame.wrapping_add(1);
                    console.show();
                }
            }
        };
        console.close();
        result
    };
    // Flag first, then retire the future and its supervised child/download.
    drop(work);
    match result {
        None => {
            shell.notice("runtime setup cancelled; desktop setup has not run");
            Ok(false)
        }
        Some(Err(error)) => {
            shell.error(format!("runtime setup failed: {error:#}"));
            Ok(false)
        }
        Some(Ok(_))
            if shell.close_requested() || app.agent.session().resource_owner_key() != owner =>
        {
            shell.notice("runtime setup completed for a retired owner; not activating it");
            Ok(false)
        }
        Some(Ok(_)) => {
            if let Err(error) = app
                .executable_extensions
                .activate_python_extension(extension)
                .await
            {
                shell.error(format!("Python runtime is ready, but {extension} could not start: {error:#}; retry with /extensions setup {extension}"));
                request_extension_ui(shell, app);
                return Ok(false);
            }
            request_extension_ui(shell, app);
            shell.notice(format!("{extension}: Python runtime provisioned and extension activated; desktop driver and OS permissions are separate setup steps"));
            Ok(true)
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
                "A stopped extension may need host authority: use Grant host authority here. \
                 Under safe mode, granted code runs outside the tool-effect broker with your OS permissions. \
                 See /extensions status for other launch failures."
                    .to_owned(),
            ),
            ..octet_agent::ExtensionMenu::default()
        },
        generated: true,
    }
}

/// Runs one menu action with live progress, then shows what it reported.
/// `place` names the menu it was chosen from, for confirmations.
async fn run_extension_menu_action(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    extension: &str,
    place: &str,
    item: &octet_agent::ExtensionMenuItem,
    generated: bool,
) -> anyhow::Result<ExtensionMenuActionDisposition> {
    let Some(command) = item.command.clone() else {
        return Ok(ExtensionMenuActionDisposition::Continue);
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
            return Ok(ExtensionMenuActionDisposition::Continue);
        };
        arguments.extend(text.split_whitespace().map(str::to_owned));
    }
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    let (result, ui_yield) = {
        let mut console = ExtensionActionConsole::new(shell, input, &dialogs, item.label.clone());
        let result = async {
            if item.destructive {
                let request = octet_agent::extension_process::ConfirmationRequest {
                    parent_request_id: None,
                    prompt: format!("{}?", item.label),
                    detail: Some(format!("{place} · offered by {extension}")),
                    destructive: true,
                    default: false,
                };
                if !console.confirm(extension, &request).await? {
                    anyhow::bail!("{} was cancelled", item.label);
                }
            }
            run_interactive_extension_command(
                app,
                &mut console,
                Some(extension),
                &command,
                arguments,
                true,
                usize::from(item.destructive),
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("{extension} no longer offers {:?}", item.label))
        }
        .await;
        let ui_yield = if console.should_yield_to_fullscreen() {
            ExtensionMenuActionDisposition::ReturnToIdle
        } else {
            ExtensionMenuActionDisposition::Continue
        };
        console.close();
        (result, ui_yield)
    };
    request_extension_ui(shell, app);
    present_extension_menu_action_result(shell, input, &item.label, result, ui_yield).await
}

async fn present_extension_menu_action_result<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    label: &str,
    result: anyhow::Result<String>,
    ui_yield: ExtensionMenuActionDisposition,
) -> anyhow::Result<ExtensionMenuActionDisposition>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    match result {
        // Admission is sticky, even if the component closed before the command
        // failed. It suppresses success chrome, never the command error.
        Err(error) => shell.error(format!("{label}: {error:#}")),
        Ok(_) if ui_yield == ExtensionMenuActionDisposition::ReturnToIdle => {}
        Ok(output) if output.trim().is_empty() => {
            shell.notice(format!("{label} finished"));
        }
        Ok(output) => read_only_document(shell, input, label, output).await?,
    }
    Ok(ui_yield)
}

async fn confirm_host_authority(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    name: &str,
) -> anyhow::Result<bool> {
    let request = octet_agent::extension_process::ConfirmationRequest {
        parent_request_id: None,
        prompt: format!("Grant {name} host authority?"),
        detail: Some("This starts the extension's code as a host process with your OS permissions, even under safe mode. The tool-effect broker cannot confine the extension process. Only grant it to a reviewed source; use OS isolation for untrusted code.".to_owned()),
        destructive: false,
        default: false,
    };
    extension_confirmation_picker(shell, input, "octet", &request).await
}

/// Persist/revoke a source-specific host authority grant and rebuild the runtime.
pub(super) async fn set_extension_host_authority(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    name: &str,
    allowed: bool,
) -> anyhow::Result<App> {
    if !crate::cli::extension_host_authority_menu_authoritative() {
        shell.error("Host authority is controlled by OCTET_TRUSTED_EXTENSIONS; the user config is read-only".into());
        return Ok(app);
    }
    let Some(summary) = app
        .executable_extensions
        .summaries()
        .into_iter()
        .find(|summary| summary.name == name)
    else {
        shell.error(format!("{name}: selected extension is no longer available"));
        return Ok(app);
    };
    let Some(grant) = authority_grant(&summary) else {
        shell.error(format!("{name}: --extension-dir grants authority for this invocation; remove the option to revoke it"));
        return Ok(app);
    };
    if !allowed
        && app
            .config
            .invocation_trusted_extensions
            .iter()
            .any(|value| value == name)
    {
        shell.error(format!("{name}: --trust-extension grants authority for this invocation; remove the option to revoke it"));
        return Ok(app);
    }
    if allowed && !confirm_host_authority(shell, input, name).await? {
        return Ok(app);
    }
    if refuse_resource_reload(&app, shell) {
        return Ok(app);
    }
    let config_path = crate::cli::global_config_path();
    let before_config = config_path.as_deref().and_then(configuration_snapshot);
    let previously_granted = app.config.trusted_extensions.contains(&grant);
    let persisted = match crate::cli::persist_extension_host_authority(&grant, allowed) {
        Ok(persisted) => persisted,
        Err(error) => {
            shell.error(format!("{name}: host authority was not changed: {error}"));
            return Ok(app);
        }
    };
    app.config.trusted_extensions = persisted;
    app = match reload_resources(app, shell, input).await {
        Ok((app, _)) => app,
        Err(error) => {
            let rollback = crate::cli::persist_extension_host_authority(&grant, previously_granted);
            return match rollback {
                Ok(_) => Err(error.context(format!("{name} runtime rebuild failed; host authority change was rolled back"))),
                Err(rollback_error) => Err(error.context(format!("{name} runtime rebuild failed and host authority rollback failed: {rollback_error}"))),
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
    shell.notice(format!(
        "{name}: host authority {}",
        if allowed { "granted" } else { "revoked" }
    ));
    shell.clear_error();
    Ok(app)
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
    let grant = if enabled && app.config.effect_policy != octet_agent::EffectPolicy::UnsafeHost {
        app.executable_extensions
            .summaries()
            .into_iter()
            .find(|summary| summary.name == name && !summary.trusted)
            .and_then(|summary| authority_grant(&summary))
    } else {
        None
    };
    let grant = if let Some(grant) = grant {
        if !crate::cli::extension_host_authority_menu_authoritative() {
            shell.error(format!("{name}: host authority is controlled by OCTET_TRUSTED_EXTENSIONS; enablement was not changed"));
            return Ok((app, false));
        }
        confirm_host_authority(shell, input, name)
            .await?
            .then_some(grant)
    } else {
        None
    };
    let config_path = crate::cli::global_config_path();
    let before_config = config_path.as_deref().and_then(configuration_snapshot);
    let previous_grants = app.config.trusted_extensions.clone();
    if let Some(grant) = &grant {
        match crate::cli::persist_extension_host_authority(grant, true) {
            Ok(grants) => app.config.trusted_extensions = grants,
            Err(error) => {
                shell.error(format!(
                    "{name} was not enabled: could not grant host authority: {error}"
                ));
                return Ok((app, false));
            }
        }
    }
    let persisted = match crate::cli::persist_extension_enabled(name, enabled) {
        Ok(persisted) => persisted,
        Err(error) => {
            if let Some(grant) = &grant {
                crate::cli::persist_extension_host_authority(grant, false)?;
                app.config.trusted_extensions = previous_grants;
            }
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
            let rollback =
                crate::cli::persist_extension_enabled(name, currently_enabled).and_then(|_| {
                    if let Some(grant) = &grant {
                        crate::cli::persist_extension_host_authority(grant, false)?;
                    }
                    Ok(())
                });
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
        "; host authority not granted; use Grant host authority in this extension's menu to start it"
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

    #[tokio::test]
    async fn fullscreen_menu_yield_preserves_command_errors_after_surface_closed() {
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        let disposition = present_extension_menu_action_result(
            &mut shell,
            &mut input,
            "Draw",
            Err(anyhow::anyhow!("capture admission failed").context("command failed")),
            ExtensionMenuActionDisposition::ReturnToIdle,
        )
        .await
        .unwrap();
        assert_eq!(disposition, ExtensionMenuActionDisposition::ReturnToIdle);
        assert_eq!(
            shell.debug_error().as_deref(),
            Some("Draw: command failed: capture admission failed")
        );
        shell.render();
        let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
        assert!(
            frame.contains("Draw: command failed: capture admission failed"),
            "{frame}"
        );
        assert!(!shell.has_overlay());
        assert!(!shell.debug_snapshot().contains("Draw finished"));
    }

    #[tokio::test]
    async fn fullscreen_menu_success_does_not_reopen_a_document() {
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        for output in ["", "component result"] {
            let disposition = present_extension_menu_action_result(
                &mut shell,
                &mut input,
                "Draw",
                Ok(output.into()),
                ExtensionMenuActionDisposition::ReturnToIdle,
            )
            .await
            .unwrap();
            assert_eq!(disposition, ExtensionMenuActionDisposition::ReturnToIdle);
            assert!(!shell.has_overlay());
            assert!(!shell.debug_snapshot().contains("Draw finished"));
        }
    }

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

    fn summary(
        source: octet_agent::extension_process::ExtensionSource,
        trusted: bool,
    ) -> crate::extensions::ExtensionSummary {
        crate::extensions::ExtensionSummary {
            name: "fixture".into(),
            version: "0.1.0".into(),
            manifest_path: std::path::PathBuf::from(
                "/workspace/.octet/extensions/fixture/extension.toml",
            ),
            manifest_digest: String::new(),
            bundle_digest: None,
            source,
            enabled: true,
            trusted,
            running: trusted,
            api_version: "0.1".into(),
            negotiated_features: Vec::new(),
            telemetry_schema: None,
            compatibility: "compatible".into(),
            health: None,
            runtime: None,
            tools: Vec::new(),
            commands: Vec::new(),
            hooks: Vec::new(),
            ui: Vec::new(),
            providers: Vec::new(),
        }
    }

    #[test]
    fn authority_rows_offer_grant_and_revoke_without_confusing_on_off() {
        use octet_agent::extension_process::ExtensionSource;
        let mut config =
            super::super::tests::terminal_theme_test_config(std::path::PathBuf::from("/workspace"));
        let mut project = summary(ExtensionSource::Project, false);
        assert_eq!(authority_label(&config, &project), "not granted");
        assert_eq!(authority_action(&config, &project, true), Some(true));
        assert_eq!(authority_action(&config, &project, false), None);
        config
            .trusted_extensions
            .push(authority_grant(&project).unwrap());
        project.trusted = true;
        assert_eq!(authority_label(&config, &project), "granted");
        assert_eq!(authority_action(&config, &project, true), Some(false));
        project.enabled = false;
        assert_eq!(
            authority_action(&config, &project, true),
            Some(false),
            "authority remains distinct from On/off"
        );
        config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
        assert_eq!(authority_label(&config, &project), "implicit (full access)");
        config.invocation_trusted_extensions.push("fixture".into());
        assert_eq!(
            authority_action(&config, &project, true),
            None,
            "one-shot grants cannot be revoked in user config"
        );
        let explicit = summary(ExtensionSource::Explicit, true);
        assert_eq!(
            authority_label(&config, &explicit),
            "granted (--extension-dir)"
        );
        assert_eq!(authority_action(&config, &explicit, true), None);
    }

    #[test]
    fn a_stopped_extension_offers_its_state_instead_of_actions() {
        let options = not_running_options();
        assert!(options.generated);
        assert!(options.menu.items.is_empty());
        assert_eq!(options.menu.status.unwrap().label, "Not running");
    }
}
