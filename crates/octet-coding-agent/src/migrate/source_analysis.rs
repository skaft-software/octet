//! Tree-sitter analysis of a migrated extension's source, and the compatibility
//! classification it feeds.
//!
//! Why this is a separate module: reading an extension's source is its own
//! pipeline - a bounded AST walk over the entrypoint's import graph, the pi 0.84.4
//! event and UI vocabulary the findings are matched against, and the classifier
//! that turns those findings into a [`MigrationPath`]. None of it is report
//! assembly, and running it against its own [`AnalysisBudget`] is what stops a
//! hostile tree from turning the scanner into an unbounded reader.
//!
//! The classification vocabulary (`PI_0_84_4_EVENTS`, `PI_0_84_4_UI_METHODS`,
//! `PI_0_84_4_RENDERER_FIELDS`, `PI_0_84_4_REGISTRATIONS`) is data about one
//! specific pi release, so it sits next to the only code that reads it.

use std::collections::VecDeque;

use tree_sitter::{Node, Parser};

use super::*;

#[derive(Default)]
struct AnalysisAccumulator {
    events: BTreeSet<String>,
    registrations: BTreeSet<String>,
    actions: BTreeSet<String>,
    ui: BTreeSet<String>,
    mutations: BTreeSet<String>,
    imports: BTreeSet<String>,
    unresolved_imports: BTreeSet<String>,
    security: SecuritySignals,
    parse_errors: usize,
    ast_nodes: usize,
}

#[cfg(test)]
pub(super) fn analyze_extension(
    entrypoint: &Path,
    package_root: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> ExtensionReport {
    analyze_extension_with_budget(
        entrypoint,
        package_root,
        &mut AnalysisBudget::default(),
        diagnostics,
    )
}

pub(super) fn analyze_extension_with_budget(
    entrypoint: &Path,
    package_root: &Path,
    analysis_budget: &mut AnalysisBudget,
    diagnostics: &mut Vec<Diagnostic>,
) -> ExtensionReport {
    let mut accumulator = AnalysisAccumulator::default();
    let mut queue = VecDeque::from([entrypoint.to_path_buf()]);
    let mut seen = BTreeSet::new();
    let mut source_bytes = 0usize;
    while let Some(path) = queue.pop_front() {
        if seen.contains(&path) {
            continue;
        }
        if seen.len() >= MAX_ANALYZED_FILES {
            diagnostics.push(Diagnostic::warning(
                "ast_file_limit",
                format!("extension analysis stopped at {MAX_ANALYZED_FILES} files"),
                Some(entrypoint.to_path_buf()),
            ));
            accumulator.parse_errors += 1;
            break;
        }
        if analysis_budget.files >= MAX_SCAN_ANALYZED_FILES {
            diagnostics.push(Diagnostic::warning(
                "scan_file_limit",
                format!("setup analysis stopped at {MAX_SCAN_ANALYZED_FILES} source files"),
                Some(entrypoint.to_path_buf()),
            ));
            accumulator.parse_errors += 1;
            break;
        }
        seen.insert(path.clone());
        analysis_budget.files += 1;
        let extension_remaining = MAX_EXTENSION_SOURCE_BYTES.saturating_sub(source_bytes);
        let scan_remaining = MAX_SCAN_SOURCE_BYTES.saturating_sub(analysis_budget.source_bytes);
        let remaining = extension_remaining.min(scan_remaining);
        if remaining == 0 {
            diagnostics.push(Diagnostic::warning(
                "ast_byte_limit",
                "extension or setup analysis exhausted its bounded source-byte budget",
                Some(entrypoint.to_path_buf()),
            ));
            accumulator.parse_errors += 1;
            break;
        }
        let bytes = match octet_agent::secure_fs::read_regular_file_bounded(
            &path,
            MAX_SOURCE_FILE_BYTES.min(remaining),
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.push(Diagnostic::warning(
                    "source_read",
                    format!("could not read extension source: {error}"),
                    Some(path.clone()),
                ));
                accumulator.parse_errors += 1;
                continue;
            }
        };
        source_bytes = source_bytes.saturating_add(bytes.len());
        analysis_budget.source_bytes = analysis_budget.source_bytes.saturating_add(bytes.len());
        let source = match std::str::from_utf8(&bytes) {
            Ok(source) => source,
            Err(error) => {
                diagnostics.push(Diagnostic::warning(
                    "source_utf8",
                    format!("extension source is not UTF-8: {error}"),
                    Some(path.clone()),
                ));
                accumulator.parse_errors += 1;
                continue;
            }
        };
        let imports = analyze_source_ast(
            source,
            &path,
            &mut accumulator,
            analysis_budget,
            diagnostics,
        );
        for import in imports
            .into_iter()
            .filter(|import| import.starts_with('.') || import.starts_with('#'))
        {
            match resolve_local_import(&path, package_root, &import) {
                Some(import_path) if !seen.contains(&import_path) => queue.push_back(import_path),
                Some(_) => {}
                None if is_code_import(&import) => {
                    accumulator.unresolved_imports.insert(import);
                }
                None => {}
            }
        }
    }

    let (migration, reasons) = classify_extension(&accumulator);
    ExtensionReport {
        path: entrypoint.to_path_buf(),
        migration,
        reasons,
        analyzed_files: seen.into_iter().collect(),
        analyzed_source_bytes: source_bytes,
        syntax_nodes: accumulator.ast_nodes,
        surfaces: ExtensionSurfaces {
            events: accumulator.events.into_iter().collect(),
            registrations: accumulator.registrations.into_iter().collect(),
            actions: accumulator.actions.into_iter().collect(),
            ui: accumulator.ui.into_iter().collect(),
            mutations: accumulator.mutations.into_iter().collect(),
            imports: accumulator.imports.into_iter().collect(),
            unresolved_imports: accumulator.unresolved_imports.into_iter().collect(),
        },
        security: accumulator.security,
        parse_errors: accumulator.parse_errors,
    }
}

fn first_factory_api_binding(function: Node<'_>, source: &[u8]) -> Option<String> {
    if !matches!(
        function.kind(),
        "arrow_function" | "function_expression" | "function_declaration"
    ) {
        return None;
    }
    let parameters = function.child_by_field_name("parameters")?;
    let mut cursor = parameters.walk();
    let parameter = parameters.named_children(&mut cursor).next()?;
    let pattern = parameter
        .child_by_field_name("pattern")
        .unwrap_or(parameter);
    (pattern.kind() == "identifier")
        .then(|| node_text(pattern, source).map(str::to_owned))
        .flatten()
}

fn extension_api_bindings(root: Node<'_>, source: &[u8]) -> BTreeSet<String> {
    let mut bindings = BTreeSet::new();
    let mut exported_names = BTreeSet::new();
    let mut cursor = root.walk();
    let top_level = root.named_children(&mut cursor).collect::<Vec<_>>();

    for statement in &top_level {
        if statement.kind() == "export_statement" {
            if let Some(value) = statement.child_by_field_name("value") {
                if let Some(binding) = first_factory_api_binding(value, source) {
                    bindings.insert(binding);
                } else if value.kind() == "identifier" {
                    if let Some(name) = node_text(value, source) {
                        exported_names.insert(name.to_owned());
                    }
                }
            }
        }
        let expression = if statement.kind() == "expression_statement" {
            statement.named_child(0)
        } else {
            Some(*statement)
        };
        let Some(assignment) = expression.filter(|node| node.kind() == "assignment_expression")
        else {
            continue;
        };
        let Some(left) = assignment.child_by_field_name("left") else {
            continue;
        };
        let Some(chain) = member_chain(left, source) else {
            continue;
        };
        if matches!(chain.as_str(), "module.exports" | "exports.default") {
            if let Some(value) = assignment.child_by_field_name("right") {
                if let Some(binding) = first_factory_api_binding(value, source) {
                    bindings.insert(binding);
                }
            }
        }
    }

    if !exported_names.is_empty() {
        for statement in top_level {
            if statement.kind() == "function_declaration" {
                let exported = statement
                    .child_by_field_name("name")
                    .and_then(|name| node_text(name, source))
                    .is_some_and(|name| exported_names.contains(name));
                if exported {
                    if let Some(binding) = first_factory_api_binding(statement, source) {
                        bindings.insert(binding);
                    }
                }
                continue;
            }
            if !matches!(
                statement.kind(),
                "lexical_declaration" | "variable_declaration"
            ) {
                continue;
            }
            let mut cursor = statement.walk();
            for declarator in statement.named_children(&mut cursor) {
                if declarator.kind() != "variable_declarator" {
                    continue;
                }
                let Some(name) = declarator
                    .child_by_field_name("name")
                    .and_then(|name| node_text(name, source))
                else {
                    continue;
                };
                if !exported_names.contains(name) {
                    continue;
                }
                if let Some(value) = declarator.child_by_field_name("value") {
                    if let Some(binding) = first_factory_api_binding(value, source) {
                        bindings.insert(binding);
                    }
                }
            }
        }
    }
    bindings
}

fn is_extension_api_direct_method(
    chain: &str,
    method: &str,
    api_bindings: &BTreeSet<String>,
) -> bool {
    let Some((binding, suffix)) = chain.split_once('.') else {
        return false;
    };
    suffix == method && api_bindings.contains(binding)
}

fn extension_api_event_bus_method<'a>(
    chain: &'a str,
    api_bindings: &BTreeSet<String>,
) -> Option<&'a str> {
    let mut parts = chain.split('.');
    let binding = parts.next()?;
    if parts.next()? != "events" || !api_bindings.contains(binding) {
        return None;
    }
    let method = parts.next()?;
    parts.next().is_none().then_some(method)
}

fn analyze_source_ast(
    source: &str,
    path: &Path,
    accumulator: &mut AnalysisAccumulator,
    analysis_budget: &mut AnalysisBudget,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let mut parser = Parser::new();
    if let Err(error) = parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into()) {
        diagnostics.push(Diagnostic::error(
            "ast_language",
            format!("could not initialize the TypeScript parser: {error}"),
            Some(path.to_path_buf()),
        ));
        accumulator.parse_errors += 1;
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        diagnostics.push(Diagnostic::warning(
            "ast_parse",
            "TypeScript parser returned no syntax tree",
            Some(path.to_path_buf()),
        ));
        accumulator.parse_errors += 1;
        return Vec::new();
    };
    if tree.root_node().has_error() {
        diagnostics.push(Diagnostic::warning(
            "ast_syntax",
            "extension contains syntax the TypeScript parser could not fully recover",
            Some(path.to_path_buf()),
        ));
        accumulator.parse_errors += 1;
    }
    let mut local_imports = Vec::new();
    let api_bindings = extension_api_bindings(tree.root_node(), source.as_bytes());
    if !visit_ast(
        tree.root_node(),
        source.as_bytes(),
        &api_bindings,
        accumulator,
        analysis_budget,
        &mut local_imports,
    ) {
        diagnostics.push(Diagnostic::warning(
            "ast_node_limit",
            "extension or setup analysis exhausted its bounded syntax-node budget",
            Some(path.to_path_buf()),
        ));
        accumulator.parse_errors += 1;
    }
    local_imports
}

fn visit_ast(
    root: Node<'_>,
    source: &[u8],
    api_bindings: &BTreeSet<String>,
    accumulator: &mut AnalysisAccumulator,
    analysis_budget: &mut AnalysisBudget,
    local_imports: &mut Vec<String>,
) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if accumulator.ast_nodes >= MAX_AST_NODES
            || analysis_budget.syntax_nodes >= MAX_SCAN_AST_NODES
        {
            return false;
        }
        accumulator.ast_nodes += 1;
        analysis_budget.syntax_nodes += 1;
        match node.kind() {
            "import_statement" | "export_statement" => {
                if let Some(module) = node
                    .child_by_field_name("source")
                    .and_then(|source_node| string_value(source_node, source))
                {
                    record_import(&module, accumulator);
                    local_imports.push(module);
                }
            }
            "call_expression" => {
                inspect_call(node, source, api_bindings, accumulator, local_imports)
            }
            "new_expression" => inspect_new(node, source, accumulator),
            "member_expression" | "subscript_expression" => {
                if let Some(chain) = member_chain(node, source) {
                    inspect_member_chain(&chain, accumulator);
                }
            }
            "assignment_expression" | "augmented_assignment_expression" => {
                if let Some(left) = node
                    .child_by_field_name("left")
                    .and_then(|left| node_text(left, source))
                {
                    inspect_mutation(left, accumulator);
                }
            }
            "pair" => inspect_object_pair(node, source, api_bindings, accumulator),
            _ => {}
        }

        let mut cursor = node.walk();
        let children = node.children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    true
}

fn inspect_object_pair(
    node: Node<'_>,
    source: &[u8],
    api_bindings: &BTreeSet<String>,
    accumulator: &mut AnalysisAccumulator,
) {
    let Some(key) = node
        .child_by_field_name("key")
        .and_then(|key| node_text(key, source))
        .map(|key| key.trim_matches(['\'', '"']))
    else {
        return;
    };
    if matches!(key, "renderCall" | "renderResult" | "renderMessage") {
        accumulator.ui.insert(key.to_owned());
    }
    if matches!(
        key,
        "systemPrompt" | "input" | "arguments" | "args" | "result" | "content" | "isError"
    ) && enclosing_pi_event(node, source, api_bindings).is_some()
    {
        accumulator.mutations.insert(key.to_owned());
    }
}

fn enclosing_pi_event(
    mut node: Node<'_>,
    source: &[u8],
    api_bindings: &BTreeSet<String>,
) -> Option<String> {
    for _ in 0..64 {
        node = node.parent()?;
        if node.kind() != "call_expression" {
            continue;
        }
        let function = node.child_by_field_name("function")?;
        let chain = member_chain(function, source)?;
        if is_extension_api_direct_method(&chain, "on", api_bindings) {
            return first_string_argument(node, source);
        }
    }
    None
}

fn inspect_call(
    node: Node<'_>,
    source: &[u8],
    api_bindings: &BTreeSet<String>,
    accumulator: &mut AnalysisAccumulator,
    local_imports: &mut Vec<String>,
) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    if function.kind() == "import" {
        accumulator.security.dynamic_imports = true;
        if let Some(module) = first_string_argument(node, source) {
            record_import(&module, accumulator);
            local_imports.push(module);
        }
        return;
    }
    if function.kind() == "identifier" {
        match node_text(function, source) {
            Some("fetch") => accumulator.security.network = true,
            Some("require") => {
                if let Some(module) = first_string_argument(node, source) {
                    record_import(&module, accumulator);
                    local_imports.push(module);
                }
                return;
            }
            _ => {}
        }
    }
    let Some(chain) = member_chain(function, source) else {
        return;
    };
    let method = chain.rsplit('.').next().unwrap_or(&chain);
    if is_extension_api_direct_method(&chain, "on", api_bindings) {
        if let Some(event) = first_string_argument(node, source) {
            accumulator.events.insert(event);
        }
    }
    if let Some(event_bus_method) = extension_api_event_bus_method(&chain, api_bindings) {
        if matches!(event_bus_method, "on" | "emit") {
            accumulator.actions.insert("eventBus".to_owned());
        } else {
            accumulator
                .actions
                .insert(format!("unknown:events.{event_bus_method}"));
        }
    }
    if matches!(
        method,
        "registerTool"
            | "registerCommand"
            | "registerShortcut"
            | "registerFlag"
            | "registerProvider"
            | "unregisterProvider"
            | "registerMessageRenderer"
            | "registerMarkdownTransformer"
            | "registerEntryRenderer"
    ) {
        accumulator.registrations.insert(method.to_owned());
    }
    if method == "exec" {
        accumulator.security.process = true;
    }
    if matches!(
        method,
        "exec"
            | "appendEntry"
            | "setSessionName"
            | "getSessionName"
            | "setLabel"
            | "getActiveTools"
            | "setActiveTools"
            | "getAllTools"
            | "getCommands"
            | "getFlag"
            | "refreshTools"
            | "sendMessage"
            | "sendUserMessage"
            | "getModel"
            | "setModel"
            | "getThinkingLevel"
            | "setThinkingLevel"
            | "newSession"
            | "fork"
            | "navigateTree"
            | "switchSession"
            | "reload"
            | "compact"
            | "getSystemPrompt"
            | "getSystemPromptOptions"
            | "getContextUsage"
            | "waitForIdle"
            | "hasPendingMessages"
            | "shutdown"
    ) {
        accumulator.actions.insert(method.to_owned());
    }
    let direct_pi_method = is_extension_api_direct_method(&chain, method, api_bindings);
    if direct_pi_method
        && !matches!(
            method,
            "on" | "registerTool"
                | "registerCommand"
                | "registerShortcut"
                | "registerFlag"
                | "registerProvider"
                | "unregisterProvider"
                | "registerMessageRenderer"
                | "registerMarkdownTransformer"
                | "registerEntryRenderer"
                | "exec"
                | "appendEntry"
                | "setSessionName"
                | "getSessionName"
                | "setLabel"
                | "getActiveTools"
                | "setActiveTools"
                | "getAllTools"
                | "getCommands"
                | "getFlag"
                | "sendMessage"
                | "sendUserMessage"
                | "setModel"
                | "getThinkingLevel"
                | "setThinkingLevel"
        )
    {
        accumulator.actions.insert(format!("unknown:{method}"));
    }
    if let Some((owner, ui_suffix)) = chain.split_once(".ui.") {
        if !ui_suffix.contains('.') {
            if !owner.contains('.') && api_bindings.contains(owner) {
                accumulator
                    .actions
                    .insert(format!("unknown:ui.{ui_suffix}"));
            } else {
                accumulator.ui.insert(ui_suffix.to_owned());
            }
        }
    }
    inspect_member_chain(&chain, accumulator);
}

fn inspect_new(node: Node<'_>, source: &[u8], accumulator: &mut AnalysisAccumulator) {
    let constructor = node
        .child_by_field_name("constructor")
        .or_else(|| node.child_by_field_name("function"));
    if constructor
        .and_then(|constructor| node_text(constructor, source))
        .is_some_and(|name| matches!(name, "WebSocket" | "EventSource" | "XMLHttpRequest"))
    {
        accumulator.security.network = true;
    }
}

fn inspect_member_chain(chain: &str, accumulator: &mut AnalysisAccumulator) {
    if chain == "process.env" || chain.starts_with("process.env.") {
        accumulator.security.secrets = true;
    }
    if chain.starts_with("Deno.read")
        || chain.starts_with("Deno.write")
        || chain.starts_with("Bun.file")
        || chain.starts_with("Bun.write")
    {
        accumulator.security.filesystem = true;
    }
    if chain.starts_with("Deno.connect")
        || chain.starts_with("Deno.listen")
        || chain.starts_with("Bun.connect")
    {
        accumulator.security.network = true;
    }
    if chain.starts_with("Deno.Command") || chain.starts_with("Bun.spawn") {
        accumulator.security.process = true;
    }
    if chain.contains("sessionManager")
        || chain.contains("modelRegistry")
        || chain.contains("systemPrompt")
    {
        accumulator.actions.insert(chain.to_owned());
    }
}

fn inspect_mutation(left: &str, accumulator: &mut AnalysisAccumulator) {
    let compact = left.replace(' ', "");
    for field in [
        "arguments",
        ".args",
        ".input",
        ".result",
        ".content",
        "systemPrompt",
        "activeTools",
    ] {
        if compact.contains(field) {
            accumulator
                .mutations
                .insert(field.trim_start_matches('.').to_owned());
        }
    }
}

fn record_import(module: &str, accumulator: &mut AnalysisAccumulator) {
    accumulator.imports.insert(module.to_owned());
    let bare = module.strip_prefix("node:").unwrap_or(module);
    if matches!(bare, "fs" | "fs/promises") {
        accumulator.security.filesystem = true;
    }
    if bare == "process" {
        accumulator.security.secrets = true;
    }
    if matches!(bare, "child_process" | "cluster" | "worker_threads") {
        accumulator.security.process = true;
    }
    if matches!(
        bare,
        "http" | "https" | "http2" | "net" | "tls" | "dns" | "dgram"
    ) || module.starts_with("undici")
        || module.starts_with("node-fetch")
        || module.starts_with("axios")
    {
        accumulator.security.network = true;
    }
    if module.ends_with(".node") || module.contains("node-gyp") || module.contains("node-pre-gyp") {
        accumulator.security.native_modules = true;
    }
}

fn is_private_pi_import(module: &str) -> bool {
    module.starts_with("@earendil-works/pi-coding-agent/dist/")
        || module.starts_with("@earendil-works/pi-coding-agent/src/")
        || module.starts_with("@earendil-works/pi-tui/dist/")
        || module.starts_with("@earendil-works/pi-tui/src/")
}

const PI_0_84_4_EVENTS: &[&str] = &[
    "project_trust",
    "resources_discover",
    "session_start",
    "session_info_changed",
    "session_before_switch",
    "session_before_fork",
    "session_before_compact",
    "session_compact",
    "session_compact_failed",
    "session_shutdown",
    "session_before_tree",
    "session_tree",
    "context",
    "before_provider_request",
    "before_provider_headers",
    "after_provider_response",
    "before_agent_start",
    "agent_start",
    "agent_end",
    "agent_settled",
    "ui_prompt_start",
    "ui_prompt_end",
    "turn_start",
    "turn_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "model_select",
    "thinking_level_select",
    "user_bash",
    "input",
    "tool_call",
    "tool_result",
];

const PI_0_84_4_UI_METHODS: &[&str] = &[
    "select",
    "confirm",
    "input",
    "editor",
    "notify",
    "onTerminalInput",
    "setStatus",
    "setWorkingMessage",
    "setWorkingVisible",
    "setWorkingIndicator",
    "setHiddenThinkingLabel",
    "setWidget",
    "setFooter",
    "setHeader",
    "setTitle",
    "custom",
    "pasteToEditor",
    "setEditorText",
    "getEditorText",
    "addAutocompleteProvider",
    "setEditorComponent",
    "getEditorComponent",
    "getAllThemes",
    "getTheme",
    "setTheme",
    "getToolsExpanded",
    "setToolsExpanded",
];

const PI_0_84_4_RENDERER_FIELDS: &[&str] = &["renderCall", "renderResult", "renderMessage"];

const PI_0_84_4_REGISTRATIONS: &[&str] = &[
    "registerTool",
    "registerCommand",
    "registerShortcut",
    "registerFlag",
    "registerProvider",
    "unregisterProvider",
    "registerMessageRenderer",
    "registerMarkdownTransformer",
    "registerEntryRenderer",
];

fn classify_extension(accumulator: &AnalysisAccumulator) -> (MigrationPath, Vec<String>) {
    let mut reasons = Vec::new();
    let parse_incomplete =
        accumulator.parse_errors > 0 || !accumulator.unresolved_imports.is_empty();
    let private_pi_imports = accumulator
        .imports
        .iter()
        .filter(|module| is_private_pi_import(module))
        .cloned()
        .collect::<Vec<_>>();
    if !private_pi_imports.is_empty() {
        reasons.push(format!(
            "imports Pi private/internal modules outside the public 0.84.4 compatibility profile: {}",
            private_pi_imports.join(", ")
        ));
        return (MigrationPath::Blocked, reasons);
    }
    let unknown_events = accumulator
        .events
        .iter()
        .filter(|event| !PI_0_84_4_EVENTS.contains(&event.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let unknown_ui = accumulator
        .ui
        .iter()
        .filter(|method| {
            !PI_0_84_4_UI_METHODS.contains(&method.as_str())
                && !PI_0_84_4_RENDERER_FIELDS.contains(&method.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    let unknown_registrations = accumulator
        .registrations
        .iter()
        .filter(|method| !PI_0_84_4_REGISTRATIONS.contains(&method.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let unknown_actions = accumulator
        .actions
        .iter()
        .filter_map(|action| action.strip_prefix("unknown:").map(str::to_owned))
        .collect::<Vec<_>>();
    if !unknown_events.is_empty()
        || !unknown_ui.is_empty()
        || !unknown_registrations.is_empty()
        || !unknown_actions.is_empty()
    {
        if !unknown_events.is_empty() {
            reasons.push(format!(
                "uses event names outside the pinned Pi 0.84.4 compatibility profile: {}",
                unknown_events.join(", ")
            ));
        }
        if !unknown_ui.is_empty() {
            reasons.push(format!(
                "uses UI methods outside the pinned Pi 0.84.4 compatibility profile: {}",
                unknown_ui.join(", ")
            ));
        }
        if !unknown_registrations.is_empty() {
            reasons.push(format!(
                "uses registrations outside the pinned Pi 0.84.4 compatibility profile: {}",
                unknown_registrations.join(", ")
            ));
        }
        if !unknown_actions.is_empty() {
            reasons.push(format!(
                "uses ExtensionAPI methods outside the pinned Pi 0.84.4 compatibility profile: {}",
                unknown_actions.join(", ")
            ));
        }
        if parse_incomplete {
            reasons.push(
                "the source graph was only partially analyzed; review parse diagnostics and unresolved imports"
                    .to_owned(),
            );
        }
        return (MigrationPath::Blocked, reasons);
    }
    let imports_tui = accumulator
        .imports
        .iter()
        .any(|module| module.contains("pi-tui") || module.ends_with("/tui") || module == "ink");
    let arbitrary_ui = accumulator.ui.iter().any(|method| {
        matches!(
            method.as_str(),
            "setEditor"
                | "setEditorComponent"
                | "getEditorComponent"
                | "setFooter"
                | "setHeader"
                | "setWidget"
                | "custom"
                | "overlay"
                | "addAutocompleteProvider"
                | "setAutocompleteProvider"
                | "onTerminalInput"
        )
    });
    let provider = accumulator.registrations.iter().any(|registration| {
        matches!(
            registration.as_str(),
            "registerProvider" | "unregisterProvider"
        )
    }) || accumulator.events.iter().any(|event| {
        matches!(
            event.as_str(),
            "before_provider_request" | "before_provider_headers" | "after_provider_response"
        )
    });
    let message_renderer = accumulator.registrations.iter().any(|registration| {
        matches!(
            registration.as_str(),
            "registerMessageRenderer" | "registerMarkdownTransformer" | "registerEntryRenderer"
        )
    });
    let deep_session = accumulator.actions.iter().any(|action| {
        action.contains("sessionManager")
            || matches!(
                action.as_str(),
                "appendEntry"
                    | "setSessionName"
                    | "setLabel"
                    | "newSession"
                    | "fork"
                    | "navigateTree"
                    | "switchSession"
                    | "compact"
            )
    }) || accumulator.events.iter().any(|event| {
        matches!(
            event.as_str(),
            "session_info_changed"
                | "session_before_switch"
                | "session_before_fork"
                | "session_before_compact"
                | "session_compact"
                | "session_compact_failed"
                | "session_before_tree"
                | "session_tree"
        )
    });
    if arbitrary_ui || provider || deep_session {
        if arbitrary_ui {
            reasons.push("depends on Pi's arbitrary TUI/editor component surface".to_owned());
        }
        if provider {
            reasons.push("registers provider-native behavior".to_owned());
        }
        if deep_session {
            reasons.push("depends on Pi session, entry, or compaction internals".to_owned());
        }
        if parse_incomplete {
            reasons.push(
                "the source graph was only partially analyzed; review parse diagnostics and unresolved imports"
                    .to_owned(),
            );
        }
        return (MigrationPath::Manual, reasons);
    }

    let unsupported_registration = accumulator
        .registrations
        .iter()
        .any(|registration| matches!(registration.as_str(), "registerShortcut" | "registerFlag"));
    let unsupported_event = accumulator.events.iter().any(|event| {
        matches!(
            event.as_str(),
            "project_trust"
                | "resources_discover"
                | "input"
                | "before_agent_start"
                | "tool_result"
                | "context"
                | "message_start"
                | "message_update"
                | "message_end"
                | "model_select"
                | "thinking_level_select"
                | "user_bash"
                | "session_before_switch"
                | "session_before_fork"
                | "session_before_compact"
                | "session_before_tree"
        )
    });
    let active_tools = accumulator
        .actions
        .iter()
        .any(|action| matches!(action.as_str(), "getActiveTools" | "setActiveTools"));
    let unsupported_action = accumulator.actions.iter().any(|action| {
        matches!(
            action.as_str(),
            "getFlag"
                | "sendMessage"
                | "sendUserMessage"
                | "getModel"
                | "setModel"
                | "setThinkingLevel"
                | "reload"
                | "getSystemPrompt"
                | "getSystemPromptOptions"
                | "getContextUsage"
                | "waitForIdle"
                | "hasPendingMessages"
                | "shutdown"
        )
    });
    let custom_tool_renderer = accumulator
        .ui
        .iter()
        .any(|surface| matches!(surface.as_str(), "renderCall" | "renderResult"));
    let semantic_ui_port = imports_tui
        || message_renderer
        || accumulator
            .ui
            .iter()
            .any(|surface| matches!(surface.as_str(), "custom" | "select" | "theme"));
    if unsupported_registration
        || unsupported_event
        || active_tools
        || unsupported_action
        || custom_tool_renderer
        || semantic_ui_port
        || !accumulator.mutations.is_empty()
    {
        if unsupported_registration {
            reasons.push(
                "uses shortcut or CLI-flag registration that API 0.2 does not expose".to_owned(),
            );
        }
        if unsupported_event {
            reasons.push(
                "subscribes to a mutating Pi hook without an equivalent octet hook".to_owned(),
            );
        }
        if active_tools {
            reasons.push(
                "mutates Pi's active tool set instead of a bounded octet policy overlay".to_owned(),
            );
        }
        if unsupported_action {
            reasons.push(
                "uses Pi host-state or agent-control actions without an equivalent octet bridge service"
                    .to_owned(),
            );
        }
        if custom_tool_renderer {
            reasons
                .push("uses a Pi component renderer that needs a semantic octet port".to_owned());
        }
        if semantic_ui_port {
            reasons.push(
                "uses Pi UI components that need a semantic octet presentation port".to_owned(),
            );
        }
        if !accumulator.mutations.is_empty() {
            reasons.push("mutates prompt, tool arguments, input, or tool results".to_owned());
        }
        if parse_incomplete {
            reasons.push(
                "the source graph was only partially analyzed; review parse diagnostics and unresolved imports"
                    .to_owned(),
            );
        }
        return (MigrationPath::NativePort, reasons);
    }

    if parse_incomplete {
        reasons.push(
            "source graph could not be analyzed completely, so compatibility is unknown".to_owned(),
        );
        return (MigrationPath::Blocked, reasons);
    }

    reasons.push(
        "uses capability-shaped registrations supported by a compatibility process".to_owned(),
    );
    (MigrationPath::Bridge, reasons)
}

fn is_code_import(import: &str) -> bool {
    matches!(
        Path::new(import).extension().and_then(OsStr::to_str),
        None | Some("ts" | "tsx" | "mts" | "cts" | "js" | "mjs" | "cjs")
    )
}

fn resolve_local_import(importer: &Path, package_root: &Path, import: &str) -> Option<PathBuf> {
    let parent = importer.parent()?;
    let base = if import.starts_with('.') {
        normalize_absolute(&parent.join(import)).ok()?
    } else {
        let import = import.strip_prefix("#src/")?;
        normalize_absolute(&package_root.join("src").join(import)).ok()?
    };
    if !base.starts_with(package_root) {
        return None;
    }
    let candidates = if base.extension().is_some() {
        let mut candidates = vec![base.clone()];
        if matches!(
            base.extension().and_then(OsStr::to_str),
            Some("js" | "mjs" | "cjs")
        ) {
            candidates.extend(
                ["ts", "tsx", "mts", "cts", "d.ts"]
                    .into_iter()
                    .map(|extension| base.with_extension(extension)),
            );
        }
        candidates
    } else {
        let mut candidates = ["ts", "tsx", "mts", "cts", "js", "mjs", "cjs", "d.ts"]
            .into_iter()
            .map(|extension| base.with_extension(extension))
            .collect::<Vec<_>>();
        candidates.extend(
            [
                "index.ts",
                "index.tsx",
                "index.mts",
                "index.cts",
                "index.js",
                "index.mjs",
                "index.cjs",
            ]
            .into_iter()
            .map(|name| base.join(name)),
        );
        candidates
    };
    candidates.into_iter().find(|candidate| {
        candidate.starts_with(package_root)
            && std::fs::symlink_metadata(candidate)
                .is_ok_and(|metadata| metadata.file_type().is_file())
    })
}

fn first_string_argument(node: Node<'_>, source: &[u8]) -> Option<String> {
    let arguments = node.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let value = arguments
        .named_children(&mut cursor)
        .find_map(|argument| string_value(argument, source));
    value
}

fn string_value(node: Node<'_>, source: &[u8]) -> Option<String> {
    if !matches!(
        node.kind(),
        "string" | "string_fragment" | "template_string"
    ) {
        return None;
    }
    let text = node_text(node, source)?;
    let value = text
        .strip_prefix(['\'', '"', '`'])
        .and_then(|text| text.strip_suffix(['\'', '"', '`']))
        .unwrap_or(text);
    (!value.contains("${")).then(|| value.to_owned())
}

fn member_chain(node: Node<'_>, source: &[u8]) -> Option<String> {
    member_chain_inner(node, source, 0)
}

fn member_chain_inner(node: Node<'_>, source: &[u8], depth: usize) -> Option<String> {
    if depth >= 64 {
        return None;
    }
    match node.kind() {
        "identifier" | "property_identifier" | "private_property_identifier" | "this" => {
            node_text(node, source).map(str::to_owned)
        }
        "member_expression" => {
            let object = node.child_by_field_name("object")?;
            let property = node.child_by_field_name("property")?;
            Some(format!(
                "{}.{}",
                member_chain_inner(object, source, depth + 1)?,
                node_text(property, source)?
            ))
        }
        "subscript_expression" => {
            let object = node.child_by_field_name("object")?;
            let index = node.child_by_field_name("index")?;
            let property = string_value(index, source)?;
            Some(format!(
                "{}.{}",
                member_chain_inner(object, source, depth + 1)?,
                property
            ))
        }
        _ => None,
    }
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(&source[node.byte_range()]).ok()
}
