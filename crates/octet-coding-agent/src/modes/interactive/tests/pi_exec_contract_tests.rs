//! Rust App -> real Pi adapter -> policy-bound native exec acceptance.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_app, pi_app_policy};
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_exec_native_preserves_argv_output_and_cancels_with_partial_output() {
    let (directory, mut app) = pi_app(
        r#"
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.registerCommand('probe', { handler: async (_args, ctx) => {
    trace(await pi.exec('/bin/sh', ['-c', 'printf "%s" "$1"; printf err >&2; exit 7', '--', 'a b;$(false)'], { cwd: ctx.cwd }));
    trace(await pi.exec('/bin/sh', ['-c', 'printf partial; sleep 30'], { timeout: 80 }));
    const controller = new AbortController();
    const pending = pi.exec('/bin/sh', ['-c', 'printf signal; sleep 30'], { signal: controller.signal });
    setTimeout(() => controller.abort(), 80);
    trace(await pending);
    trace(await pi.exec('/bin/sh', ['-c', "printf '%070000d' 0"]));
  }});
};
"#,
    );
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "probe").await.unwrap();
    let records: Vec<serde_json::Value> =
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(
        records[0],
        serde_json::json!({"stdout":"a b;$(false)", "stderr":"err", "code":7, "killed":false})
    );
    assert_eq!(records[1]["stdout"], "partial");
    assert_eq!(records[1]["killed"], true);
    assert_eq!(records[2]["stdout"], "signal");
    assert_eq!(records[2]["killed"], true);
    assert_eq!(records[3]["stdout"].as_str().unwrap().len(), 70000);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_exec_native_policy_denial_is_not_a_success_and_never_launches() {
    let (directory, mut app) = pi_app_policy(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.registerCommand('denied', { handler: async () => {
    try { await pi.exec('/bin/sh', ['-c', 'touch forbidden']); appendFileSync(TRACE, 'unexpected success'); }
    catch (error) { appendFileSync(TRACE, JSON.stringify({ denied: true, message: error.message })); }
  }});
};
"#,
        octet_agent::EffectPolicy::Controlled,
    );
    let mut picker = ExecPicker {
        shell: InteractiveShell::test_shell(),
        approved: false,
        prompts: 0,
    };
    app.executable_extensions
        .execute_command_with_confirmation("denied", Vec::new(), &mut picker)
        .await
        .unwrap();
    assert_eq!(picker.prompts, 1);
    let result: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap(),
    )
    .unwrap();
    assert_eq!(result["denied"], true);
    assert!(!app.config.workspace.join("forbidden").exists());
    app.executable_extensions.shutdown().await;
}

struct ExecPicker {
    shell: InteractiveShell,
    approved: bool,
    prompts: usize,
}
impl crate::extensions::ExtensionConfirmationHandler for ExecPicker {
    fn command_shell(&mut self) -> Option<&mut InteractiveShell> {
        Some(&mut self.shell)
    }
    fn confirm<'a>(
        &'a mut self,
        _extension: &'a str,
        request: &'a octet_agent::extension_process::ConfirmationRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + 'a>> {
        assert!(request
            .detail
            .as_deref()
            .unwrap()
            .contains("complete intent sha256"));
        self.prompts += 1;
        Box::pin(std::future::ready(Ok(self.approved)))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_exec_controlled_safe_runs_without_picker_and_risky_uses_exact_approval() {
    let (directory, mut app) = pi_app_policy(
        r#"
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.registerCommand('safe', { handler: async () => trace(await pi.exec('/bin/echo', ['safe a b', '$(touch forbidden)'])) });
  pi.registerCommand('risky', { handler: async () => trace(await pi.exec('/bin/sh', ['-c', 'touch approved; printf approved'])) });
};
"#,
        octet_agent::EffectPolicy::Controlled,
    );
    let mut picker = ExecPicker {
        shell: InteractiveShell::test_shell(),
        approved: true,
        prompts: 0,
    };
    app.executable_extensions
        .execute_command_with_confirmation("safe", Vec::new(), &mut picker)
        .await
        .unwrap();
    assert_eq!(picker.prompts, 0);
    assert!(!app.config.workspace.join("forbidden").exists());
    app.executable_extensions
        .execute_command_with_confirmation("risky", Vec::new(), &mut picker)
        .await
        .unwrap();
    assert_eq!(picker.prompts, 1);
    assert!(app.config.workspace.join("approved").exists());
    let records: Vec<serde_json::Value> =
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(records[0]["stdout"], "safe a b $(touch forbidden)\n");
    assert_eq!(records[1]["stdout"], "approved");
    app.executable_extensions.shutdown().await;
}
