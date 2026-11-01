use super::*;
use crate::extensions::resource_paths::{GuardedSkills, ResourceLease};
use crate::extensions::{ExecutableExtensions, ExtensionProviderRuntime, ExtensionStartupTiming};
use crate::session_store::SessionStore;
use octet_agent::{CoreTools, ExtensionHost, SkillRegistry};

pub(super) struct ModelessExtensions {
    pub(super) host: ExtensionHost,
    pub(super) extensions: ExecutableExtensions,
    pub(super) config: Config,
    baseline_skills: Arc<dyn SkillRegistry>,
    baseline_prompts: Arc<crate::prompts::PromptRegistry>,
    lease: Option<ResourceLease>,
    resources_dirty: bool,
}

impl ModelessExtensions {
    #[cfg(test)]
    pub(super) fn new(
        config: &Config,
        session: &Session,
        sessions: &SessionStore,
        reason: &'static str,
        shell: Option<&InteractiveShell>,
    ) -> anyhow::Result<Self> {
        Self::with_runtime(
            config,
            session,
            sessions,
            reason,
            shell,
            ExtensionProviderRuntime::default(),
        )
    }

    pub(super) fn with_runtime(
        config: &Config,
        session: &Session,
        sessions: &SessionStore,
        reason: &'static str,
        shell: Option<&InteractiveShell>,
        runtime: ExtensionProviderRuntime,
    ) -> anyhow::Result<Self> {
        let mut host = ExtensionHost::new();
        host.load(&CoreTools);
        let tool_config = config.clone();
        host.set_tool_policy(move |name| tool_config.tool_available(name));
        host.finalize_tool_surface();
        let skills: Arc<dyn SkillRegistry> = Arc::new(
            crate::resources::FileSystemSkillRegistry::new_with_invocation(
                config.workspace.clone(),
                config.invocation_cwd.clone(),
                config.skill_paths.clone(),
                config.workspace_trusted,
            )?,
        );
        let prompts = Arc::new(crate::prompts::PromptRegistry::discover(
            &config.workspace,
            &config.prompt_paths,
            config.workspace_trusted,
        ));
        let mut extensions = ExecutableExtensions::discover_and_start_for_selection(
            config,
            session,
            None,
            sessions,
            &mut host,
            None,
            runtime,
            crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
            shell,
            reason,
            ExtensionStartupTiming::AfterFirstFrame,
        );
        extensions.bind_command_resources(
            skills.clone(),
            prompts.clone(),
            Default::default(),
            None,
        );
        Ok(Self {
            host,
            extensions,
            config: config.clone(),
            baseline_skills: skills,
            baseline_prompts: prompts,
            lease: None,
            resources_dirty: true,
        })
    }

    pub(super) fn pump(
        &mut self,
        shell: &mut InteractiveShell,
        session: &mut Session,
        sessions: &SessionStore,
    ) {
        self.extensions
            .refresh_host_state_for_selection(session, None, sessions);
        for message in self.extensions.drain_events_for_shell(shell) {
            shell.notice(message);
        }
        if self
            .extensions
            .apply_modeless_session_host_requests(session, sessions)
        {
            update_session_title(shell, sessions, session);
        }
        self.extensions.sync_semantic_ui(shell);
        self.extensions
            .sync_editor_state(shell.extension_editor_snapshot());
        shell.set_extension_commands(Arc::from(native_slash_command_suggestions(
            &self.extensions,
        )));
    }

    pub(super) async fn attach(&mut self, shell: &mut InteractiveShell, session: &Session) {
        let progress = self
            .extensions
            .pump_deferred_startup(&mut self.host, session)
            .await;
        self.resources_dirty |= progress.changed;
        for notice in progress.notices {
            shell.notice(notice);
        }
    }

    async fn work<F, T, S>(
        &mut self,
        shell: &mut InteractiveShell,
        input: &mut S,
        session: &mut Session,
        sessions: &SessionStore,
        future: F,
    ) -> anyhow::Result<T>
    where
        F: Future<Output = anyhow::Result<T>>,
        S: Stream<Item = std::io::Result<Event>> + Unpin,
    {
        tokio::pin!(future);
        let wake = self.extensions.remote_ui_wake();
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            self.pump(shell, session, sessions);
            if shell.close_requested() {
                anyhow::bail!("extension work cancelled by close");
            }
            tokio::select! {
                biased;
                _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                    shell.request_close();
                    anyhow::bail!("extension work cancelled by shutdown");
                }
                result = &mut future => return result,
                _ = crate::extensions::remote_ui::notified(&wake) => { shell.render(); }
                _ = tick.tick() => { shell.render(); }
                event = input.next() => {
                    let event = match event {
                        Some(event) => event?,
                        None => { shell.request_close(); anyhow::bail!("input closed during extension work"); }
                    };
                    if self.extensions.route_remote_ui_event(shell, &event, false) { continue; }
                    observe_extension_terminal_event(&mut self.extensions, &event);
                    match event {
                        Event::Key(key) if is_ctrl_c(&key) => {
                            crate::tui::terminal::request_coordinated_shutdown(RAW_CTRL_C_SIGNAL)?;
                            shell.request_close();
                        }
                        Event::Key(key) if keymap::is_close_key(&key) => shell.request_close(),
                        Event::Resize(columns, rows) => shell.set_size(columns, rows),
                        event => {
                            if !shell.intercept_transcript_input(&event) && !paste_clipboard_text(shell, &event).await {
                                let _ = handle_cancellable_wait_input(shell, event);
                            }
                        }
                    }
                    shell.render();
                }
            }
        }
    }

    pub(super) async fn refresh_resources<S>(
        &mut self,
        shell: &mut InteractiveShell,
        input: &mut S,
        session: &mut Session,
        sessions: &SessionStore,
        baseline: &Config,
    ) -> anyhow::Result<()>
    where
        S: Stream<Item = std::io::Result<Event>> + Unpin,
    {
        if shell.close_requested() || self.extensions.startup_pending() {
            return Ok(());
        }
        let resources = self.extensions.has_resource_consumer_processes();
        let dirty = self.resources_dirty
            || self
                .lease
                .as_ref()
                .is_some_and(|lease| !lease.matches(&self.extensions));
        if !self.extensions.has_pending_session_starts() && !dirty {
            return Ok(());
        }
        let starts = self.extensions.resource_session_starts()?;
        let lease = self.work(shell, input, session, sessions, starts).await?;
        if !resources && self.lease.is_none() {
            self.resources_dirty = false;
            return Ok(());
        }
        let reason = if self.lease.is_none() {
            octet_agent::extension_process::ExtensionResourceDiscoveryReason::Startup
        } else {
            octet_agent::extension_process::ExtensionResourceDiscoveryReason::Reload
        };
        if let Some(old) = self.lease.take() {
            old.revoke();
        }
        let discovery = self.extensions.prepare_resource_discovery(reason)?;
        let loader = self.extensions.resource_loader();
        let base = baseline.clone();
        let background = shell.theme().background();
        let candidate = self
            .work(shell, input, session, sessions, async move {
                let batch = discovery.await;
                tokio::task::spawn_blocking(move || {
                    loader.load_resource_discovery(&base, batch, background)
                })
                .await?
            })
            .await;
        let loaded = match candidate {
            Ok(loaded) => loaded,
            Err(error) if shell.close_requested() => return Err(error),
            Err(error) => {
                let loader = self.extensions.resource_loader();
                let base = baseline.clone();
                let skills = self.baseline_skills.clone();
                let prompts = self.baseline_prompts.clone();
                tokio::task::spawn_blocking(move || {
                    let theme = load_theme_for_background(&base, background);
                    loader.retained_baseline(
                        base,
                        skills,
                        prompts,
                        theme,
                        format!("extension resources withdrawn: {error}"),
                    )
                })
                .await??
            }
        };
        anyhow::ensure!(
            loaded.is_current(&self.extensions) && lease.matches(&self.extensions),
            "stale model-less resource candidate"
        );
        if loaded.default_model.is_some() || loaded.default_thinking_level.is_some() {
            shell.notice("Pi model/thinking defaults require a configured model and restart; prompts remain disabled");
        }
        for diagnostic in loaded.diagnostics {
            shell.notice(diagnostic);
        }
        let skills: Arc<dyn SkillRegistry> = Arc::new(GuardedSkills {
            inner: loaded.skills,
            lease: lease.clone(),
        });
        let prompts = Arc::new((*loaded.prompts).clone().with_resource_lease(lease.clone()));
        self.extensions.bind_command_resources(
            skills,
            prompts,
            loaded.source_infos,
            Some(lease.clone()),
        );
        self.config = loaded.config;
        shell.set_theme(loaded.selected_theme);
        shell.set_runtime_config(self.config.clone());
        shell.set_session_keybindings(&loaded.keybindings);
        self.extensions.refresh_frontend_keybindings(shell);
        self.lease = Some(lease);
        self.resources_dirty = false;
        Ok(())
    }

    pub(super) async fn reload<S>(
        &mut self,
        shell: &mut InteractiveShell,
        input: &mut S,
        session: &mut Session,
        sessions: &SessionStore,
        baseline: &Config,
    ) -> anyhow::Result<crate::extensions::ExtensionReloadReport>
    where
        S: Stream<Item = std::io::Result<Event>> + Unpin,
    {
        if self.extensions.startup_pending() {
            shell.notice("extensions are still loading; retry /extensions reload after startup");
            return Ok(crate::extensions::ExtensionReloadReport::default());
        }
        self.extensions
            .revoke_terminal_grant_for_shell(shell, "the extensions are being reloaded");
        let work = self.extensions.prepare_resource_process_reload();
        let results = self
            .work(
                shell,
                input,
                session,
                sessions,
                async move { Ok(work.await) },
            )
            .await?;
        let report = self
            .extensions
            .finish_resource_process_reload(results)
            .await;
        self.resources_dirty = true;
        self.refresh_resources(shell, input, session, sessions, baseline)
            .await?;
        Ok(report)
    }

    pub(super) async fn command<S>(
        &mut self,
        shell: &mut InteractiveShell,
        input: &mut S,
        session: &mut Session,
        sessions: &SessionStore,
        text: &str,
    ) -> anyhow::Result<()>
    where
        S: Stream<Item = std::io::Result<Event>> + Unpin,
    {
        let Some((name, arguments)) = split_prompt_invocation(text) else {
            return Ok(());
        };
        let owner = self.extensions.command_owner(name);
        let registered = owner
            .as_deref()
            .and_then(|owner| self.extensions.registered_command_name(owner, name));
        if commands::is_reserved_slash_command_name(name)
            && !self.extensions.shadows_native_command(name)
            || !native_slash_command_suggestions(&self.extensions)
                .iter()
                .any(|(command, _)| command == name)
        {
            shell.error(format!("/{name} is unavailable; see /extensions status"));
            return Ok(());
        }
        let Some(registered) = registered else {
            shell.error(format!("/{name} is unavailable; see /extensions status"));
            return Ok(());
        };
        self.pump(shell, session, sessions);
        let Some(mut command) = self.extensions.prepare_owned_command(
            owner.as_deref(),
            &registered,
            arguments.split_whitespace().map(str::to_owned).collect(),
            true,
            0,
        ) else {
            shell.error(format!("/{name} is unavailable"));
            return Ok(());
        };
        let dialogs = self.extensions.lifecycle_snapshot();
        let result = {
            let mut frontend = InteractiveExtensionConfirmations {
                shell,
                input,
                dialogs: &dialogs,
                input_retired: false,
            };
            let failure = command
                .advance_without_model(&mut self.extensions, &mut frontend, session, sessions)
                .await
                .err();
            command
                .finish(&mut self.extensions, &mut frontend, failure)
                .await
        };
        self.pump(shell, session, sessions);
        match result {
            Ok(output) if output.trim().is_empty() => {}
            Ok(output) => shell.show_extension_output(name, output),
            Err(error) => shell.error(format!("/{name} failed: {error}")),
        }
        shell.render();
        Ok(())
    }
}
