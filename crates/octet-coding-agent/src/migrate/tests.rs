//! Tests for the legacy `pi` migration scanner.
//!
//! Why this is a separate module: the scanner walks a foreign directory tree and
//! is easier to reason about from its expectations than from its traversal code,
//! so the inventory fixtures live beside it rather than inside it.

use super::*;
use std::fs;

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn options(root: &Path) -> ScanOptions {
    let root = fs::canonicalize(root).unwrap();
    ScanOptions {
        pi_home: root.join("home/.pi/agent"),
        project: root.join("workspace"),
        npm_roots: Vec::new(),
    }
}

#[test]
fn inventories_local_package_without_executing_it() {
    let temp = tempfile::tempdir().unwrap();
    let options = options(temp.path());
    let package = temp.path().join("package");
    let marker = temp.path().join("executed");
    write(
        &options.pi_home.join("settings.json"),
        &format!(
            r#"{{"packages":[{}]}}"#,
            serde_json::to_string(package.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &package.join("package.json"),
        r#"{
              "name": "fixture-pi-package",
              "version": "1.2.3",
              "pi": {
                "extensions": ["src/index.ts"],
                "skills": ["skills"],
                "prompts": ["prompts"],
                "themes": ["themes"]
              }
            }"#,
    );
    write(
        &package.join("src/index.ts"),
        &format!(
            r#"import {{ writeFileSync }} from "node:fs";
                   import helper from "./helper.js";
                   writeFileSync({}, "ran");
                   export default function (pi: ExtensionAPI) {{
                     pi.registerTool({{ name: "fixture", execute: helper }});
                     pi.registerCommand("fixture", {{ handler: helper }});
                     pi.on("input", (event) => ({{ input: event.text.trim() }}));
                   }}"#,
            serde_json::to_string(marker.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &package.join("src/helper.ts"),
        r#"import { spawn } from "node:child_process";
               export default () => spawn("true");"#,
    );
    write(&package.join("skills/review/SKILL.md"), "# Review\n");
    write(&package.join("prompts/review.md"), "Review this.\n");
    write(&package.join("themes/dark.json"), "{}\n");
    write(&package.join("package-lock.json"), "{}\n");

    let report = scan_pi(&options);

    assert!(!marker.exists(), "the scanner executed extension source");
    assert_eq!(report.model_usage, "disabled");
    assert!(!report.package_code_executed);
    assert_eq!(report.found.packages, 1);
    assert_eq!(report.found.extensions, 1);
    assert_eq!(report.found.skills, 1);
    assert_eq!(report.found.prompts, 1);
    assert_eq!(report.found.themes, 1);
    let package = &report.packages[0];
    assert_eq!(package.name.as_deref(), Some("fixture-pi-package"));
    assert_eq!(package.version.as_deref(), Some("1.2.3"));
    assert!(package.source_hash.is_some());
    assert!(package.lock_hash.is_some());
    assert_eq!(package.migration, MigrationPath::Manual);
    let extension = &package.extensions[0];
    assert_eq!(extension.migration, MigrationPath::NativePort);
    assert!(extension
        .surfaces
        .registrations
        .contains(&"registerTool".to_owned()));
    assert!(extension.surfaces.events.contains(&"input".to_owned()));
    assert!(extension.security.filesystem);
    assert!(extension.security.process);
    assert_eq!(extension.analyzed_files.len(), 2);

    let original_hash = package.source_hash.clone();
    write(
        &temp.path().join("package/src/helper.ts"),
        r#"export default () => "changed";"#,
    );
    let changed = scan_pi(&options);
    assert_ne!(changed.packages[0].source_hash, original_hash);
}

#[test]
fn manifest_root_extension_resolves_index_once() {
    let temp = tempfile::tempdir().unwrap();
    let options = options(temp.path());
    let package = temp.path().join("self-extension");
    let source = serde_json::to_string(package.to_str().unwrap()).unwrap();
    write(
        &options.pi_home.join("settings.json"),
        &format!(r#"{{"packages":[{source}]}}"#),
    );
    write(
        &package.join("package.json"),
        r#"{"name":"self-extension","version":"1.0.0","pi":{"extensions":["./"]}}"#,
    );
    write(
        &package.join("index.ts"),
        "export default (pi) => pi.registerTool({ name: 'self' });\n",
    );

    let report = scan_pi(&options);

    let package_root = fs::canonicalize(&package).unwrap();
    let package_report = &report.packages[0];
    assert_eq!(package_report.resources.len(), 1);
    assert_eq!(
        package_report.resources[0].path,
        package_root.join("index.ts")
    );
    assert!(package_report.resources[0].enabled);
    assert_eq!(package_report.extensions.len(), 1);
    assert!(!package_report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "manifest_cycle"));

    write(
        &options.pi_home.join("settings.json"),
        &format!(r#"{{"packages":[{{"source":{source},"autoload":false}}]}}"#),
    );
    let disabled = scan_pi(&options);
    assert_eq!(disabled.packages[0].resources.len(), 1);
    assert!(!disabled.packages[0].resources[0].enabled);
    assert!(disabled.packages[0].extensions.is_empty());
}

#[test]
fn analyzes_bundled_extension_files_above_the_legacy_two_mib_limit() {
    let temp = tempfile::tempdir().unwrap();
    let extension = temp.path().join("bundle.js");
    let mut source = vec![b' '; 3 * 1024 * 1024];
    source.extend_from_slice(
        b"\nexport default (api) => api.registerCommand('ok', { handler() {} });\n",
    );
    fs::write(&extension, source).unwrap();
    let mut diagnostics = Vec::new();
    let report = analyze_extension(&extension, temp.path(), &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Bridge);
    assert!(report.analyzed_source_bytes > 2 * 1024 * 1024);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn bounds_nested_extension_manifests() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("manifest-chain");
    let mut current = root.clone();
    for _ in 0..=MAX_EXTENSION_MANIFEST_DEPTH {
        write(
            &current.join("package.json"),
            r#"{"pi":{"extensions":["next"]}}"#,
        );
        current = current.join("next");
    }
    let root = fs::canonicalize(root).unwrap();
    let mut traversal = ResourceTraversal::default();
    let mut diagnostics = Vec::new();

    let paths = collect_resource_path(
        &root,
        ResourceKind::Extension,
        &root,
        &mut traversal,
        &mut diagnostics,
    );

    assert!(paths.is_empty());
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "manifest_depth_limit"));
    assert!(traversal.active_extension_manifests.is_empty());
}

#[test]
fn single_file_package_hash_excludes_unrelated_siblings() {
    let temp = tempfile::tempdir().unwrap();
    let options = options(temp.path());
    let directory = fs::canonicalize(temp.path()).unwrap().join("loose");
    let extension = directory.join("extension.ts");
    let unrelated = directory.join("unrelated.ts");
    write(
        &options.pi_home.join("settings.json"),
        &format!(
            r#"{{"packages":[{}]}}"#,
            serde_json::to_string(extension.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &extension,
        "export default (pi) => pi.registerCommand('x', {});\n",
    );
    write(&unrelated, "export const value = 1;\n");

    let first = scan_pi(&options).packages[0].source_hash.clone();
    write(&unrelated, "export const value = 2;\n");
    let second = scan_pi(&options).packages[0].source_hash.clone();

    assert_eq!(first, second);
}

#[test]
fn analyzes_top_level_extensions_with_the_same_ast_pipeline() {
    let temp = tempfile::tempdir().unwrap();
    let options = options(temp.path());
    write(
        &options.pi_home.join("extensions/normalize.ts"),
        r#"export default (pi) => pi.on("tool_result", (event) => ({ content: event.content }));"#,
    );

    let report = scan_pi(&options);

    assert_eq!(report.found.extensions, 1);
    assert_eq!(report.extensions.len(), 1);
    assert_eq!(report.extensions[0].migration, MigrationPath::NativePort);
    assert_eq!(report.resources[0].scope, Scope::User);
    assert_eq!(report.resources[0].migration, MigrationPath::NativePort);
}

#[test]
fn project_package_wins_over_same_user_identity() {
    let temp = tempfile::tempdir().unwrap();
    let options = options(temp.path());
    let user_package = options.pi_home.join("npm/node_modules/@scope/example");
    let project_package = options.project.join(".pi/npm/node_modules/@scope/example");
    write(
        &options.pi_home.join("settings.json"),
        r#"{"packages":["npm:@scope/example@1.0.0"]}"#,
    );
    write(
        &options.project.join(".pi/settings.json"),
        r#"{"packages":["npm:@scope/example@2.0.0"]}"#,
    );
    write(
        &user_package.join("package.json"),
        r#"{"name":"@scope/example","version":"1.0.0","pi":{"skills":["skills"]}}"#,
    );
    write(&user_package.join("skills/a/SKILL.md"), "# A\n");
    write(
        &project_package.join("package.json"),
        r#"{"name":"@scope/example","version":"2.0.0","pi":{"skills":["skills"]}}"#,
    );
    write(&project_package.join("skills/b/SKILL.md"), "# B\n");

    let report = scan_pi(&options);

    assert_eq!(report.packages.len(), 1);
    assert_eq!(report.packages[0].scope, Scope::Project);
    assert_eq!(report.packages[0].version.as_deref(), Some("2.0.0"));
    assert_eq!(report.packages[0].resources.len(), 1);
    assert!(report.packages[0].resources[0]
        .path
        .ends_with("skills/b/SKILL.md"));
}

#[test]
fn project_autoload_delta_filters_the_user_install() {
    let temp = tempfile::tempdir().unwrap();
    let options = options(temp.path());
    let user_package = options.pi_home.join("npm/node_modules/example");
    write(
        &options.pi_home.join("settings.json"),
        r#"{"packages":["npm:example@1.0.0"]}"#,
    );
    write(
        &options.project.join(".pi/settings.json"),
        r#"{"packages":[{"source":"npm:example","autoload":false,"skills":["skills/one/**"]}]}"#,
    );
    write(
        &user_package.join("package.json"),
        r#"{"name":"example","version":"1.0.0","pi":{"skills":["skills"]}}"#,
    );
    write(&user_package.join("skills/one/SKILL.md"), "# One\n");
    write(&user_package.join("skills/two/SKILL.md"), "# Two\n");

    let report = scan_pi(&options);

    assert_eq!(report.packages.len(), 1);
    let package = &report.packages[0];
    assert_eq!(package.scope, Scope::Project);
    assert_eq!(package.root.as_deref(), Some(user_package.as_path()));
    assert_eq!(package.resources.len(), 2);
    assert_eq!(
        package
            .resources
            .iter()
            .filter(|resource| resource.enabled)
            .count(),
        1
    );
}

#[test]
fn classifies_capability_shaped_and_pi_native_extensions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let bridge = root.join("bridge.ts");
    write(
        &bridge,
        r#"export default (pi) => {
                 pi.registerTool({ name: "search", execute: async () => ({ content: [{ type: "text", text: "ok" }] }) });
                 pi.on("session_start", (_event, ctx) => {
                   pi.events.emit("ready");
                   ctx.ui.notify("ready");
                 });
               };"#,
    );
    let mut diagnostics = Vec::new();
    let report = analyze_extension(&bridge, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Bridge);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let manual = root.join("manual.tsx");
    write(
        &manual,
        r#"import { Component } from "@earendil-works/pi-tui";
               export default (pi) => pi.registerProvider("custom", {});"#,
    );
    let report = analyze_extension(&manual, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Manual);
}

#[test]
fn classifies_pi_0844_surfaces_and_fails_closed_on_unknown_apis() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut diagnostics = Vec::new();

    let provider = root.join("provider.ts");
    write(
        &provider,
        r#"export default (pi) => {
                 pi.on("before_provider_headers", () => {});
                 pi.registerProvider("custom", {});
               };"#,
    );
    let report = analyze_extension(&provider, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Manual);

    let autocomplete = root.join("autocomplete.ts");
    write(
        &autocomplete,
        r#"export default (pi) => pi.on("session_start", (_event, ctx) => {
                 ctx.ui.addAutocompleteProvider((current) => current);
               });"#,
    );
    let report = analyze_extension(&autocomplete, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Manual);

    let input = root.join("input.ts");
    write(
        &input,
        r#"export default (pi) => pi.on("input", (event) => ({
                 action: "transform", text: event.text.trim()
               }));"#,
    );
    let report = analyze_extension(&input, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::NativePort);

    let renderer = root.join("renderer.ts");
    write(
        &renderer,
        r#"export default (api) => api.registerTool({
                 name: "rendered",
                 execute: async () => ({ content: [{ type: "text", text: "ok" }] }),
                 renderCall: () => null,
                 renderResult: () => null
               });"#,
    );
    let report = analyze_extension(&renderer, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::NativePort);
    assert!(report
        .reasons
        .iter()
        .any(|reason| reason.contains("component renderer")));

    let ordinary_pi = root.join("ordinary-pi.ts");
    write(
        &ordinary_pi,
        r#"import { normalize } from "./ordinary-helper.js";
               export default (api) => api.registerTool({
                 name: "ordinary",
                 execute: async () => ({ content: [{ type: "text", text: normalize(" pi ") }] })
               });"#,
    );
    write(
        &root.join("ordinary-helper.ts"),
        r#"export function normalize(pi: string): string { return pi.trim(); }"#,
    );
    let report = analyze_extension(&ordinary_pi, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Bridge);
    assert!(!report
        .surfaces
        .actions
        .iter()
        .any(|action| action == "unknown:trim"));

    let private_import = root.join("private-import.ts");
    write(
        &private_import,
        r#"import { ExtensionRunner } from "@earendil-works/pi-coding-agent/dist/core/extensions/runner.js";
               export default (_api) => void ExtensionRunner;"#,
    );
    let report = analyze_extension(&private_import, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Blocked);
    assert!(report
        .reasons
        .iter()
        .any(|reason| reason.contains("private/internal")));

    let event_bus = root.join("event-bus.ts");
    write(
        &event_bus,
        r#"export default (extensionApi) => {
                 extensionApi.events.on("acme:ready", () => {});
                 extensionApi.events.emit("acme:ready", { ok: true });
               };"#,
    );
    let report = analyze_extension(&event_bus, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Bridge);
    assert!(report.surfaces.events.is_empty());
    assert_eq!(report.surfaces.actions, ["eventBus"]);

    let unknown = root.join("unknown.ts");
    write(
        &unknown,
        r#"function extension(api) {
                 api.on("future_event", () => {});
                 api.futureCapability();
                 api.events.future();
               }
               export default extension;"#,
    );
    let report = analyze_extension(&unknown, root, &mut diagnostics);
    assert_eq!(report.migration, MigrationPath::Blocked);
    assert!(report
        .reasons
        .iter()
        .any(|reason| reason.contains("outside the pinned Pi 0.84.4 compatibility profile")));
}

#[test]
fn nested_pi_theme_methods_are_not_mistaken_for_unknown_ui_apis() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let extension = root.join("theme.ts");
    write(
        &extension,
        r#"export default (pi) => {
                 pi.registerCommand("status", {
                   handler: async (_args, ctx) => ctx.ui.notify(ctx.ui.theme.fg("accent", "ok"))
                 });
               };"#,
    );
    let mut diagnostics = Vec::new();
    let report = analyze_extension(&extension, root, &mut diagnostics);
    assert_eq!(
        report.migration,
        MigrationPath::Bridge,
        "{:?}",
        report.reasons
    );
}

#[test]
fn blocks_a_thin_wrapper_when_its_internal_source_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let entrypoint = root.join("index.ts");
    write(&entrypoint, r#"export { default } from "./missing.js";"#);
    let mut diagnostics = Vec::new();

    let report = analyze_extension(&entrypoint, root, &mut diagnostics);

    assert_eq!(report.migration, MigrationPath::Blocked);
    assert_eq!(
        report.surfaces.unresolved_imports,
        vec!["./missing.js".to_owned()]
    );
}

#[test]
fn exhausted_setup_budget_blocks_remaining_extensions() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let entrypoint = root.join("extension.ts");
    write(&entrypoint, "export default (pi) => pi.registerTool({});\n");
    let mut diagnostics = Vec::new();
    let mut budget = AnalysisBudget {
        files: MAX_SCAN_ANALYZED_FILES,
        ..AnalysisBudget::default()
    };

    let report = analyze_extension_with_budget(&entrypoint, &root, &mut budget, &mut diagnostics);

    assert_eq!(report.migration, MigrationPath::Blocked);
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scan_file_limit"));
}

#[test]
fn package_filter_uses_conventions_for_manifest_omissions() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    write(&root.join("skills/one/SKILL.md"), "# One\n");
    let manifest = PiManifest {
        extensions: Some(Vec::new()),
        ..PiManifest::default()
    };
    let filter = PackageFilter {
        source: root.display().to_string(),
        autoload: None,
        extensions: None,
        skills: None,
        prompts: None,
        themes: None,
    };
    let mut diagnostics = Vec::new();

    let unfiltered =
        collect_package_resources(&root, Some(&manifest), None, Scope::User, &mut diagnostics);
    let filtered = collect_package_resources(
        &root,
        Some(&manifest),
        Some(&filter),
        Scope::User,
        &mut diagnostics,
    );

    assert!(unfiltered.is_empty());
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].kind, ResourceKind::Skill);
}

#[test]
fn filters_package_resources_without_losing_disabled_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    write(&root.join("skills/one/SKILL.md"), "# One\n");
    write(&root.join("skills/two/SKILL.md"), "# Two\n");
    let filter = PackageFilter {
        source: root.display().to_string(),
        autoload: None,
        extensions: None,
        skills: Some(vec!["skills/one/**".to_owned()]),
        prompts: None,
        themes: None,
    };
    let mut diagnostics = Vec::new();
    let resources =
        collect_package_resources(&root, None, Some(&filter), Scope::User, &mut diagnostics);
    let skills = resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Skill)
        .collect::<Vec<_>>();
    assert_eq!(skills.len(), 2);
    assert_eq!(skills.iter().filter(|resource| resource.enabled).count(), 1);
}

#[test]
fn parses_scoped_and_unscoped_npm_specs() {
    assert_eq!(npm_package_name("pkg@1.2.3").as_deref(), Some("pkg"));
    assert_eq!(
        npm_package_name("@scope/pkg@^2").as_deref(),
        Some("@scope/pkg")
    );
    assert_eq!(
        npm_package_name("@scope/pkg").as_deref(),
        Some("@scope/pkg")
    );
    assert!(npm_package_name("../pkg").is_none());
}

#[test]
fn parses_supported_git_sources_without_ref_in_install_path() {
    assert_eq!(
        parse_git_source("git:github.com/user/repo@v1"),
        Some(("github.com".to_owned(), PathBuf::from("user/repo")))
    );
    assert_eq!(
        parse_git_source("https://github.com/user/repo.git@abc"),
        Some(("github.com".to_owned(), PathBuf::from("user/repo")))
    );
}

#[cfg(unix)]
#[test]
fn refuses_symlinked_package_roots() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let options = options(temp.path());
    let real = temp.path().join("real-package");
    let link = temp.path().join("linked-package");
    fs::create_dir_all(&real).unwrap();
    symlink(&real, &link).unwrap();
    write(
        &options.pi_home.join("settings.json"),
        &format!(
            r#"{{"packages":[{}]}}"#,
            serde_json::to_string(link.to_str().unwrap()).unwrap()
        ),
    );

    let report = scan_pi(&options);

    assert_eq!(report.packages[0].migration, MigrationPath::Blocked);
    assert!(report.packages[0]
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "package_unresolved"));
}
