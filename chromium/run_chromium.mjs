#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import os from "node:os";
import { spawn } from "node:child_process";

const ROOT = path.resolve(import.meta.dirname, "..");
const DATA_JSON = path.join(ROOT, "data", "chat.json");
const DATA_JS = path.join(ROOT, "data", "chat-data.js");
const USE_GPU = process.argv.includes("--gpu") || process.env.XB_GPU === "1";

function findChrome() {
  if (process.env.CHROME_BIN) return process.env.CHROME_BIN;
  const candidates = process.platform === "darwin"
    ? [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
      ]
    : ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"];
  for (const c of candidates) {
    if (c.startsWith("/")) {
      if (fs.existsSync(c)) return c;
    } else {
      return c;
    }
  }
  return "google-chrome";
}

fs.writeFileSync(DATA_JS, "window.CHAT_DATA = " + fs.readFileSync(DATA_JSON, "utf8") + ";\n");

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".ttf": "font/ttf",
};

const server = http.createServer((req, res) => {
  const urlPath = (req.url || "/").split("?")[0];
  const filePath = path.join(ROOT, urlPath.replace(/^\/+/, ""));
  if (!filePath.startsWith(ROOT) || !fs.existsSync(filePath) || fs.statSync(filePath).isDirectory()) {
    res.writeHead(404);
    res.end("not found");
    return;
  }
  const ext = path.extname(filePath);
  res.writeHead(200, { "Content-Type": MIME[ext] || "application/octet-stream", "Cache-Control": "no-store" });
  fs.createReadStream(filePath).pipe(res);
});

await new Promise((r) => server.listen(0, "127.0.0.1", r));
const port = server.address().port;
const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "xb-chrome-"));
const traceFile = path.join(tmpDir, "trace.json");
const debugPort = 9222 + Math.floor(Math.random() * 500);

const TRACE_CATS = [
  "blink",
  "blink.user_timing",
  "fonts",
  "disabled-by-default-blink.debug.layout",
  "cc",
  "skia",
  "v8",
  "toplevel",
  "devtools.timeline",
  "loading",
  "navigation",
  "browser",
  "startup",
  "gpu",
].join(",");

const args = [
  "--headless=new",
  "--no-sandbox",
  ...(USE_GPU ? [] : ["--disable-gpu", "--disable-gpu-compositing"]),
  "--disable-frame-rate-limit",
  "--disable-gpu-vsync",
  "--disable-extensions",
  "--disable-background-networking",
  "--disable-sync",
  "--metrics-recording-only",
  "--no-first-run",
  "--window-size=1040,720",
  "--force-device-scale-factor=1",
  `--remote-debugging-port=${debugPort}`,
  `--user-data-dir=${path.join(tmpDir, "profile")}`,
  `--trace-startup=${TRACE_CATS}`,
  "--trace-startup-format=json",
  `--trace-startup-file=${traceFile}`,
  "--trace-startup-duration=6",
  `http://127.0.0.1:${port}/chromium/index.html`,
];

const chromeBin = findChrome();
const t0 = performance.now();
const child = spawn(chromeBin, args, { stdio: ["ignore", "pipe", "pipe"] });
let stderr = "";
child.stderr.on("data", (d) => { stderr += d.toString(); });

let doneAt = null;
const deadline = Date.now() + 25000;
while (Date.now() < deadline) {
  await new Promise((r) => setTimeout(r, 100));
  try {
    const resp = await fetch(`http://127.0.0.1:${debugPort}/json`);
    const pages = await resp.json();
    const page = pages.find((p) => p.type === "page" && p.url.includes("index.html"));
    if (page && page.title === "XB_DONE") {
      doneAt = performance.now() - t0;
      break;
    }
  } catch {}
}

if (!doneAt) {
  console.error("Timed out waiting for XB_DONE. stderr:", stderr.slice(-1000));
  child.kill("SIGKILL");
  server.close();
  process.exit(1);
}

const waitTraceUntil = Date.now() + 12000;
let lastSize = -1;
while (Date.now() < waitTraceUntil) {
  await new Promise((r) => setTimeout(r, 250));
  if (fs.existsSync(traceFile)) {
    const sz = fs.statSync(traceFile).size;
    if (sz > 1000 && sz === lastSize) break;
    lastSize = sz;
  }
}

try {
  const v = await (await fetch(`http://127.0.0.1:${debugPort}/json/version`)).json();
  const ws = new WebSocket(v.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.onopen = () => { ws.send(JSON.stringify({ id: 1, method: "Browser.close" })); setTimeout(res, 300); };
    ws.onerror = rej;
  });
  ws.close();
} catch {
  child.kill("SIGTERM");
}

const code = await new Promise((r) => child.on("close", r));
const wallMs = performance.now() - t0;
server.close();

if (!fs.existsSync(traceFile)) {
  console.error("No trace file written! code:", code, "stderr:", stderr.slice(-1000));
  process.exit(1);
}

const outTrace = path.join(ROOT, "results", "chromium-trace.json");
fs.mkdirSync(path.dirname(outTrace), { recursive: true });
fs.copyFileSync(traceFile, outTrace);
fs.rmSync(tmpDir, { recursive: true, force: true });

console.log(JSON.stringify({ ok: true, gpu: USE_GPU, code, doneAtMs: +doneAt.toFixed(1), wallMs: +wallMs.toFixed(1), traceBytes: fs.statSync(outTrace).size }));
