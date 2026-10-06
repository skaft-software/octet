//! Real App + adapter acceptance for model facts and in-place idle setters.
//! Active request-boundary selection and broader provider forms remain gated.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_app};
use super::*;

const CUSTOM_PROVIDER: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  const config = text => ({baseUrl:'http://127.0.0.1:9/local/', api:'openai-completions', apiKey:'explicit-local-dummy',
    models:[{id:'virtual-model',name:'Local Virtual',reasoning:false,input:['text','image'],cost:{input:1,output:2,cacheRead:0.1,cacheWrite:0.2},contextWindow:8192,maxTokens:1024}],
    streamSimple: (model, context, options) => (async function* () {
      // The logical native selection reaches exactly this process-owned
      // streamer; its physical dispatch choice stays local to the extension.
      appendFileSync(TRACE,JSON.stringify({kind:'dispatch',id:model.id,provider:model.provider,baseUrl:model.baseUrl,
        explicitKey:options.apiKey==='explicit-local-dummy',system:context.systemPrompt,messages:context.messages})+'\n');
      const partial={role:'assistant',model:'local-physical',api:'openai-completions',provider:'physical-local',content:[]};
      yield {type:'start',partial};
      if(context.systemPrompt==='cancel-native') {
        await new Promise(resolve=>options.signal.addEventListener('abort',resolve,{once:true}));
        appendFileSync(TRACE,JSON.stringify({kind:'cancelled',aborted:options.signal.aborted})+'\n');
        return;
      }
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
    assert!(values.iter().any(|value| value["kind"] == "cancelled" && value["aborted"] == true),
        "native removal must abort the existing custom callback, not just withdraw its catalog entry");
    assert_eq!(
        dispatch["messages"][0]["content"][0]["text"],
        "local fixture input"
    );
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
      available, scoped, usingOAuth: ctx.modelRegistry.isUsingOAuth(model), keyDenied: denied(() => ctx.modelRegistry.getApiKey(model)),
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
        assert_eq!(value["usingOAuth"], false);
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
