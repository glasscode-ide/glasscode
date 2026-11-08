// Glasscode's memory — local, private, in the control plane.
//
// Substrate: ElevenLabs already writes a per-conversation summary + title for every
// call, so we don't need a second LLM. We fold those summaries into a local store and,
// at the start of each new session, hand Glasscode a digest of what you worked on last time
// (plus a first-message opener) so every session continues instead of starting cold.
//
// The store is a plain JSON file under ~/.glasscode — yours, on your machine, never sent
// anywhere. The add/search shape mirrors Mem0/Graphiti so a graph engine can slot in
// behind this later without changing Glasscode.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import path from 'node:path';
import os from 'node:os';

const EL_API = 'https://api.elevenlabs.io';
const STORE = path.join(os.homedir(), '.glasscode', 'glasscode-memory.json');

function load() {
  try { return JSON.parse(readFileSync(STORE, 'utf8')); }
  catch { return { updated: 0, sessions: [], facts: [], seen: {} }; }
}
function save(s) {
  try { mkdirSync(path.dirname(STORE), { recursive: true }); writeFileSync(STORE, JSON.stringify(s, null, 2)); }
  catch (e) { console.error('[glasscode-memory] save failed:', e.message); }
}

let store = load();

// Pull the latest conversations from ElevenLabs and fold any new ones into the store.
// Idempotent: `seen` keeps us from counting a conversation twice. Mis-taps (a 1–3s call
// with no summary) are marked seen and skipped so they never pollute the memory.
export async function ingest(elKey, agentId, { limit = 50 } = {}) {
  if (!elKey) return { added: 0, reason: 'no key' };
  try {
    const url = `${EL_API}/v1/convai/conversations?page_size=${limit}`
      + (agentId ? `&agent_id=${encodeURIComponent(agentId)}` : '');
    const r = await fetch(url, { headers: { 'xi-api-key': elKey } });
    if (!r.ok) return { added: 0, reason: `elevenlabs ${r.status}` };
    const { conversations = [] } = await r.json();
    let added = 0;
    for (const c of conversations) {
      const id = c.conversation_id;
      if (!id || store.seen[id]) continue;
      const summary = (c.transcript_summary || '').trim();
      if ((c.call_duration_secs || 0) < 15 && !summary) { store.seen[id] = 1; continue; }
      store.sessions.push({
        id,
        at: (c.start_time_unix_secs || 0) * 1000,
        title: (c.call_summary_title || '').trim() || null,
        summary: summary || null,
        secs: c.call_duration_secs || 0,
        topics: Array.isArray(c.tool_names) ? c.tool_names : [],
      });
      store.seen[id] = 1;
      added++;
    }
    if (added) {
      store.sessions.sort((a, b) => b.at - a.at);
      store.sessions = store.sessions.slice(0, 200);
      store.updated = Date.now();
      save(store);
    }
    return { added, total: store.sessions.length };
  } catch (e) {
    return { added: 0, reason: e.message };
  }
}

function fmtDate(ms) {
  try { return new Date(ms).toLocaleDateString('en-US', { month: 'short', day: 'numeric' }); }
  catch { return ''; }
}

// The blob injected at session start: a short digest of recent work + durable facts,
// plus an opener line so Glasscode's first words pick up where you left off.
export function context({ max = 6 } = {}) {
  const recent = store.sessions.filter((s) => s.summary || s.title).slice(0, max);
  const facts = store.facts.slice(-20);
  let memory = '';
  if (recent.length) {
    memory += 'Recent voice sessions with the user (most recent first):\n';
    for (const s of recent) memory += `- ${fmtDate(s.at)}: ${s.title || '(untitled)'}${s.summary ? ` — ${s.summary}` : ''}\n`;
  }
  if (facts.length) {
    memory += '\nThings he asked you to remember:\n';
    for (const f of facts) memory += `- ${f.text}\n`;
  }
  memory = memory.trim();

  let opener = "Hey — I'm Glasscode. What do you need?";
  const last = recent[0];
  if (last && (last.title || last.summary)) {
    const what = last.title || (last.summary || '').split(/[.!?]/)[0].trim();
    if (what) opener = `Hey — last time we were on ${what}. Want to pick that back up, or something new?`;
  }
  return { memory, opener, sessions: recent.length, facts: facts.length };
}

// Keyword search over sessions + facts (deliberately simple; a vector/graph engine can
// replace this body without changing callers).
export function recall(query, { max = 6 } = {}) {
  const q = (query || '').toLowerCase().trim();
  if (!q) return { results: [] };
  const words = q.split(/\s+/).filter((w) => w.length > 2);
  const score = (text) => {
    const t = (text || '').toLowerCase();
    let n = 0;
    for (const w of words) if (t.includes(w)) n++;
    return n;
  };
  const hits = [];
  for (const s of store.sessions) {
    const n = score(`${s.title || ''} ${s.summary || ''} ${(s.topics || []).join(' ')}`);
    if (n) hits.push({ n, kind: 'session', at: s.at, title: s.title, summary: s.summary });
  }
  for (const f of store.facts) {
    const n = score(f.text);
    if (n) hits.push({ n, kind: 'fact', at: f.at, text: f.text });
  }
  hits.sort((a, b) => b.n - a.n || b.at - a.at);
  return { results: hits.slice(0, max).map(({ n, ...r }) => r) };
}

// A durable fact Glasscode saves on request ("remember that I prefer X").
export function note(text) {
  const t = (text || '').trim();
  if (!t) return { ok: false };
  store.facts.push({ text: t, at: Date.now() });
  store.facts = store.facts.slice(-200);
  store.updated = Date.now();
  save(store);
  return { ok: true, facts: store.facts.length };
}

export function dump() {
  return { updated: store.updated, sessions: store.sessions.length, facts: store.facts.length };
}
