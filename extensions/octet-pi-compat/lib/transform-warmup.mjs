import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';
import { availableParallelism } from 'node:os';
import { readFileSync, statSync } from 'node:fs';

const TYPESCRIPT = /\.[cm]?tsx?$/;
const MAX_FILE_BYTES = 8 * 1024 * 1024;

export async function warmTransformCache(files, fsCache, { workers, timeoutMs = 60000 } = {}) {
  if (!fsCache) return { transformed: 0, workers: 0 };
  const targets = [...new Set(files)].filter(file => TYPESCRIPT.test(file)).map(file => {
    try { return { file, size: statSync(file).size }; } catch { return undefined; }
  }).filter(item => item && item.size <= MAX_FILE_BYTES).sort((a, b) => b.size - a.size).map(item => item.file);
  const count = Math.min(workers ?? Math.min(6, availableParallelism() - 1), Math.ceil(targets.length / 16));
  if (count < 2) return { transformed: 0, workers: 0 };
  let next = 0, transformed = 0;
  const pool = [];
  const finished = Promise.all(Array.from({ length: count }, () => new Promise(resolve => {
    const worker = new Worker(new URL(import.meta.url), { workerData: { octetTransformWarmup: true, fsCache } });
    pool.push(worker);
    const feed = () => worker.postMessage(next < targets.length ? targets[next++] : null);
    worker.on('message', message => { if (message === 'done') transformed++; feed(); });
    worker.once('online', feed);
    worker.once('error', () => resolve());
    worker.once('exit', () => resolve());
  })));
  let timer;
  const expired = new Promise(resolve => { timer = setTimeout(resolve, timeoutMs); timer.unref?.(); });
  await Promise.race([finished, expired]);
  clearTimeout(timer);
  await Promise.all(pool.map(worker => worker.terminate().catch(() => {})));
  return { transformed, workers: count };
}

if (!isMainThread && workerData?.octetTransformWarmup) {
  const { createJiti } = await import('jiti');
  const jiti = createJiti(import.meta.url, { fsCache: workerData.fsCache, moduleCache: false });
  parentPort.on('message', file => {
    if (file === null) { parentPort.close(); return; }
    try {
      jiti.transform({ filename: file, source: readFileSync(file, 'utf8'), ts: true, async: true });
      parentPort.postMessage('done');
    } catch {
      parentPort.postMessage('failed');
    }
  });
}
