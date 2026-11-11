#!/usr/bin/env node
// Launches the release desktop app on a PDF, waits for it to render, captures
// the screen to a PNG, then closes the app: a check of what the app really
// shows on an OS nobody is looking at (CI runners). Not a pixel comparison:
// the images are for a person to review.
// Usage: node scripts/screenshot-app.mjs <file.pdf> <out.png> [settleSeconds=10]
// Requires the release app (`npm --prefix apps/desktop run tauri -- build
// --no-bundle -- --locked`). On Linux, run under a display (xvfb-run) with
// ImageMagick's `import`; on macOS, `screencapture`; on Windows, PowerShell.
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, statSync } from 'node:fs';
import path from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const exe = path.join(root, 'target', 'release',
  `castle-pdf-desktop${process.platform === 'win32' ? '.exe' : ''}`);
const [pdfArg, outArg, settleArg] = process.argv.slice(2);
if (!pdfArg || !outArg) {
  console.error('usage: screenshot-app.mjs <file.pdf> <out.png> [settleSeconds=10]');
  process.exit(2);
}
const pdf = path.resolve(pdfArg);
const out = path.resolve(outArg);
const settleSeconds = Number.parseInt(settleArg ?? '10', 10);
for (const [what, file] of [['app', exe], ['document', pdf]]) {
  if (!existsSync(file)) {
    console.error(`missing ${what}: ${file}`);
    process.exit(1);
  }
}

function capture() {
  switch (process.platform) {
    case 'linux':
      execFileSync('import', ['-window', 'root', out]);
      break;
    case 'darwin':
      execFileSync('screencapture', ['-x', out]);
      break;
    case 'win32': {
      const script = [
        'Add-Type -AssemblyName System.Windows.Forms, System.Drawing',
        '$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds',
        '$img = New-Object System.Drawing.Bitmap $b.Width, $b.Height',
        '$g = [System.Drawing.Graphics]::FromImage($img)',
        '$g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)',
        `$img.Save('${out.replace(/'/g, "''")}', [System.Drawing.Imaging.ImageFormat]::Png)`,
      ].join('; ');
      execFileSync('powershell.exe', ['-NoProfile', '-Command', script]);
      break;
    }
    default:
      throw new Error(`no capture on ${process.platform}`);
  }
}

function stop(child) {
  if (process.platform === 'win32') {
    // WM_CLOSE to the main window: the app shuts its worker down itself.
    try { execFileSync('taskkill', ['/PID', String(child.pid)], { stdio: 'ignore' }); }
    catch { /* already gone */ }
  } else {
    child.kill('SIGTERM');
  }
}

const child = spawn(exe, [pdf], { stdio: 'ignore' });
await sleep(settleSeconds * 1000);
if (child.exitCode !== null) {
  console.error(`the app exited early with code ${child.exitCode}`);
  process.exit(1);
}
try {
  capture();
} finally {
  stop(child);
}
await Promise.race([
  new Promise(resolve => child.once('exit', resolve)),
  sleep(10_000),
]);
const size = existsSync(out) ? statSync(out).size : 0;
if (size === 0) {
  console.error(`no screenshot written to ${out}`);
  process.exit(1);
}
console.log(`${path.basename(pdf)}: ${out} (${size} bytes)`);
