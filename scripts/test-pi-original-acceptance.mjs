// Static AST inventory only: never imports a corpus factory. Explicit paths only.
// node scripts/test-pi-original-acceptance.mjs TS_PARSER PACKAGE_ROOT...
import { createRequire } from 'node:module';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';
import { createHash } from 'node:crypto';
const [parser, ...roots] = process.argv.slice(2);
if (!parser || !roots.length) throw Error('supply reviewed local TypeScript parser and package roots');
const ts = createRequire(import.meta.url)(resolve(parser));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
function files(root) { const out = []; function walk(dir) { for (const d of readdirSync(dir, { withFileTypes: true })) { if (['node_modules', '.git'].includes(d.name) || d.isSymbolicLink()) continue; const p = join(dir, d.name); if (d.isDirectory()) walk(p); else if (/\.(?:[cm]?[jt]sx?|json)$/.test(d.name)) out.push(p); } } walk(root); return out.sort(); }
const packages = roots.map(root => {
  root = resolve(root); const manifest = JSON.parse(readFileSync(join(root, 'package.json')));
  const hashes = {}, imports = [], calls = [], assignments = [], diagnostics = [];
  for (const path of files(root)) {
    const source = readFileSync(path, 'utf8'); const rel = relative(root, path); hashes[rel] = sha(source);
    if (path.endsWith('.json')) continue;
    const sf = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true);
    for (const d of sf.parseDiagnostics) diagnostics.push({ path, start: d.start, message: ts.flattenDiagnosticMessageText(d.messageText, '\n') });
    const at = node => ({ path, line: sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1 });
    function visit(node) {
      if (ts.isImportDeclaration(node) || ts.isExportDeclaration(node) && node.moduleSpecifier) imports.push({ ...at(node), specifier: node.moduleSpecifier.text, type_only: node.importClause?.isTypeOnly ?? node.isTypeOnly ?? false, text: node.getText(sf) });
      if (ts.isCallExpression(node) || ts.isNewExpression(node)) {
        const callee = node.expression.getText(sf); const text = node.getText(sf);
        // All calls are retained, not a regex API-name total. Matrix integration can
        // classify aliases/dynamic sites without rerunning or executing packages.
        calls.push({ ...at(node), callee, arguments: (node.arguments ?? []).map(a => a.getText(sf)),
          candidates: [ /spawn|exec|fork|\.resolve$/.test(callee) && 'cli-process-resolution', /Session|session|readFile|writeFile|readdir/.test(text) && 'session-file-sdk', /prototype|defineProperty|patch|setEditorComponent|getEditorComponent/.test(text) && 'private-patch-editor' ].filter(Boolean) });
      }
      if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken && ts.isPropertyAccessExpression(node.left)) assignments.push({ ...at(node), target: node.left.getText(sf), text: node.getText(sf) });
      ts.forEachChild(node, visit);
    } visit(sf);
  }
  return { name: manifest.name, version: manifest.version, root, pi: manifest.pi, engines: manifest.engines, peerDependencies: manifest.peerDependencies, status: 'inventory-only', source_hash_algorithm: 'sha256 of each relative path sorted, NUL, file sha256, LF', source_hash: sha(Object.entries(hashes).map(([p,h]) => `${p}\0${h}\n`).join('')), files: hashes, imports, calls, assignments, parse_diagnostics: diagnostics };
});
console.log(JSON.stringify({ scope: 'static TypeScript AST; no package executed; candidates are navigation hints, not exhaustive semantic classification or compatibility', parser: { path: resolve(parser), version: ts.version, sha256: sha(readFileSync(parser)) }, packages }, null, 2));
