//! Real App/fleet/adapter acceptance for registration and authoritative tool selection.
//! No synthetic reverse RPC peer, external provider call, or fabricated host snapshot.
//! Execution acceptance scripts inference on loopback while keeping the real App/Agent.
#![cfg(unix)]
use super::*;
use super::pi_contract_support::{command, pi_app};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tools_default_activation_late_registration_and_selection() {
    let (directory, mut app) = pi_app(r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  const tool = (name, defaultActive = true) => ({name,label:name,description:'Tool '+name,
    parameters:{type:'object'},defaultActive,outputSchema:{type:'object'},
    annotations:{readOnlyHint:true},namespace:{name:'fixture',description:'Fixture tools'},
    async execute(){return {content:[{type:'text',text:name}],structuredContent:{name},details:{name}}}});
  pi.registerTool(tool('listed'));
  pi.registerTool(tool('optional', false));
  pi.registerCommand('probe', {handler: () => {
    const initial = pi.getActiveTools();
    if (!initial.includes('listed') || initial.includes('optional')) throw new Error('defaultActive not applied');
    const all = pi.getAllTools();
    const optional = all.find(t => t.name === 'optional');
    if (!optional || !optional.annotations.readOnlyHint || optional.namespace.name !== 'fixture' || !optional.parameters) throw new Error('registered tool facts missing');
    pi.setActiveTools(['optional','unknown','optional']);
    const selected = pi.getActiveTools();
    if (JSON.stringify(selected) !== JSON.stringify(['optional'])) throw new Error('selection not authoritative: '+JSON.stringify(selected));
    pi.registerTool(tool('late'));
    const late = pi.getActiveTools();
    if (!late.includes('late') || !late.includes('optional') || late.includes('listed')) throw new Error('late activation not applied');
    pi.registerTool({...tool('late'),description:'Replacement'});
    if (pi.getAllTools().find(t => t.name === 'late').description !== 'Replacement') throw new Error('replacement not applied');
    appendFileSync(TRACE, JSON.stringify({initial,selected,late})+'\n');
  }});
};
"#);
    let mut shell = InteractiveShell::test_shell();
    let result = command(&mut app, &mut shell, "probe").await;
    app.executable_extensions.shutdown().await;
    result.unwrap();
    let trace: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap().trim(),
    ).unwrap();
    assert_eq!(trace["selected"], serde_json::json!(["optional"]));
    assert!(trace["late"].as_array().unwrap().iter().any(|name| name == "late"));
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert_eq!(reopened.entries().len(), app.agent.session().entries().len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_hook_can_read_and_select_tools_without_borrowing_the_agent() {
    let (directory, mut app) = pi_app(r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.registerTool({name:'hook_tool',label:'Hook tool',description:'Hook tool',parameters:{type:'object'},
    async execute(){return {content:[{type:'text',text:'ok'}],details:undefined}}});
  pi.on('session_start', () => {
    const all = pi.getAllTools();
    if (!all.find(tool => tool.name === 'hook_tool')) throw new Error('hook tool catalog missing');
    pi.setActiveTools(['hook_tool']);
    const active = pi.getActiveTools();
    if (JSON.stringify(active) !== JSON.stringify(['hook_tool'])) throw new Error('hook selection missing');
    appendFileSync(TRACE, JSON.stringify({hook:true,active})+'\n');
  });
  pi.registerCommand('probe', {handler:() => {}});
};
"#);
    let mut shell = InteractiveShell::test_shell();
    let result = command(&mut app, &mut shell, "probe").await;
    app.executable_extensions.shutdown().await;
    result.unwrap();
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    let observed: serde_json::Value = serde_json::from_str(trace.lines().next().unwrap()).unwrap();
    assert_eq!(observed["active"], serde_json::json!(["hook_tool"]));
}

#[derive(Clone)]
struct CapturePiToolExecution(Arc<Mutex<Vec<AgentEvent>>>);
impl octet_agent::EventObserver for CapturePiToolExecution {
    fn on_event(&self, event: &AgentEvent) {
        let captured = match event {
            AgentEvent::ToolProgress { id, progress } => Some(AgentEvent::ToolProgress { id: id.clone(), progress: progress.clone() }),
            AgentEvent::ToolFinished { id, result, duration } => Some(AgentEvent::ToolFinished { id: id.clone(), result: result.clone(), duration: *duration }),
            _ => None,
        };
        if let Some(event) = captured { self.0.lock().unwrap().push(event); }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_context_outcomes_and_partial_details_media() {
    use super::support::{scripted_model, text_turn};
    use serde_json::{json, Value};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = requests.clone();
    let first = [
        json!({"type":"message_start","message":{"id":"tools-unit","usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"native-outer","name":"outer"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ].into_iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect::<String>();
    Mock::given(method("POST")).and(path("/v1/messages"))
        .respond_with(move |request: &wiremock::Request| {
            let mut requests = capture.lock().unwrap();
            let body = if requests.is_empty() { first.clone() } else { text_turn() };
            requests.push(serde_json::from_slice(&request.body).unwrap());
            ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(body)
        }).mount(&server).await;
    let (directory, mut app) = pi_app(r#"
import {appendFileSync} from 'node:fs';
export default pi => {
  const image={type:'image',mimeType:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=='};
  pi.registerTool({name:'leaf',label:'Leaf',description:'Leaf tool',parameters:{type:'object',properties:{count:{type:'integer',minimum:1},mode:{type:'string'}},required:['count'],additionalProperties:false},
    prepareArguments:raw=>({...raw,count:Number(raw.count)}),
    async execute(id,args,signal,update){
      update({content:[image],details:{where:'leaf',count:args.count},structuredContent:null});
      return {content:[{type:'text',text:'leaf-private'},image],details:{count:args.count},
        ...(args.mode==='omit'?{}:{structuredContent:null}),isError:args.mode==='error'};
    }});
  pi.registerTool({name:'outer',label:'Outer',description:'Outer tool',parameters:{type:'object'},
    async execute(id,args,signal,update,ctx){
      const tools=ctx.tools;
      if(!tools.some(t=>t.name==='leaf')||!tools.some(t=>t.name==='read')||!tools.find(t=>t.name==='leaf').parameters)throw Error('native frozen tools missing');
      update({content:[image],details:{where:'outer'},structuredContent:null});
      const updates=[];
      const success=await ctx.executeTool('leaf',{count:'2'}, {onUpdate:value=>updates.push(value)});
      const omitted=await ctx.executeTool('leaf',{count:'3',mode:'omit'});
      const error=await ctx.executeTool('leaf',{count:'4',mode:'error'});
      const unknown=await ctx.executeTool('absent',{});
      if(updates.length!==1||updates[0].content[0].type!=='image'||updates[0].details.where!=='leaf'||updates[0].structuredContent!==null)throw Error('native partial callback lost');
      if(success.isError||success.result.details.count!==2||success.result.content[1].type!=='image'||success.result.structuredContent!==null)throw Error('native result lost');
      if(Object.hasOwn(omitted.result,'structuredContent')||!error.isError||error.result.details.count!==4||!unknown.isError)throw Error('native omission/error lost');
      appendFileSync(TRACE,JSON.stringify({success,omitted,error,unknown,updates})+'\n');
      return {content:[{type:'text',text:'outer final'}],details:{completed:true}};
    }});
};
"#);
    let model = scripted_model(&server.uri());
    app.catalog.register_endpoint((*model.endpoint).clone()).unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    app = rebuild_app(app, Some(model), None, None, None).unwrap();
    app.executable_extensions.activate_session_lifecycle_driver();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input).await.unwrap();
    assert!(!app.resource_paths_pending());
    let observed = Arc::new(Mutex::new(Vec::new()));
    app.agent.observe(CapturePiToolExecution(observed.clone()));
    app.executable_extensions.refresh_host_state(app.agent.session(), &app.model, &app.reasoning, &app.sessions);
    let composition = app.executable_extensions.compose_prompt(&app.system, "tool surface acceptance".into()).await.unwrap();
    app.agent.set_system_prompt(composition.system);
    let mut prompt: octet_agent::UserInput = composition.prompt.into();
    prompt.custom_messages.extend(composition.custom_messages);
    let inspection = ActiveRunInspection::capture(&app);
    let mut run = app.agent.prompt(prompt).await.unwrap();
    let turn = app.executable_extensions.begin_turn().await;
    app.executable_extensions.commit_prompt_context(composition.pending_context_count);
    let run_id = shell.begin_run("tool-surface");
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut ticker = tokio::time::interval(Duration::from_millis(16));
    let outcome = tokio::time::timeout(Duration::from_secs(20), drive_active_run(
        &mut run, &control, &mut shell, &mut input, &mut ticker,
        &mut VecDeque::new(), &mut false, None, None, &mut app.executable_extensions,
        &mut false, &inspection, &mut None,
    )).await.expect("native Pi tool context run timed out").unwrap();
    drop(run);
    app.executable_extensions.settle_turn(turn, &outcome).await;
    app.executable_extensions.shutdown().await;
    assert_eq!(outcome, HostRunOutcome::Completed, "{}", shell.debug_snapshot());
    let trace: Value = serde_json::from_str(std::fs::read_to_string(directory.path().join("trace.jsonl"))
        .unwrap_or_else(|error| panic!("native tool callback trace missing: {error}; events: {:?}; shell: {}",
            observed.lock().unwrap(), shell.debug_snapshot())).trim()).unwrap();
    assert_eq!(trace["success"]["toolCall"]["id"], "native-outer/1");
    assert_eq!(trace["omitted"]["toolCall"]["id"], "native-outer/2");
    assert_eq!(trace["error"]["toolCall"]["id"], "native-outer/3");
    assert_eq!(trace["unknown"]["toolCall"]["id"], "native-outer/4");
    assert_eq!(trace["success"]["toolCall"]["arguments"]["count"], 2);
    assert!(trace["success"]["result"]["structuredContent"].is_null());
    assert!(trace["omitted"]["result"].get("structuredContent").is_none());
    let observed = observed.lock().unwrap();
    let partials = observed.iter().filter_map(|event| match event {
        AgentEvent::ToolProgress { progress: octet_agent::ToolProgress::PartialResult(output), .. } => Some(output),
        _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(partials.len(), 1, "private nested updates must not leak to the outer panel");
    assert_eq!(partials[0].metadata().unwrap()["pi_details"]["where"], "outer");
    assert_eq!(partials[0].structured_content(), Some(&Value::Null));
    assert_eq!(partials[0].media_kinds(), &[octet_agent::ToolOutputMediaKind::Image]);
    assert!(observed.iter().any(|event| matches!(event, AgentEvent::ToolFinished { result: Ok(output), .. } if output.text == "outer final")));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].to_string().contains("leaf-private"));
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert_eq!(reopened.entries().len(), app.agent.session().entries().len());
}
