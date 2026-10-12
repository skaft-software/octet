//! `/setup` → "Remove a provider or sign-in".
//!
//! Lists only credentials octet itself stored, asks for confirmation (Cancel is
//! the default), deletes through each store's own safe path, then rebuilds the
//! model catalog. A provider the current model is using is refused rather than
//! removed, so nobody is stranded on a model whose credential was just deleted.
//! Environment-variable credentials are never touched; the notice says when one
//! still supplies the provider.

use super::*;

/// What a removable entry deletes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Removable {
    Codex,
    Copilot,
    /// A subscription login, identified by its registry selector.
    Subscription(&'static str),
    ApiKey(crate::provider_setup::ApiKeyProvider),
    CustomProvider(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) label: String,
    pub(super) description: String,
    pub(super) kind: Removable,
}

/// Every credential or provider octet has stored on this machine.
pub(super) fn removable_entries() -> Vec<Entry> {
    let mut entries = Vec::new();
    if crate::auth::codex::default_path()
        .try_exists()
        .unwrap_or(false)
    {
        entries.push(Entry {
            label: "ChatGPT / Codex sign-in".into(),
            description: "Signs out; the saved login is deleted".into(),
            kind: Removable::Codex,
        });
    }
    if crate::auth::copilot::default_path()
        .ok()
        .is_some_and(|path| path.try_exists().unwrap_or(false))
    {
        entries.push(Entry {
            label: "GitHub Copilot sign-in".into(),
            description: "Signs out; the saved login is deleted".into(),
            kind: Removable::Copilot,
        });
    }
    for flow in crate::auth::subscription::registry::all() {
        let path = crate::auth::subscription::store::default_path(flow.login());
        if path.try_exists().unwrap_or(false) {
            entries.push(Entry {
                label: format!("{} sign-in", flow.label()),
                description: "Signs out; the saved login is deleted".into(),
                kind: Removable::Subscription(flow.login()),
            });
        }
    }
    if let Ok(store) = crate::provider_setup::BuiltinApiKeyStore::default_store() {
        for provider in store.configured_providers() {
            entries.push(Entry {
                label: format!("{} API key", provider.label),
                description: "Deletes the key octet saved; an environment variable is not changed"
                    .into(),
                kind: Removable::ApiKey(provider),
            });
        }
    }
    let custom = crate::auth::custom::CredentialStore::new(crate::auth::custom::default_path());
    if let Ok(Some(registry)) = custom.load_registry() {
        for (id, provider) in &registry.providers {
            let name = if provider.label.trim().is_empty() {
                id.as_str()
            } else {
                provider.label.as_str()
            };
            entries.push(Entry {
                label: format!("Custom provider: {name}"),
                description: "Removes the endpoint, its key and its saved model list".into(),
                kind: Removable::CustomProvider(id.clone()),
            });
        }
    }
    entries
}

/// Whether the active model comes from the provider an entry would delete.
pub(super) fn is_in_use(endpoint_id: &str, model_id: &str, kind: &Removable) -> bool {
    let namespace = model_id.split_once('/').map(|(namespace, _)| namespace);
    let matches = |ids: &[&str]| {
        ids.iter()
            .any(|id| endpoint_id == *id || namespace == Some(*id))
    };
    match kind {
        Removable::Codex => matches(&[crate::auth::codex::ENDPOINT_ID, "codex"]),
        Removable::Copilot => matches(&["github-copilot", "copilot"]),
        Removable::Subscription(login) => crate::auth::subscription::registry::resolve(login)
            .is_some_and(|flow| matches(&[flow.endpoint_id(), flow.provider_id()])),
        Removable::ApiKey(provider) => matches(&[provider.id]),
        Removable::CustomProvider(id) => namespace == Some(id.as_str()),
    }
}

/// Delete the credential or provider. Returns an optional follow-up note (for
/// example that an environment variable still supplies the provider).
pub(super) async fn delete_entry(kind: &Removable) -> anyhow::Result<Option<String>> {
    match kind {
        Removable::Codex => {
            crate::auth::codex::CredentialStore::new(crate::auth::codex::default_path())
                .delete_async()
                .await?;
            Ok(None)
        }
        Removable::Copilot => {
            crate::auth::copilot::CredentialStore::new(crate::auth::copilot::default_path()?)
                .delete()?;
            Ok(None)
        }
        Removable::Subscription(login) => {
            let flow = crate::auth::subscription::registry::resolve(login)
                .ok_or_else(|| anyhow::anyhow!("unknown sign-in"))?;
            crate::auth::subscription::store_for(&flow)
                .delete_async()
                .await?;
            Ok(None)
        }
        Removable::ApiKey(provider) => {
            let store = crate::provider_setup::BuiltinApiKeyStore::default_store()?;
            store.remove(provider.id)?;
            Ok(std::env::var_os(provider.credential_variable)
                .is_some()
                .then(|| {
                    format!(
                        "{} is still available through ${}",
                        provider.label, provider.credential_variable
                    )
                }))
        }
        Removable::CustomProvider(id) => {
            let store =
                crate::auth::custom::CredentialStore::new(crate::auth::custom::default_path());
            store.remove_provider(id)?;
            Ok(None)
        }
    }
}

/// The first `/setup` choice. Offered only when octet has something saved to
/// remove; a fresh setup goes straight to adding a provider.
pub(super) enum SetupChoice {
    Add,
    Remove,
    Closed,
}

pub(super) async fn choose_setup_action(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<SetupChoice> {
    if removable_entries().is_empty() {
        return Ok(SetupChoice::Add);
    }
    Ok(
        match provider_setup_picker(
            shell,
            input,
            "Provider setup",
            vec![
                "Add or update a provider".to_owned(),
                "Remove a provider or sign-in".to_owned(),
            ],
            vec![
                Some("Add an API key, sign in, or set up a local endpoint".into()),
                Some("Delete a saved key, sign-in or custom provider".into()),
            ],
            0,
        )
        .await?
        {
            Some(0) => SetupChoice::Add,
            Some(1) => SetupChoice::Remove,
            _ => SetupChoice::Closed,
        },
    )
}

/// Pick, guard and confirm one removal. `None` means nothing was removed (the
/// user backed out, the provider is in use, or there was nothing to remove);
/// the reason has already been shown.
pub(super) async fn pick_confirmed_entry(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    active: Option<(&str, &str)>,
) -> anyhow::Result<Option<Entry>> {
    let entries = removable_entries();
    if entries.is_empty() {
        shell.notice("no providers or sign-ins saved by octet to remove");
        shell.render();
        return Ok(None);
    }
    let mut labels: Vec<String> = entries.iter().map(|entry| entry.label.clone()).collect();
    let mut descriptions: Vec<Option<String>> = entries
        .iter()
        .map(|entry| Some(entry.description.clone()))
        .collect();
    labels.push("Back".into());
    descriptions.push(None);
    let Some(selected) = provider_setup_picker(
        shell,
        input,
        "Remove a provider or sign-in",
        labels,
        descriptions,
        0,
    )
    .await?
    else {
        return Ok(None);
    };
    let Some(entry) = entries.get(selected).cloned() else {
        return Ok(None);
    };

    if active.is_some_and(|(endpoint, model)| is_in_use(endpoint, model, &entry.kind)) {
        shell.error(format!(
            "{} supplies the current model. Switch to a model from another provider with /model, then remove it.",
            entry.label
        ));
        shell.render();
        return Ok(None);
    }

    let confirmed = provider_setup_picker(
        shell,
        input,
        &format!("Remove {}?", entry.label),
        vec!["Cancel".into(), format!("Remove {}", entry.label)],
        vec![
            Some("Keep everything as it is".into()),
            Some(entry.description.clone()),
        ],
        0,
    )
    .await?;
    if confirmed != Some(1) {
        shell.notice("nothing removed");
        shell.render();
        return Ok(None);
    }
    Ok(Some(entry))
}

/// Delete a confirmed entry and rebuild the catalog without it. `None` means the
/// failure was already shown; otherwise the new catalog and the closing notice.
pub(super) async fn delete_and_rebuild(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    entry: &Entry,
    offline: bool,
) -> anyhow::Result<
    Option<(
        octet_ai::ModelCatalog,
        crate::app::bootstrap::CodexContextNotes,
        String,
    )>,
> {
    let follow_up = match delete_entry(&entry.kind).await {
        Ok(note) => note,
        Err(error) => {
            // Local filesystem errors only; no credential value is in them.
            shell.error(format!("could not remove {}: {error:#}", entry.label));
            shell.render();
            return Ok(None);
        }
    };
    match run_blocking_lifecycle(shell, input, "updating models…", move || {
        crate::app::bootstrap::model_catalog_for_readiness(
            offline,
            &crate::app::bootstrap::CatalogReadiness::Fleet,
        )
    })
    .await
    {
        Ok((catalog, notes)) => {
            let notice = match follow_up {
                Some(note) => format!("removed {} · {note}", entry.label),
                None => format!("removed {}", entry.label),
            };
            Ok(Some((catalog, notes, notice)))
        }
        Err(error) if shell.close_requested() => Err(error),
        Err(_) => {
            shell.error(format!(
                "Removed {}, but the model list could not be refreshed; restart octet to reload it.",
                entry.label
            ));
            shell.render();
            Ok(None)
        }
    }
}

/// In-session removal: the current model must not depend on what is deleted.
pub(super) async fn remove_provider(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<App> {
    let active = (app.model.endpoint.id.0.clone(), app.model.spec.id.0.clone());
    let Some(entry) =
        pick_confirmed_entry(shell, input, Some((active.0.as_str(), active.1.as_str()))).await?
    else {
        return Ok(app);
    };
    let Some((catalog, notes, notice)) =
        delete_and_rebuild(shell, input, &entry, app.config.offline).await?
    else {
        return Ok(app);
    };
    if let Err(error) = app.apply_provider_setup_catalog(catalog, notes) {
        shell.error(format!(
            "Removed {}, but the model list could not be refreshed: {error}",
            entry.label
        ));
        shell.render();
        return Ok(app);
    }
    shell.set_model_cycle(app.model_cycle());
    shell.clear_error();
    shell.notice(notice);
    shell.render();
    Ok(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_is_in_use_when_the_current_endpoint_or_model_namespace_matches() {
        let key = Removable::ApiKey(crate::provider_setup::ApiKeyProvider {
            id: "cerebras",
            label: "Cerebras",
            credential_variable: "CEREBRAS_API_KEY",
        });
        assert!(is_in_use("cerebras", "cerebras/gpt-oss-120b", &key));
        assert!(is_in_use("other", "cerebras/gpt-oss-120b", &key));
        assert!(!is_in_use("openai", "openai/gpt-5", &key));
        assert!(!is_in_use("openai", "cerebrasish/model", &key));
        assert!(is_in_use(
            "openai-codex",
            "codex/gpt-6-luna",
            &Removable::Codex
        ));
        assert!(!is_in_use("openai", "openai/gpt-5", &Removable::Codex));
        assert!(is_in_use(
            "custom-openai",
            "home-server/llama",
            &Removable::CustomProvider("home-server".into())
        ));
        assert!(!is_in_use(
            "custom-openai",
            "other/llama",
            &Removable::CustomProvider("home-server".into())
        ));
    }
}
