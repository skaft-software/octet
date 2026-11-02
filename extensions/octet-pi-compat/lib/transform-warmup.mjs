import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';
import { availableParallelism, setPriority } from 'node:os';
import { spawn } from 'node:child_process';
import { readFileSync, statSync } from 'node:fs';

const TYPESCRIPT = /\.[cm]?tsx?$/;
// Everything jiti transforms: TypeScript, and ESM JavaScript (large bundled
// factories cost seconds of babel time each). .cjs is loaded natively.
const TRANSFORMED = /\.(?:[mc]?tsx?|[mj]sx?)$/;
const MAX_FILE_BYTES = 8 * 1024 * 1024;
const CHILD_FLAG = '--octet-transform-warmup';

// Warm-up runs in a child process so the host event loop stays free. While it
// blocks mirror preparation (cold prepare) it runs at normal priority with one
// core left for the host (measured: more workers do not help, the largest
// bundle bounds the wall time):
// the first frame is already up and wall time is what the user waits on. A
// caller that warms in the background passes `nice` to yield cores instead.
export function warmupWorkerCount(targets, workers, cpus = availableParallelism()) {
  return Math.max(0, Math.min(workers ?? Math.min(8, Math.max(1, cpus - 1)), Math.ceil(targets / 16)));
}

// Starts the warm-up and returns { done, stop }: `done` settles with the result
// when the child exits, times out or is stopped; `stop()` kills it early (the
// capture that overlapped it has filled the cache itself).
export function startTransformWarmup(files, fsCache, { workers, timeoutMs = 60000, nice = 0 } = {}) {
  const none = { done: Promise.resolve({ transformed: 0, workers: 0 }), stop() {} };
  if (!fsCache) return none;
  const targets = [...new Set(files)].filter(file => TRANSFORMED.test(file) && !/\.d\.[cm]?ts$/.test(file)).map(file => {
    try { return { file, size: statSync(file).size }; } catch { return undefined; }
  }).filter(item => item && item.size <= MAX_FILE_BYTES).sort((a, b) => b.size - a.size).map(item => item.file);
  const count = warmupWorkerCount(targets.length, workers);
  if (count < 1) return none;
  const child = spawn(process.execPath, [new URL(import.meta.url).pathname, CHILD_FLAG, String(count), String(nice)],
    { stdio: ['pipe', 'pipe', 'ignore'], env: { ...process.env, OCTET_TRANSFORM_WARMUP_CACHE: fsCache } });
  let output = '';
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', chunk => { output += chunk; });
  const exited = new Promise(resolve => { child.once('error', resolve); child.once('close', resolve); });
  child.stdin.on('error', () => {});
  child.stdin.end(JSON.stringify(targets));
  let timer;
  const expired = new Promise(resolve => { timer = setTimeout(resolve, timeoutMs); timer.unref?.(); });
  const stop = () => { clearTimeout(timer); if (child.exitCode === null) child.kill('SIGKILL'); };
  const done = Promise.race([exited, expired]).then(() => {
    stop();
    const match = /(\d+)\s*$/.exec(output);
    return { transformed: match ? Number(match[1]) : 0, workers: count };
  });
  return { done, stop };
}

export async function warmTransformCache(files, fsCache, options) {
  return startTransformWarmup(files, fsCache, options).done;
}

async function runPool(targets, fsCache, count) {
  let next = 0, transformed = 0;
  const pool = [];
  await Promise.all(Array.from({ length: count }, () => new Promise(resolve => {
    const worker = new Worker(new URL(import.meta.url), { workerData: { octetTransformWarmup: true, fsCache } });
    pool.push(worker);
    const feed = () => worker.postMessage(next < targets.length ? targets[next++] : null);
    worker.on('message', message => { if (message === 'done') transformed++; feed(); });
    worker.once('online', feed);
    worker.once('error', () => resolve());
    worker.once('exit', () => resolve());
  })));
  await Promise.all(pool.map(worker => worker.terminate().catch(() => {})));
  return transformed;
}

if (isMainThread && process.argv[2] === CHILD_FLAG) {
  try { if (Number(process.argv[4])) setPriority(0, Number(process.argv[4])); } catch { /* best effort; still bounded */ }
  let input = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) input += chunk;
  const targets = JSON.parse(input);
  const transformed = await runPool(targets, process.env.OCTET_TRANSFORM_WARMUP_CACHE, Number(process.argv[3]));
  process.stdout.write(`${transformed}\n`, () => process.exit(0));
}

if (!isMainThread && workerData?.octetTransformWarmup) {
  const { createJiti } = await import('jiti');
  const jiti = createJiti(import.meta.url, { fsCache: workerData.fsCache, moduleCache: false });
  parentPort.on('message', file => {
    if (file === null) { parentPort.close(); return; }
    try {
      jiti.transform({ filename: file, source: readFileSync(file, 'utf8'), ts: TYPESCRIPT.test(file), async: true });
      parentPort.postMessage('done');
    } catch {
      parentPort.postMessage('failed');
    }
  });
}
