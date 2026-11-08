//! Real App + adapter acceptance for model facts and in-place idle setters.
//! Active request-boundary selection and broader provider forms remain gated.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_app};
use super::*;
use serde_json::Value;

const CUSTOM_PROVIDER: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  const config = text => ({baseUrl:'http://127.0.0.1:9/local/', api:'openai-completions', apiKey:'explicit-local-dummy',
    models:[{id:'virtual-model',name:'Local Virtual',reasoning:false,input:['text','image'],cost:{input:1,output:2,cacheRead:0.1,cacheWrite:0.2},contextWindow:8192,maxTokens:1024}],
    streamSimple: (model, context, options) => (async function* () {
      // The logical native selection reaches exactly this process-owned
      // streamer; its physical dispatch choice stays local to the extension.
      const system = context.messages[0]?.role === 'system' ? context.messages[0].content : undefined;
      appendFileSync(TRACE,JSON.stringify({kind:'dispatch',id:model.id,provider:model.provider,baseUrl:model.baseUrl,
        explicitKey:options.apiKey==='explicit-local-dummy',system,messages:context.messages})+'\n');
      const partial={role:'assistant',model:'local-physical',api:'openai-completions',provider:'physical-local',content:[]};
      if(system==='cancel-native') {
        const aborted = new Promise(resolve=>options.signal.addEventListener('abort',resolve,{once:true}));
        try {
          yield {type:'start',partial};
          await aborted;
        } finally {
          appendFileSync(TRACE,JSON.stringify({kind:'cancelled',aborted:options.signal.aborted})+'\n');
        }
        return;
      }
      yield {type:'start',partial};
      partial.content.push({type:'text',text:''});
      yield {type:'text_start',contentIndex:0,partial};
      partial.content[0].text=text; yield {type:'text_delta',contentIndex:0,delta:text,partial};
      yield {type:'text_end',contentIndex:0,content:text,partial};
      yield {type:'done',reason:'stop',message:{...partial,usage:{input:1,output:2,cacheRead:0,cacheWrite:0,totalTokens:3}}};
    })()
  });
  pi.registerProvider('pi-local-provider',config('first-local-output'));
  pi.registerCommand('select', {handler:async (_,ctx)=>{
    const target=ctx.modelRegistry.getAvailable().find(m=>m.provider==='pi-local-provider'&&m.id==='virtual-model');
    if(!target||!await pi.setModel(target)) throw new Error('native provider route not selectable');
    appendFileSync(TRACE,JSON.stringify({kind:'selected',model:ctx.model})+'\n');
  }});
  pi.registerCommand('replace',{handler:()=>pi.registerProvider('pi-local-provider',config('replacement-local-output'))});
  pi.registerCommand('remove',{handler:()=>pi.unregisterProvider('pi-local-provider')});
};
"#;

fn pi_custom_provider_request() -> octet_ai::Request {
    octet_ai::Request {
        system: Some("local native request".into()),
        messages: vec![octet_ai::Message::User(octet_ai::UserMessage {
            content: vec![octet_ai::UserPart::Text("local fixture input".into())],
        })],
        tools: Vec::new(),
        tool_choice: Default::default(),
        max_output_tokens: Some(64),
        temperature: None,
        stop: Vec::new(),
        reasoning: Default::default(),
        reasoning_mode: Default::default(),
        responses: None,
        output_format: Default::default(),
        output_modalities: Default::default(),
        compatibility: Default::default(),
        cache_retention: Default::default(),
        session_id: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_custom_provider_registration_selection_replacement_and_stream_are_native() {
    use futures_util::StreamExt;
    use octet_ai::{AssistantPart, Modality};
    eprintln!("Pi custom provider: starting real App/adapter");
    let (directory, mut app) = pi_app(CUSTOM_PROVIDER);
    eprintln!("Pi custom provider: initialized");
    let mut shell = InteractiveShell::test_shell();
    app.synchronize_extension_provider_catalog();
    eprintln!("Pi custom provider: catalog synchronized");
    request_extension_ui(&mut shell, &mut app);
    let owner = app.agent.session().resource_owner_key();
    command(&mut app, &mut shell, "select").await.unwrap();
    eprintln!("Pi custom provider: selected");
    assert_eq!(app.model.spec.api_name, "virtual-model");
    assert_eq!(app.agent.model().spec.id, app.model.spec.id);
    assert_eq!(app.agent.session().resource_owner_key(), owner);
    assert!(app
        .model
        .spec
        .capabilities
        .input_modalities
        .contains(Modality::Image));
    assert_eq!(app.model.spec.pricing.as_ref().unwrap().input.0, 1_000_000);
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        app.client
            .complete(&app.model, pi_custom_provider_request()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(&response.message.content[..], [AssistantPart::Text(text)] if text == "first-local-output")
    );
    assert_eq!(response.usage.total_tokens, 3);
    eprintln!("Pi custom provider: first stream completed");
    command(&mut app, &mut shell, "replace").await.unwrap();
    app.synchronize_extension_provider_catalog();
    command(&mut app, &mut shell, "select").await.unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        app.client
            .complete(&app.model, pi_custom_provider_request()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(&response.message.content[..], [AssistantPart::Text(text)] if text == "replacement-local-output")
    );
    eprintln!("Pi custom provider: replacement stream completed");
    let mut cancellation_request = pi_custom_provider_request();
    cancellation_request.system = Some("cancel-native".into());
    let mut active = tokio::time::timeout(
        Duration::from_secs(10),
        app.client.stream(&app.model, cancellation_request),
    )
    .await
    .expect("native provider stream admission timed out")
    .unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(10), active.next())
        .await
        .expect("native provider start event timed out")
        .unwrap()
        .is_ok());
    command(&mut app, &mut shell, "remove").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), active.next())
            .await
            .expect("removed native route did not settle")
            .unwrap()
            .is_err(),
        "removed native route cannot keep streaming"
    );
    app.synchronize_extension_provider_catalog();
    assert!(!app
        .catalog
        .models()
        .any(|model| model.api_name == "virtual-model"));
    eprintln!("Pi custom provider: removal cancelled stream");
    app.executable_extensions.shutdown().await;
    eprintln!("Pi custom provider: shutdown complete");
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    let values = trace
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let selected = values
        .iter()
        .find(|value| value["kind"] == "selected")
        .unwrap();
    assert_eq!(selected["model"]["provider"], "pi-local-provider");
    assert_eq!(selected["model"]["baseUrl"], "http://127.0.0.1:9/local/");
    assert_eq!(selected["model"]["cost"]["input"], 1);
    assert!(!selected.to_string().contains("explicit-local-dummy"));
    let dispatch = values
        .iter()
        .find(|value| value["kind"] == "dispatch")
        .unwrap();
    assert_eq!(dispatch["id"], "virtual-model");
    assert_eq!(dispatch["explicitKey"], true);
    assert_eq!(dispatch["system"], "local native request");
    assert!(
        values
            .iter()
            .any(|value| value["kind"] == "cancelled" && value["aborted"] == true),
        "native removal must abort the existing custom callback, not just withdraw its catalog entry"
    );
    assert_eq!(
        dispatch["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "user")
            .unwrap()["content"][0]["text"],
        "local fixture input"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_custom_provider_thinking_map_keeps_exact_choices_and_portable_dispatch() {
    use octet_ai::ReasoningEffort;
    let map = serde_json::json!({"off":null,"minimal":null,"low":"provider-low","medium":null,"high":"provider-high","xhigh":"max","max":"provider-maximum"});
    let factory = CUSTOM_PROVIDER
        .replacen("reasoning:false", &format!("reasoning:true,thinkingLevelMap:{map}"), 1)
        .replace("explicitKey:options.apiKey", "thinkingLevelMap:model.thinkingLevelMap,reasoning:options.reasoning,explicitKey:options.apiKey")
        .replace("appendFileSync(TRACE,JSON.stringify({kind:'selected'", "pi.setThinkingLevel('max'); appendFileSync(TRACE,JSON.stringify({kind:'selected'");
    let (directory, mut app) = pi_app(&factory);
    let mut shell = InteractiveShell::test_shell();
    app.synchronize_extension_provider_catalog();
    request_extension_ui(&mut shell, &mut app);
    command(&mut app, &mut shell, "select").await.unwrap();
    let expected = ReasoningConfig::Effort(ReasoningEffort::Max);
    assert_eq!(app.reasoning, expected);
    let capability = app.model.spec.capabilities.reasoning.as_ref().unwrap();
    assert_eq!(
        capability.choices(),
        vec![
            ReasoningConfig::Effort(ReasoningEffort::Low),
            ReasoningConfig::Effort(ReasoningEffort::High),
            ReasoningConfig::Effort(ReasoningEffort::Xhigh),
            expected.clone(),
        ]
    );
    assert!(app
        .executable_extensions
        .prepare_selected_provider_transport(&app.model)
        .unwrap()
        .is_some());
    let mut request = pi_custom_provider_request();
    request.reasoning = expected;
    tokio::time::timeout(
        Duration::from_secs(10),
        app.client.complete(&app.model, request),
    )
    .await
    .unwrap()
    .unwrap();
    app.executable_extensions.shutdown().await;
    let values = std::fs::read_to_string(directory.path().join("trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let selected = values
        .iter()
        .find(|value| value["kind"] == "selected")
        .unwrap();
    assert_eq!(selected["model"]["thinkingLevelMap"], map);
    let dispatch = values
        .iter()
        .find(|value| value["kind"] == "dispatch")
        .unwrap();
    assert_eq!(dispatch["thinkingLevelMap"], map);
    assert_eq!(dispatch["reasoning"], "max");
}

const FACTS: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.registerCommand('probe', { handler: async (_, ctx) => {
    const model = ctx.model;
    const available = ctx.modelRegistry.getAvailable();
    const scoped = ctx.scopedModels;
    const denied = action => { try { action(); return false; } catch (error) { return error.code === -32601; } };
    appendFileSync(TRACE, JSON.stringify({ model, level: pi.getThinkingLevel(), contextLevel: ctx.thinkingLevel,
      available, scoped, oauth: (() => { try { return ctx.modelRegistry.isUsingOAuth(model); } catch (error) { return 'denied'; } })(),
      oauthUnknownDenied: denied(() => ctx.modelRegistry.isUsingOAuth({ ...model, id: 'not-described' })), keyDenied: denied(() => ctx.modelRegistry.getApiKey(model)),
      all: ctx.modelRegistry.getAll(), found: ctx.modelRegistry.find(model.provider, model.id),
      missing: ctx.modelRegistry.find(model.provider, 'not-described') ?? null }) + '\n');
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
    let expected_all = app.catalog.models().count();
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
        // The host states OAuth for the model it described (a static key is not
        // a login); a model it never described is refused, not guessed.
        assert_eq!(value["oauth"], false);
        assert_eq!(value["oauthUnknownDenied"], true);
        assert_eq!(value["keyDenied"], true);
        assert_eq!(value["all"].as_array().unwrap().len(), expected_all);
        assert_eq!(value["found"], value["model"]);
        assert!(value["missing"].is_null());
        for model in value["available"].as_array().unwrap() {
            assert!(value["all"].as_array().unwrap().contains(model));
        }
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

const STARTUP_FACTS: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('session_start', (_, ctx) => {
    let row;
    try {
      // pi-subagents snapshots ctx this way before doing any reverse/UI work.
      const snapshot = { ...ctx };
      row = { scoped: snapshot.scopedModels, available: snapshot.modelRegistry.getAvailable() };
    } catch (error) {
      row = { code: error.code, error: String(error.message) };
    }
    appendFileSync(TRACE, JSON.stringify(row) + '\n');
  });
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_interactive_model_facts_precede_startup_and_reload() {
    for level in [None, Some("off"), Some("on")] {
        let (directory, mut app) = pi_app(STARTUP_FACTS);
        let model_id = app.model.spec.api_name.clone();
        let expected_available = app
            .catalog
            .models()
            .filter(|spec| {
                app.catalog
                    .resolve(&spec.id)
                    .is_ok_and(|model| model.endpoint.auth.is_configured())
            })
            .count();
        let patterns = level.map(|level| format!("{}:{level}", app.model.spec.id.0));
        app.set_model_scope_patterns(patterns.as_deref()).unwrap();
        let scope = app.model_scope.clone();
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        assert!(app.executable_extensions.has_pending_session_starts());
        // Do not prewarm with request_extension_ui: the product resource phase
        // must publish model facts before spawning its deferred startup tasks.
        tokio::time::timeout(
            Duration::from_secs(20),
            resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("trace.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let (mut app, reloaded) = tokio::time::timeout(
            Duration::from_secs(20),
            reload_resources_announcing(app, &mut shell, &mut input, true),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(reloaded);
        let rows = std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2, "exactly one start per native binding");
        for row in rows {
            if level == Some("on") {
                // Native On is not a portable Pi thinking level. Publishing
                // facts early must not fabricate an empty scope for it.
                assert_eq!(row["code"], -32601, "{row}");
                assert!(row["error"]
                    .as_str()
                    .unwrap()
                    .contains("unsupported_feature ctx.scopedModels"));
                assert!(row.get("scoped").is_none(), "{row}");
                continue;
            }
            assert!(row.get("error").is_none(), "{row}");
            assert_eq!(
                row["available"].as_array().unwrap().len(),
                expected_available
            );
            if level.is_some() {
                assert_eq!(row["scoped"].as_array().unwrap().len(), 1, "{row}");
                assert_eq!(row["scoped"][0]["model"]["id"], model_id);
                assert_eq!(row["scoped"][0]["thinkingLevel"], "off");
            } else {
                assert_eq!(row["scoped"], serde_json::json!([]));
            }
        }
        assert_eq!(app.model_scope, scope);
        assert!(app.agent.session().usage_records().is_empty());
        app.executable_extensions.shutdown().await;
    }
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
    // Catalog iteration is unordered. Cover native Responses updates and an
    // ordinary token-budget route explicitly: portable high persists as an
    // effort or an explicit budget, depending on the selected native model.
    for (source_id, native_updates, expected_reasoning) in [
        (
            "gpt-6-astra",
            true,
            ReasoningConfig::Effort(ReasoningEffort::High),
        ),
        ("claude-sonnet-4-5", false, ReasoningConfig::Budget(6144)),
    ] {
        eprintln!("Pi idle model setters: {source_id}");
        let (directory, mut app) = pi_app(SETTERS);
        // A real configured, unauthenticated local route, not a fake credential
        // or fabricated adapter snapshot. This test performs no provider request.
        let source = app.catalog.resolve(&ModelId(source_id.into())).unwrap();
        assert_eq!(
            source.responses_features().reasoning_effort_updates,
            native_updates
        );
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
        assert_eq!(app.reasoning, expected_reasoning);
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
        assert_eq!(app.config.model, default_model);
        assert_eq!(app.config.reasoning, default_reasoning);

        let reopened = Session::open_read_only(&session).unwrap();
        assert_eq!(reopened.resource_owner_key(), owner);
        assert_eq!(
            reopened
                .responses_reasoning(&app.model.endpoint.id, &spec.id)
                .unwrap()
                .map(|(_, effective)| effective),
            native_updates.then(|| expected_reasoning.clone())
        );
        drop(reopened);

        // Resume with the real reader from an independent App still on its
        // default model/reasoning. A missing durable selection cannot pass by
        // inheriting the already-selected in-memory values from the producer.
        let (_resume_directory, mut resumed) = crate::compaction::tests::app_for_estimate();
        resumed.catalog = app.catalog.clone();
        assert_ne!(resumed.model.spec.id, spec.id);
        assert_ne!(resumed.reasoning, expected_reasoning);
        drop(app);
        let mut resumed = rebuild_app(
            resumed,
            None,
            None,
            None,
            Some(SessionSelection::OpenExisting(session.clone())),
        )
        .unwrap();
        resumed.executable_extensions.shutdown().await;
        assert_eq!(resumed.model.spec.id, spec.id);
        assert_eq!(resumed.agent.model().spec.id, spec.id);
        assert_eq!(resumed.reasoning, expected_reasoning);
        assert_eq!(resumed.agent.reasoning(), &expected_reasoning);
        assert_eq!(resumed.agent.session().path(), session);
        assert_eq!(resumed.agent.session().resource_owner_key(), owner);
        assert_eq!(
            level_from_reasoning(&resumed.reasoning, &resumed.model).unwrap(),
            ThinkingLevel::High
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_inherited_provider_overrides_and_restores_the_original_native_identity() {
    let factory = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.registerProvider('openai', { api: 'openai-completions', streamSimple(model, context, options) {
    appendFileSync(TRACE, JSON.stringify({ id:model.id, provider:model.provider, keyAbsent: options.apiKey === undefined, system:context.systemPrompt })+'\n');
    return (async function*() {
      const partial = {role:'assistant',api:model.api,model:model.id,provider:model.provider,content:[{type:'text',text:'inherited output'}]};
      yield {type:'start',partial}; yield {type:'text_start',contentIndex:0,partial};
      yield {type:'text_delta',contentIndex:0,delta:'inherited output',partial};
      yield {type:'text_end',contentIndex:0,content:'inherited output',partial};
      yield {type:'done',reason:'stop',message:{...partial,usage:{input:1,output:2,cacheRead:0,cacheWrite:0,totalTokens:3}}};
    })();
  }});
  pi.registerCommand('remove-inherited', {handler() {pi.unregisterProvider('openai');}});
};
"#;
    let (directory, mut app) = pi_app(factory);
    let original = app.model.clone();
    app.synchronize_extension_provider_catalog();
    let inherited = app.catalog.resolve(&original.spec.id).unwrap();
    assert_eq!(inherited.spec.id, original.spec.id);
    assert_ne!(inherited.endpoint.id, original.endpoint.id);
    assert_eq!(inherited.spec.capabilities, original.spec.capabilities);
    assert_eq!(inherited.spec.pricing, original.spec.pricing);
    assert_eq!(
        serde_json::to_value(&inherited.spec.preset).unwrap(),
        serde_json::to_value(&original.spec.preset).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&inherited.spec.cache).unwrap(),
        serde_json::to_value(&original.spec.cache).unwrap()
    );
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        app.client
            .complete(&inherited, pi_custom_provider_request()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(&response.message.content[..], [octet_ai::AssistantPart::Text(text)] if text == "inherited output")
    );
    assert_eq!(response.message.model, original.spec.id);
    assert_eq!(response.usage.total_tokens, 3);
    let trace: Value = serde_json::from_str(
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(trace["id"], original.spec.api_name);
    assert_eq!(trace["provider"], "openai");
    assert_eq!(trace["keyAbsent"], true);
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "remove-inherited")
        .await
        .unwrap();
    app.synchronize_extension_provider_catalog();
    let restored = app.catalog.resolve(&original.spec.id).unwrap();
    assert_eq!(restored.endpoint.id, original.endpoint.id);
    assert_eq!(
        serde_json::to_value(&*restored.spec).unwrap(),
        serde_json::to_value(&*original.spec).unwrap()
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_inherited_native_delegate_retains_request_hooks_auth_and_signed_accounting() {
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    struct BoundHook(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl octet_ai::ProviderRequestHook for BoundHook {
        async fn before_request(
            &self,
            context: &octet_ai::ProviderRequestContext,
            payload: Value,
        ) -> Result<Option<Value>, octet_ai::AiError> {
            assert_eq!(context.model.provider, "native-anthropic-fixture");
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(payload))
        }
    }
    let server = MockServer::start().await;
    let events = [
        serde_json::json!({"type":"message_start","message":{"id":"native-fixture","usage":{"input_tokens":2,"output_tokens":0}}}),
        serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"native reasoning"}}),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-native-signature"}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"native response"}}),
        serde_json::json!({"type":"content_block_stop","index":1}),
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
        serde_json::json!({"type":"message_stop"}),
    ].into_iter().map(|event| format!("event: {}\ndata: {}\n\n", event["type"].as_str().unwrap(), event)).collect::<String>();
    Mock::given(method("POST"))
        .and(header("x-api-key", "native-fixture-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(events),
        )
        .expect(1)
        .mount(&server)
        .await;
    let factory = r#"
import { anthropicMessagesApi } from '@earendil-works/pi-ai';
import { appendFileSync } from 'node:fs';
export default pi => {pi.registerCommand('register-native', {handler() {pi.registerProvider('anthropic', {api:'anthropic-messages',streamSimple(model,context,options) {
  appendFileSync(TRACE,JSON.stringify({keyAbsent:options.apiKey === undefined})+'\n');
  return anthropicMessagesApi().streamSimple(model,context,{...options,onPayload(payload) {
    payload.system[0].text='wrapped native system';
    appendFileSync(TRACE,JSON.stringify({payload:true})+'\n');
  }});
}});}});
pi.registerCommand('remove-native', {handler() {pi.unregisterProvider('anthropic');}});
};
"#;
    let (directory, mut app) = pi_app(factory);
    let mut spec = (*app.model.spec).clone();
    spec.id = ModelId("anthropic/native-fixture".into());
    spec.api_name = "native-fixture".into();
    spec.protocol = octet_ai::Protocol::AnthropicMessages;
    spec.capabilities.reasoning = None;
    spec.endpoint = octet_ai::EndpointId("native-anthropic-fixture".into());
    spec.preset = Default::default();
    let mut endpoint = (*app.model.endpoint).clone();
    endpoint.id = spec.endpoint.clone();
    endpoint.base_url = url::Url::parse(&(server.uri() + "/")).unwrap();
    endpoint.auth = octet_ai::Auth::header(
        http::HeaderName::from_static("x-api-key"),
        "native-fixture-secret",
    );
    app.catalog.register_endpoint(endpoint).unwrap();
    app.catalog.register_model(spec.clone()).unwrap();
    let native = app.catalog.resolve(&spec.id).unwrap();
    app.executable_extensions
        .provider_runtime()
        .bind_native_provider_catalog(&app.catalog, &app.client);
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "register-native")
        .await
        .unwrap();
    app.synchronize_extension_provider_catalog();
    let inherited = app.catalog.resolve(&spec.id).unwrap();
    assert_ne!(inherited.endpoint.id, native.endpoint.id);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let client = app
        .client
        .with_provider_request_hooks(vec![Arc::new(BoundHook(calls.clone()))]);
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        client.complete(&inherited, pi_custom_provider_request()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(response.usage.input_tokens, 2);
    assert_eq!(response.usage.output_tokens, 3);
    assert_eq!(response.usage.total_tokens, 5);
    assert!(response.cost.is_some());
    assert!(
        matches!(&response.message.content[0], octet_ai::AssistantPart::Reasoning(reasoning) if reasoning.text.as_deref() == Some("native reasoning") && matches!(reasoning.state.as_ref().map(|state| &state.kind), Some(octet_ai::ReasoningStateKind::AnthropicSignature { signature }) if signature == "opaque-native-signature"))
    );
    assert!(
        matches!(&response.message.content[1], octet_ai::AssistantPart::Text(text) if text == "native response")
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let payload: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(payload["model"], spec.api_name);
    assert_eq!(payload["system"][0]["text"], "wrapped native system");
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    assert!(!trace.contains("native-fixture-secret"));
    assert!(trace.contains("\"keyAbsent\":true"));
    assert!(trace.contains("\"payload\":true"));
    struct ParkHook {
        entered: Arc<tokio::sync::Notify>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }
    struct Dropped(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[async_trait::async_trait]
    impl octet_ai::ProviderRequestHook for ParkHook {
        async fn before_request(
            &self,
            _: &octet_ai::ProviderRequestContext,
            _: Value,
        ) -> Result<Option<Value>, octet_ai::AiError> {
            let _dropped = Dropped(self.dropped.clone());
            self.entered.notify_one();
            futures_util::future::pending::<()>().await;
            Ok(None)
        }
    }
    for operation in ["drop", "route"] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let client = app
            .client
            .with_provider_request_hooks(vec![Arc::new(ParkHook {
                entered: entered.clone(),
                dropped: dropped.clone(),
            })]);
        let active = tokio::time::timeout(
            Duration::from_secs(10),
            client.stream(&inherited, pi_custom_provider_request()),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("native request hook entered");
        if operation == "route" {
            command(&mut app, &mut shell, "remove-native")
                .await
                .unwrap();
        }
        drop(active);
        tokio::time::timeout(Duration::from_secs(10), async {
            while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("native continuation cancelled before HTTP dispatch");
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "cancelled native attempt must not dispatch HTTP"
        );
    }
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_custom_http_delegate_uses_process_fetch_and_current_host_hooks() {
    // Exercise the guarded custom-HTTP delegate, not the installed-Pi helper.
    // This fixture deliberately has no managed Pi installation. Public Pi
    // helpers use their own HTTP runtime and are covered by adapter tests.
    let delegate = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat/lib/native-provider.mjs");
    let delegate = url::Url::from_file_path(delegate.canonicalize().unwrap()).unwrap();
    let factory = r#"
import {openaiCompletionsDelegate as streamOpenAICompletions} from NATIVE_DELEGATE;
import {appendFileSync} from 'node:fs';
export default pi => {
  const model={id:'custom-model',provider:'custom-http-fixture',api:'openai-completions',baseUrl:'https://custom.invalid/v1',
    name:'Custom HTTP fixture',reasoning:false,input:['text'],cost:{input:1,output:2,cacheRead:0,cacheWrite:0},contextWindow:8192,maxTokens:1024};
  const provider={id:model.provider,name:'Custom HTTP fixture',baseUrl:model.baseUrl,
    auth:{apiKey:{resolve(){return {auth:{apiKey:'custom-fixture-secret'}};}}},getModels(){return [model];},
    streamSimple(selected,context,options){return streamOpenAICompletions(selected,context,{...options,fetch:async(url,init)=>{
      const headers=new Headers(init.headers), body=JSON.parse(init.body);
      if(url!=='https://custom.invalid/v1/chat/completions'||headers.get('authorization')!=='Bearer custom-fixture-secret'||headers.get('x-current-host')!=='yes'||body.model!==model.id||body.messages.at(-1).content!=='host-edited-input') throw Error('custom request binding failed');
      appendFileSync(TRACE,JSON.stringify({kind:'fetch',url,hostHeader:headers.get('x-current-host'),model:body.model})+'\n');
      const text='data: '+JSON.stringify({id:'custom-response',choices:[{delta:{content:'custom response'}}]})+'\n\n'
        +'data: '+JSON.stringify({choices:[{delta:{},finish_reason:'stop'}]})+'\n\n'
        +'data: '+JSON.stringify({choices:[],usage:{prompt_tokens:2,completion_tokens:3,total_tokens:5}})+'\n\n'
        +'data: [DONE]\n\n';
      const bytes=new TextEncoder().encode(text);
      return new Response(new ReadableStream({start(c){for(let i=0;i<bytes.length;i+=7)c.enqueue(bytes.slice(i,i+7));c.close();}}),{headers:{'content-type':'text/event-stream','x-custom-response':'yes'}});
    }});}
  };
  provider.stream=provider.streamSimple;
  pi.registerProvider(provider);
  pi.registerCommand('select-custom-http',{handler:async(_,ctx)=>{const selected=ctx.modelRegistry.find(model.provider,model.id);if(!selected||!await pi.setModel(selected))throw Error('custom model missing');}});
};
"#;
    let factory = factory.replace(
        "NATIVE_DELEGATE",
        &serde_json::to_string(delegate.as_str()).unwrap(),
    );
    struct CurrentHooks(Arc<std::sync::Mutex<Vec<&'static str>>>);
    #[async_trait::async_trait]
    impl octet_ai::ProviderRequestHook for CurrentHooks {
        async fn before_request(
            &self,
            _: &octet_ai::ProviderRequestContext,
            mut payload: Value,
        ) -> Result<Option<Value>, octet_ai::AiError> {
            assert_eq!(payload["model"], "custom-model");
            payload["messages"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["content"] = serde_json::json!("host-edited-input");
            self.0.lock().unwrap().push("payload");
            Ok(Some(payload))
        }
        async fn before_headers(
            &self,
            _: &octet_ai::ProviderRequestContext,
            headers: &mut http::HeaderMap,
        ) -> Result<(), octet_ai::AiError> {
            assert!(!headers.contains_key("authorization"));
            headers.insert("x-current-host", http::HeaderValue::from_static("yes"));
            self.0.lock().unwrap().push("headers");
            Ok(())
        }
        async fn after_response(
            &self,
            _: &octet_ai::ProviderRequestContext,
            status: http::StatusCode,
            headers: &http::HeaderMap,
        ) -> Result<(), octet_ai::AiError> {
            assert_eq!(status, http::StatusCode::OK);
            assert_eq!(headers["x-custom-response"], "yes");
            self.0.lock().unwrap().push("response");
            Ok(())
        }
    }
    let (directory, mut app) = pi_app(&factory);
    let mut shell = InteractiveShell::test_shell();
    app.synchronize_extension_provider_catalog();
    request_extension_ui(&mut shell, &mut app);
    command(&mut app, &mut shell, "select-custom-http")
        .await
        .unwrap();
    let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let client = app
        .client
        .with_provider_request_hooks(vec![Arc::new(CurrentHooks(observations.clone()))]);
    assert!(client
        .complete_with_host_options(
            &app.model,
            pi_custom_provider_request(),
            Default::default(),
            octet_ai::HostRequestOptions {
                api_key: Some("native-fixture-secret".into()),
                ..Default::default()
            },
        )
        .await
        .is_err());
    assert!(observations.lock().unwrap().is_empty());
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        client.complete(&app.model, pi_custom_provider_request()),
    )
    .await
    .unwrap()
    .unwrap_or_else(|error| {
        panic!(
            "{error}; completed hook phases: {:?}",
            observations.lock().unwrap()
        )
    });
    assert_eq!(
        *observations.lock().unwrap(),
        // Native codecs build the payload before final outgoing headers.
        ["payload", "headers", "response"]
    );
    assert_eq!(response.usage.input_tokens, 2);
    assert_eq!(response.usage.output_tokens, 3);
    assert_eq!(response.usage.total_tokens, 5);
    assert!(response.cost.is_some());
    assert!(
        matches!(&response.message.content[..], [octet_ai::AssistantPart::Text(text)] if text == "custom response")
    );
    app.executable_extensions.shutdown().await;
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.contains("\"kind\":\"fetch\""))
            .count(),
        1
    );
    assert!(!trace.contains("custom-fixture-secret"));
    assert!(!trace.contains("native-fixture-secret"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_responses_custom_provider_accepts_equivalent_canonical_replay_but_not_opaque_input() {
    let factory = CUSTOM_PROVIDER.replace("openai-completions", "openai-responses");
    let (directory, mut app) = pi_app(&factory);
    let mut shell = InteractiveShell::test_shell();
    app.synchronize_extension_provider_catalog();
    request_extension_ui(&mut shell, &mut app);
    command(&mut app, &mut shell, "select").await.unwrap();
    assert_eq!(app.model.spec.protocol, octet_ai::Protocol::OpenAiResponses);
    let mut request = pi_custom_provider_request();
    let octet_ai::Message::User(user) = &request.messages[0] else {
        panic!("fixture user")
    };
    request.responses = Some(octet_ai::ResponsesOptions::full_replay(
        octet_ai::responses::encode_responses_replay(
            &app.model,
            request.system.as_deref(),
            &[octet_ai::ResponsesReplayItem::User(user.clone())],
        )
        .unwrap(),
    ));
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        app.client.complete(&app.model, request.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(&response.message.content[..], [octet_ai::AssistantPart::Text(text)] if text == "first-local-output")
    );
    assert_eq!(response.usage.total_tokens, 3);
    request.responses = Some(octet_ai::ResponsesOptions::full_replay(octet_ai::ResponsesInput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({"type":"reasoning","encrypted_content":"private-native-state","id":"opaque-item"})).unwrap()])));
    assert!(tokio::time::timeout(
        Duration::from_secs(10),
        app.client.complete(&app.model, request)
    )
    .await
    .unwrap()
    .is_err());
    app.executable_extensions.shutdown().await;
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.contains("\"kind\":\"dispatch\""))
            .count(),
        1
    );
    assert!(!trace.contains("private-native-state"));
}
