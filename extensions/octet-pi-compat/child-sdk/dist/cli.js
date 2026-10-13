#!/usr/bin/env node
// This process is a JSON client of octet-owned sessions, never a Pi agent.
import { runChildCliClient } from '../../lib/child-cli-client.mjs';
try { process.exitCode = await runChildCliClient(process.argv.slice(2)); }
catch (error) { process.stderr.write(`[octet Pi child facade] ${String(error?.message ?? error).slice(0, 4096)}\n`); process.exitCode = 1; }
