//! Real App/fleet/adapter acceptance for registration and authoritative tool selection.
//! No synthetic reverse RPC peer, external provider call, or fabricated host snapshot.
//! Execution acceptance scripts inference on loopback while keeping the real App/Agent.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_app, pi_non_resource_app};
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tools_default_activation_late_registration_and_selection() {
    let (directory, mut app) = pi_app(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  const tool = (name, defaultActive = true) => ({name,label:name,description:'Tool '+name,
    parameters:{type:'object'},defaultActive,outputSchema:{type:'object'},
    annotations:{readOnlyHint:true},namespace:{name:'fixture',description:'Fixture tools'},
    async execute(){return {content:[{type:'text',text:name}],structuredContent:{name},details:{name}}}});
  pi.registerTool(tool('mcpScript'));
  pi.registerTool(tool('optional', false));
  pi.registerTool(tool('deniedTool'));
  pi.registerTool(tool('deniedtool'));
  let started = false;
  pi.on('session_start', () => { started = true; });
  pi.registerCommand('probe', {handler: () => {
    if (!started) throw new Error('command admitted before session_start');
    const initial = pi.getActiveTools();
    if (!initial.includes('mcpScript') || initial.includes('optional')) throw new Error('defaultActive not applied');
    if (initial.includes('deniedTool') || !initial.includes('deniedtool')) throw new Error('case-sensitive exclusion not applied');
    const all = pi.getAllTools();
    const optional = all.find(t => t.name === 'optional');
    if (!optional || !optional.annotations.readOnlyHint || optional.namespace.name !== 'fixture' || !optional.parameters) throw new Error('registered tool facts missing');
    pi.setActiveTools(['optional','unknown','optional']);
    const selected = pi.getActiveTools();
    if (JSON.stringify(selected) !== JSON.stringify(['optional'])) throw new Error('selection not authoritative: '+JSON.stringify(selected));
    pi.registerTool(tool('lateTool'));
    const late = pi.getActiveTools();
    if (!late.includes('lateTool') || !late.includes('optional') || late.includes('mcpScript')) throw new Error('late activation not applied');
    pi.registerTool({...tool('lateTool'),description:'Replacement'});
    if (pi.getAllTools().find(t => t.name === 'lateTool').description !== 'Replacement') throw new Error('replacement not applied');
    try { pi.registerTool(tool('deniedTool')); } catch {}
    if (pi.getAllTools().some(t => t.name === 'deniedTool')) throw new Error('late registration bypassed exclusion');
    appendFileSync(TRACE, JSON.stringify({initial,selected,late})+'\n');
  }});
};
"#,
    );
    app.config.tools.exclude("deniedTool").unwrap();
    // Apply the production host policy, rather than only the fixture registry.
    app = rebuild_app(app, None, None, None, None).unwrap();
    assert!(app.executable_extensions.has_resource_consumer_processes());
    assert!(app.resource_paths_pending());
    assert!(app.executable_extensions.has_pending_session_starts());
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "probe").await.unwrap();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    let (mut app, reloaded) = tokio::time::timeout(
        Duration::from_secs(20),
        reload_resources_announcing(app, &mut shell, &mut input, true),
    )
    .await
    .expect("Pi tool catalog reload must settle")
    .unwrap();
    assert!(reloaded);
    let result = command(&mut app, &mut shell, "probe").await;
    app.executable_extensions.shutdown().await;
    result.unwrap();
    let traces: Vec<serde_json::Value> =
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(traces.len(), 2, "both initial and reloaded owners must run");
    for trace in traces {
        assert_eq!(trace["selected"], serde_json::json!(["optional"]));
        assert!(trace["late"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "lateTool"));
    }
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert_eq!(
        reopened.entries().len(),
        app.agent.session().entries().len()
    );
}

/// Both callbacks run under the native lifecycle guard. registerTool must get
/// its real catalog ACK before either callback can settle, not time out or skip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_startup_and_resource_hooks_acknowledge_late_tool_registration() {
    let (directory, mut app) = pi_app(
        r#"
import { appendFileSync, writeFileSync, realpathSync } from 'node:fs';
import { dirname, join } from 'node:path';
export default pi => {
  const tool = name => ({name,label:name,description:name,parameters:{type:'object'},
    async execute(){return {content:[{type:'text',text:name}]}}});
  pi.on('session_start', () => {
    appendFileSync(TRACE, 'start-entered\n');
    pi.registerTool(tool('startup_late'));
    if (!pi.getActiveTools().includes('startup_late')) throw Error('startup ACK not published');
    appendFileSync(TRACE, 'start-acknowledged\n');
  });
  pi.on('resources_discover', () => {
    pi.registerTool(tool('discovery_late'));
    if (!pi.getAllTools().some(t => t.name === 'discovery_late')) throw Error('discovery ACK not published');
    const prompt = join(realpathSync(dirname(TRACE)), 'late-catalog-prompt.md');
    writeFileSync(prompt, '---\ndescription: Late catalog regression\n---\nActual resource after catalog ACK.\n');
    appendFileSync(TRACE, 'resources-acknowledged\n');
    return {promptPaths:[prompt]};
  });
  pi.registerCommand('probe', {handler: () => {
    for (const name of ['startup_late','discovery_late']) {
      if (!pi.getAllTools().some(t => t.name === name)) throw Error('missing admitted tool '+name);
    }
    appendFileSync(TRACE, 'command\n');
  }});
};
"#,
    );
    assert!(app.resource_paths_pending());
    assert!(app.executable_extensions.has_pending_session_starts());
    let mut shell = InteractiveShell::test_shell();
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        command(&mut app, &mut shell, "probe"),
    )
    .await;
    let resources_settled = !app.resource_paths_pending();
    let starts_settled = !app.executable_extensions.has_pending_session_starts();
    let prompt_loaded = app.prompts.contains("late-catalog-prompt");
    let prompt_diagnostics = app.prompts.diagnostics();
    app.executable_extensions.shutdown().await;
    result
        .expect("startup callback must receive its catalog ACK without waiting for its own lifecycle lock")
        .unwrap();
    assert!(resources_settled);
    assert!(starts_settled);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap(),
        "start-entered\nstart-acknowledged\nresources-acknowledged\ncommand\n",
        "actual callbacks, resource discovery and command admission must settle in order"
    );
    assert!(prompt_loaded, "prompt diagnostics: {prompt_diagnostics:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_hook_can_read_and_select_tools_without_borrowing_the_agent() {
    let (directory, mut app) = pi_app(
        r#"
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
  pi.registerCommand('probe', {handler:() => {
    appendFileSync(TRACE, JSON.stringify({command:true})+'\n');
  }});
};
"#,
    );
    assert!(app.executable_extensions.has_resource_consumer_processes());
    assert!(app.resource_paths_pending());
    assert!(app.executable_extensions.has_pending_session_starts());
    let mut shell = InteractiveShell::test_shell();
    let first = command(&mut app, &mut shell, "probe").await;
    let second = command(&mut app, &mut shell, "probe").await;
    assert!(!app.resource_paths_pending());
    assert!(!app.executable_extensions.has_pending_session_starts());
    app.executable_extensions.shutdown().await;
    first.unwrap();
    second.unwrap();
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    let observed = trace
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            serde_json::json!({"hook": true, "active": ["hook_tool"]}),
            serde_json::json!({"command": true}),
            serde_json::json!({"command": true}),
        ],
        "the real startup hook must finish exactly once before commands run"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_non_resource_headless_start_settles_once_without_loading_resources() {
    let (directory, mut app) = pi_non_resource_app(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => pi.on('session_start', () => {
  pi.getAllTools(); // Requires the real headless reverse-request pump.
  appendFileSync(TRACE, 'started\n');
});
"#,
    );
    assert!(!app.executable_extensions.has_resource_consumer_processes());
    assert!(!app.resource_paths_pending());
    assert!(app.executable_extensions.has_pending_session_starts());
    let skills = app.skills.clone();
    let prompts = app.prompts.clone();
    let trace = directory.path().join("trace.jsonl");
    assert!(!trace.exists());
    for _ in 0..2 {
        tokio::time::timeout(
            Duration::from_secs(10),
            app.refresh_resource_paths_headless(),
        )
        .await
        .expect("non-resource session_start must settle")
        .unwrap();
        assert!(!app.executable_extensions.has_pending_session_starts());
        assert!(!app.resource_paths_pending());
        assert!(Arc::ptr_eq(&skills, &app.skills));
        assert!(Arc::ptr_eq(&prompts, &app.prompts));
        assert_eq!(std::fs::read_to_string(&trace).unwrap(), "started\n");
    }
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_non_resource_start_worker_failure_blocks_admission() {
    use crate::extensions::resource_paths::consumer_tests::hold_session_start;

    for headless in [false, true] {
        let (directory, mut app) = pi_non_resource_app(
            r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('session_start', () => appendFileSync(TRACE, 'started\n'));
  pi.registerCommand('probe', {handler: () => appendFileSync(TRACE, 'command\n')});
};
"#,
        );
        assert!(!app.executable_extensions.has_resource_consumer_processes());
        assert!(!app.resource_paths_pending());
        let (release, wait) = tokio::sync::oneshot::channel();
        hold_session_start(&mut app, wait);
        let mut shell = InteractiveShell::test_shell();
        let result = {
            let phase = async {
                if headless {
                    app.refresh_resource_paths_headless().await
                } else {
                    command(&mut app, &mut shell, "probe").await.map(|_| ())
                }
            };
            tokio::pin!(phase);
            assert!(futures_util::poll!(phase.as_mut()).is_pending());
            // The barrier now owns the held worker. Dropping its required
            // receipt causes a real JoinError, not a recoverable callback error.
            drop(release);
            tokio::time::timeout(Duration::from_secs(5), phase).await
        };
        app.executable_extensions.shutdown().await;
        let error = result
            .expect("failed startup worker must settle the boundary")
            .unwrap_err();
        assert!(error.to_string().contains("session_start worker failed"));
        assert!(
            !directory.path().join("trace.jsonl").exists(),
            "failed startup must not admit a command (headless={headless})"
        );
    }
}

#[derive(Clone)]
struct CapturePiToolExecution(Arc<Mutex<Vec<AgentEvent>>>);
impl octet_agent::EventObserver for CapturePiToolExecution {
    fn on_event(&self, event: &AgentEvent) {
        let captured = match event {
            AgentEvent::ToolProgress { id, progress } => Some(AgentEvent::ToolProgress {
                id: id.clone(),
                progress: progress.clone(),
            }),
            AgentEvent::ToolFinished {
                id,
                result,
                duration,
            } => Some(AgentEvent::ToolFinished {
                id: id.clone(),
                result: result.clone(),
                duration: *duration,
            }),
            _ => None,
        };
        if let Some(event) = captured {
            self.0.lock().unwrap().push(event);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_tool_context_outcomes_and_partial_details_media() {
    use super::support::{scripted_model, text_turn};
    use serde_json::{json, Value};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = requests.clone();
    let first = [
        json!({"type":"message_start","message":{"id":"tools-unit","usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"native-outer","name":"outerTool"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ].into_iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect::<String>();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |request: &wiremock::Request| {
            let mut requests = capture.lock().unwrap();
            let body = if requests.is_empty() {
                first.clone()
            } else {
                text_turn()
            };
            requests.push(serde_json::from_slice(&request.body).unwrap());
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
        })
        .mount(&server)
        .await;
    let (directory, mut app) = pi_app(
        r#"
import {appendFileSync} from 'node:fs';
export default pi => {
  const image={type:'image',mimeType:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=='};
  pi.registerTool({name:'leafTool',label:'Leaf',description:'Leaf tool',parameters:{type:'object',properties:{count:{type:'integer',minimum:1},mode:{type:'string'}},required:['count'],additionalProperties:false},
    prepareArguments:raw=>({...raw,count:Number(raw.count)}),
    async execute(id,args,signal,update){
      update({content:[image],details:{where:'leaf',count:args.count},structuredContent:null});
      return {content:[{type:'text',text:'leaf-private'},image],details:{count:args.count},
        ...(args.mode==='omit'?{}:{structuredContent:null}),isError:args.mode==='error'};
    }});
  pi.registerTool({name:'outerTool',label:'Outer',description:'Outer tool',parameters:{type:'object'},
    async execute(id,args,signal,update,ctx){
      const tools=ctx.tools;
      if(!tools.some(t=>t.name==='leafTool')||!tools.some(t=>t.name==='read')||!tools.find(t=>t.name==='leafTool').parameters)throw Error('native frozen tools missing');
      update({content:[image],details:{where:'outer'},structuredContent:null});
      const updates=[];
      const success=await ctx.executeTool('leafTool',{count:'2'}, {onUpdate:value=>updates.push(value)});
      const omitted=await ctx.executeTool('leafTool',{count:'3',mode:'omit'});
      const error=await ctx.executeTool('leafTool',{count:'4',mode:'error'});
      const unknown=await ctx.executeTool('absent',{});
      if(updates.length!==1||updates[0].content[0].type!=='image'||updates[0].details.where!=='leaf'||updates[0].structuredContent!==null)throw Error('native partial callback lost');
      if(success.isError||success.result.details.count!==2||success.result.content[1].type!=='image'||success.result.structuredContent!==null)throw Error('native result lost');
      if(Object.hasOwn(omitted.result,'structuredContent')||!error.isError||error.result.details.count!==4||!unknown.isError)throw Error('native omission/error lost');
      appendFileSync(TRACE,JSON.stringify({success,omitted,error,unknown,updates})+'\n');
      return {content:[{type:'text',text:'outer final'}],details:{completed:true}};
    }});
};
"#,
    );
    let model = scripted_model(&server.uri());
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    app = rebuild_app(app, Some(model), None, None, None).unwrap();
    app.executable_extensions
        .activate_session_lifecycle_driver();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input)
        .await
        .unwrap();
    assert!(!app.resource_paths_pending());
    let observed = Arc::new(Mutex::new(Vec::new()));
    app.agent.observe(CapturePiToolExecution(observed.clone()));
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let composition = app
        .executable_extensions
        .compose_prompt(&app.system, "tool surface acceptance".into())
        .await
        .unwrap();
    app.agent.set_system_prompt(composition.system);
    let mut prompt: octet_agent::UserInput = composition.prompt.into();
    prompt.custom_messages.extend(composition.custom_messages);
    let inspection = ActiveRunInspection::capture(&app);
    let mut run = app.agent.prompt(prompt).await.unwrap();
    let turn = app.executable_extensions.begin_turn().await;
    app.executable_extensions
        .commit_prompt_context(composition.pending_context_count);
    let run_id = shell.begin_run("tool-surface");
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut ticker = tokio::time::interval(Duration::from_millis(16));
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut VecDeque::new(),
            &mut false,
            None,
            None,
            &mut app.executable_extensions,
            &mut false,
            &inspection,
            &mut None,
        ),
    )
    .await
    .expect("native Pi tool context run timed out")
    .unwrap();
    drop(run);
    app.executable_extensions.settle_turn(turn, &outcome).await;
    app.executable_extensions.shutdown().await;
    assert_eq!(
        outcome,
        HostRunOutcome::Completed,
        "{}",
        shell.debug_snapshot()
    );
    let trace: Value = serde_json::from_str(
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap_or_else(|error| {
                panic!(
                    "native tool callback trace missing: {error}; events: {:?}; shell: {}",
                    observed.lock().unwrap(),
                    shell.debug_snapshot()
                )
            })
            .trim(),
    )
    .unwrap();
    assert_eq!(trace["success"]["toolCall"]["id"], "native-outer/1");
    assert_eq!(trace["omitted"]["toolCall"]["id"], "native-outer/2");
    assert_eq!(trace["error"]["toolCall"]["id"], "native-outer/3");
    assert_eq!(trace["unknown"]["toolCall"]["id"], "native-outer/4");
    assert_eq!(trace["success"]["toolCall"]["arguments"]["count"], 2);
    assert!(trace["success"]["result"]["structuredContent"].is_null());
    assert!(trace["omitted"]["result"]
        .get("structuredContent")
        .is_none());
    let observed = observed.lock().unwrap();
    let partials = observed
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolProgress {
                progress: octet_agent::ToolProgress::PartialResult(output),
                ..
            } => Some(output),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        partials.len(),
        1,
        "private nested updates must not leak to the outer panel"
    );
    assert_eq!(
        partials[0].metadata().unwrap()["pi_details"]["where"],
        "outer"
    );
    assert_eq!(partials[0].structured_content(), Some(&Value::Null));
    assert_eq!(
        partials[0].media_kinds(),
        &[octet_agent::ToolOutputMediaKind::Image]
    );
    assert!(observed.iter().any(|event| matches!(event, AgentEvent::ToolFinished { result: Ok(output), .. } if output.text == "outer final")));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].to_string().contains("leaf-private"));
    assert!(std::fs::read_to_string(app.agent.session().path())
        .unwrap()
        .contains("\"outerTool\""));
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert_eq!(
        reopened.entries().len(),
        app.agent.session().entries().len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_command_catalog_availability_metadata_reload_and_withdrawal() {
    let (directory, mut app) = pi_app(
        r#"
import {appendFileSync,writeFileSync,readFileSync,mkdirSync,existsSync,realpathSync,unlinkSync} from 'node:fs';
import {dirname,join} from 'node:path';
export default pi => {
  const root=realpathSync(dirname(TRACE));
  const record=stage=>appendFileSync(TRACE,JSON.stringify({stage,commands:pi.getCommands()})+'\n');
  pi.on('session_start',()=>{
    record('startup');
    // This callback keeps the settled startup owner, not a later command's
    // parent. The Rust test opens the gate only after each command returns.
    setInterval(()=>{
      const gate=join(root,'retained-gate');
      if(!existsSync(gate))return;
      const phase=readFileSync(gate,'utf8');unlinkSync(gate);
      record('retained-'+phase);
    },10);
  });
  pi.on('resources_discover',()=>{
    const phase=existsSync(join(root,'phase'))?readFileSync(join(root,'phase'),'utf8'):'a';
    if(phase==='empty')return {skillPaths:[],promptPaths:[]};
    const skill=join(root,'resources',phase,'catalog-'+phase,'SKILL.md');
    mkdirSync(dirname(skill),{recursive:true});
    writeFileSync(skill,'---\nname: catalog-'+phase+'\ndescription: Available unactivated skill\n---\nBody');
    const prompt=join(root,'resources',phase,'catalog-prompt-'+phase+'.md');
    writeFileSync(prompt,'---\ndescription: Admitted template\n---\nPrompt body');
    const broken=join(root,'resources',phase,'broken.md');writeFileSync(broken,'not a valid skill');
    return {skillPaths:[skill,broken],promptPaths:[prompt]};
  });
  pi.registerCommand('reload',{handler(){}});
  pi.registerCommand('probe',{description:'Catalog probe',handler:()=>record('command')});
};
"#,
    );
    async fn retained_snapshot(
        app: &mut App,
        shell: &mut InteractiveShell,
        root: &std::path::Path,
        phase: &str,
    ) {
        std::fs::write(root.join("retained-gate"), phase).unwrap();
        let marker = format!("\"stage\":\"retained-{phase}\"");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                app.executable_extensions.drain_events_for_shell(shell);
                if std::fs::read_to_string(root.join("trace.jsonl"))
                    .unwrap()
                    .contains(&marker)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("settled same-owner callback must read the live native catalog");
    }
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "probe").await.unwrap();
    retained_snapshot(&mut app, &mut shell, directory.path(), "a").await;
    assert!(app
        .agent
        .session()
        .resolve_active_skills(&app.agent.session().head().unwrap())
        .unwrap()
        .active_skills
        .is_empty());
    std::fs::write(directory.path().join("phase"), "b").unwrap();
    app.mark_resource_paths_reload();
    command(&mut app, &mut shell, "probe").await.unwrap();
    retained_snapshot(&mut app, &mut shell, directory.path(), "b").await;
    std::fs::write(directory.path().join("phase"), "empty").unwrap();
    app.mark_resource_paths_reload();
    command(&mut app, &mut shell, "probe").await.unwrap();
    retained_snapshot(&mut app, &mut shell, directory.path(), "empty").await;
    let rows: Vec<serde_json::Value> =
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
    assert_eq!(rows.len(), 7);
    // macOS exposes /var through /private/var; imported provenance is canonical.
    let source_root = directory.path().canonicalize().unwrap();
    let commands = |i: usize| rows[i]["commands"].as_array().unwrap();
    assert!(commands(0).iter().all(|c| c["source"] == "extension"));
    for (i, phase) in [(1, "a"), (2, "a"), (3, "b"), (4, "b")] {
        let commands = commands(i);
        assert_eq!(
            commands
                .iter()
                .map(|c| c["source"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["extension", "extension", "prompt", "skill"]
        );
        assert_eq!(commands[0]["name"], "octet-pi-compat:reload");
        assert_eq!(commands[1]["name"], "probe");
        assert_eq!(
            commands[1]["sourceInfo"]["path"],
            source_root.join("probe.ts").to_str().unwrap()
        );
        assert_eq!(commands[2]["name"], format!("catalog-prompt-{phase}"));
        assert_eq!(commands[3]["name"], format!("skill:catalog-{phase}"));
        for c in &commands[2..] {
            assert_eq!(c["sourceInfo"]["source"], "extension:probe");
            assert_eq!(c["sourceInfo"]["scope"], "temporary");
            assert_eq!(c["sourceInfo"]["origin"], "top-level");
            assert_eq!(c["sourceInfo"]["baseDir"], source_root.to_str().unwrap());
        }
    }
    for i in [5, 6] {
        assert!(commands(i).iter().all(|c| c["source"] == "extension"));
    }
    app.config.enabled_extensions.clear();
    app = rebuild_app(app, None, None, None, None).unwrap();
    assert!(app.executable_extensions.command_suggestions().is_empty());
    assert!(app.prompts.descriptors().is_empty());
    assert!(app.skills.descriptors().is_empty());
    app.executable_extensions.shutdown().await;
}
