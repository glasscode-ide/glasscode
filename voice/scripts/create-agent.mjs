// Provision (or update) the Glasscode agent in your ElevenLabs account.
//   ELEVENLABS_API_KEY=... node scripts/create-agent.mjs
// Optional: TEDDY_VOICE_ID=<voice> to pin a voice, TEDDY_LLM=<model> to pick the brain.
// Writes the resulting agentId into voice.config.json so the server/app pick it up.
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { agentConfig } from './agent-def.mjs';

const EL_API = 'https://api.elevenlabs.io';
const KEY = process.env.ELEVENLABS_API_KEY || '';
const DIR = path.dirname(fileURLToPath(import.meta.url));
const CONFIG = path.join(DIR, '..', 'voice.config.json');

// Warm, conversational voices to prefer by name when the user hasn't pinned one.
const PREFERRED = ['Jessica', 'Charlotte', 'Matilda', 'Alice', 'Sarah', 'Lily', 'Rachel'];

if (!KEY) {
  console.error('Set ELEVENLABS_API_KEY first.  e.g.  ELEVENLABS_API_KEY=sk_... node scripts/create-agent.mjs');
  process.exit(1);
}
const headers = { 'xi-api-key': KEY, 'content-type': 'application/json' };

async function resolveVoice() {
  const pinned = process.env.TEDDY_VOICE_ID || process.env.LUNA_VOICE_ID;
  if (pinned) return pinned;
  const r = await fetch(`${EL_API}/v1/voices`, { headers });
  if (!r.ok) throw new Error(`list voices failed: ${r.status} ${await r.text()}`);
  const { voices = [] } = await r.json();
  if (!voices.length) throw new Error('no voices on this account');
  for (const name of PREFERRED) {
    const hit = voices.find((v) => (v.name || '').toLowerCase() === name.toLowerCase());
    if (hit) return hit.voice_id;
  }
  return voices[0].voice_id;
}

async function loadConfig() {
  try { return JSON.parse(await readFile(CONFIG, 'utf8')); } catch { return { connectionType: 'webrtc' }; }
}

const cfg = await loadConfig();
const voiceId = await resolveVoice();
const body = agentConfig({ voiceId });
console.log(`Voice: ${voiceId}   Brain: ${body.conversation_config.agent.prompt.llm}   Tools: ${body.conversation_config.agent.prompt.tools.length}`);

let agentId = cfg.agentId;
let res;
if (agentId) {
  console.log(`Updating existing agent ${agentId}…`);
  res = await fetch(`${EL_API}/v1/convai/agents/${agentId}`, { method: 'PATCH', headers, body: JSON.stringify(body) });
} else {
  console.log('Creating Glasscode…');
  res = await fetch(`${EL_API}/v1/convai/agents/create`, { method: 'POST', headers, body: JSON.stringify(body) });
}
if (!res.ok) { console.error(`API error ${res.status}: ${await res.text()}`); process.exit(1); }
const data = await res.json();
agentId = data.agent_id || agentId;

await writeFile(CONFIG, JSON.stringify({ ...cfg, agentId, connectionType: cfg.connectionType || 'webrtc', voiceId }, null, 2) + '\n');
console.log(`\n✅ Glasscode is ready.  agentId: ${agentId}`);
console.log('   Saved to voice.config.json. Restart the server if it was running, then open the app.');
