import { Console } from 'node:console';
import { Writable } from 'node:stream';

// Console methods (including dir/table/trace) never acquire protocol stdout.
export function diagnosticConsole() {
  let remaining = 65_536;
  const stderr = process.stderr.write.bind(process.stderr);
  const sink = new Writable({
    write(chunk, _encoding, callback) {
      const bytes = chunk.subarray(0, Math.min(4096, remaining));
      remaining -= bytes.length;
      if (bytes.length) stderr(bytes);
      callback();
    },
  });
  return new Console({stdout: sink, stderr: sink});
}
