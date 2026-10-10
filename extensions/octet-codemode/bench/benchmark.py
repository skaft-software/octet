"""Benchmark the Rust codemode runner against the previous Node/Pi runtime.

Identical scripts and an identical fixture host measure three cases, each with
N samples:

* ``cold_first_script``  fresh extension process through initialize to the first
  script result (for the Rust runner this includes the one-time module compile).
* ``warm_script``        the same one-line script repeated in one warm process.
* ``tool_call_50``       one script making 50 sequential brokered tool calls.

Peak RSS and CPU are summed over the extension process tree (the extension plus
the runner it owns). Python is harness tooling only: the measured processes are
the real Rust executable and, for the reference, the real Node launcher with the
vendored Pi runtime.

The shipped Node/Pi bundle rejects the 0.9.0 feature offer (it requires the
0.8.2 key set), so the reference is measured on the offer it accepts while the
Rust extension always runs the real 0.9.0 offer. That difference is recorded in
the results file and the README.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = ROOT.parents[1]
sys.path.insert(0, str(ROOT / "tests"))
sys.path.insert(0, str(ROOT))

from support import offer  # noqa: E402
from test_adapter import Broker, context  # noqa: E402


def release_binary():
    """Benchmarks measure the optimized build; tests use the debug build."""
    override = os.environ.get("CODEMODE_BENCH_BINARY")
    if override:
        return Path(override).resolve()
    for profile in ("release", "debug"):
        candidate = ROOT / "target" / profile / "octet-codemode"
        if candidate.is_file():
            return candidate
    raise SystemExit("build the extension first: cargo build --release --locked")

COLD = "return 42;"
CALLS = "for (let i = 0; i < 50; i++) { await tools.first({value: i}); } return 'ok';"
NODE_BUNDLE_REVISION = "901e12eb"


def legacy_offer(_optional=None):
    """The feature offer the shipped Node/Pi bundle accepts."""
    shaped = offer(["tool_composition_v1", "request_progress", "artifacts"])
    protocol = shaped["protocol"]
    protocol.pop("session_snapshot_transport_v1", None)
    protocol["limits"] = {
        "max_concurrent_requests": 8,
        "resource_refs_v1": {"max_records": 256, "max_registrations_per_parent": 32},
    }
    return shaped


def node_bundle(directory):
    """Materialize the previous shipped bundle from git (Node + Pi runtime)."""
    bundle = directory / "node-bundle"
    bundle.mkdir(parents=True, exist_ok=True)
    archive = subprocess.run(
        ["git", "-C", str(REPOSITORY), "archive", NODE_BUNDLE_REVISION, "extensions/octet-codemode"],
        check=True, stdout=subprocess.PIPE, stdin=subprocess.DEVNULL)
    subprocess.run(["tar", "-x", "-C", str(bundle)], input=archive.stdout, check=True)
    return bundle / "extensions/octet-codemode"


def tree_stats(root_pid):
    """Peak RSS (VmHWM) and accumulated CPU of one process tree, from /proc."""
    rss = 0
    cpu = 0.0
    pending = [root_pid]
    seen = set()
    while pending:
        pid = pending.pop()
        if pid in seen:
            continue
        seen.add(pid)
        try:
            status = Path(f"/proc/{pid}/status").read_text()
            stat = Path(f"/proc/{pid}/stat").read_text()
            children = Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
        except OSError:
            continue
        for line in status.splitlines():
            if line.startswith("VmHWM:"):
                rss += int(line.split()[1]) * 1024
        fields = stat.rsplit(")", 1)[1].split()
        cpu += (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
        pending.extend(int(child) for child in children)
    return rss, cpu


class Measured:
    """One extension process driving the shared fixture host."""

    def __init__(self, engine, command, cwd, env, offer_builder):
        self.engine, self.command, self.cwd, self.env = engine, command, cwd, env
        self.offer_builder = offer_builder
        self.temporary = tempfile.TemporaryDirectory(prefix="codemode-bench-")
        scratch = Path(self.temporary.name) / "scratch"
        scratch.mkdir(mode=0o700)
        self.broker = Broker(engine, scratch, Path(command[0]), command=command, cwd=cwd,
                             env=env, offer_builder=offer_builder)
        self.broker.context = context()

    async def __aenter__(self):
        await self.broker.start()
        return self

    async def __aexit__(self, *_):
        await self.broker.close()
        self.temporary.cleanup()

    async def run(self, code):
        started = time.perf_counter()
        result = await self.broker.run(code)
        wall = time.perf_counter() - started
        if result["is_error"]:
            raise SystemExit(f"script failed: {code[:40]} -> {result}")
        rss, cpu = tree_stats(self.broker.process.pid)
        return wall, rss, cpu


async def collect(engine, command, cwd, env, offer_builder, samples):
    cold = []
    rss = cpu = 0
    for _ in range(samples):
        async with Measured(engine, command, cwd, env, offer_builder) as measured:
            wall, tree_rss, tree_cpu = await measured.run(COLD)
        cold.append(wall)
        rss, cpu = max(rss, tree_rss), max(cpu, tree_cpu)
    warm = []
    tool50 = []
    async with Measured(engine, command, cwd, env, offer_builder) as measured:
        await measured.run(COLD)  # first script in this process is the warm-up
        for _ in range(max(1, samples - 1)):
            wall, tree_rss, tree_cpu = await measured.run(COLD)
            warm.append(wall)
            rss, cpu = max(rss, tree_rss), max(cpu, tree_cpu)
        for _ in range(samples):
            wall, tree_rss, tree_cpu = await measured.run(CALLS)
            tool50.append(wall)
            rss, cpu = max(rss, tree_rss), max(cpu, tree_cpu)
    return {"cold": cold, "warm": warm, "tool50": tool50, "rss": rss, "cpu": cpu}


def summarize(case, walls, rss, cpu):
    ordered = sorted(walls)
    return {
        "case": case,
        "samples": len(walls),
        "median_ms": round(statistics.median(ordered) * 1000, 2),
        "p95_ms": round(ordered[min(len(ordered) - 1, int(len(ordered) * 0.95))] * 1000, 2),
        "min_ms": round(ordered[0] * 1000, 2),
        "peak_rss_mib": round(rss / 2**20, 1),
        "cpu_seconds": round(cpu, 3),
    }


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--output", type=Path, default=ROOT / "bench/results.json")
    arguments = parser.parse_args()

    rust = release_binary()
    with tempfile.TemporaryDirectory(prefix="codemode-bench-bundle-") as temporary:
        node = node_bundle(Path(temporary))
        engines = (
            ("rust-wasi", "wasi", [str(rust), "serve", "--engine", "wasi"], rust.parent,
             {"PATH": ""}, None),
            ("node-pi", "wasi", [sys.executable, "extension.py"], node,
             {"PATH": os.environ.get("PATH", ""), "OCTET_EXTENSION_DIR": str(node)},
             legacy_offer),
        )
        results = {
            "samples": arguments.samples,
            "node_bundle_revision": NODE_BUNDLE_REVISION,
            "note": (
                "The shipped Node/Pi bundle rejects the 0.9.0 feature offer; it is "
                "measured on the offer it accepts. The Rust extension runs the real "
                "0.9.0 offer."
            ),
            "engines": {},
        }
        for name, engine, command, cwd, env, offer_builder in engines:
            sample = await collect(engine, command, cwd, env, offer_builder, arguments.samples)
            cases = [
                summarize("cold_first_script", sample["cold"], sample["rss"], sample["cpu"]),
                summarize("warm_script", sample["warm"], sample["rss"], sample["cpu"]),
                summarize("tool_call_50", sample["tool50"], sample["rss"], sample["cpu"]),
            ]
            results["engines"][name] = cases
            for case in cases:
                print(f"{name:10} {case['case']:20} median {case['median_ms']:9.2f} ms "
                      f"p95 {case['p95_ms']:9.2f} ms rss {case['peak_rss_mib']:6.1f} MiB "
                      f"cpu {case['cpu_seconds']:.2f} s")
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(json.dumps(results, indent=2, sort_keys=True) + "\n")
        print(f"wrote {arguments.output}")


if __name__ == "__main__":
    asyncio.run(main())
