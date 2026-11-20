# The manager's voice

The manager is the one assistant above all your sessions in Glasscode. This folder gives it a
voice: you talk to it, it talks back. It runs on a machine of your own. Nothing here is needed
when you only type to the manager.

Two programs run here:

- **LiveKit server**: a "room" that your Mac, your phone or your glasses join with the
  microphone.
- **The worker**: it sits in that room, turns what you say into text and turns the manager's
  answers into speech. That is all it does.

The manager's brain is not here. It stays in Glasscode. The worker sends it the text of what
you said, and speaks the text that comes back.

The worker sends what it heard to `glasscode manager ask`, which goes to the manager's chat. That
brain runs `claude -p` or `codex exec` with only Glasscode's tools, in `~/.glasscode/manager/work`
and nowhere else, and does not start on a CLI older than the minimum versions (Claude Code
2.1.322, Codex 0.167.0).

```
your voice ──► Mac helper ──► room ──► worker ──► speech to text ──► text ──► Glasscode (the brain)
speaker    ◄── Mac helper ◄── room ◄── worker ◄── text to speech ◄── text ◄── Glasscode
```

Read [What was tested, and what was not](#what-was-tested-and-what-was-not) before you rely on
it: no real speech service has been tried yet.

## What you need

1. A machine with Docker that your Mac can reach (`docker compose version` should print
   2.24 or newer). The compose file uses the host's network, which works on Linux.
2. A LiveKit key and secret. You make them yourself, they cost nothing:
   ```
   docker run --rm livekit/livekit-server:v1.13.11 generate-keys
   ```
3. A key for one speech-to-text service and one for a text-to-speech service. The simplest
   start is one **Deepgram** key for both (`MANAGER_STT=deepgram`, `MANAGER_TTS=deepgram`).
   The default is Deepgram for listening and Cartesia for speaking.

   | To listen (`MANAGER_STT`) | To speak (`MANAGER_TTS`) | Key in `.env` |
   | --- | --- | --- |
   | `deepgram` | `deepgram` | `DEEPGRAM_API_KEY` |
   | `openai` | `openai` | `OPENAI_API_KEY` |
   | `elevenlabs` | `elevenlabs` | `ELEVEN_API_KEY` |
   | | `cartesia` | `CARTESIA_API_KEY` |

## Start it

```
cd voice/manager
cp .env.example .env        # then fill in .env
docker compose up -d
docker compose logs -f worker
```

In `.env` you fill in: the LiveKit key and secret, `LIVEKIT_NODE_IP` (the address of this
machine as your Mac reaches it), the two service names, and their keys. `.env` stays on that
machine; git ignores it.

Without the LiveKit key and secret nothing starts: Compose stops and says which one is
missing. To only check the compose file, without a `.env`:

```
LIVEKIT_API_KEY=example LIVEKIT_API_SECRET=example docker compose config
```

It works when the worker's log shows `registered worker`.

### Ports

Open these on the machine, towards the devices that use it (not necessarily the internet):

| Port | For |
| --- | --- |
| 7880/tcp | the room's address |
| 7881/tcp | audio, when UDP is blocked |
| 7882/udp | audio |
| 80/tcp and 443/tcp | only with the `tls` profile (below) |

There is no TURN server. You only need one when a device sits behind a firewall that blocks
both 7882/udp and 7881/tcp.

### The address: `ws://` or `wss://`

- **Inside your own network or VPN** (home network, Tailscale, WireGuard): the address is
  `ws://<address of the machine>:7880`. The audio itself is always encrypted; with `ws://`
  the sign-in to the room is not, so only use it on a network you trust.
- **With a public name**: point the name at the machine, put it in `.env` as
  `LIVEKIT_DOMAIN`, set `LIVEKIT_USE_EXTERNAL_IP=true`, and start with
  `docker compose --profile tls up -d`. Caddy gets a certificate by itself and the address
  is `wss://<your name>`. A phone app needs `wss://`.

## In Glasscode

1. **Settings › Manager**: switch the manager on.
2. **Settings › Manager › Voice**, three fields:
   - **LiveKit URL**: the `ws://…` or `wss://…` from above.
   - **LiveKit key** and **LiveKit secret**: the same two values as in `.env`. Type both and
     press **Save**. The row then says "Saved". They are kept in the macOS Keychain, not in
     the settings file, and are not shown again. **Remove** forgets them.
3. **He listens**, in the same place, chooses when: on the wake word (its name), while you
   hold, or never.

(Without the app: `security add-generic-password -U -s glasscode-manager-voice -a livekit -w`
stores the same Keychain item. It asks for the value: the key, a colon, the secret.)

Then open the manager's page and tap the orb. Say its name, then what you want:
"AI Mira, what needs me?". After it answered you have 20 seconds to reply without the name.

The glasses app (`glasses/GlasscodeGlasses`) joins the same room when you pick the manager or
Talk on the lens. It needs nothing set up here: it asks the Mac for a pass to the room.

## Test it from the command line

On the machine that runs it:

```
docker compose ps                      # livekit and worker are "running"
curl http://127.0.0.1:7880/            # prints OK
docker compose logs worker | grep "registered worker"
```

From the Mac, without the app. Make a token on the machine:

```
docker compose exec livekit /livekit-server create-join-token --room manager --identity mac-test
```

Then on the Mac, start the helper by hand with that token:

```
MANAGER_VOICE_URL=ws://<machine>:7880 MANAGER_VOICE_TOKEN=<token> MANAGER_VOICE_NAME=Manager \
  <path to>/Glasscode.app/Contents/Resources/manager-voice
```

(Built from source, the helper is `voice/helper/.build/release/manager-voice` after
`cd voice/helper && swift build -c release --product manager-voice`.)

It prints `{"ready":true}` and then `{"state":"idle"}` (the worker joined). Now:

- Say "Manager, hello". It prints `{"heard":"hello"}`.
- Type `{"say":{"id":"1","text":"Hello, I can hear you."}}` and press Return. You hear it.
- Press Ctrl-D to end.

(macOS asks once whether Terminal may use the microphone.)

## Run the tests

Without Docker, the wake word and the conversation window:

```
cd voice/manager/worker
python3 -m unittest
```

With Docker on a Mac, the worker and the Mac helper end to end:

```
cd voice/helper && swift build -c release --product manager-voice && cd ../..
voice/manager/test/run.sh
```

This uses no keys, makes no sound and opens nothing. The two speech services are replaced by
stand-ins (a fixed text for whatever was said, a tone for speech). It starts its own LiveKit
server on `127.0.0.1:17880` and removes it afterwards, so it does not get in the way of one
on port 7880. It makes a Python virtualenv in a temporary folder (Python 3.10 or newer; set
`PYTHON=python3.13` to choose one) and prints `RESULT: ALL OK` twice when everything passed.

## What it costs

LiveKit server and the worker are free. You pay the speech services, by use. These are rough
numbers from the services' price lists in November 2026. Prices change: check the services'
own pages.

| | Service | Price | Roughly |
| --- | --- | --- | --- |
| Listening | Deepgram Nova-3, streaming | $0.0048 per minute | $0.29 per hour the orb is on |
| Listening | OpenAI gpt-4o-mini-transcribe | $0.003 per minute of speech | only what you say |
| Speaking | Deepgram Aura-2 | $0.030 per 1,000 characters | about $0.02 per minute spoken |
| Speaking | Cartesia Sonic | 1 credit per character ($5 for 100,000) | about $0.04 per minute spoken |

So a working day (eight hours) with the orb on all the time and the manager talking for ten
minutes is about $2.70 with the defaults.

**Know this:** with a streaming service (Deepgram), the microphone goes to that service the
whole time the orb is on, also while you say nothing and before you say the manager's name.
That is what the hourly price above is. If you do not want that, set
`MANAGER_STT_MODE=segments`: then only what sounds like speech is sent, sentence by
sentence. It answers a little later (about half a second) and costs less.

## The brain on the server (optional)

Normally Glasscode on the Mac answers. With `MANAGER_BRAIN=server` the worker asks the
manager that runs in `glasscoded` on this machine instead: it runs
`glasscoded manager ask "<what you said>"` and speaks what that prints. Voice then works
while the Mac is off (from the phone).

`glasscoded` lives on the machine, not in the worker's container. So for this mode run the
worker outside Docker:

```
docker compose up -d livekit
MANAGER_BRAIN=server ./worker/run-host.sh
```

(`run-host.sh` makes a Python virtualenv inside `worker/` and installs nothing elsewhere.
Another command than `glasscoded manager ask`: set `MANAGER_BRAIN_CMD`.)

## Another speech service

Each service is one small function in `worker/providers.py`. To add one: install its LiveKit
plugin (add it to `worker/requirements.txt`), write a function that returns the plugin's
object, and add it to the `STT` or `TTS` table in that file. Then
`docker compose up -d --build`.

## How it is built

- `docker-compose.yml`: LiveKit server `v1.13.11` and the worker, both on the host's network.
- `worker/worker.py`: LiveKit Agents `1.8.12` (Python). It uses an `AgentSession` **without
  a language model**: the session does voice detection (Silero), transcription, turn taking
  and interruption; the agent's `on_user_turn_completed` hook receives each finished
  sentence and hands it on; `session.say()` speaks.
- `worker/wake.py`: the wake word (the manager's name at the start of a sentence, loosely
  matched) and the 20-second conversation window.
- `voice/helper/Sources/manager-voice`: the Mac helper (Swift, LiveKit's Swift SDK). It has no
  window. Glasscode starts it, and they talk in JSON lines.
- The messages in the room are listed at the top of `worker/worker.py` and in
  `crates/glasscode_core/src/manager/voice.rs`. Keep the two in step.
- Each device tells the worker how to listen to it: `wake` (after the manager's name),
  `always` (you chose to talk there, or you hold the talk key: everything counts) or
  `never`. It listens to one microphone at a time: a device that asked for `always` first,
  else the one that joined last. What it heard is marked with the device that heard it, and
  only that device asks the brain.
- When you talk while it speaks, it stops. When its own voice comes back through a
  microphone (a device without echo cancelling), that is dropped.

## What was tested, and what was not

Tested on a Mac with `test/run.sh`, against a real LiveKit server `v1.13.11` (in Docker, on
localhost), with stand-ins for the two speech services:

- The worker is sent into the room, joins as `manager-voice`, and reports its state.
- With recorded speech as the microphone: the wake word, the conversation window, speaking
  with an id, stop, talking over it, the echo guard, and the three ways of listening
  (kept per device).
- The Mac helper (without microphone and speaker): joining, a refused token, passing the
  name on, say, stop, and ending when the app goes away.

Also checked: the wake-word unit tests pass, the Mac helper and the glasses app build, the
worker's image builds (on a Mac), and `docker compose config` accepts the compose file (with
example values for the key and secret, as shown above).

**Not tested:**

- Any real speech-to-text or text-to-speech service. No keys were available, so no audio
  ever went to Deepgram, Cartesia, OpenAI or ElevenLabs from this code.
- The Mac helper with a real microphone and speaker, and echo cancelling.
- The glasses app on a phone and on glasses. It builds; it has not run.
- This compose file on a Linux server (host networking, your firewall, UDP from outside).
- The `tls` profile (Caddy and a certificate).
- `MANAGER_BRAIN=server` with a real `glasscoded`.
- How well the wake word works with a real transcription of your manager's name.
- Saving the key and secret in Settings, and the `security` command above, on a fresh Mac.

If you try one of these, an issue or a pull request with what you found is welcome.

## When it does not work

| What you see | Look at |
| --- | --- |
| "The voice server could not be reached" | the LiveKit URL in Glasscode, port 7880, `docker compose ps` |
| "The voice server did not accept the key" | key and secret are the same in `.env` and in Settings › Manager › Voice (save them again) |
| "The voice worker on the server did not join" | `docker compose logs worker` |
| "The … service could not be set up" | that service's key in `.env`, then `docker compose up -d` |
| Joined, but no sound either way | `LIVEKIT_NODE_IP` in `.env`, ports 7882/udp and 7881/tcp |
| It does not react to its name | set `MANAGER_LOG_HEARD=1` in `.env`, restart, and read in `docker compose logs worker` what it understood; try `MANAGER_LANGUAGE` |

The helper's own log on the Mac is `~/.glasscode/manager/voice-helper.log`.
