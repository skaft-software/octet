// Keep this the first import in executable entrypoints: reject unsupported Node
// before evaluating adapter dependencies or importing any reviewed factory.
export function assertRuntimePrerequisites(versions = process.versions) {
  // Bun reports a Node compatibility version too. Preserve its existing path;
  // this Node prerequisite is not a claim of Bun compatibility qualification.
  if (versions.bun) return;
  const [major, minor] = versions.node.split('.').map(Number);
  if (major > 22 || (major === 22 && minor >= 19)) return;
  throw new Error(`[pi-compat] Node 22.19.0 or newer is required; found Node ${versions.node}. Upgrade Node and rerun this command (automatic mirror requires it on PATH).`);
}

assertRuntimePrerequisites();
