#!/usr/bin/env node
// 用构建产物里的 web 资源渲染截图。
//
// 输入：prepare-frontend.ps1 暂存的 frontend-<version>-<hash>/web（逐字节来自 tauri frontendDist）。
// 做法：headless Chrome 打开真实的 index.html + app.js + styles.css，注入最小 Tauri IPC stub
// 与固定 fixture 状态；不启动桌面应用、不读写用户配置。
//
// 用法：
//   node capture.mjs --list
//   node capture.mjs --assets .screenshots/.build/frontend-0.3.2-abcd1234
//   node capture.mjs --assets ... --state panel-3 --state settings-bottom
//
// 产物：.screenshots/build/bead-<state>.png + 同名 .json + index.json

import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn, execFileSync } from 'node:child_process';
import { STATES, NOT_CAPTURABLE } from './fixtures.mjs';

const HERE = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const REPO_ROOT = path.resolve(HERE, '..', '..', '..');
const SAMPLE_PNG = path.resolve(HERE, '..', 'assets', 'ocr-sample.png');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const fail = (message) => {
  console.error(message);
  process.exit(1);
};

function parseArgs(argv) {
  const options = { states: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (token === '--all') continue;
    if (token === '--list') { options.list = true; continue; }
    const next = argv[i + 1];
    const take = () => {
      if (next === undefined || next.startsWith('--')) fail(`${token} 缺少值`);
      i += 1;
      return next;
    };
    if (token === '--assets') options.assets = take();
    else if (token === '--out') options.out = take();
    else if (token === '--state') options.states.push(take());
    else fail(`未知参数：${token}`);
  }
  return options;
}

function findAssets() {
  const buildRoot = path.join(REPO_ROOT, '.screenshots', '.build');
  if (!fs.existsSync(buildRoot)) {
    fail('还没有暂存资源。先运行 prepare-frontend.ps1。');
  }
  const candidates = fs
    .readdirSync(buildRoot, { withFileTypes: true })
    .filter((item) => item.isDirectory() && item.name.startsWith('frontend-'))
    .map((item) => path.join(buildRoot, item.name))
    .filter((dir) => fs.existsSync(path.join(dir, 'manifest.json')))
    .sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs);
  if (!candidates.length) fail('没有带 manifest.json 的暂存目录，先运行 prepare-frontend.ps1。');
  return candidates[0];
}

function hashTree(webDir) {
  const entries = [];
  const walk = (dir) => {
    for (const item of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const full = path.join(dir, item.name);
      if (item.isDirectory()) walk(full);
      else {
        const relative = path.relative(webDir, full).split(path.sep).join('/');
        const sha256 = crypto.createHash('sha256').update(fs.readFileSync(full)).digest('hex');
        entries.push({ path: `web/${relative}`, sha256 });
      }
    }
  };
  walk(webDir);
  entries.sort((a, b) => a.path.localeCompare(b.path));
  const text = entries.map((entry) => `${entry.path}\0${entry.sha256}\n`).join('');
  return crypto.createHash('sha256').update(text, 'utf8').digest('hex');
}

/** #rrggbb → CDP RGBA；应用本身是透明窗口，背板只服务于截图可读性。 */
function hexToRgba(hex) {
  const value = hex.replace('#', '');
  return {
    r: parseInt(value.slice(0, 2), 16),
    g: parseInt(value.slice(2, 4), 16),
    b: parseInt(value.slice(4, 6), 16),
    a: 1,
  };
}

function mimeType(file) {
  return {
    '.html': 'text/html; charset=utf-8',
    '.js': 'text/javascript; charset=utf-8',
    '.mjs': 'text/javascript; charset=utf-8',
    '.css': 'text/css; charset=utf-8',
    '.json': 'application/json; charset=utf-8',
    '.png': 'image/png',
    '.jpg': 'image/jpeg',
    '.jpeg': 'image/jpeg',
    '.svg': 'image/svg+xml',
    '.ico': 'image/x-icon',
    '.woff': 'font/woff',
    '.woff2': 'font/woff2',
  }[path.extname(file).toLowerCase()] ?? 'application/octet-stream';
}

function startServer(root) {
  const server = http.createServer((req, res) => {
    const url = decodeURIComponent((req.url ?? '/').split('?')[0]);
    const relative = url === '/' ? 'index.html' : url.replace(/^\/+/, '');
    const full = path.resolve(root, relative);
    if (!full.startsWith(path.resolve(root) + path.sep)) {
      res.writeHead(403); res.end('forbidden'); return;
    }
    fs.readFile(full, (error, data) => {
      if (error) { res.writeHead(404); res.end('not found'); return; }
      res.writeHead(200, { 'Content-Type': mimeType(full), 'Cache-Control': 'no-store' });
      res.end(data);
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, port: server.address().port }));
  });
}

function findBrowser() {
  const candidates = [
    process.env.BEAD_CHROME,
    'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
    'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
    'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
    'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe',
  ].filter(Boolean);
  for (const candidate of candidates) if (fs.existsSync(candidate)) return candidate;
  for (const name of ['chrome.exe', 'msedge.exe']) {
    try {
      const found = execFileSync('where.exe', [name], { encoding: 'utf8' }).split(/\r?\n/)[0].trim();
      if (found && fs.existsSync(found)) return found;
    } catch {
      // 继续找下一个
    }
  }
  fail('找不到 Chrome/Edge。设置 BEAD_CHROME 指向浏览器可执行文件。');
}

function launchBrowser() {
  const exe = findBrowser();
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'bead-shot-'));
  const child = spawn(exe, [
    '--headless=new',
    '--disable-gpu',
    '--hide-scrollbars',
    '--mute-audio',
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-extensions',
    '--force-device-scale-factor=1',
    `--user-data-dir=${profile}`,
    '--remote-debugging-port=0',
    'about:blank',
  ], { stdio: 'ignore', windowsHide: true });
  return { exe, profile, child };
}

async function waitForPort(profile) {
  const portFile = path.join(profile, 'DevToolsActivePort');
  for (let i = 0; i < 100; i += 1) {
    if (fs.existsSync(portFile)) {
      const [port] = fs.readFileSync(portFile, 'utf8').split(/\r?\n/);
      if (port) return Number(port);
    }
    await sleep(100);
  }
  throw new Error('浏览器没有打开 DevTools 端口');
}

class Cdp {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 0;
    this.pending = new Map();
    socket.addEventListener('message', (event) => {
      const message = JSON.parse(event.data);
      if (!message.id || !this.pending.has(message.id)) return;
      const { resolve, reject, timer } = this.pending.get(message.id);
      this.pending.delete(message.id);
      clearTimeout(timer);
      if (message.error) reject(new Error(message.error.message));
      else resolve(message.result);
    });
  }

  static async connect(url) {
    const socket = new WebSocket(url);
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('WebSocket 超时')), 5000);
      socket.addEventListener('open', () => { clearTimeout(timer); resolve(); }, { once: true });
      socket.addEventListener('error', () => { clearTimeout(timer); reject(new Error('WebSocket 失败')); }, { once: true });
    });
    return new Cdp(socket);
  }

  send(method, params = {}) {
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} 超时`));
      }, 15000);
      this.pending.set(id, { resolve, reject, timer });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }
}

function stubSource(state) {
  const fixture = {
    view: state.view,
    ocrMode: state.ocrMode ?? 'text',
  };
  return `(() => {
  const fixture = ${JSON.stringify(fixture)};
  const listeners = new Map();
  const clone = (value) => value === undefined ? value : JSON.parse(JSON.stringify(value));
  const invoke = async (command) => {
    if (command === 'get_state') return clone(fixture.view);
    if (command === 'list_history') return clone(fixture.view.historyItems ?? []);
    if (command === 'speech_capabilities') return {};
    if (command === 'panel_frame') {
      return { x: 0, y: 0, width: window.innerWidth, height: window.innerHeight };
    }
    if (command === 'ocr_clipboard_image_for_lang') {
      if (fixture.ocrMode === 'pending') return new Promise(() => {});
      if (fixture.ocrMode === 'fail') throw new Error('fixture OCR failure');
      return fixture.ocrMode === 'empty' ? '' : 'fixture OCR text';
    }
    return clone(fixture.view);
  };
  window.__TAURI__ = {
    core: { invoke, convertFileSrc: (value) => value },
    event: {
      listen: async (name, callback) => {
        listeners.set(name, callback);
        return () => {};
      },
    },
    window: { WebviewWindow: class {} },
  };
  window.__emitBeadState = (view) => {
    const callback = listeners.get('state');
    if (callback) callback({ payload: view });
  };
  window.__emitBeadOcrProgress = (payload) => {
    const callback = listeners.get('ocr-progress');
    if (callback) callback({ payload });
  };
})();`;
}

async function evaluate(cdp, expression) {
  const result = await cdp.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
  return result.result.value;
}

async function waitForView(cdp, expected, timeoutMs = 6000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const state = await evaluate(cdp, `(() => ({
      ready: document.readyState === 'complete',
      stubbed: Boolean(window.__emitBeadState),
      view: document.getElementById('root')?.dataset.view ?? null,
      board: Boolean(document.getElementById('board')),
    }))()`);
    // readyState 与 stub 都就绪，避免把 index.html 里静态的 data-view="capsule"
    // 当成 app.js 已完成首帧；展开态还要求 #board 真的渲染出来。
    if (state.ready && state.stubbed && state.view === expected
      && (expected !== 'expanded' || state.board)) return;
    await sleep(100);
  }
  throw new Error(`等待视图超时：${expected}`);
}

async function runAction(cdp, action, sampleBase64) {
  if (action.type === 'click') {
    const ok = await evaluate(cdp, `(() => {
      const el = document.querySelector(${JSON.stringify(action.selector)});
      if (!el) return false;
      el.click();
      return true;
    })()`);
    if (!ok) throw new Error(`click 目标不存在：${action.selector}`);
    await sleep(action.waitMs ?? 400);
    return;
  }
  if (action.type === 'scroll') {
    await evaluate(cdp, `(() => {
      const el = document.querySelector(${JSON.stringify(action.selector)});
      if (!el) return false;
      el.scrollTop = ${action.position === 'bottom' ? 'el.scrollHeight' : '0'};
      return true;
    })()`);
    await sleep(250);
    return;
  }
  if (action.type === 'hover') {
    const rect = await evaluate(cdp, `(() => {
      const el = document.querySelector(${JSON.stringify(action.selector)});
      if (!el) return null;
      const r = el.getBoundingClientRect();
      return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
    })()`);
    if (!rect) throw new Error(`hover 目标不存在：${action.selector}`);
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: rect.x, y: rect.y, button: 'none' });
    await sleep(350);
    return;
  }
  if (action.type === 'ocrProgress') {
    await evaluate(cdp, `window.__emitBeadOcrProgress(${JSON.stringify(action.payload)})`);
    await sleep(250);
    return;
  }
  if (action.type === 'paste') {
    await evaluate(cdp, `(() => {
      const el = document.querySelector('textarea.card__text[data-lang=${JSON.stringify(action.lang)}]');
      if (!el) return false;
      const binary = atob(${JSON.stringify(sampleBase64)});
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
      const file = new File([bytes], 'bead-ocr-sample.png', { type: 'image/png' });
      const transfer = new DataTransfer();
      transfer.items.add(file);
      const event = new Event('paste', { bubbles: true, cancelable: true });
      Object.defineProperty(event, 'clipboardData', { value: transfer });
      el.dispatchEvent(event);
      return true;
    })()`);
    await sleep(action.waitMs ?? 500);
    return;
  }
  throw new Error(`未知动作：${action.type}`);
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.list) {
    for (const state of STATES) console.log(`${state.name}\t${state.width}x${state.height}\t${state.title}`);
    for (const item of NOT_CAPTURABLE) console.log(`${item.name}\t-\t${item.title}（不出图：${item.reason}）`);
    return;
  }

  const assetsRoot = options.assets ? path.resolve(options.assets) : findAssets();
  const webRoot = path.join(assetsRoot, 'web');
  const manifestPath = path.join(assetsRoot, 'manifest.json');
  if (!fs.existsSync(webRoot) || !fs.existsSync(manifestPath)) fail('--assets 必须指向 prepare-frontend 的暂存目录。');
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  const actualHash = hashTree(webRoot);
  if (actualHash !== manifest.treeHash) {
    fail(`暂存资源已被改动：treeHash ${actualHash} != manifest ${manifest.treeHash}。重新运行 prepare-frontend.ps1。`);
  }

  const outDir = options.out ? path.resolve(options.out) : path.join(REPO_ROOT, '.screenshots', 'build');
  fs.mkdirSync(outDir, { recursive: true });
  const sampleBase64 = fs.readFileSync(SAMPLE_PNG).toString('base64');

  const selected = options.states.length
    ? STATES.filter((state) => options.states.includes(state.name))
    : STATES;
  const unknown = options.states.filter((name) => !STATES.some((state) => state.name === name));
  if (unknown.length) fail(`未知状态：${unknown.join(', ')}（用 --list 查看）`);

  const { server, port } = await startServer(webRoot);
  const browser = launchBrowser();
  let cdp;
  const indexEntries = [];

  try {
    const debugPort = await waitForPort(browser.profile);
    const targetList = await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();
    const page = targetList.find((item) => item.type === 'page');
    if (!page) throw new Error('浏览器没有 page target');
    cdp = await Cdp.connect(page.webSocketDebuggerUrl);
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');

    let scriptId = null;
    for (const state of selected) {
      // 胶囊是半透明白块，纯白背板会看不见；面板保持白底，与实机整板一致。
      const backdrop = state.backdrop ?? (state.width <= 100 ? '#dfe6f0' : '#ffffff');
      if (scriptId) {
        await cdp.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: scriptId }).catch(() => {});
      }
      scriptId = (await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: stubSource(state) })).identifier;

      await cdp.send('Emulation.setDeviceMetricsOverride', {
        width: state.width,
        height: state.height,
        deviceScaleFactor: 1,
        mobile: false,
      });
      await cdp.send('Emulation.setDefaultBackgroundColorOverride', {
        color: hexToRgba(backdrop),
      });
      await cdp.send('Page.navigate', {
        url: `http://127.0.0.1:${port}/index.html?shot=${encodeURIComponent(state.name)}`,
      });

      const expected = state.view.view === 'capsule' ? 'capsule' : 'expanded';
      await waitForView(cdp, expected);
      await sleep(expected === 'capsule' ? 300 : 1500);

      for (const action of state.actions ?? []) {
        await runAction(cdp, action, sampleBase64);
      }
      await sleep(200);

      const shot = await cdp.send('Page.captureScreenshot', {
        format: 'png',
        fromSurface: true,
        clip: { x: 0, y: 0, width: state.width, height: state.height, scale: 1 },
      });
      const file = path.join(outDir, `bead-${state.name}.png`);
      fs.writeFileSync(file, Buffer.from(shot.data, 'base64'));

      const sidecar = {
        file,
        state: state.name,
        title: state.title,
        source: 'built-frontend',
        fixture: true,
        sampleContent: true,
        note: '界面为构建产物渲染；文案为固定 fixture 示例，不是实机翻译结果。',
        trigger: state.trigger,
        viewport: { width: state.width, height: state.height },
        backdrop,
        view: state.view,
        assets: {
          stagedDir: assetsRoot,
          treeHash: manifest.treeHash,
          appVersion: manifest.appVersion,
          fileCount: manifest.fileCount,
          exe: manifest.artifact?.path,
          exeSha256: manifest.artifact?.sha256,
          exeVersion: manifest.artifact?.version,
          gitCommit: manifest.gitCommit,
          gitDirtyWeb: manifest.gitDirtyWeb,
        },
        capturedAt: new Date().toISOString(),
        browser: browser.exe,
      };
      fs.writeFileSync(path.join(outDir, `bead-${state.name}.json`), JSON.stringify(sidecar, null, 2));
      indexEntries.push({
        name: state.name,
        file: path.basename(file),
        sidecar: `bead-${state.name}.json`,
        title: state.title,
        captured: true,
        trigger: state.trigger,
        sampleContent: true,
        viewport: sidecar.viewport,
        backdrop,
      });
      console.log(`saved ${file}`);
    }

    for (const item of NOT_CAPTURABLE) {
      indexEntries.push({ name: item.name, title: item.title, captured: false, reason: item.reason });
    }

    const index = {
      generatedAt: new Date().toISOString(),
      source: 'built-frontend',
      notice: '所有图片由构建产物 web 资源 + headless 渲染生成；译文为固定 fixture 示例，不是实机或真实翻译结果。',
      assets: {
        stagedDir: assetsRoot,
        treeHash: manifest.treeHash,
        appVersion: manifest.appVersion,
        frontendDist: manifest.frontendDist,
        fileCount: manifest.fileCount,
        exe: manifest.artifact?.path,
        exeSha256: manifest.artifact?.sha256,
        exeVersion: manifest.artifact?.version,
        exeMtime: manifest.artifact?.mtime,
        gitCommit: manifest.gitCommit,
        gitDirtyWeb: manifest.gitDirtyWeb,
      },
      states: indexEntries,
    };
    fs.writeFileSync(path.join(outDir, 'index.json'), JSON.stringify(index, null, 2));
    console.log(`index ${path.join(outDir, 'index.json')}`);
  } finally {
    if (cdp) {
      await cdp.send('Browser.close').catch(() => {});
      try { cdp.socket.close(); } catch { /* 忽略 */ }
    }
    try { browser.child.kill(); } catch { /* 忽略 */ }
    server.close();
    await sleep(300);
    try { fs.rmSync(browser.profile, { recursive: true, force: true }); } catch { /* 忽略 */ }
  }
}

main().catch((error) => fail(error.stack ?? String(error)));
