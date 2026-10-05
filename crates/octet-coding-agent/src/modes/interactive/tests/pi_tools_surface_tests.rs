//! Real App/fleet/adapter acceptance for registration and authoritative tool selection.
//! No synthetic reverse RPC peer, provider call, or fabricated host snapshot.
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
