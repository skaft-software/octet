// Development-only oracle refresh. No network, install, or package scripts.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const packageDir = process.argv[2];
if (!packageDir) throw new Error('Usage: node capture.mjs /path/to/grok-mermaid-0.2.3/package');
const metadata = JSON.parse(fs.readFileSync(path.join(packageDir, 'package.json'), 'utf8'));
if (metadata.name !== 'grok-mermaid' || metadata.version !== '0.2.3') {
  throw new Error('Expected grok-mermaid 0.2.3');
}
const { render } = await import(pathToFileURL(path.resolve(packageDir, 'dist/index.js')));
const dir = path.dirname(fileURLToPath(import.meta.url));
const fixturePath = path.join(dir, 'pi-flowcharts.rs');
const existing = fs.readFileSync(fixturePath, 'utf8');
const sources = existing.split('\n').filter(line => line.trim().startsWith('('))
  .map(line => JSON.parse(`[${line.trim().slice(1, -2)}]`)[0]);
const outputs = sources.map(source => {
  const art = render(source);
  if (!art || art.warnings.length) throw new Error(`Unexpected oracle failure: ${source}`);
  return `    (${JSON.stringify(source)}, ${JSON.stringify(art.plain.join('\n'))}),`;
});
fs.writeFileSync(fixturePath,
  '// Captured from grok-mermaid 0.2.3 dist/index.js, not from octet.\n' +
  '// Regeneration/evidence: README.md in this directory.\n' +
  'const PI_FLOWCHARTS: &[(&str, &str)] = &[\n' + outputs.join('\n') + '\n];\n');

const source = fs.readFileSync(path.join(dir, 'reported-architecture.mmd'), 'utf8');
const raw = render(source);
const equivalent = source.replace('ProxyEnv -.PROXY_ENV or ./proxy.env.-> LocalProxy',
  'ProxyEnv -.->|PROXY_ENV or ./proxy.env| LocalProxy');
if (equivalent === source) throw new Error('Reported edge spelling changed; review oracle documentation');
const complete = render(equivalent);
if (!complete || complete.warnings.length) throw new Error('Equivalent-spelling oracle failed');
fs.writeFileSync(path.join(dir, 'reported-architecture.pi.txt'), complete.plain.join('\n') + '\n');
console.log(JSON.stringify({ cases: outputs.length,
  reported: { width: raw?.width, rows: raw?.plain.length, warnings: raw?.warnings },
  equivalent: { width: complete.width, rows: complete.plain.length, warnings: complete.warnings },
}, null, 2));
