#!/usr/bin/env node
// Entry point; everything lives in ../lib so tests can call main() directly.
import { main } from "../lib/cli.mjs";

const code = await main(process.argv.slice(2), {
  stdout: (s) => process.stdout.write(s),
  stderr: (s) => process.stderr.write(s),
  env: process.env,
  isTTY: Boolean(process.stdout.isTTY),
});
process.exitCode = code;
