# Glasses: talk to the manager

| | |
|---|---|
| Document | GC-GLS-001, plan, issue 1 (draft, not approved) |
| Date | 2026-11-21 |
| Branch | `glasscode/mobile-glasses-manager` |
| Builds on | [`manager-v2-design.md`](manager-v2-design.md) (P1 to P5, D1 to D31), [`manager-v2-slices.md`](manager-v2-slices.md) (format), [`server-sync-plan.md`](server-sync-plan.md) ("the phone is parked") |
| Supersedes | D25's "this plan does not change `voice/` or the glasses' control API", for the parts the owner decides in section 9 |
| Progress | G00 done: on 2026-11-21 the owner accepted every proposal of section 9 (O1 to O9) and asked for the whole plan to be built. Open: where the `ios/` source is (O1; G11 is written without it) and the hardware for G01 (O6). |

## Contents

1. What the owner asked for
2. What exists today
3. The spoken flows, end to end
4. The shape
5. The lens and the Neural Band
6. Rights: what a paired phone may do (D2, D3)
7. What is removed
8. Latency
9. Decisions for the owner
10. Slices
11. Risks
12. Meta's toolkit: what is known, what is not

## 1. What the owner asked for

| # | Requirement |
|---|---|
| G-R1 | One mobile app: the iPhone app for Meta Ray-Ban Display glasses and the Neural Band. No second app. |
| G-R2 | The person talks to the manager directly from the glasses, by voice (and by text on the phone), wherever the manager runs: the Mac or the server. |
| G-R3 | The phone, the Mac and the server share one tailnet and reach each other over it. |
| G-R4 | The manager can do everything from there: go to any project, session, pull request or tab ("go to session X", "open the checkout PR"), explain what is happening, and act ("run the tests", "tell that session to rebase"), within the gate's rules. |
| G-R5 | The old menu (Home, Sessions, Session, Talk, Voice) goes. The app is built around the manager. |
| G-R6 | The right panel's "Glasscode" tab goes. In its place, a glasses icon in the title bar with a green dot while the glasses are connected. |
| G-R7 | The Neural Band drives the lens. |
| G-R8 | What was hardened stays: a phone is its own caller (`Phone{device}`); Mac input is done only when a paired device signed it (#288); the server gives a phone an allow-list and no shell (#292); a phone's terminal is view only (#305); voice tokens go only to the person in the app or a paired device. |

## 2. What exists today

### Two mobile apps

| | Glasscode for iPhone | The glasses app |
|---|---|---|
| Source | `ios/`, **in neither this repository nor the organisation on GitHub, nor on this Mac** ([`architecture.md`](architecture.md#iphone) describes it) | `glasses/GlasscodeGlasses` (Swift, iOS 18, about 3,400 lines) |
| Reaches | the server only: SSH with a Secure Enclave key to `glasscoded gate --device <id>` | the Mac only: `voice/server.mjs` on port 4517, with one shared bearer token, or the self-hosted relay |
| Identity | its own P-256 key, pinned on the Mac (`authority/devices.json`) and on the server (`authorized_keys`, forced command) | none: whoever holds the token |
| Does | sessions, view-only terminal (SwiftTerm), answers, Today, approvals, push, widgets, Live Activity | lens UI (DAT 1.0.1: Core, Display, Inputs, Speech), Neural Band browsing, the manager's LiveKit voice room, the old ElevenLabs copilot, wake word |
| Signs Mac input (#288) | not yet: the Mac refuses its input until it does | no |
| Ran on hardware | not checked here; `architecture.md` lists end-to-end tests against the Docker server | not recorded: "It builds; it has not run" (`voice/manager/README.md`) |

**The owner's choice (2026-11-21): the glasses app is the one app.** Glasscode for iPhone is retired. What stays of it is its server side, which the glasses app reuses: `phones.*`, `gate --device`, the allow-list, push.

### The shared-token door (a security item either way)

`voice/server.mjs` listens on `0.0.0.0:4517`. It runs the `glasscode` command for any request that carries the shared token (or comes from loopback): `session send`, `session new`, `pr build`, `board add`, `manager ask`, `manager voice-token`. The gate sees a `Terminal` caller, so at v2 the acts wait on cards and the questions are "outside" words. But `manager.voice_token` refuses only agents (`ops_run.rs`, `manager_voice_token`), so **anyone with the token on the network gets a pass into the manager's voice room as the person**. That breaks G-R8's last rule today. Worse, the relay host (`glasscoded relay`, `relay_link.rs`) joins relayed connections to `127.0.0.1:4517`, so `server.mjs` sees them as loopback and asks for no token: the relay's shared secret alone is enough. No fix for either is open on GitHub today. Slice G02 closes both first; after it, the current glasses app cannot reach the Mac until the new one (G11).

### The manager's side

- **Chat** (M52): every question goes through `manager_chat.rs` into one conversation (`~/.glasscode/manager/conversation/`), by topic. Session and PR references become chips; "Show me" chips move the window when accepted. The focus is a `ui.state` snapshot taken when the message is sent.
- **Whose words** (D1): composer text is tier 1. Device and voice input are tier 2. Tier 2 cannot back a chat grant (D3), and a spoken "yes" never binds.
- **Navigation and acts** are registry ops: `ui.open`, `ui.panel`, `ui.state`, `session.open`, `worktree.open` (Window host), `session.send`, `session.answer`, `session.keys`, `session.new`, `pr.view`, `pr.checks`, `pr.diff`, `approvals.list`, `approvals.decide`.
- **Who is home when you walk away.** Under Automatic with a server paired, the Mac *claims* home only after the person was present at it for a minute (`home::mac_should_be_home`, `PRESENT_FOR_MS`; presence is the Mac's own input), and then *keeps* it while it stays awake and reaches the server (a renewal needs no presence). It loses it on sleep, so after a wake the server is home until the person is back at the Mac. The server's manager acts only on the server's own sessions: for a Mac session every op but a window op answers "runs on the Mac; the server has no handler for it yet" (`brain_host.rs`, `run_op`), and window ops wait for the Mac.
- **The server as home** (M57, M58): glasscoded's chat lane answers the newest unanswered message in the synced conversation, checking every 2 seconds, and posts the answer when it is complete (no streaming). The phone has no way to post a question there: `manager.*` is not in `PHONE_ALLOWED`, and nothing tells a phone where the manager is home (`state` does not say; `home.*` is machine-only).
- **Voice**: the LiveKit room (`voice/manager`) turns speech into text and back; the device that heard a sentence asks the brain. Its token is a Window op: the LiveKit keys are in the Mac's Keychain, so no token can be made while the Mac sleeps. No real speech service has been tried.

### The gate disagrees with itself about phones

- `gate::phone()` lets a paired phone run any op that is not person-only, as the person, and skips `mode_raised`.
- `caller::rule` gives a phone the outside rule (ask for everything but reads).
- The words ledger ranks its text tier 2.

Section 6 picks one answer and names the code that changes.

## 3. The spoken flows, end to end

Each flow starts with the glasses on, the app connected, and the manager on. "Pinch" is the Neural Band's index pinch; "swipe" is the thumb swipe (section 5).

**F1. "What needs me?"**
1. Pinch. The lens shows *Listening* and the words as they are heard.
2. A pause ends the turn. The phone sends the text with the lens's focus to the manager's home.
3. The answer comes from code (`runtime::quick`), in under a second: "Two things. Checkout-fix asks to run npm test, and CI failed on #42."
4. The lens shows the two rows. Swipe down to one, pinch, and its session card opens.

**F2. "Go to the checkout session and tell me what's happening."**
1. The manager finds the session and calls `device.open` (new, section 4): the lens shows the session card (title, branch, state, last reply, the open question).
2. It reads the turn report and the transcript, and says two sentences.
3. The card's chips: *Pull request*, *Answer*, *Say something*.

**F3. "Open the checkout PR."** `device.open` shows the PR card: checks (two failing, by name), reviews, mergeable. It says what failed.

**F4. "Run the tests."** "This" is the session on the lens (the focus).
- In a **Standard** project the manager may drive sessions: it sends "run the tests" to that session. The lens shows the action line *Asked checkout-fix to run the tests*. When the turn ends, the turn report comes back as a lens note.
- In a **Careful** project (this one) the manager may not, and the device's words are tier 2, so it cannot use a grant. Its `session.send` becomes a **card**: *Send to checkout-fix: "Run the tests"*, with Allow and Deny. Pinch Allow: the phone signs `approvals.decide` over the op's arguments, and the manager's send runs through the gate and the effect ledger, as any approved card does. One pinch, through the wall that exists.
- Considered and not proposed: a "draft" the phone sends as the person's own input. It would put text the manager wrote into a session as the person's, with no card and no effect record, and keep the session's person profile (`control_mark_driven` skips phones); D1 ranks text Glasscode wrote as "never".

**F5. "Tell that session to rebase on main."** As F4. "That" is the focus. If the name heard is unsure ("checkout" matches two sessions), the lens shows both and asks; nothing is sent until one is picked.

**F6. "Explain what this PR changes."** The manager reads `pr.diff` and explains. The lens shows the outline one point at a time, in step with the sentences being spoken; swipe down moves ahead, swipe up goes back. *More on the phone* opens the full answer on the phone, with its pictures (scenes).

**F7. A session asks something while you walk.** A note on the lens: *checkout-fix asks: Allow npm test?* with the options. Swipe to one, pinch: the phone answers with its signed `sessions.answer`. An option that allows for good is not offered (`prompt::grants_for_good`).

**F8. A card that needs you.** "Message Sam that the fix is ready." The outbox drafts it; sending needs the person's OK. The lens shows the card with the **full** text. Allow is offered on the lens only when the whole payload fits on it; otherwise *Read it on the phone*, and Allow is there. The phone signs `approvals.decide` over the op's argument hash. A spoken "yes" does nothing.

**F9. Away from the desk.** This is the normal case, not an edge case (section 2: the server is home when the person has not used the Mac for a minute).
- **The Mac is awake** (lid open at home or at the office): the Mac stays home (its lease is renewed without presence), so F1 to F8 work as at the desk, through the Mac's door. After a wake, O9 lets the glasses' use count as presence, so the Mac claims home back.
- **The Mac sleeps**: the server is home. Questions, reads, answers from code and cards work. Acting works on the server's own sessions, which is where Server mode put the open ones before you left. A Mac session cannot be driven: the manager says so ("checkout-fix is on your Mac, which is asleep"). What only the Mac can do (its window, the messenger) is queued and said: "That waits for your Mac."

**F10. "Show it on my Mac."** Where "go to" shows is decision O8. Proposed: the lens always; the Mac's window as well while the Mac is home and you were at it (D5's presence), as the old glasses app moved the IDE to the session it opened; and always when you say "on my Mac". Otherwise a move becomes a *Show me* chip in the Mac's conversation.

## 4. The shape

```
                               one tailnet (WireGuard)
 glasses ◀─DAT (Bluetooth)─▶ iPhone app ───────────────────────────────────────────┐
  lens 600×600                │  speech on the phone (STT, TTS)                     │
  Neural Band                 │  device key in the Secure Enclave                   │
                              │                                                     │
                              ├── device link ─▶ Mac: the device door (in the app) ─┤─▶ gate ─▶ manager chat, ops
                              │    JSON lines,      Phone{device} after a signed     │
                              │    the rpc shape    handshake; Mac's tailnet only    │
                              │                                                     │
                              └── device link ─▶ server: ssh → glasscoded gate --device ─▶ gate ─▶ manager chat (M57)
                                   (the same lines over the existing SSH gate)
   which one: the manager's home (manager.home), the other as fallback
```

### The device link

One protocol, two carriers.

- **The lines** are glasscoded's `rpc` JSON lines (`{"id","method","params"}` and replies), with server-pushed events on the same connection (`{"event":…}`): conversation items as they stream, presence, notes. The Swift client is written once.
- **To the server**: the existing path. SSH with the device key to `glasscoded gate --device <id>`, the forced command, pinned host keys. The gate declares `Phone{device}`. Nothing new is exposed on the server.
- **To the Mac**: a new **device door** in the app (`crates/glasscode/src/device_door.rs`, logic in core `device_link.rs`).
  - It listens only on the Mac's tailnet address (and loopback in tests), and only while a device is paired. It closes when the app quits.
  - **Handshake.** The Mac sends a nonce, its launch id (as in #288's `input_message`) and a signature with its own door key (P-256, in the Keychain, made at pairing and pinned by the phone). The phone answers with its device key's signature over both nonces and the launch id. The door checks it against `authority/devices.json`. From then on the connection is `Phone{device}`, as an SSH session is.
  - **Wall (P1).** Enforced: WireGuard (Tailscale) carries confidentiality and peer integrity; the Rust handshake binds the connection to a paired device key; the door refuses any source outside the tailnet's ranges. Not claimed: protection from a compromised tailnet coordinator (Tailnet Lock is the person's own setting; the doc recommends it).
  - No new crate: `p256` and `ring` are already in core.
- **Which end.** A new phone read, `manager.home` (on both ends), says which machine is home and its generation. The app asks the home. If the home cannot be reached, it asks the other end, which answers in Degraded mode (reads, answers from code, cards queued), as the Mac does today without the server.
- **No relay, no port 4517.** The relay and the shared token go (section 7).

### The manager over the link

New ops, on the registry (`table_manager.rs`, `table_device.rs`) and in both allow-lists:

| Op | Who | What |
|---|---|---|
| `manager.home` | phone, read | where the manager is home, its generation, its name and state |
| `manager.ask` (phone form) | phone | `{text, voice, focus, client_id}`. Posts a *You* item with origin `Device(id)` (tier 2), marked "from your glasses", in the current topic. The answer streams back on the connection as it is written. |
| `manager.conversation` | phone, read | the conversation since a cursor, for the phone's screen |
| `manager.stop` (phone form) | phone | stop the answer it asked for (only its own) |
| `device.open` | the chat lane only | put a session, PR, project, Today or the conversation on the asking device. Navigate class. Only within 30 s of that device's message (the `may_move_screen` rule, made per surface). |
| `device.presence` | phone | glasses connected, worn, lens on; what the voice is doing. Drives the title bar indicator; the server passes it to the Mac with the sync. With O9, use through the Mac's door counts as presence for home. |

- **Focus.** The phone sends its focus (what the lens shows: view, session, PR, project) with each question. The home checks the ids and builds a `DeviceSnapshot`, used as `UiSnapshot` is: "this", "here" and "that" resolve from it, and the brain is told the person is on their glasses.
- **Showing.** From a device's question, "show" means the device (`device.open`), and the Mac's window as decided in O8 (F10). A window move outside that rule becomes a *Show me* chip on the Mac.
- **The server.** glasscoded learns `manager.ask` (phone form) and `manager.home`; its chat lane wakes when a question is posted instead of every 2 seconds, and streams; `device.open` on the server sends the event down the asking phone's connection.
- **The conversation stays one.** What is said from the glasses is in the Mac's Manager page and chat bubble, with a small glasses mark.

### Speech

Recommended (decision O3): **speech on the phone**.
- Speech to text: Apple's on-device recognizer (`SpeechAnalyzer`, iOS 26). Streaming, on the phone, free; the heard words show on the lens before they are sent.
- Text to speech: the system voices (`AVSpeechSynthesizer`, an enhanced voice), sentence by sentence as the answer streams, played to the glasses' speakers. Barge-in: a pinch or speech stops it.
- Why: it works whoever is home (the voice room needs a token only the Mac can make), needs no server, Docker or speech keys, costs nothing, sends only text, and saves two network hops.
- The LiveKit room stays the Mac's voice. The phone may join it later (slice G16 option) once the server can make a room token from its own keys.
- Meta's `MWDATSpeech` is not used: it is marked "dev and beta only".

### The title bar indicator (G-R6)

A glasses icon at the right of the title bar, beside the Server mode switch.

| Look | Means |
|---|---|
| not shown | no device paired |
| grey | paired, not connected |
| green dot | the app is connected (to this Mac or through the server) and the glasses are on |
| green dot, pulsing | you are talking from the glasses: listening, thinking or speaking |
| amber dot | connected, but the glasses' lens is off or the manager cannot answer it (why, in the tooltip) |

A click opens a small panel: the device, where it is connected (this Mac, or the server), what it last asked, *Open the conversation*, *Disconnect*, *Devices…* (Settings › Devices). There is no tab and no lens mirror.

## 5. The lens and the Neural Band

The lens is 600 by 600, drawn with DAT's FlexBox (text, icons, images, buttons), redrawn whole on each change. It dims after 20 seconds and sleeps after 25; the app wakes it only for something the person asked for or a note that needs them.

### Gestures

| Gesture | Does |
|---|---|
| **Pinch** (index) | Talk, from the glance and from any card. On a focused button or row: that button or row. |
| **Swipe up / down** | Move between rows and buttons; scroll a long answer one point at a time. |
| **Swipe right** | Open the focused row (into a session, a PR). |
| **Swipe left** | Back one card. From the glance: nothing (it never ends the session). |
| Pinch while it speaks | Stop speaking and listen. |
| Middle pinch, the glasses' own back | Meta's: they may end the lens session. The app starts a new one (today's "stay on Glasscode"). |

Which band events reach the app, and whether a held pinch can be told from a pinch, is proof G01.3. DAT's `InputEvent` has `nav(up/down/left/right)`, `select`, `back`, `button`, `capture` and `drag`.

Optional (decision O5): a wake word, the manager's name, heard on the phone on-device. Off by default: it keeps the microphone open.

### Cards

1. **Glance** (rest): the manager's initials, where it is home ("On your Mac" / "On the server"), one line ("2 need you · CI failed on #42"), up to three rows. Pinch talks.
2. **Listening**: a level bar, the words as heard. A pause sends; swipe left cancels.
3. **Working**: "Thinking", then the action lines as they happen ("Reading checkout-fix…", "Opened #42").
4. **Answer**: up to five short lines, said aloud at the same time; chips under it (sessions, PRs) as buttons. *More on the phone* when longer.
5. **Session** (from `device.open`): title, branch, state, last reply in two lines, the open question.
6. **Pull request**: title, checks by name, reviews, what CI says.
7. **A session act to approve**: "Send to checkout-fix: …" with the full text, Allow and Deny (a card, as in F4).
8. **Card to approve**: the op's own sentence and its full payload, Allow and Deny. Allow only when the payload fits; else *Read it on the phone*.
9. **Question from a session**: its options as rows; "allow for good" is left out.
10. **Note**: a session finished, failed, or needs you. Pinch opens it; swipe left lets it go.

The phone's own screen shows the same conversation in full, the session and PR views, the view-only terminal and the cards. It is the place for long text and settings.

## 6. Rights: what a paired phone may do (D2, D3)

**Recommended (decision O2): narrower than the person.** A phone is its own caller, with a fixed list, tier 2 words, and the person's yes only through a signed approval or the person's own input.

| Right | Rule | Plane (P1) | Code |
|---|---|---|---|
| Who it is | `Phone{device}`, from the door's handshake or the SSH gate's `--device`; never from the text | Enforced: Rust handshake against `authority/devices.json`; sshd and the forced command on the server | `device_link.rs` (new), `glasscoded/src/main.rs` |
| What it may call | one shared list for both ends: reads (`state`, transcript, peek, PR view and checks, Today, approvals list, `manager.home`, `manager.conversation`), `manager.ask`, `manager.stop` (its own), `device.presence`, signed `approvals.decide`, `sessions.send/keys/answer` to an agent's session, `sessions.attach` view only, `phones.push_token` | Enforced: `caller::daemon_refusal` (server) and the door (Mac), from one `caller::PHONE_ALLOWED` | `caller.rs`, `device_door.rs` |
| Ops as the person | none beyond the list: `gate::phone()` stops allowing every op that is not person-only, and runs `mode_raised` | Enforced: gate | `gate/mod.rs` `phone()`; `caller::rule` agrees |
| Its words | tier 2 (`Device`), typed or spoken | Enforced: words ledger | `words.rs` (`command_origin`, `tier_of`) unchanged |
| What the manager does on its question | the project's level default; no chat grant (tier 2 cannot back one) | Enforced: gate (`Manager` chat lane) | unchanged |
| Beyond the level | a card; the phone approves with `approvals.decide` signed over the argument hash; from the lens only when the payload fits; only while the glasses are worn or the phone is unlocked | Enforced: gate `PHONE_CONFIRMS`, the signature, and the payload length (the home checks it). Guarded: "worn or unlocked" and "it was shown on the lens" are what the phone app says | `gate/mod.rs`, `today.rs` |
| The person's own input | answers to a session's question, and words the person speaks or types for a session, go as signed `sessions.send/answer`: agent sessions only, no `!`, no `BTab`, no keys while a question is open, no "allow for good". Never text the manager wrote. | Enforced: `phone_session_refusal`, `remote::check_relayed` (#288) | unchanged; the app now signs |
| Spoken yes | never binds | Enforced: only a signed decide approves | unchanged |
| Person-only ops (levels, policy, pairing, pause release, settings) | never from a device | Enforced: gate | unchanged |
| Terminal | view only | Enforced: tmux `-r` (#305) | unchanged |
| Voice room token | only `Ui`, `ShellTab` or a paired phone over a link; `Terminal` refused | Enforced: op check | `ops_run.rs` `manager_voice_token` (G02) |
| Moving a view | `device.open` only by the chat lane, within 30 s of that device's message; the Mac's window only on F10 | Enforced: gate (`may_move_screen` per surface) | `gate/mod.rs` |

**The other two options**, for the owner:
- **The person's own** (today's `gate::phone()`): the phone runs ordinary ops as the person, and its typed text is tier 1. Fewer cards, but a lost unlocked phone, or a misheard sentence, acts as the person. Not recommended.
- **Face ID for tier 1**: text typed on the phone and signed with a key that needs Face ID counts as tier 1; voice stays tier 2. Useful later; it does not help hands-free use.

## 7. What is removed

**Asked by the owner:**
- The right panel's **Glasscode tab** and the lens mirror: `RightTab::Glasscode`, `glasscode_panel` and the mirror code in `glasscode_ui.rs` (`GlassesView`, `GlassesRow`, `glasses_fresh`, `glasses_current`, `pending_open`, the 200 ms poll of `/api/voice/state`), its entries in `shell.rs`, `capabilities.rs` (`TABS`) and `right_panel.rs`, and `ui.panel glasscode`.
- The glasses app's **menu**: `GlasscodeFlow.swift`'s Home ▸ Sessions ▸ Session, Talk and Voice, `GlasscodeScreens.swift`, `GlassesHUD.swift`, and the phone mirror in `ContentView.swift`. Rewritten around the manager (section 5).
- **Glasscode for iPhone** (`ios/`, outside this repository): retired. Its server side stays.

**Proposed (decision O4):**
- The **ElevenLabs copilot**: `voice/server.mjs`, `voice/public/`, `voice/memory.mjs`, `voice/scripts/`, `voice.config.json`, the `luna-voice` helper (`glasscode-voice`), `GlasscodeVoice.swift`, `WakeWord.swift` ("Hey Glasscode"), and the ElevenLabs Swift package. The Today microphone, the status bar microphone and the voice screen (U19) then start the manager's own voice (`manager_voice.rs`), so `glasscode_ui.rs` keeps only what the voice screen needs, or goes into it.
- The **shared token and its client**: `GlasscodeConfig.swift`, `GlasscodeControl.swift`, `GlasscodeLink.swift`.
- The **relay's use by the glasses**: `GlasscodeRelayProxy.swift`, and the relay's `control` channel to port 4517 (G02). The relay itself stays: it also carries SSH between the Mac and the server (`ServerConfig::through_relay`), which this plan does not touch.
- Settings › Server's row "Reach Glasscode on your network" (port 4517).
- [`CONNECTIVITY.md`](CONNECTIVITY.md) is rewritten for the tailnet and pairing.

If the owner keeps the copilot, G02 still binds `server.mjs` to loopback and refuses voice tokens to `Terminal`.

## 8. Latency

Measured so far: the Claude brain's first text 1.9 to 3.2 s and its full answer 3.8 to 5.7 s with one tool call (P01, P11); spawn to ready 0.5 to 0.7 s. Codex streams no text. The server's chat lane polls every 2 s and does not stream.

| Step | Budget |
|---|---|
| End of speech detected (on the phone) | 0.5 to 0.7 s of silence |
| Lens shows what was heard, an earcon | 0.15 s after that |
| Phone to home over the tailnet | 0.05 s direct; up to 0.3 s through a Tailscale relay |
| Answer from code | 0.1 s |
| Brain, first sentence | 2 to 3.5 s (Claude, streamed); Codex at its end |
| Text to speech, first sound | 0.2 s after the first sentence |

**Targets** (G17 measures them, as `chat.turn` events plus the phone's own clock):
- What was heard on the lens: ≤ 0.2 s after the pause.
- An answer from code, first sound: ≤ 1.2 s p50.
- A brain answer on the Mac (Claude), first sound: ≤ 3.5 s p50, ≤ 6 s p95. Full answers take longer; the first sentence is spoken while the rest streams.
- The same through the server once it streams and wakes on post (G08); until then add up to 2 s and the whole answer time.

## 9. Decisions for the owner

| # | Decision | Proposal |
|---|---|---|
| O1 | The one app | The glasses app (owner, 2026-11-21). It stays in `glasses/` and is renamed Glasscode. Glasscode for iPhone is retired. Its source is not here: if it is found, G11 takes its SSH transport and pairing (L instead of XL). Its App Store and TestFlight records, bundle id and push key: reuse the push key for the new bundle id. |
| O2 | A phone's rights | Narrower (section 6). |
| O3 | Speech | On the phone (section 4). The voice room stays the Mac's. |
| O4 | The old voice stack and the relay | Remove them (section 7). |
| O5 | How to talk | Pinch from anywhere; the wake word as an option, off. |
| O6 | Hardware and Meta | Do you have Ray-Ban Display with the Neural Band, Developer Mode on, a Meta account in a supported country? G01 needs them. Move DAT from 1.0.1 to 1.1.0. |
| O7 | The Mac's door | The app's own door on the tailnet address (section 4). The alternative, macOS Remote Login with a forced command, needs an admin setting and writes `~/.ssh/authorized_keys` on the Mac. |
| O8 | Where "go to" shows | The lens always; the Mac's window as well while the Mac is home and you were at it; always when you say "on my Mac" (F10). The glasses icon goes in the title bar, beside the Server mode switch. |
| O9 | Home while you use the glasses | Use through the Mac's door counts as presence, so an awake Mac stays home and its sessions can be driven from the glasses. Without it, away from the desk the server is home and Mac sessions cannot be driven (F9). |

## 10. Slices

The format of [`manager-v2-slices.md`](manager-v2-slices.md). Sizes: S (a day), M (two to three), L (a week), XL (more). One slice, one worktree, one pull request; merge `origin/main` often, because `caller.rs`, `gate/mod.rs` and `glasscoded/src/daemon.rs` change under other work.

Order: wave 0 (G00 to G02), wave 1 (G03, G05, G06), wave 2 (G04, G07, G08, G10), wave 3 (G11 to G14), wave 4 (G09, G15 to G19).

### G00 Plan approved (wave 0, no code)
- **Steps:** the owner answers section 9; this document records the answers and becomes issue 2.

### G01 Proofs on the hardware (wave 0, M)
- **Steps:** on real glasses with DAT 1.1.0, in a scratch build:
  1. the microphone: does opening the glasses' microphone (HFP) end the lens session (as `AudioRoute.swift` says)? How clear is the phone's microphone in a pocket? Is there a DAT audio stream?
  2. the app in the background: does the lens keep updating, and for how long, with the screen locked?
  3. the band: which `InputEvent`s arrive for pinch, held pinch, swipes, middle pinch; does `back` end the session?
  4. the lens: redraw time of a full card; the 20 s dim; a card with eight rows.
  5. speech: `SpeechAnalyzer` accuracy on twenty sentences with session names, PR numbers and branch names; the time from the pause to the final text.
  6. Tailscale on iOS: direct or relayed, and the round trip, at home and on cellular.
- **Tests:** none; the results go into `docs/glasses-proofs.md` with numbers.
- **Server:** none.

### G02 Close the shared-token door (wave 0, S)
- **Files:** `voice/server.mjs`, `crates/glasscoded/src/relay_link.rs`, `crates/glasscode/src/ops_run.rs` (`manager_voice_token`), `crates/glasscode/src/settings_ui.rs`.
- **Steps:**
  1. bind `server.mjs` to loopback by default;
  2. the relay's `control` channel no longer joins port 4517 (relayed requests arrive as loopback and skip the token); `ssh` stays;
  3. refuse `manager.voice_token` to `Terminal` (and to `Session`, `Manager` as today);
  4. say in Settings that the glasses reach the Mac again with the new app. Until G11 the current glasses app cannot reach the Mac.
- **Tests:** `voice_token_refused_to_a_terminal`; `voice_token_for_the_person`; `relay_control_channel_is_closed`.
- **Verify:** `cargo test -p glasscode -p glasscoded`; `curl` from another tailnet device to port 4517 is refused.
- **Server:** the relay change. **Windows:** none.

### G03 The device link protocol (wave 1, L)
- **Files:** new `crates/glasscode_core/src/device_link.rs`; `remote.rs` (events on a connection); `caller.rs`.
- **Steps:** the handshake (messages, signatures, launch id, nonces kept per run); the line format with events; the door key (make, keep in the Keychain, rotate); one `PHONE_ALLOWED` for both ends.
- **Tests:** `handshake_needs_a_paired_key`; `handshake_replayed_is_refused`; `another_launch_is_refused`; `a_line_before_the_handshake_is_refused`; `the_lists_agree_on_both_ends`.
- **Server:** the list. **Windows:** builds; not offered.

### G04 The Mac's device door (wave 2, L)
- **Files:** new `crates/glasscode/src/device_door.rs`; `control_ui.rs` (calls from the door as `Phone{device}`); `workspace.rs`.
- **Steps:** listen on the tailnet address only while a device is paired; one thread per connection; requests go through the registry and the gate as `Phone{device}`; events out (conversation, notes, presence); close on quit.
- **Depends on:** G03, G05, G06.
- **Tests:** `door_refuses_a_lan_address`; `door_caller_is_the_phone`; `door_runs_only_the_list`; `door_closes_without_devices`.
- **Verify:** `cargo test -p glasscode_core -p glasscode`.
- **Demonstration:** in a scratch home, a test client with a scratch key pairs, connects and reads `state`; an unpaired key is refused.
- **Server:** none (the server keeps SSH). **Windows:** not offered.

### G05 Pairing without a server; Settings › Devices (wave 1, M)
- **Files:** `crates/glasscode/src/pair_ui.rs`, `settings_ui.rs`, `glasscode_core/src/gate/authority.rs` (`pin_device`).
- **Steps:** pairing pins the device on the Mac with or without a server; the reply carries the door's address and key, and the server's SSH endpoint when one is paired (`phones.add` as today); Settings › Devices lists them, with Remove (on both ends).
- **Depends on:** G03.
- **Tests:** `pairing_without_a_server_pins_the_device`; `remove_unpins_on_both`; `pairing_reply_carries_the_door_key`.
- **Verify:** `cargo test -p glasscode`.
- **Demonstration:** in a scratch home with no server, pair a test client by its QR payload; Settings › Devices lists it.
- **Server:** `phones.add` as today when one is paired. **Windows:** not ported (S16).

### G06 A phone's rights (wave 1, M)
- **Files:** `crates/glasscode_core/src/gate/mod.rs` (`phone`), `caller.rs` (`rule`), `glasscoded/src/daemon.rs`.
- **Steps:** section 6's table, as code: the list, `mode_raised` for phones, the approval only while worn or unlocked (a flag the signature covers), the "fits on the lens" length checked by the home.
- **Depends on:** G03.
- **Tests:** one per row of the table (`phone_cannot_start_a_bypass_session`, `phone_words_are_tier_two`, `lens_approval_refused_for_a_long_payload`, …).
- **Verify:** `cargo test -p glasscode_core -p glasscoded`; M02's callers evals.
- **Server:** the same list in `daemon_refusal`. **Windows:** the same core.

### G07 The manager over the link (wave 2, L)
- **Files:** `crates/glasscode/src/manager_chat.rs`, `ops_run.rs`, `glasscode_core/src/ops/table_manager.rs`, new `table_device.rs`, `runtime/chat.rs` (`DeviceSnapshot`), `conversation.rs` (the device mark).
- **Depends on:** G03, G04, G06.
- **Steps:** `manager.home`, `manager.ask` (phone form, streamed), `manager.conversation`, `manager.stop`; the focus snapshot; the brain's instructions for a device's turn ("the person is on their glasses: show with `device.open`; keep spoken answers short"); O8's window rule; with O9, door use counts as presence in `home_ui.rs`.
- **Tests:** `device_question_is_tier_two_and_marked`; `focus_resolves_this`; `answer_streams_to_the_asker_only`; `window_move_follows_o8`; `door_use_keeps_an_awake_mac_home` (O9).
- **Verify:** `cargo test -p glasscode_core -p glasscode`.
- **Demonstration:** a test client asks "what needs me?" through the door of a scratch app; the answer streams back, and the Manager page shows the question with the glasses mark.
- **Server:** G08. **Windows:** builds; no door.

### G08 The server: the manager for phones (wave 2, L)
- **Files:** `crates/glasscoded/src/brain.rs`, `daemon.rs`, `manager.rs`, `home.rs`.
- **Steps:** the phone ops of G07 on glasscoded; the chat lane wakes on a posted question and streams; `device.open` and notes go down the phone's connection; `manager.home` for phones.
- **Depends on:** G03, G06, G07.
- **Tests:** `phone_asks_the_server_home`; `chat_wakes_on_post`; `server_answer_streams`; `mac_session_act_says_why`; on the Docker test server, a paired phone stand-in.
- **Verify:** `cargo test -p glasscoded`; `script/test-server/run.sh`.
- **Demonstration:** with the scratch Mac app quit, a phone stand-in asks the server; the answer streams, and reopening the Mac shows it in the conversation.
- **Server:** this slice. **Windows:** client only.

### G09 Cards, questions and notes on the device (wave 4, M)
- **Files:** `gate/pending.rs`, `notify_ui.rs`, `ops_run.rs`, `table_device.rs`; `crates/glasscoded/src/today.rs`.
- **Depends on:** G06, G07, G08.
- **Steps:** pending cards, session questions and turn-report notes as device events on both ends; the lens's Allow only for a payload the home says fits.
- **Tests:** `card_reaches_the_asking_device`; `lens_allow_refused_for_a_long_payload`; `allow_for_good_never_offered`.
- **Verify:** `cargo test -p glasscode_core -p glasscode -p glasscoded`.
- **Demonstration:** in a Careful scratch project, "run the tests" from a test client gives a card; a signed Allow runs the send once.
- **Server:** the same events from glasscoded. **Windows:** none.

### G10 The IDE: no tab, a glasses indicator (wave 2, M)
- **Files:** `right_panel.rs`, `glasscode_ui.rs`, `shell.rs` (`render_title_bar`), `capabilities.rs`, `workspace.rs`, `manager_ui.rs` (the glasses mark).
- **Steps:** remove the tab and the mirror; the indicator and its panel from `device.presence` (direct or through the server's sync).
- **Depends on:** G07 (presence).
- **Tests:** `no_glasscode_tab`; `indicator_states_from_presence`.
- **Verify:** `cargo test -p glasscode`; `GLASSCODE_DEBUG_OPEN` shows each state.
- **Server:** presence comes with the sync. **Windows:** the indicator shows; no voice.

### G11 The one app: identity, link and routing (wave 3, XL; L if the `ios/` transport is found)
- **Files:** `glasses/GlasscodeGlasses/` (renamed Glasscode): new `Link/` (device key in the Secure Enclave, the door carrier, the SSH carrier with swift-nio-ssh, routing by `manager.home`, reconnect), `Pairing/` (QR, `glasscode://pair`), `project.yml` (iOS 26, DAT 1.1.0, no ElevenLabs).
- **Depends on:** G03, G04, G05, G08 (the server's ops), O1.
- **Steps:** the key, both carriers, routing to the home and fallback to the other end, signed input (#288's `input_message`), the old `GlasscodeControl`, `GlasscodeLink`, `GlasscodeConfig` and relay proxy deleted.
- **Tests:** unit tests of the link against a stand-in door; the Docker server's phone path (`script/test-server`).
- **Verify:** `xcodebuild test` on the simulator.
- **Demonstration:** on a phone, pair with a scratch Mac app; the app shows where the manager is home, and asks it.
- **Server:** uses G08. **Windows:** none.

### G12 The lens and the band (wave 3, L)
- **Files:** new `Lens/` (cards of section 5), `Band/` (gestures), the "stay on Glasscode" restart kept; `GlasscodeFlow.swift`, `GlasscodeScreens.swift`, `GlassesHUD.swift` deleted.
- **Depends on:** G01 (which events arrive), G11.
- **Tests:** the card builders (pure, one per card); the focus and gesture state machine (pure); the mock device for the session lifecycle (`StayOnTests` keeps working). Whether the mock draws display content is unknown (G01).
- **Verify:** `xcodebuild test`.
- **Demonstration:** on the glasses, F1 and F2 with a scratch Mac.
- **Server:** none. **Windows:** none.

### G13 Speech on the phone (wave 3, L)
- **Files:** new `Speech/` (on-device speech to text, voices by sentence, barge-in, the audio route from G01); `AudioRoute.swift`, `BackgroundKeepAlive.swift` kept and adapted.
- **Depends on:** G01, G11.
- **Tests:** the turn-taking state machine (pure), with recorded audio; the pause length; a pinch stops the voice.
- **Verify:** `xcodebuild test`.
- **Demonstration:** on the glasses, ten questions; the time to the first sound is logged.
- **Server:** none. **Windows:** none.

### G14 The phone's screens (wave 3, L)
- **Files:** new `Phone/` (SwiftUI).
- **Depends on:** G11.
- **Steps:** the conversation, session and PR views, the view-only terminal, cards with their full payload, Settings (devices, voice, wake word).
- **Tests:** view models (pure); UI tests in the simulator.
- **Verify:** `xcodebuild test`.
- **Demonstration:** the same conversation on the phone and on the Mac's Manager page.
- **Server:** none. **Windows:** none.

### G15 Notes and push (wave 4, M)
- **Files:** `crates/glasscoded/src/push.rs`; the app's notification handling.
- **Depends on:** G09, G11.
- **Steps:** the server's push (APNs) for the new bundle id; lens notes when the app is connected, push when it is not.
- **Tests:** `push_once_per_need` (kept); the app's routing of a push to its card.
- **Verify:** `cargo test -p glasscoded`; a push on a phone.
- **Server:** this slice. **Windows:** none.

### G16 The old voice stack goes (wave 4, M; on O4)
- **Files:** section 7's list; `glasscode_ui.rs`, `voice_view.rs`, `today_ui.rs`, `status_bar.rs`, `settings_ui.rs`.
- **Depends on:** G02, G10.
- **Steps:** delete the list; the voice screen and microphones start the manager's voice (`manager_voice.rs`). Option: a room token made on the server from its own LiveKit keys, so the phone can join the room.
- **Tests:** `mic_starts_the_managers_voice`; the build has no reference to port 4517.
- **Verify:** `cargo test -p glasscode`; `cargo check` for Windows.
- **Server:** the relay commands go. **Windows:** no voice, as today.

### G17 Latency and a soak (wave 4, S)
- **Depends on:** G07, G08, G13.
- **Steps:** measure section 8's targets; a two-hour walk with the glasses (Mac home, then server home, then a switch).
- **Verify:** the numbers in `docs/glasses-proofs.md`.

### G18 Distribution (wave 4, S)
- **Depends on:** G11 to G14.
- **Steps:** TestFlight for the owner; Meta's private Beta release channel; the MetaAppID, client token and team id from Meta's developer center (never in the repository).

### G19 Docs (wave 4, S)
- **Depends on:** everything before it.
- **Steps:** `architecture.md`'s iPhone section becomes "The glasses app"; `CONNECTIVITY.md`; `voice/README.md`; this plan's results.

## 11. Risks

| Risk | What it means | What the plan does |
|---|---|---|
| Meta's band input and speech are "dev and beta only" | They work in Developer Mode and in a private Beta channel, not in a public release | The app is for the owner's own use first; speech is on the phone; band input is the open risk if Meta keeps the limit |
| Public publishing is not open | Meta says submission is "coming soon" | Private channel and TestFlight only |
| The microphone | The glasses' microphone as a Bluetooth call may end the lens session; the phone's microphone in a pocket hears less | G01.1 measures it before any speech work |
| Background | iOS suspends apps; today the app plays silence to stay alive | G01.2; acceptable for TestFlight, an App Review risk later |
| Away from the desk the server is home | Its manager cannot drive Mac sessions; its chat does not stream yet | O9 keeps an awake Mac home while the glasses are used; Server mode moves the open sessions before you leave; G08 is on the critical path |
| Brain latency | 2 to 6 s for a brain answer | Answers from code first, streaming, an earcon, the lens shows progress |
| The server does not stream yet | +2 s and whole answers through the server | G08 |
| Mishearing names | "checkout" matches two sessions | The lens shows what was heard and asks when unsure; nothing acts on an unsure target |
| Careful projects ask a lot | Tier 2 means a card for most acts | One pinch to approve on the lens; the full text is shown |
| A new door on the Mac | A listening socket | Tailnet only, closed without devices, handshake first, the same list as the server, its own tests |
| Tailscale on the phone | The VPN can be off or relayed | The app says so on the glance; G01.6 measures |
| Parallel security work | `caller.rs`, `gate/mod.rs`, `daemon.rs` change often | Small slices, merge `origin/main` before each |

## 12. Meta's toolkit: what is known, what is not

From Meta's pages (read 2026-11-21):
- The Wearables Device Access Toolkit left preview at Connect 2026; its newest version is 1.1.0 (Meta AI app V292, Display glasses V130). Display access opened in May 2026.
- Supported: Ray-Ban Meta (Gen 1 and 2), Meta Ray-Ban Display, Oakley Meta HSTN and Vanguard; capabilities differ.
- iOS modules: MWDATCore, MWDATDisplay, MWDATInputs, MWDATSpeech, MWDATCamera, MWDATMotion, MWDATMockDevice.
- The lens: 600 × 600; FlexBox, Text, Image, Button, Icon, Video (MP4, at most 400 px a side); vertical scrolling only; each send replaces the whole display; it dims after 20 s and sleeps at 25 s; "the glasses do not retain application state"; the system may interrupt for calls and notifications.
- Inputs: `nav(up/down/left/right)`, `select`, `back`, `button`, `capture`, `drag`; marked "dev and beta only".
- Speech: on-device recognition on the glasses; "dev and beta only".
- "Only the Device Access Toolkit can use the glasses' camera or capture microphone audio."
- Testers: one private "Beta" channel per integration, with a limit shown in the developer center (100 during the preview); testers need a regular Meta account. Full access only in countries where the glasses are sold. Developer Mode: in the Meta AI app.

Not stated by Meta's pages, so proved in G01: how an iOS app gets the glasses' microphone, whether the lens updates while the app is in the background, whether the camera streams during a lens session, and whether the mock device draws display content.

## 13. Results (2026-11-21)

Built on `glasscode/mobile-glasses-manager`, one commit or more per slice, with `origin/main`
merged in between. Nothing is pushed.

| Slice | Built | Checked by |
|---|---|---|
| G00 | The owner accepted section 9 | this document |
| G01 | **Not run: it needs the glasses** (O6) | waiting for the owner |
| G02 | `server.mjs` on loopback; the relay's control channel closed; the voice token rule (main's, #327, taken in the merge) | `relay_control_channel_is_closed` |
| G03 | `glasscode_core::device_link`; one `PHONE_ALLOWED` | 11 tests, the pinned cross-language vector |
| G04 | `crates/glasscode/src/device_door.rs` | 6 tests, and the Swift link code end to end (`door_end_to_end_with_the_swift_client`) |
| G05 | Pairing without a server; Settings › Server › Glasses | the app's tests |
| G06 | `gate::phone()`, `caller::rule` | `a_paired_phone_never_acts_as_the_person`, `phone_cannot_start_a_bypass_session`, `the_rules_list_agrees_for_a_phone`, the callers evals |
| G07 | `manager.home/ask/conversation/stop`, `device.presence`, `device.list`, `device.open`, the glasses block, O8, O9 | registry goldens, chat tests |
| G08 | `crates/glasscoded/src/phone_manager.rs`, the chat lane woken and streaming, presence in the sync | 4 tests |
| G09 | Cards and questions through Today; `shown` = lens checked against `LENS_FITS` | `lens_allow_refused_for_a_long_payload` |
| G10 | No Glasscode tab; the title bar's glasses icon | `indicator_states_from_presence` |
| G11 to G14 | The app, rebuilt (`glasses/GlasscodeGlasses`) | 17 Swift tests; `StayOnTests` on the simulated glasses |
| G15 | Push registration; the server's topic | — |
| G16 | The ElevenLabs copilot removed; the voice screen starts the manager's voice | `the_screen_follows_the_managers_voice` |
| G17 | **Not measured: it needs the glasses** | waiting for G01 |
| G18 | **Not done: TestFlight and Meta's channel need the owner's accounts.** DAT stays at 1.0.1 until G01 shows 1.1.0 changes nothing | waiting for the owner |
| G19 | `architecture.md`, `CONNECTIVITY.md`, `voice/README.md`, this section | — |

**Changed from the plan while building:**
- Speech to text uses `SFSpeechRecognizer` on the device (with `requiresOnDeviceRecognition` where the
  language has it), not `SpeechAnalyzer`: it runs on iOS 18 and is well known. `SpeechAnalyzer` can
  replace it after G01 measures both.
- The relay stays for the Mac and the server (section 7, corrected).
- Under Automatic, an awake Mac keeps home without presence (section 2 and F9, corrected); O9 only
  matters after a wake.
- The phone's on-screen pad repeats the band's moves for when the band is not attached.
- `StayOnTests` passes each test on its own; in one full run one test failed once with the
  simulated glasses' "No eligible device available" after the test before it. It passed twice
  alone.

**Not tested here**, because they need hardware or accounts: the app on a phone and on the
glasses, the band's real events, the microphone path, background running, the SSH carrier against
a real server (it builds; the gate side is covered by glasscoded's tests), push delivery, and the
latency targets of section 8.
