//! First-run choices. Credentials never pass through the conversation composer.

use super::*;
use crate::provider_setup::builtin_api_key_providers as api_key_providers;

const FIRST_RUN_CHOICES: &[&str] = &[
    "Add an API key",
    "Sign in with ChatGPT / other supported OAuth subscriptions",
    "Local/self-hosted models",
    "Continue without a provider",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupRoute {
    ApiKey,
    Subscription,
    Local,
}

async fn choose_route<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<Option<SetupRoute>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    Ok(
        match provider_setup_picker(
            shell,
            input,
            "Set up a provider",
            FIRST_RUN_CHOICES
                .iter()
                .map(|choice| (*choice).to_owned())
                .collect(),
            vec![
                Some(
                    "Choose a provider and paste its API key; no environment variables needed"
                        .into(),
                ),
                Some("Use an existing ChatGPT or GitHub Copilot subscription".into()),
                Some("Set up LM Studio or an OpenAI-compatible endpoint".into()),
                Some("Open read-only without writing provider data".into()),
            ],
            0,
        )
        .await?
        {
            Some(0) => Some(SetupRoute::ApiKey),
            Some(1) => Some(SetupRoute::Subscription),
            Some(2) => Some(SetupRoute::Local),
            _ => None,
        },
    )
}

/// Preserve explicit model selection and existing inventories. This is onboarding,
/// not a new precedence layer over a configured invocation.
fn should_offer(config: &Config, catalog: &octet_ai::ModelCatalog) -> bool {
    !config.model_explicit && catalog.models().next().is_none()
}

/// The selected local model is already reviewed/persisted by guided setup. Cloud
/// authentication refreshes the inventory, then uses the ordinary startup model
/// picker (including its persistence and resumed-session precedence).
struct SetupResult {
    catalog: octet_ai::ModelCatalog,
    model: Option<ModelId>,
}

pub(super) async fn run(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    boot: &mut Bootstrap,
) -> anyhow::Result<()> {
    if !should_offer(&boot.config, &boot.catalog) {
        return Ok(());
    }
    while !shell.close_requested() {
        let Some(route) = choose_route(shell, input).await? else {
            return Ok(());
        };
        let result = match route {
            SetupRoute::Local => guided_provider_setup(shell, input, &boot.config)
                .await?
                .map(|completed| SetupResult {
                    catalog: completed.catalog,
                    model: Some(completed.model),
                }),
            SetupRoute::ApiKey => {
                if !add_api_key(shell, input).await? {
                    continue;
                }
                refresh_catalog(shell, input, boot.config.offline).await?
            }
            SetupRoute::Subscription => {
                if !sign_in(shell, input, boot.config.offline).await? {
                    continue;
                }
                refresh_catalog(shell, input, boot.config.offline).await?
            }
        };
        if let Some(result) = result {
            let notice = match &result.model {
                Some(model) => format!("provider setup saved · {}", model.0),
                None => "provider configured; select a model to get started".to_owned(),
            };
            install_result(boot, result);
            shell.set_runtime_config(boot.config.clone());
            shell.clear_error();
            shell.notice(notice);
            shell.render();
            return Ok(());
        }
    }
    Ok(())
}

fn install_result(boot: &mut Bootstrap, result: SetupResult) {
    boot.catalog = result.catalog;
    if let Some(model) = result.model {
        boot.config.model = Some(model);
        boot.config.model_explicit = false;
    }
}

async fn refresh_catalog<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    offline: bool,
) -> anyhow::Result<Option<SetupResult>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    match run_blocking_lifecycle(shell, input, "refreshing provider models…", move || {
        crate::app::bootstrap::model_catalog_with_offline(offline)
    })
    .await
    {
        Ok(catalog) if catalog.models().next().is_some() => Ok(Some(SetupResult {
            catalog,
            model: None,
        })),
        Ok(_) => {
            shell.error("Credential saved, but no models are available. Retry setup or continue without a provider; offline discovery may require restarting online.".into());
            shell.render();
            Ok(None)
        }
        Err(error) if shell.close_requested() => Err(error),
        Err(_) => {
            // Provider failures may contain remote response details. Never echo
            // those into onboarding's transcript after accepting a secret.
            shell.error("Credential saved, but model discovery failed. Retry setup or restart octet to reload models.".into());
            shell.render();
            Ok(None)
        }
    }
}

async fn add_api_key<S>(shell: &mut InteractiveShell, input: &mut S) -> anyhow::Result<bool>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let providers = api_key_providers();
    let mut labels: Vec<_> = providers
        .iter()
        .map(|provider| provider.label.to_owned())
        .collect();
    labels.push("Back".into());
    let descriptions = vec![None; labels.len()];
    let Some(selected) = provider_setup_picker(
        shell,
        input,
        "Choose your API-key provider",
        labels,
        descriptions,
        0,
    )
    .await?
    else {
        return Ok(false);
    };
    let Some(provider) = providers.get(selected) else {
        return Ok(false);
    };
    let storage = (|| -> anyhow::Result<_> {
        let store = crate::provider_setup::BuiltinApiKeyStore::default_store()?;
        let replace_existing = store.path(provider.id)?.try_exists()?;
        Ok((store, replace_existing))
    })();
    let (store, replace_existing) = match storage {
        Ok(storage) => storage,
        Err(_) => {
            shell.error("The private credential location is unavailable. Check permissions and retry, or choose another setup method.".into());
            shell.render();
            return Ok(false);
        }
    };
    if replace_existing
        && provider_setup_picker(
            shell,
            input,
            "An API key is already saved",
            vec!["Replace the saved key after review".into(), "Back".into()],
            vec![
                Some("The current key is unchanged until you confirm the new key".into()),
                None,
            ],
            1,
        )
        .await?
            != Some(0)
    {
        return Ok(false);
    }
    enter_and_save_api_key(shell, input, provider.label, |key| {
        store
            .save(provider.id, key, replace_existing)
            .map(|_| ())
            .map_err(Into::into)
    })
    .await
}

/// The persistence callback is invoked only after explicit review. Kept narrow
/// so keyboard cancellation/retry tests never need a real credential or network.
async fn enter_and_save_api_key<S, F>(
    shell: &mut InteractiveShell,
    input: &mut S,
    label: &str,
    mut save: F,
) -> anyhow::Result<bool>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
    F: FnMut(String) -> anyhow::Result<()>,
{
    while !shell.close_requested() {
        let Some(key) = guided_setup_input(
            shell,
            input,
            &format!("{label} API key (input hidden; paste, then Enter):"),
            true,
        )
        .await?
        else {
            return Ok(false);
        };
        let key = key.trim().to_owned();
        if key.is_empty()
            || key.len() > octet_ai::auth::MAX_ENV_VALUE_BYTES
            || !key.bytes().all(|byte| byte.is_ascii_graphic())
        {
            shell.error(
                "Enter a non-empty API key without spaces or line breaks (maximum 4 KiB).".into(),
            );
            shell.render();
            continue;
        }
        match provider_setup_picker(
            shell,
            input,
            &format!("Save {label} API key?"),
            vec!["Save API key".into(), "Enter a different key".into(), "Back".into()],
            vec![
                Some("Store the original key in octet's owner-private credential file, not in config or conversation history. Provider requests require the original secret, not a hash.".into()),
                Some("Discard this key and paste another".into()),
                Some("Discard this key without saving".into()),
            ],
            0,
        ).await? {
            Some(0) => match save(key) {
                Ok(()) => return Ok(true),
                Err(_) => {
                    shell.error("The API key could not be saved. Check the private credential file permissions and retry; no key is shown in diagnostics.".into());
                    shell.render();
                }
            },
            Some(1) => continue,
            _ => return Ok(false),
        }
    }
    Ok(false)
}

#[derive(Clone, Copy)]
enum Subscription {
    ChatGpt,
    Copilot,
}

async fn sign_in<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    offline: bool,
) -> anyhow::Result<bool>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let selection = provider_setup_picker(
        shell,
        input,
        "Sign in with a subscription",
        vec![
            "ChatGPT (OpenAI Codex)".into(),
            "GitHub Copilot".into(),
            "Back".into(),
        ],
        vec![
            Some("OpenAI's hosted device login; available models depend on your account".into()),
            Some("GitHub.com's device login; only supported Copilot routes are offered".into()),
            Some("Return without starting sign-in".into()),
        ],
        0,
    )
    .await?;
    let subscription = match selection {
        Some(0) => Subscription::ChatGpt,
        Some(1) => Subscription::Copilot,
        _ => return Ok(false),
    };
    if offline {
        shell.error("Subscription sign-in requires a connection. Restart without --offline to sign in, or choose an API key/local endpoint.".into());
        shell.render();
        return Ok(false);
    }
    // Reuse the real host-owned flows, including their browser/device-code
    // fallback. No credentials are imported from other applications.
    shell.set_run_label("signing in…");
    shell.render();
    shell.suspend();
    let result = tokio::select! {
        biased;
        _ = crate::tui::terminal::wait_for_shutdown_signal() => {
            shell.request_close();
            Ok(false)
        }
        result = async {
            match subscription {
                Subscription::ChatGpt => {
                    let store = crate::auth::codex::CredentialStore::new(crate::auth::codex::default_path());
                    crate::auth::codex::login(&store, false).await
                }
                Subscription::Copilot => {
                    let store = crate::auth::copilot::CredentialStore::new(crate::auth::copilot::default_path()?);
                    crate::auth::copilot::login(&store, false).await
                }
            }
        } => result.map(|()| true),
    };
    // Always restore the renderer, including failure and cancellation paths.
    shell.resume()?;
    shell.set_run_label("idle");
    match result {
        Ok(success) => Ok(success),
        Err(_) => {
            shell.error("Sign-in did not complete. Try again or choose another provider.".into());
            shell.render();
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn paste(value: &str) -> std::io::Result<Event> {
        Ok(Event::Paste(value.to_owned()))
    }

    #[tokio::test]
    async fn first_run_choices_are_ordered_and_keyboard_cancellable() {
        for (down, expected) in [
            (0, Some(SetupRoute::ApiKey)),
            (1, Some(SetupRoute::Subscription)),
            (2, Some(SetupRoute::Local)),
            (3, None),
        ] {
            let mut shell = InteractiveShell::test_shell();
            let mut events: Vec<_> = (0..down).map(|_| key(KeyCode::Down)).collect();
            events.push(key(KeyCode::Enter));
            let result = choose_route(&mut shell, &mut tokio_stream::iter(events))
                .await
                .unwrap();
            assert_eq!(result, expected);
        }
        for event in [
            key(KeyCode::Esc),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            ))),
        ] {
            let mut shell = InteractiveShell::test_shell();
            assert_eq!(
                choose_route(&mut shell, &mut tokio_stream::iter([event]))
                    .await
                    .unwrap(),
                None
            );
        }
    }

    #[tokio::test]
    async fn api_key_cancel_and_cancel_review_never_save_or_leak_to_composer() {
        let secret = "sk-test-private-onboarding";
        let sequences = vec![
            vec![paste(secret), key(KeyCode::Esc)],
            vec![paste(secret), key(KeyCode::Enter), key(KeyCode::Esc)],
            vec![
                paste(secret),
                key(KeyCode::Enter),
                key(KeyCode::Down),
                key(KeyCode::Down),
                key(KeyCode::Enter),
            ],
            vec![
                paste(secret),
                Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                ))),
            ],
        ];
        for events in sequences {
            let mut shell = InteractiveShell::test_shell();
            shell.extension_set_editor("existing draft".into());
            let mut saves = 0;
            let saved = enter_and_save_api_key(
                &mut shell,
                &mut tokio_stream::iter(events),
                "Test provider",
                |_| {
                    saves += 1;
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert!(!saved);
            assert_eq!(saves, 0);
            assert_eq!(shell.pending(), "existing draft");
            assert!(!shell.debug_snapshot().contains(secret));
            assert!(!shell
                .dump_rendered_frame()
                .await
                .unwrap()
                .join("\n")
                .contains(secret));
        }
    }

    #[tokio::test]
    async fn api_key_edit_and_failed_save_retry_keep_secrets_out_of_diagnostics() {
        let mut shell = InteractiveShell::test_shell();
        let mut input = tokio_stream::iter([
            paste("discarded-secret"),
            key(KeyCode::Enter),
            key(KeyCode::Down),
            key(KeyCode::Enter),
            paste("failed-secret"),
            key(KeyCode::Enter),
            key(KeyCode::Enter),
            paste("retry-secret"),
            key(KeyCode::Enter),
            key(KeyCode::Enter),
        ]);
        let mut saved = Vec::new();
        assert!(
            enter_and_save_api_key(&mut shell, &mut input, "Test provider", |key| {
                saved.push(key.clone());
                if saved.len() == 1 {
                    anyhow::bail!("simulated failure including {key}");
                }
                Ok(())
            })
            .await
            .unwrap()
        );
        assert_eq!(saved, ["failed-secret", "retry-secret"]);
        let transcript = shell.debug_snapshot();
        let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
        for secret in ["discarded-secret", "failed-secret", "retry-secret"] {
            assert!(!transcript.contains(secret));
            assert!(!frame.contains(secret));
            assert!(!shell.pending().contains(secret));
        }
    }

    #[tokio::test]
    async fn empty_key_retries_without_saving_and_eof_cancels() {
        let mut shell = InteractiveShell::test_shell();
        let mut input = tokio_stream::iter([key(KeyCode::Enter)]);
        assert!(
            !enter_and_save_api_key(&mut shell, &mut input, "Test provider", |_| {
                panic!("an empty key must not be saved")
            })
            .await
            .unwrap()
        );
        assert!(shell.pending().is_empty());
    }

    #[tokio::test]
    async fn subscription_cancel_and_offline_never_start_oauth() {
        for events in [
            vec![key(KeyCode::Esc)],
            vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)],
            vec![key(KeyCode::Enter)],
            vec![key(KeyCode::Down), key(KeyCode::Enter)],
        ] {
            let mut shell = InteractiveShell::test_shell();
            assert!(!sign_in(&mut shell, &mut tokio_stream::iter(events), true)
                .await
                .unwrap());
        }
    }

    #[test]
    fn picker_offers_only_single_key_fixed_endpoint_providers() {
        let providers = api_key_providers();
        for expected in ["openai", "anthropic", "openrouter"] {
            assert!(providers.iter().any(|provider| provider.id == expected));
        }
        for unavailable in [
            "codex",
            "github-copilot",
            "bedrock",
            "azure-openai",
            "vertex",
            "cloudflare-workers-ai",
            "cloudflare-ai-gateway",
        ] {
            assert!(!providers.iter().any(|provider| provider.id == unavailable));
        }
    }

    #[test]
    fn startup_inventory_refresh_preserves_configured_selection_and_resume_precedence() {
        let directory = tempfile::tempdir().unwrap();
        let mut config =
            super::super::tests::terminal_theme_test_config(directory.path().to_path_buf());
        config.session_dir = directory.path().join("sessions");
        let empty = octet_ai::ModelCatalog::default();
        assert!(should_offer(&config, &empty));
        config.model_explicit = true;
        assert!(!should_offer(&config, &empty));
        config.model_explicit = false;
        let catalog = octet_ai::ModelCatalog::builtin().unwrap();
        assert!(!should_offer(&config, &catalog));
        config.model = Some(ModelId("configured-model".into()));
        let mut boot = crate::app::bootstrap::bootstrap(config).unwrap();
        install_result(
            &mut boot,
            SetupResult {
                catalog: catalog.clone(),
                model: None,
            },
        );
        assert_eq!(boot.config.model, Some(ModelId("configured-model".into())));
        assert!(!boot.config.model_explicit);
        assert_eq!(boot.catalog.models().count(), catalog.models().count());
        let model = catalog.models().next().unwrap().id.clone();
        install_result(
            &mut boot,
            SetupResult {
                catalog,
                model: Some(model.clone()),
            },
        );
        assert_eq!(boot.config.model, Some(model));
        assert!(!boot.config.model_explicit);
    }
}
