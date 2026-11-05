// Glasscode Voice — the shared control plane for Glasscode.
// Wraps the `glasscode` CLI (JSON) so any voice surface — the in-IDE helper and the
// Meta glasses — can read live sessions and write prompts into them, and mints the
// ElevenLabs conversation token so the API key never leaves this host.
//
// Reachability + lock: binds on all interfaces so the glasses can reach it over the
// LAN, but loopback (the local IDE helper, the web UI) is trusted with no token while
// any off-host request must carry the shared bearer token (`.glasscode-token`). This matters
// because `send_prompt` types straight into your sessions.
import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { readFileSync, writeFileSync, chmodSync } from 'node:fs';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import * as memory from './memory.mjs';

const execFileP = promisify(execFile);
const DIR = path.dirname(fileURLToPath(import.meta.url));
const PUBLIC = path.join(DIR, 'public');
const CLI = process.env.GLASSCODE_BIN || path.join(process.env.HOME || '', '.local/bin/glasscode');
const PORT = Number(process.env.PORT) || 4517;
const EL_KEY = process.env.ELEVENLABS_API_KEY || '';
const EL_API = 'https://api.elevenlabs.io';

// The shared bearer token off-host surfaces (the glasses) must present. Loopback skips it.
// Order: $LUNA_TOKEN → .glasscode-token file → freshly generated and written (chmod 600).
const TOKEN_FILE = path.join(DIR, '.glasscode-token');
function loadToken() {
  if (process.env.LUNA_TOKEN) return process.env.LUNA_TOKEN.trim();
  try { const t = readFileSync(TOKEN_FILE, 'utf8').trim(); if (t) return t; } catch {}
  const t = crypto.randomBytes(24).toString('base64url');
  try { writeFileSync(TOKEN_FILE, t + '\n', { mode: 0o600 }); chmodSync(TOKEN_FILE, 0o600); } catch {}
  return t;
}
const TOKEN = loadToken();

function isLoopback(req) {
  const a = req.socket.remoteAddress || '';
  return a === '127.0.0.1' || a === '::1' || a === '::ffff:127.0.0.1';
}
// A request is allowed when it is loopback, or presents the token (Bearer header or ?token=).
function authed(req, u) {
  if (isLoopback(req)) return true;
  const h = req.headers['authorization'] || '';
  const bearer = h.startsWith('Bearer ') ? h.slice(7).trim() : '';
  const qp = u.searchParams.get('token') || '';
  return bearer === TOKEN || qp === TOKEN;
}
function lanIPs() {
  const out = [];
  for (const list of Object.values(os.networkInterfaces())) {
    for (const ni of list || []) if (ni.family === 'IPv4' && !ni.internal) out.push(ni.address);
  }
  return out;
}

// voice.config.json → { agentId, connectionType }
let VOICE = { agentId: '', connectionType: 'webrtc' };
try { VOICE = { ...VOICE, ...JSON.parse(await readFile(path.join(DIR, 'voice.config.json'), 'utf8')) }; } catch {}

// in-memory "which session am I in" — the voice agent's current context
let currentSession = null;

// Shared voice state, so any surface (IDE panel, glasses) can control and mirror the one
// live session: `intentLive` is what a controller asks for; `live`/`mode`/`caption` are what
// the voice runtime (the browser app) reports back.
let voice = { intentLive: false, live: false, mode: 'idle', caption: '', suggest: '', ts: Date.now() };

// Debounced memory sweep: after a session ends, give ElevenLabs ~10s to finalize the
// transcript summary, then fold new conversations into Glasscode's memory.
let ingestTimer = null;
function scheduleIngest(delay = 10000) {
  if (ingestTimer) clearTimeout(ingestTimer);
  ingestTimer = setTimeout(async () => {
    ingestTimer = null;
    const r = await memory.ingest(EL_KEY, VOICE.agentId);
    if (r.added) console.log(`[glasscode-memory] folded in ${r.added} new session(s); ${r.total} known`);
  }, delay);
}

const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.json': 'application/json', '.ico': 'image/x-icon' };

function send(res, code, body, type = 'application/json') {
  const payload = typeof body === 'string' || Buffer.isBuffer(body) ? body : JSON.stringify(body);
  res.writeHead(code, { 'content-type': type, 'cache-control': 'no-store' });
  res.end(payload);
}

function readBody(req) {
  return new Promise((resolve) => {
    let data = '';
    req.on('data', (c) => (data += c));
    req.on('end', () => { try { resolve(data ? JSON.parse(data) : {}); } catch { resolve({}); } });
  });
}

async function cli(args) {
  const { stdout } = await execFileP(CLI, args, { maxBuffer: 16 * 1024 * 1024 });
  return stdout;
}
async function cliJson(args) {
  return JSON.parse(await cli([...args, '--json']));
}

async function serveStatic(res, url) {
  const rel = url === '/' ? '/index.html' : url;
  const file = path.join(PUBLIC, path.normalize(rel));
  if (!file.startsWith(PUBLIC)) return send(res, 403, { error: 'forbidden' });
  try {
    const data = await readFile(file);
    send(res, 200, data, TYPES[path.extname(file)] || 'application/octet-stream');
  } catch {
    send(res, 404, { error: 'not found' });
  }
}

const server = http.createServer(async (req, res) => {
  const u = new URL(req.url, 'http://localhost');
  const seg = u.pathname.split('/').filter(Boolean); // e.g. ['api','sessions','<id>','send']
  try {
    // Lock the tool/data plane for off-host callers. Health stays open (reachability probe).
    if (seg[0] === 'api' && u.pathname !== '/api/health' && !authed(req, u)) {
      return send(res, 401, { error: 'unauthorized', hint: 'present the Glasscode token (Bearer or ?token=)' });
    }
    // GET /api/sessions  → live session list + current context
    if (u.pathname === '/api/sessions' && req.method === 'GET') {
      const args = ['session', 'list'];
      if (u.searchParams.get('needs')) args.push('--needs-you');
      const proj = u.searchParams.get('project');
      if (proj) args.push('--project', proj);
      const sessions = await cliJson(args);
      return send(res, 200, { sessions, current: currentSession });
    }
    // POST /api/current  {id}  → set the session the assistant is "in"
    if (u.pathname === '/api/current' && req.method === 'POST') {
      const body = await readBody(req);
      currentSession = body.id || null;
      return send(res, 200, { current: currentSession });
    }
    // GET /api/sessions/:id  → one session's details
    if (seg[0] === 'api' && seg[1] === 'sessions' && seg[2] && !seg[3] && req.method === 'GET') {
      return send(res, 200, await cliJson(['session', 'show', seg[2]]));
    }
    // GET /api/sessions/:id/transcript?last=N
    if (seg[0] === 'api' && seg[1] === 'sessions' && seg[3] === 'transcript' && req.method === 'GET') {
      const last = u.searchParams.get('last') || '6';
      const text = await cli(['session', 'read', seg[2], '--last', String(last)]);
      return send(res, 200, { text });
    }
    // POST /api/sessions/:id/send  {message}  → type a prompt into the session
    if (seg[0] === 'api' && seg[1] === 'sessions' && seg[3] === 'send' && req.method === 'POST') {
      const body = await readBody(req);
      await cli(['session', 'send', seg[2], String(body.message || '')]);
      return send(res, 200, { ok: true });
    }
    // GET /api/config  → what the browser needs to start a voice session
    if (u.pathname === '/api/config' && req.method === 'GET') {
      return send(res, 200, {
        agentId: VOICE.agentId || '',
        connectionType: VOICE.connectionType || 'webrtc',
        authMode: EL_KEY ? 'token' : 'public', // 'token' = private agent via server; 'public' = connect with agentId only
        ready: Boolean(VOICE.agentId),
      });
    }
    // POST /api/voice/token  → mint a WebRTC conversation token (keeps the API key server-side)
    if (u.pathname === '/api/voice/token' && req.method === 'POST') {
      if (!EL_KEY) return send(res, 200, { token: null, authMode: 'public' });
      if (!VOICE.agentId) return send(res, 400, { error: 'no agentId configured' });
      const r = await fetch(`${EL_API}/v1/convai/conversation/token?agent_id=${encodeURIComponent(VOICE.agentId)}`, {
        headers: { 'xi-api-key': EL_KEY },
      });
      if (!r.ok) return send(res, r.status, { error: `elevenlabs ${r.status}: ${await r.text()}` });
      const data = await r.json();
      return send(res, 200, { token: data.token, authMode: 'token' });
    }
    // GET /api/voice/state  → the shared voice state (controllers read; everyone mirrors)
    if (u.pathname === '/api/voice/state' && req.method === 'GET') {
      return send(res, 200, { ...voice, current: currentSession });
    }
    // POST /api/voice/state  {intentLive?, live?, mode?, caption?}  → merge in a change
    if (u.pathname === '/api/voice/state' && req.method === 'POST') {
      const b = await readBody(req);
      const wasLive = voice.live;
      for (const k of ['intentLive', 'live', 'mode', 'caption', 'suggest', 'view']) if (k in b) voice[k] = b[k];
      // Stamp the glasses snapshot with *server* time (same clock as the IDE), so the mirror
      // can tell a live view from one left behind after the glasses went away.
      if (b.view && voice.view && typeof voice.view === 'object') voice.view.ts = Date.now();
      voice.ts = Date.now();
      // When a session ends, fold its (now-finalized) summary into Glasscode's memory. ElevenLabs
      // needs a moment to write the summary, so wait a beat, then sweep — debounced.
      if ((b.live === false && wasLive) || b.intentLive === false) scheduleIngest();
      return send(res, 200, { ok: true, voice });
    }
    // ---- Glasscode's memory: context at session start, recall, and notes ----
    if (u.pathname === '/api/memory/context' && req.method === 'GET') {
      return send(res, 200, memory.context());
    }
    if (u.pathname === '/api/memory/recall' && req.method === 'POST') {
      const b = await readBody(req);
      return send(res, 200, memory.recall(String(b.query || ''), { max: Number(b.max) || 6 }));
    }
    if (u.pathname === '/api/memory/note' && req.method === 'POST') {
      const b = await readBody(req);
      return send(res, 200, memory.note(String(b.text || '')));
    }
    if (u.pathname === '/api/memory/ingest' && req.method === 'POST') {
      return send(res, 200, await memory.ingest(EL_KEY, VOICE.agentId));
    }
    // ---- structure: projects & worktrees ----
    if (u.pathname === '/api/projects' && req.method === 'GET') {
      return send(res, 200, { projects: await cliJson(['project', 'list']) });
    }
    if (u.pathname === '/api/worktrees' && req.method === 'GET') {
      const args = ['worktree', 'list'];
      const p = u.searchParams.get('project');
      if (p) args.push('--project', p);
      return send(res, 200, { worktrees: await cliJson(args) });
    }
    // ---- act on a session: open (the "click") and rename ----
    if (seg[0] === 'api' && seg[1] === 'sessions' && seg[3] === 'open' && req.method === 'POST') {
      await cli(['session', 'open', seg[2]]);
      currentSession = seg[2];
      return send(res, 200, { ok: true });
    }
    if (seg[0] === 'api' && seg[1] === 'sessions' && seg[3] === 'rename' && req.method === 'POST') {
      const body = await readBody(req);
      await cli(['session', 'rename', seg[2], String(body.name || '')]);
      return send(res, 200, { ok: true });
    }
    // ---- start a new session (project/worktree/ticket/pr/review/message) ----
    if (u.pathname === '/api/sessions/new' && req.method === 'POST') {
      const b = await readBody(req);
      const args = ['session', 'new'];
      const opt = (flag, key) => { if (b[key] !== undefined && b[key] !== null && b[key] !== '') args.push(flag, String(b[key])); };
      opt('--project', 'project'); opt('--agent', 'agent'); opt('--title', 'title');
      opt('--branch', 'branch'); opt('--worktree', 'worktree'); opt('--ticket', 'ticket');
      opt('--pr', 'pr'); opt('--review', 'review'); opt('--from', 'from');
      opt('--model', 'model'); opt('--mode', 'mode'); opt('--message', 'message');
      if (b.open) args.push('--open');
      const out = await cli([...args, '--json']).catch(() => cli(args));
      let parsed; try { parsed = JSON.parse(out); } catch { parsed = { ok: true, out: String(out).trim().slice(0, 400) }; }
      return send(res, 200, parsed);
    }
    // ---- pull requests: reviews list, review/build, status ----
    if (u.pathname === '/api/reviews' && req.method === 'GET') {
      return send(res, 200, { reviews: await cliJson(['pr', 'list']) });
    }
    if (seg[0] === 'api' && seg[1] === 'pr' && seg[2] && seg[3] === 'review' && req.method === 'POST') {
      const b = await readBody(req);
      const args = ['pr', 'review', seg[2]];
      if (b.project) args.push('--project', String(b.project));
      if (b.agent) args.push('--agent', String(b.agent));
      const out = await cli(args);
      return send(res, 200, { ok: true, out: String(out).trim().slice(0, 400) });
    }
    if (seg[0] === 'api' && seg[1] === 'pr' && seg[2] && seg[3] === 'build' && req.method === 'POST') {
      const b = await readBody(req);
      const args = ['pr', 'build', seg[2]];
      if (b.project) args.push('--project', String(b.project));
      if (b.message) args.push('--message', String(b.message));
      const out = await cli(args);
      return send(res, 200, { ok: true, out: String(out).trim().slice(0, 400) });
    }
    if (seg[0] === 'api' && seg[1] === 'pr' && seg[2] && seg[3] === 'status' && req.method === 'GET') {
      const args = ['pr', 'status', seg[2]];
      const p = u.searchParams.get('project');
      if (p) args.push('--project', p);
      return send(res, 200, await cliJson(args));
    }
    // ---- Today board ----
    if (u.pathname === '/api/board' && req.method === 'GET') {
      const args = ['board'];
      if (u.searchParams.get('all')) args.push('--all');
      return send(res, 200, await cliJson(args));
    }
    if (u.pathname === '/api/board' && req.method === 'POST') {
      const b = await readBody(req);
      const args = ['board', 'add', String(b.text || '')];
      if (b.due) args.push('--due', String(b.due));
      if (b.asked_by) args.push('--asked-by', String(b.asked_by));
      if (b.waiting_on) args.push('--waiting-on', String(b.waiting_on));
      if (b.list) args.push('--list', String(b.list));
      await cli(args);
      return send(res, 200, { ok: true });
    }

    if (u.pathname === '/api/health') return send(res, 200, { ok: true, cli: CLI, agent: Boolean(VOICE.agentId), key: Boolean(EL_KEY) });

    return serveStatic(res, u.pathname);
  } catch (e) {
    send(res, 500, { error: String(e?.stderr || e?.message || e) });
  }
});

const BIND = process.env.VOICE_BIND || '0.0.0.0';
server.listen(PORT, BIND, () => {
  console.log(`[glasscode-voice] control plane up on :${PORT}  (cli: ${CLI})`);
  console.log(`  loopback (IDE helper): http://127.0.0.1:${PORT}  — no token needed`);
  for (const ip of lanIPs()) console.log(`  LAN (glasses):         http://${ip}:${PORT}  — token required`);
  console.log(`  Glasscode token: ${TOKEN}`);
  // Warm the memory from past conversations on boot.
  memory.ingest(EL_KEY, VOICE.agentId).then((r) => {
    const d = memory.dump();
    console.log(`[glasscode-memory] ready — ${d.sessions} sessions, ${d.facts} notes${r.added ? ` (+${r.added} new)` : ''}`);
  });
});
