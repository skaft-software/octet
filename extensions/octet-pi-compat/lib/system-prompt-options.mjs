import { dirname } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { invalid } from './errors.mjs';
import { formatSkillsForPrompt } from '../shims/pi-1.0.2/coding-pure.mjs';
import { createAPI } from './api.mjs';
import { toolSnapshot, setActiveTools } from './tools.mjs';

export function normalizeSystemPromptOptions(input = {}) {
  return {
    customPrompt: input.customPrompt,
    forceSystemPrompt: input.forceSystemPrompt,
    selectedTools: [...(input.selectedTools ?? ['read', 'bash', 'edit', 'write'])],
    toolSnippets: { ...(input.toolSnippets ?? {}) },
    toolGuidelines: Object.fromEntries(Object.entries(input.toolGuidelines ?? {}).map(([name, guidelines]) => [name, [...guidelines]])),
    promptGuidelines: [...(input.promptGuidelines ?? [])],
    appendSystemPrompt: input.appendSystemPrompt ?? '',
    sections: { ...(input.sections ?? {}) },
    cwd: input.cwd,
    contextFiles: (input.contextFiles ?? []).map(file => ({ ...file })),
    skills: (input.skills ?? []).map(skill => ({ ...skill })),
  };
}

function promptRules(options) {
  const rules = new Set();
  const add = rule => { if (rule.trim()) rules.add(rule.trim()); };
  const shell = ['bash', 'powershell'].filter(name => options.selectedTools.includes(name));
  if (shell.length && !['grep', 'find', 'ls'].some(name => options.selectedTools.includes(name))) {
    add(shell.length === 2 ? 'Use bash or PowerShell for file operations like listing, searching, and finding files'
      : shell[0] === 'powershell' ? 'Use PowerShell for file operations like listing, searching, and finding files'
        : 'Use bash for file operations like ls, rg, find');
  }
  for (const name of options.selectedTools) for (const rule of options.toolGuidelines[name] ?? []) add(rule);
  for (const rule of options.promptGuidelines) add(rule);
  add('Be concise in your responses');
  add('Show file paths clearly when working with files');
  return [...rules].map(rule => `- ${rule}`).join('\n');
}

function replaceSection(prompt, name, content) {
  const section = new RegExp(`(^|\\n\\n)<${name}>\\n[\\s\\S]*?\\n</${name}>(?=\\n\\n|$)`);
  const rendered = content ? `<${name}>\n${content}\n</${name}>` : '';
  if (section.test(prompt)) return prompt.replace(section, (_, prefix) => rendered ? `${prefix}${rendered}` : '');
  return rendered ? [prompt, rendered].filter(Boolean).join('\n\n') : prompt;
}

export function promptOptions(runtime, store, base, factory) {
  const host = store.state.host;
  const input = { cwd: store.state.workspace, ...host.system_prompt_options };
  if (factory !== undefined && runtime.features.has('command_catalog_v1') && input.skills === undefined) {
    input.skills = runtime.scope.run({ ...store, factory }, () => createAPI(runtime, factory).getCommands())
      .filter(command => command.source === 'skill').map(command => {
        const path = command.sourceInfo?.path;
        return { name: command.name.replace(/^skill:/, ''), description: command.description ?? '',
          ...(path ? { filePath: path, baseDir: command.sourceInfo?.baseDir ?? dirname(path) } : {}),
          sourceInfo: command.sourceInfo, disableModelInvocation: false };
      });
  }
  if (factory !== undefined && runtime.features.has('active_tools')) {
    const snapshot = runtime.scope.run({ ...store, factory }, () => toolSnapshot(runtime, factory));
    input.selectedTools = snapshot.active_tools;
    input.toolSnippets = Object.fromEntries(snapshot.all_tools.filter(tool => tool.promptSnippet !== undefined || tool.prompt_snippet !== undefined)
      .map(tool => [tool.name, tool.promptSnippet ?? tool.prompt_snippet]));
    input.toolGuidelines = Object.fromEntries(snapshot.all_tools.map(tool => [tool.name, tool.promptGuidelines ?? tool.prompt_guidelines ?? []]));
  }
  const options = normalizeSystemPromptOptions(input);
  const initial = structuredClone(options);
  const render = () => {
    if (options.forceSystemPrompt !== undefined) return options.forceSystemPrompt;
    for (const name of Object.keys(options.sections)) {
      if (!/^[a-z][a-z0-9_-]*$/.test(name) || name === 'preamble') throw new Error(`Invalid system prompt section name: ${name}`);
    }
    const changed = key => !isDeepStrictEqual(options[key], initial[key]);
    const customChanged = changed('customPrompt');
    const toolsChanged = changed('selectedTools');
    const sections = {};
    if (!options.customPrompt) {
      if (toolsChanged || changed('toolSnippets')) {
        const tools = options.selectedTools.filter(name => options.toolSnippets[name]).map(name => `- ${name}: ${options.toolSnippets[name]}`).join('\n');
        sections.tools = `${tools || '(none)'}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project.`;
      }
      if (toolsChanged || changed('toolGuidelines') || changed('promptGuidelines')) sections.rules = promptRules(options);
    }
    if (customChanged || changed('appendSystemPrompt')) sections.addendum = options.appendSystemPrompt;
    if (customChanged || changed('contextFiles')) sections.project_context = options.contextFiles.length
      ? ['Project-specific instructions and guidelines:', ...options.contextFiles.map(({ path, content }) => `<project_instructions path="${path}">\n${content}\n</project_instructions>`)].join('\n\n') : '';
    if (customChanged || toolsChanged || changed('skills')) {
      const readTool = ['read', 'bash'].find(name => options.selectedTools.includes(name));
      sections.skills = readTool ? formatSkillsForPrompt(options.skills, readTool).trim() : '';
    }
    if (customChanged || changed('cwd')) sections.cwd = options.cwd.replace(/\\/g, '/');
    for (const [name, content] of Object.entries(options.sections)) if (content) sections[name] = content;
    const order = ['tools', 'rules', 'docs', 'addendum', 'project_context', 'skills', 'cwd', ...Object.keys(sections).filter(name => !['tools', 'rules', 'docs', 'addendum', 'project_context', 'skills', 'cwd'].includes(name))];
    let text = customChanged && options.customPrompt ? options.customPrompt : base;
    for (const name of order) if (Object.hasOwn(sections, name)) text = replaceSection(text, name, sections[name]);
    return text;
  };
  const applyTools = () => {
    if (!isDeepStrictEqual(options.selectedTools, initial.selectedTools)) {
      if (factory === undefined) invalid('system prompt tool selection requires an extension owner');
      runtime.scope.run({ ...store, factory }, () => setActiveTools(runtime, factory, options.selectedTools));
    }
    if (factory !== undefined && runtime.features.has('active_tools')) {
      options.selectedTools = [...runtime.scope.run({ ...store, factory }, () => toolSnapshot(runtime, factory)).active_tools];
    }
  };
  return { options, render, applyTools };
}
