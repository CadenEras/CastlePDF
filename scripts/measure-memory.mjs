#!/usr/bin/env node
// Measures the memory of the release desktop app: shell, worker and WebView
// processes, over several successive starts, idle or with a document open.
// Usage: node scripts/measure-memory.mjs [runs=10] [settleSeconds=5] [file.pdf]
// With a file, the app opens it at start (as from the command line) and is
// measured once its first page has had `settleSeconds` to render.
// Requires `npm --prefix apps/desktop run tauri -- build --no-bundle -- --locked`
// first (from bash: PowerShell drops the first `--`).
// Prints a Markdown report; it does not judge results against a budget.
//
// Attribution per OS:
// - Windows: the shell process tree (WebView2 processes are descendants).
// - Linux: the shell process tree (WebKitGTK processes are descendants).
// - macOS: the shell tree plus WebKit XPC services (com.apple.WebKit.*) that
//   appeared after the launch: they are children of launchd, not of the app.
//   Reliable only when no other WebKit app starts meanwhile (e.g. a CI runner).
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const exe = path.join(root, 'target', 'release',
  `castle-pdf-desktop${process.platform === 'win32' ? '.exe' : ''}`);
const runs = Number.parseInt(process.argv[2] ?? '10', 10);
const settleSeconds = Number.parseInt(process.argv[3] ?? '5', 10);
const pdf = process.argv[4] ? path.resolve(process.argv[4]) : null;
const MiB = 1024 * 1024;
const WORKER = /^pdf-worker/;
const MACOS_WEBKIT = /^com\.apple\.WebKit\./;

if (!existsSync(exe)) {
  console.error(`missing ${exe}: build the release app first`);
  process.exit(1);
}
if (pdf !== null && !existsSync(pdf)) {
  console.error(`missing ${pdf}`);
  process.exit(1);
}
if (!(runs > 0 && settleSeconds > 0)) {
  console.error('runs and settleSeconds must be positive integers');
  process.exit(1);
}

// Linux only: proportional and private memory from smaps_rollup, in bytes.
function linuxDetails(pid) {
  try {
    const text = readFileSync(`/proc/${pid}/smaps_rollup`, 'utf8');
    const kb = key => Number(new RegExp(`^${key}:\\s+(\\d+) kB`, 'm').exec(text)?.[1] ?? 0) * 1024;
    return { pss: kb('Pss'), priv: kb('Private_Clean') + kb('Private_Dirty') };
  } catch {
    return { pss: null, priv: null };
  }
}

// Returns [{ pid, ppid, name, rss, priv, pss }] in bytes; null when unknown.
function processTable() {
  if (process.platform === 'win32') {
    const script = 'Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,'
      + 'Name,WorkingSetSize,PrivatePageCount | ConvertTo-Json -Compress';
    const out = execFileSync('powershell.exe', ['-NoProfile', '-Command', script],
      { encoding: 'utf8', maxBuffer: 64 * MiB });
    return JSON.parse(out).map(p => ({
      pid: p.ProcessId, ppid: p.ParentProcessId, name: p.Name,
      rss: Number(p.WorkingSetSize), priv: Number(p.PrivatePageCount), pss: null,
    }));
  }
  const out = execFileSync('ps', ['-A', '-o', 'pid=,ppid=,rss=,comm='],
    { encoding: 'utf8', maxBuffer: 64 * MiB });
  return out.trim().split('\n').map(line => {
    const [pid, ppid, rss, ...name] = line.trim().split(/\s+/);
    return { pid: Number(pid), ppid: Number(ppid), name: path.basename(name.join(' ')),
      rss: Number(rss) * 1024, priv: null, pss: null };
  });
}

function descendants(table, rootPid) {
  const ids = new Set([rootPid]);
  for (let grew = true; grew;) {
    grew = false;
    for (const p of table) {
      if (!ids.has(p.pid) && ids.has(p.ppid)) { ids.add(p.pid); grew = true; }
    }
  }
  return ids;
}

function appProcesses(table, rootPid, webkitBefore) {
  const ids = descendants(table, rootPid);
  return table.filter(p => ids.has(p.pid)
    || (process.platform === 'darwin' && MACOS_WEBKIT.test(p.name) && !webkitBefore.has(p.pid)))
    .map(p => (process.platform === 'linux' ? { ...p, ...linuxDetails(p.pid) } : p));
}

function stop(child) {
  if (process.platform === 'win32') {
    // Without /F and /T: WM_CLOSE to the main window only, so the app shuts
    // its worker and WebView down itself.
    try { execFileSync('taskkill', ['/PID', String(child.pid)], { stdio: 'ignore' }); }
    catch { /* already gone */ }
  } else {
    // The worker then sees the end of its input and exits by itself.
    child.kill('SIGTERM');
  }
}

const median = values => {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
};
const fmt = bytes => (bytes / MiB).toFixed(1);

const samples = [];
for (let i = 0; i < runs; i++) {
  const webkitBefore = new Set(processTable()
    .filter(p => MACOS_WEBKIT.test(p.name)).map(p => p.pid));
  const child = spawn(exe, pdf === null ? [] : [pdf], { stdio: 'ignore' });
  await sleep(settleSeconds * 1000);
  if (child.exitCode !== null) {
    console.error(`run ${i + 1}: app exited early with code ${child.exitCode}`);
    process.exit(1);
  }
  const procs = appProcesses(processTable(), child.pid, webkitBefore);
  const isShell = p => p.pid === child.pid;
  const isWorker = p => WORKER.test(p.name);
  const isWebView = p => !isShell(p) && !isWorker(p);
  const sum = (filter, key) => {
    const values = procs.filter(filter).map(p => p[key]);
    return values.includes(null) ? null : values.reduce((n, v) => n + v, 0);
  };
  samples.push({
    processes: procs.length,
    shell: sum(isShell, 'rss'), worker: sum(isWorker, 'rss'), webview: sum(isWebView, 'rss'),
    total: sum(() => true, 'rss'),
    privateTotal: sum(() => true, 'priv'),
    pssTotal: sum(() => true, 'pss'),
    workerFound: procs.some(isWorker),
  });
  const workerPids = new Set(procs.filter(isWorker).map(p => p.pid));
  stop(child);
  const exited = await Promise.race([
    new Promise(resolve => child.once('exit', () => resolve(true))),
    sleep(10_000).then(() => false),
  ]);
  if (!exited) {
    console.error(`run ${i + 1}: app did not exit after close request`);
    process.exit(1);
  }
  await sleep(1000);
  if (processTable().some(p => workerPids.has(p.pid) && WORKER.test(p.name))) {
    console.error(`run ${i + 1}: pdf-worker survived the application`);
    process.exit(1);
  }
}

if (samples.some(s => !s.workerFound)) {
  console.error('pdf-worker was not found in at least one run');
  process.exit(1);
}

const resident = process.platform === 'win32' ? 'working set' : 'RSS';
const rows = [
  [`Shell (${resident})`, 'shell'],
  [`Worker (${resident})`, 'worker'],
  [`WebView (${resident})`, 'webview'],
  [`Total (${resident}, shared pages counted repeatedly)`, 'total'],
  ['Total private bytes', 'privateTotal'],
  ['Total PSS', 'pssTotal'],
].filter(([, key]) => samples.every(s => s[key] !== null));

console.log(`Platform: ${os.type()} ${os.release()} ${os.arch()}, `
  + `${os.cpus()[0]?.model ?? 'CPU?'}, ${fmt(os.totalmem())} MiB RAM, Node ${process.version}`);
console.log(`Document: ${pdf === null ? 'none (idle)' : path.basename(pdf)}`);
console.log(`Runs: ${runs}, settle: ${settleSeconds} s after start, processes per run: `
  + `${median(samples.map(s => s.processes))}`);
console.log('');
console.log('| Measure | Median (MiB) | Min | Max |');
console.log('| --- | --- | --- | --- |');
for (const [label, key] of rows) {
  const values = samples.map(s => s[key]);
  console.log(`| ${label} | ${fmt(median(values))} | ${fmt(Math.min(...values))} | `
    + `${fmt(Math.max(...values))} |`);
}
