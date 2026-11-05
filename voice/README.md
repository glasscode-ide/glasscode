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
(the key never reaches the browser). Optional: `LUNA_VOICE_ID`, `LUNA_LLM`.

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

**Glasscode's five tools** (see `scripts/agent-def.mjs`): `list_sessions`, `session_status`,
`read_transcript`, `switch_session`, and `send_prompt` — the last writes a message into a
session, and she always reads it back and gets a yes first.

## Roadmap

- **Part 1 ✅** control API + animated character, live session data.
- **Part 2 ✅** ElevenLabs Agent (Realtime) → real voice-to-voice, click-to-toggle.
- **Part 3 ✅** voice tool-calling — Glasscode lists, reads, switches, and writes into sessions.
- **Part 4** native Glasscode panel in the Glasscode IDE right sidebar.
- **Part 5** the Meta Ray-Ban Display app.
