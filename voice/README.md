# Glasscode Voice

**Glasscode** — a voice copilot for your Glasscode sessions. Click her face and she's live;
talk to her, and she drives your sessions. She lives in the IDE sidebar and, later, on
the Meta glasses.

## Run

```sh
node server.mjs            # http://127.0.0.1:4517
```

No dependencies. The server wraps the `glasscode` CLI (`--json`) so Glasscode sees your live
sessions and can write prompts into them. Override with `GLASSCODE_BIN` or `PORT`.

## Give Glasscode a voice (one-time)

Her brain + voice run on **ElevenLabs Agents**. Provision her in your account:

```sh
ELEVENLABS_API_KEY=sk_... node scripts/create-agent.mjs
```

This creates the "Glasscode — Glasscode" agent (persona + the five tools below), picks a warm
voice, and writes the `agentId` into `voice.config.json`. Restart the server and reload.
Keep `ELEVENLABS_API_KEY` in the server's environment so it can mint per-session tokens
(the key never reaches the browser). Optional: `GLASSCODE_VOICE_ID`, `GLASSCODE_LLM`.

## How she works

- **Click the face** → a live ElevenLabs conversation opens and stays open. **Click again** → idle.
  No push-to-talk, no chat history — just talk.
- She's **session-aware**: she always knows which session is current, and it follows when you switch.
- She speaks while she explains; her mouth is driven by her real audio.

## Control API (also Glasscode's tools)

| Method | Path | CLI it calls |
|---|---|---|
| GET  | `/api/sessions[?needs=1]` | `glasscode session list [--needs-you] --json` |
| GET  | `/api/sessions/:id` | `glasscode session show <id> --json` |
| GET  | `/api/sessions/:id/transcript?last=N` | `glasscode session read <id> --last N` |
| POST | `/api/sessions/:id/send` `{message}` | `glasscode session send <id> "<message>"` |
| POST | `/api/current` `{id}` | sets which session Glasscode is focused on |
| GET  | `/api/config` · POST `/api/voice/token` | voice session bootstrap |

### The manager

The manager is the one assistant above all sessions (Glasscode › Settings › Manager). These
endpoints wrap `glasscode manager …` for the phone and the glasses. When the manager is off or
its voice is not set up, the answer is `{ "error": "<a plain sentence>", "code": <exit code> }`
with status 502 (504 when it took too long), so the app can show the sentence as it is.

| Method | Path | CLI it calls | Answer |
|---|---|---|---|
| GET  | `/api/manager/status` | `glasscode manager status --json` | `{enabled, name, initials, state, caption, needs_you, …}` |
| POST | `/api/manager/ask` `{text, voice?, wait?}` | `glasscode manager ask "<text>" [--voice] --json` | `{say}`; with `wait: false` at once `{job}` |
| GET  | `/api/manager/ask/:job` | (the question started with `wait: false`) | `{done: false}`, then once `{done: true, say}` or `{done: true, error}` |
| GET  | `/api/manager/brief` | `glasscode manager brief --json` | today's update: `{day, made, lines: [{kind, text, show}], text}` |
| POST | `/api/manager/voice/token` `{identity}` | `glasscode manager voice-token --identity <identity> --json` | `{url, token, room, identity}` for the manager's LiveKit room |
| GET  | `/api/manager/messages` | `glasscode manager message list --json` | `{messages: [...]}`: drafts that wait for your OK |
| GET  | `/api/manager/followups` | `glasscode manager follow-up list --json` | `{followups: [...]}` |

A question can take up to two minutes. A phone, or a relay in between, may not keep a request
open that long, so ask with `wait: false` and fetch the answer from `/api/manager/ask/<job>`
about once a second. A finished answer is kept for five minutes and handed out once.

`glasscode manager ask` goes to the manager's chat ([`docs/manager.md`](../docs/manager.md)), as
the typed box does, so `/api/manager/ask` and the voice reach it the same way.

The room itself (the LiveKit server and the speech worker) is set up in
[`manager/`](manager/README.md).

**Glasscode's five tools** (see `scripts/agent-def.mjs`): `list_sessions`, `session_status`,
`read_transcript`, `switch_session`, and `send_prompt` — the last writes a message into a
session, and she always reads it back and gets a yes first.

## Roadmap

- **Part 1 ✅** control API + animated character, live session data.
- **Part 2 ✅** ElevenLabs Agent (Realtime) → real voice-to-voice, click-to-toggle.
- **Part 3 ✅** voice tool-calling — Glasscode lists, reads, switches, and writes into sessions.
- **Part 4** native Glasscode panel in the Glasscode IDE right sidebar.
- **Part 5** the Meta Ray-Ban Display app.
