//! Real App + adapter acceptance for model facts and in-place idle setters.
//! Active request-boundary selection and providers remain separately gated.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_app};
use super::*;

const FACTS: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.registerCommand('probe', { handler: async (_, ctx) => {
    const model = ctx.model;
    const available = ctx.modelRegistry.getAvailable();
    const scoped = ctx.scopedModels;
    const denied = action => { try { action(); return false; } catch (error) { return error.code === -32601; } };
    appendFileSync(TRACE, JSON.stringify({ model, level: pi.getThinkingLevel(), contextLevel: ctx.thinkingLevel,
      available, scoped, keyDenied: denied(() => ctx.modelRegistry.getApiKey(model)),
      fullInventoryDenied: denied(() => ctx.modelRegistry.getAll()) }) + '\n');
    // Mutating this snapshot must not change the next host observation.
    if (model) model.input.push('not-a-native-modality');
  } });
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_model_facts_follow_the_real_host_and_invocation_scope() {
    let (directory, mut app) = pi_app(FACTS);
    let default_model = app.config.model.clone();
    let default_reasoning = app.config.reasoning.clone();
    let expected_available = app
        .catalog
        .models()
        .filter(|spec| {
            app.catalog
                .resolve(&spec.id)
                .is_ok_and(|model| model.endpoint.auth.is_configured())
        })
        .count();
    let mut shell = InteractiveShell::test_shell();
    request_extension_ui(&mut shell, &mut app);
    command(&mut app, &mut shell, "probe").await.unwrap();
    app.model_scope = Some(vec![crate::cli::parity::ScopedModel {
        id: app.model.spec.id.clone(),
        pattern: app.model.spec.id.0.clone(),
        reasoning: Some("off".into()),
    }]);
    request_extension_ui(&mut shell, &mut app);
    command(&mut app, &mut shell, "probe").await.unwrap();
    app.executable_extensions.shutdown().await;
    let values = std::fs::read_to_string(directory.path().join("trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2, "{values:#?}");
    for value in &values {
        assert_eq!(value["model"]["id"], app.model.spec.api_name);
        assert_eq!(value["level"], "off");
        assert_eq!(value["contextLevel"], value["level"]);
        assert_eq!(
            value["available"].as_array().unwrap().len(),
            expected_available
        );
        assert_eq!(value["keyDenied"], true);
        assert_eq!(value["fullInventoryDenied"], true);
        assert!(!value["model"]["input"]
            .to_string()
            .contains("not-a-native-modality"));
    }
    assert_eq!(values[0]["scoped"], serde_json::json!([]));
    assert_eq!(values[1]["scoped"].as_array().unwrap().len(), 1);
    assert_eq!(
        values[1]["scoped"][0]["model"]["id"],
        app.model.spec.api_name
    );
    assert_eq!(values[1]["scoped"][0]["thinkingLevel"], "off");
    assert_eq!(app.config.model, default_model);
    assert_eq!(app.config.reasoning, default_reasoning);
}

const SETTERS: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.registerCommand('select', { handler: async (_, ctx) => {
    const target = ctx.modelRegistry.getAvailable().find(m => m.provider === 'pi-idle-local');
    if (!target) throw new Error('real local catalog route missing');
    const selected = await pi.setModel(target);
    if (!selected) throw new Error('real local model selection refused');
    pi.setThinkingLevel('high');
    appendFileSync(TRACE, JSON.stringify({selected, model:ctx.model, level:pi.getThinkingLevel()})+'\n');
    const missing = await pi.setModel({...target, id:'absent-local-model'});
    if (missing) throw new Error('unknown model fabricated success');
  } });
  pi.registerCommand('observe', { handler: async (_, ctx) => {
    appendFileSync(TRACE, JSON.stringify({model:ctx.model, level:pi.getThinkingLevel()})+'\n');
  } });
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_idle_model_setters_preserve_the_live_command_and_binding() {
    use octet_ai::{auth::Auth, EndpointId, ModelId, ReasoningEffort};
    let (directory, mut app) = pi_app(SETTERS);
    // A real configured, unauthenticated local route, not a fake credential or
    // fabricated adapter snapshot. This test performs no provider request.
    let source = app
        .catalog
        .models()
        .find(|spec| {
            spec.capabilities
                .reasoning
                .as_ref()
                .is_some_and(|reasoning| {
                    let choices = reasoning.choices();
                    choices.contains(&ReasoningConfig::Effort(ReasoningEffort::High))
                        && choices.contains(&ReasoningConfig::Effort(ReasoningEffort::Low))
                })
        })
        .expect("offline catalog contains a portable reasoning model")
        .id
        .clone();
    let source = app.catalog.resolve(&source).unwrap();
    let mut endpoint = (*source.endpoint).clone();
    endpoint.id = EndpointId("pi-idle-local".into());
    endpoint.auth = Auth::None;
    endpoint.base_url = "http://127.0.0.1:9/v1/".parse().unwrap();
    app.catalog.register_endpoint(endpoint.clone()).unwrap();
    let mut spec = (*source.spec).clone();
    spec.id = ModelId("pi-idle-model".into());
    spec.endpoint = endpoint.id;
    app.catalog.register_model(spec.clone()).unwrap();
    let session = app.agent.session().path().to_owned();
    let owner = app.agent.session().resource_owner_key();
    let default_model = app.config.model.clone();
    let default_reasoning = app.config.reasoning.clone();
    let mut shell = InteractiveShell::test_shell();
    request_extension_ui(&mut shell, &mut app);
    command(&mut app, &mut shell, "select").await.unwrap();
    assert_eq!(app.model.spec.id, spec.id);
    assert_eq!(app.agent.model().spec.id, spec.id);
    assert_eq!(app.agent.reasoning(), &app.reasoning);
    assert_eq!(app.agent.session().path(), session);
    assert_eq!(app.agent.session().resource_owner_key(), owner);
    command(&mut app, &mut shell, "observe").await.unwrap();
    app.executable_extensions.shutdown().await;
    let values = std::fs::read_to_string(directory.path().join("trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2, "{values:#?}");
    assert_eq!(values[0]["selected"], true);
    for value in &values {
        assert_eq!(value["model"]["provider"], "pi-idle-local");
        assert_eq!(value["model"]["id"], spec.api_name);
        assert_eq!(value["level"], "high");
    }
    assert!(app.agent.session().entries().iter().any(|entry| matches!(&entry.value,
        octet_agent::session::EntryValue::Config { model: Some(model), reasoning: Some(reasoning), .. }
        if model == "pi-idle-model" && reasoning == "high")));
    assert_eq!(app.config.model, default_model);
    assert_eq!(app.config.reasoning, default_reasoning);
}
