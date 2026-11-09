// Glasscode Voice — Glasscode.
// Click the face to wake her (a live ElevenLabs conversation opens and stays open);
// click again and she goes idle. She drives your sessions through the local control API.
const $ = (s) => document.querySelector(s);
const stage = $('#stage'), statusEl = $('#status'), capEl = $('#caption');
const countsEl = $('#counts'), listEl = $('#sessions'), ctxEl = $('#ctx');
const eyes = [...document.querySelectorAll('.eye')];

const SDK_URL = 'https://cdn.jsdelivr.net/npm/@elevenlabs/client@1.33.0/+esm';
let ConversationCls = null;
let convo = null, config = null, energyRAF = 0, captionTimer = 0, connecting = false;
let sessions = [], current = null;

// ---------- small helpers ----------
function setState(s) { stage.dataset.state = s; }
function say(t) { statusEl.textContent = t; }
function caption(t, sticky) {
  clearTimeout(captionTimer);
  capEl.textContent = t || '';
  capEl.classList.toggle('show', !!t);
  if (t && !sticky) captionTimer = setTimeout(() => { capEl.classList.remove('show'); }, 9000);
}
// ---------- shared voice state (so the IDE panel / glasses can drive & mirror this session) ----------
function postVoice(patch) {
  try { fetch('/api/voice/state', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(patch) }); } catch {}
}
async function syncVoice() {
  try {
    const r = await fetch('/api/voice/state');
    if (!r.ok) return;
    const v = await r.json();
    // obey a remote controller: start/stop to match its intent
    if (v.intentLive && !convo && !connecting) start();
    else if (!v.intentLive && convo) stop();
  } catch {}
}

function isNeeds(s) { return /needs/i.test(s && s.state || ''); }
function prNum(s) { return s && s.pull_request && (s.pull_request.number ?? s.pull_request) || null; }
function escapeHtml(s) { return String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c])); }

// ---------- blinking (idle charm) ----------
(function blink() {
  if (stage.dataset.state === 'idle') {
    const e = eyes[Math.floor(Math.random() * 2)];
    if (e) { e.classList.add('blink'); setTimeout(() => e.classList.remove('blink'), 240); }
  }
  setTimeout(blink, 2600 + Math.random() * 3600);
})();

// ---------- speaking energy, from Glasscode's real audio ----------
function startEnergy() {
  stopEnergy();
  const loop = () => {
    let e = 0;
    try {
      const d = convo && convo.getOutputByteFrequencyData && convo.getOutputByteFrequencyData();
      if (d && d.length) { let sum = 0; const n = Math.min(d.length, 64); for (let i = 0; i < n; i++) sum += d[i]; e = Math.min(1, (sum / n) / 135); }
    } catch {}
    document.documentElement.style.setProperty('--energy', e.toFixed(2));
    energyRAF = requestAnimationFrame(loop);
  };
  loop();
}
function stopEnergy() { cancelAnimationFrame(energyRAF); energyRAF = 0; document.documentElement.style.setProperty('--energy', '0'); }

// ---------- the voice session (toggle) ----------
async function loadSDK() {
  if (!ConversationCls) { const m = await import(SDK_URL); ConversationCls = m.Conversation; }
  return ConversationCls;
}
async function getConfig() { const r = await fetch('/api/config'); config = await r.json(); return config; }

async function start() {
  if (convo || connecting) return;
  connecting = true;
  postVoice({ intentLive: true });
  setState('connecting'); say('Waking Glasscode…'); caption('');
  try {
    const cfg = config || await getConfig();
    if (!cfg.ready) {
      setState('idle'); say('Glasscode’s voice isn’t set up yet');
      caption('Run  scripts/create-agent.mjs  with your ElevenLabs key, then reload.', true);
      return;
    }
    const Conversation = await loadSDK();
    // Ask for the browser's echo cancellation / noise suppression so Glasscode doesn't hear herself.
    await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true } });

    let conversationToken = null;
    if (cfg.authMode === 'token') {
      const tr = await fetch('/api/voice/token', { method: 'POST' });
      const td = await tr.json();
      if (!tr.ok || !td.token) throw new Error(td.error || 'could not get voice token');
      conversationToken = td.token;
    }

    convo = await Conversation.startSession({
      agentId: cfg.agentId,
      connectionType: cfg.connectionType || 'webrtc',
      ...(conversationToken ? { conversationToken } : {}),
      clientTools,
      onConnect: () => { setState('listening'); say('Listening…'); postVoice({ live: true, mode: 'listening', caption: '' }); },
      onDisconnect: () => teardown(),
      onModeChange: (m) => {
        const mode = (m && m.mode) || m;
        if (mode === 'speaking') { setState('speaking'); startEnergy(); postVoice({ mode: 'speaking' }); }
        else { stopEnergy(); if (convo) { setState('listening'); say('Listening…'); postVoice({ mode: 'listening' }); } }
      },
      onMessage: (msg) => handleMessage(msg),
      onError: (e) => { console.error('[glasscode] error', e); say('Hmm, something glitched'); },
    });
    // Keep Glasscode's playback a touch lower so the mic picks up less of her on speakers (less echo).
    try { await convo.setVolume({ volume: 0.65 }); } catch {}
  } catch (e) {
    console.error('[glasscode] start failed', e);
    setState('idle');
    if (e && /denied|permission|NotAllowed/i.test(String(e.name || e.message))) { say('I need mic access'); caption('Allow the microphone, then tap me again.', true); }
    else { say('Couldn’t start'); caption(String(e && e.message || e), true); }
    convo = null;
  } finally {
    connecting = false;
  }
}
async function stop() { const c = convo; convo = null; postVoice({ intentLive: false }); try { if (c) await c.endSession(); } catch {} teardown(); }
function teardown() { stopEnergy(); convo = null; setState('idle'); say('Tap Glasscode to start talking'); postVoice({ live: false, mode: 'idle', caption: '' }); }
function toggle() { if (convo) stop(); else start(); }

function handleMessage(msg) {
  if (!msg) return;
  const src = String(msg.source || msg.role || msg.type || '').toLowerCase();
  const text = msg.message || msg.text || '';
  if (text && (src.includes('ai') || src.includes('agent'))) { caption(text, true); postVoice({ caption: text, mode: 'speaking' }); }
}

// ---------- client tools (what Glasscode can actually DO) ----------
function resolve(ref) {
  if (!ref) return sessions.find((s) => s.id === current) || null;
  const r = String(ref).toLowerCase().trim();
  return sessions.find((s) => s.id === ref)
      || sessions.find((s) => String(s.id).toLowerCase() === r)
      || sessions.find((s) => (s.title || '').toLowerCase() === r)
      || sessions.find((s) => (s.title || '').toLowerCase().includes(r))
      || sessions.find((s) => r.includes((s.title || '').toLowerCase()) && (s.title || '').length > 3)
      || null;
}
function compact(s) { return { title: s.title, state: s.state, pr: prNum(s) || undefined, project: s.project || undefined }; }

const clientTools = {
  async list_sessions({ needs } = {}) {
    await fetchSessions();
    const list = needs ? sessions.filter(isNeeds) : sessions;
    return JSON.stringify({
      total: sessions.length,
      needs_you: sessions.filter(isNeeds).length,
      current: (sessions.find((s) => s.id === current) || {}).title || null,
      sessions: list.slice(0, 20).map(compact),
    });
  },
  async session_status({ session } = {}) {
    const s = resolve(session);
    return s ? JSON.stringify(compact(s)) : 'No session matches that — want me to list them?';
  },
  async read_transcript({ session, last } = {}) {
    const s = resolve(session);
    if (!s) return 'No session matches that.';
    const r = await fetch(`/api/sessions/${s.id}/transcript?last=${encodeURIComponent(last || 6)}`);
    if (!r.ok) return 'I could not read that session.';
    const d = await r.json();
    return (d.text || '').trim().slice(0, 1600) || 'Nothing in it yet.';
  },
  async switch_session({ session } = {}) {
    const s = resolve(session);
    if (!s) return 'No session matches that.';
    await setCurrent(s.id, true);
    return JSON.stringify({ ok: true, title: s.title, state: s.state });
  },
  async send_prompt({ session, message } = {}) {
    const s = resolve(session);
    if (!s) return 'Which session should I write to? I could not tell.';
    if (!message || !message.trim()) return 'There is no message to send.';
    const r = await fetch(`/api/sessions/${s.id}/send`, {
      method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ message }),
    });
    if (!r.ok) return 'That failed to send.';
    caption(`→ sent to “${s.title}”`, false);
    setTimeout(fetchSessions, 600);
    return JSON.stringify({ ok: true, title: s.title });
  },
};

// ---------- live session list (glanceable context) ----------
async function fetchSessions() {
  try {
    const r = await fetch('/api/sessions');
    if (!r.ok) throw new Error('server ' + r.status);
    const data = await r.json();
    sessions = data.sessions || []; current = data.current || null;
    render();
  } catch {
    countsEl.innerHTML = `<span class="err">Can’t reach Glasscode — is the app running?</span>`;
    listEl.innerHTML = '';
  }
}
function stateClass(s) {
  const st = (s.state || '').toLowerCase();
  if (st.includes('needs')) return 'needs';
  if (st.includes('not running') || st.includes('off')) return 'off';
  if (st.includes('idle')) return 'idle';
  return 'running';
}
function render() {
  const needs = sessions.filter(isNeeds).length;
  countsEl.innerHTML = `<b>${sessions.length}</b> sessions · <span class="n">${needs}</span> need you`;
  ctxEl.textContent = current ? (sessions.find((s) => s.id === current)?.title || 'session') : 'All sessions';
  const ordered = [...sessions].sort((a, b) => (isNeeds(b) - isNeeds(a)));
  listEl.innerHTML = ordered.slice(0, 7).map((s) => {
    const cls = stateClass(s);
    const pr = prNum(s) ? ` · PR #${prNum(s)}` : '';
    return `<li class="srow${s.id === current ? ' active' : ''}" data-id="${s.id}">
      <span class="dot ${cls}"></span>
      <span class="t"><span class="title">${escapeHtml(s.title || 'Untitled')}</span>
      <span class="meta">${escapeHtml(s.agent || '')}${escapeHtml(pr)} · ${escapeHtml(s.project || '')}</span></span>
      <span class="st ${cls === 'needs' ? 'needs' : ''}">${escapeHtml(s.state || '')}</span>
    </li>`;
  }).join('');
  listEl.querySelectorAll('.srow').forEach((el) => el.addEventListener('click', () => setCurrent(el.dataset.id)));
}
async function setCurrent(id, fromGlasscode) {
  current = (current === id && !fromGlasscode) ? null : id;
  try { await fetch('/api/current', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id: current }) }); } catch {}
  render();
}
ctxEl.addEventListener('click', (e) => { e.stopPropagation(); setCurrent(current); });

// ---------- wire the face + boot ----------
stage.addEventListener('click', toggle);
stage.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); } });
window.addEventListener('beforeunload', () => { try { convo && convo.endSession(); } catch {} });

// expose for the IDE embed / glasses later
window.Glasscode = { start, stop, toggle, setState, caption, fetchSessions };

getConfig().catch(() => {});
fetchSessions();
setInterval(fetchSessions, 5000);
setInterval(syncVoice, 1200); // obey remote start/stop from the IDE panel or glasses
