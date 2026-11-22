"""The manager's voice worker: speech to text, and text to speech. Nothing else.

It joins the manager's LiveKit room as `manager-voice` when the first client (the Mac
helper, the phone) comes in. It listens to the client's microphone, and when the person
finished a sentence meant for the manager it publishes the text on `manager.heard`. Text
that arrives on `manager.say` it speaks on its own audio track. The manager's brain is not
here: it runs in Glasscode (or, with MANAGER_BRAIN=server, in `glasscoded` on this machine).

Messages in the room (JSON, reliable data packets, the topic says which):

    manager.heard   worker -> clients   {"text": "...", "final": true, "from": "<identity>"}
    manager.say     client -> worker    {"id": "...", "text": "..."}
    manager.stop    client -> worker    {}                      stop speaking now
    manager.state   worker -> clients   {"state": "idle|listening|thinking|speaking", "id": "..."}
                                        (+ "error": "..." when a speech service failed)
    manager.config  client -> worker    {"name": "AI Mira", "listen": "wake|always|never"}

`listen` is kept per client: "wake" passes on what starts with the manager's name (or comes
soon after it spoke), "always" passes on every sentence (the person chose to talk), "never"
nothing. `from` in `manager.heard` is the LiveKit identity of the client whose microphone
heard it; a client only answers what carries its own identity. One microphone is listened
to at a time: a client that asked for "always" first, else the one that joined last.

Built on LiveKit Agents 1.8.12. The supported way to run "speech to text -> my own code ->
text to speech" there is an AgentSession WITHOUT a language model: the session does voice
detection, transcription, turn taking, interruption and playback; the agent's
`on_user_turn_completed` hook gets each finished sentence (and stops the session from
answering by itself), and `session.say()` speaks. See README.md.

With a talker (MANAGER_TALKER set, see providers.py and talker.py) the session also has a fast
language model: for a client that said `"talker": true` in `manager.config`, the worker holds
the conversation itself and hands the work to the manager through that client:

    manager.heard   worker -> clients   {..., "handled": true}   shown, not asked: the talker answers
    manager.said    worker -> clients   {"text": "..."}          what the talker itself says (captions)
    manager.tool    worker -> a client  {"call", "name", "arguments", "to"}   run one of the talker's tools
    manager.result  client -> worker    {"call", "text"}         what that tool found
    manager.config  client -> worker    {..., "talker": true, "place": "mac|glasses|phone"}

The manager's answer still comes on `manager.say`; the talker hears it too (it goes into its
conversation), and a later sentence of an answer the person talked over is dropped.

Run: `python worker.py start` (the Dockerfile does), or `python worker.py dev` to try it.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import shlex
import time

from livekit import rtc
from livekit.agents import (
    Agent,
    AgentServer,
    AgentSession,
    JobContext,
    JobProcess,
    JobRequest,
    StopResponse,
    cli,
    room_io,
    stt,
)
from livekit.agents.job import DEFAULT_PARTICIPANT_KINDS
from livekit.plugins import silero

import providers
import talker
from providers import env
from wake import Attention, is_echo, listen_value

log = logging.getLogger("manager-voice")

# The room protocol. Keep in step with crates/glasscode_core/src/manager/voice.rs.
WORKER = "manager-voice"
HEARD = "manager.heard"
SAY = "manager.say"
STOP = "manager.stop"
STATE = "manager.state"
CONFIG = "manager.config"
SAID = "manager.said"
TOOL = talker.TOOL
RESULT = talker.RESULT
# How long a device has to answer one of the talker's tools (it answers at once: the
# manager's own work runs in the background).
TOOL_SECS = float(env("MANAGER_TOOL_SECS", "12"))

ROOM = env("MANAGER_ROOM", "manager")
# How long after the manager spoke (or was spoken to) speech still counts without its name.
WINDOW_SECS = float(env("MANAGER_WINDOW_SECS", "20"))
# How long "thinking" is shown while nothing comes back for what was heard.
THINKING_SECS = float(env("MANAGER_THINKING_SECS", "90"))
# "client": Glasscode answers (default). "server": `glasscoded manager ask` on this machine does.
BRAIN = env("MANAGER_BRAIN", "client").lower()
BRAIN_COMMAND = shlex.split(env("MANAGER_BRAIN_CMD", "glasscoded manager ask"))
BRAIN_TIMEOUT_SECS = float(env("MANAGER_BRAIN_TIMEOUT_SECS", "120"))
# "stream": the microphone goes to the speech-to-text service as it comes (fastest).
# "segments": only what the voice detector takes for speech is sent, sentence by sentence
# (a little slower; less is sent and paid for).
STT_MODE = env("MANAGER_STT_MODE", "stream").lower()
# 1: write what was understood, and what became of it, to the log (to see why it does not
# react). Off by default: what is said near the microphone does not belong in a log.
LOG_HEARD = env("MANAGER_LOG_HEARD") == "1"


class Ears(Agent):
    """The session's agent. Without a talker it only hands each sentence on; with one it lets
    the talker answer what a talker client said to the manager."""

    def __init__(self, voice: "ManagerVoice", tools: list) -> None:
        super().__init__(instructions=voice.talker_instructions(), tools=tools)
        self._voice = voice

    async def on_user_turn_completed(self, turn_ctx, new_message) -> None:
        text = await self._voice.heard(new_message.text_content or "")
        if text is None:
            # The session must not answer by itself, and the sentence is not kept.
            raise StopResponse()
        # Without the manager's name in front: the talker answers what was asked.
        new_message.content = [text]

    def tts_node(self, text, model_settings):
        # Never an id aloud, whatever the talker wrote.
        return Agent.default.tts_node(self, talker.speakable_stream(text), model_settings)

    def transcription_node(self, text, model_settings):
        return Agent.default.transcription_node(self, talker.speakable_stream(text), model_settings)


class ManagerVoice:
    """One stay in the room: from the first client coming in until the room is empty."""

    def __init__(self, ctx: JobContext) -> None:
        self.ctx = ctx
        self.room = ctx.room
        # The name and the way of listening of the client that is listened to right now.
        self.attention = Attention(env("MANAGER_NAME", "Manager"), listen_value(env("MANAGER_LISTEN", "wake")) or "wake", WINDOW_SECS)
        # Until a client says otherwise: how to listen, and the manager's name (it has one
        # name, so the last one any client sent holds for all).
        self.default_listen = self.attention.listen
        self.last_name = self.attention.name
        self.session: AgentSession | None = None
        # Clients in the room, in the order they came, and what each asked for in
        # `manager.config` ({"name", "listen"}).
        self.clients: list[str] = []
        self.asked: dict[str, dict[str, str]] = {}
        # The client whose microphone is listened to.
        self.listened: str | None = None
        # What is being spoken or waits to be: (speech handle, the id from `manager.say`, the text).
        self.speeches: list[tuple[object, str, str]] = []
        # What was spoken last and when it ended (monotonic seconds), to know its echo.
        self.last_spoken = ("", 0.0)
        # Until when "thinking" is shown (monotonic seconds); 0: not thinking.
        self.thinking_until = 0.0
        # The person is speaking, and it is (or looks) meant for the manager.
        self.user_speaking = False
        self.addressed = False
        self.error: str | None = None
        self._last_state: tuple[str, str | None, str | None] | None = None
        self._tasks: set[asyncio.Task] = set()
        self._brain_task: asyncio.Task | None = None
        self._closing = False
        # The talker (None: there is none) and its agent.
        self.talker_llm = None
        self.agent: Ears | None = None
        self._instructions_for: tuple[str, str, str] | None = None
        # The clients whose talk was given the conversation so far (`_seed`), once each.
        self._seeded: set[str] = set()
        # The talker's tool calls waiting for the device: call id -> (identity, future).
        self.calls: dict[str, tuple[str, asyncio.Future]] = {}
        self._call_seq = 0
        # Answers of the manager the person talked over: their later sentences are not spoken.
        self.cut_ids: set[str] = set()
        # What came on `manager.say` lately, so it is not told again as the talker's own words.
        self.said_texts: list[str] = []

    # ----- starting -----------------------------------------------------------------------------

    async def run(self) -> None:
        # Before joining: a client sends the manager's name the moment it sees the worker.
        self.room.on("data_received", self._on_data)
        vad = self.ctx.proc.userdata["vad"]
        try:
            speech_to_text = providers.make_stt()
            segments = STT_MODE == "segments" and speech_to_text.capabilities.streaming
            if segments:
                speech_to_text = stt.StreamAdapter(stt=speech_to_text, vad=vad)
            text_to_speech = providers.make_tts()
            self.talker_llm = providers.make_talker()
        except ValueError as error:
            # Join anyway, say why in words, and leave: the person sees it in Glasscode.
            await self.ctx.connect()
            await self._fail(str(error))
            return

        # Soniox says itself when a sentence ended (it hears the pause and the words); every
        # other service, and Soniox behind the segment adapter, leaves that to the voice detector.
        turn_detection = "stt" if env("MANAGER_STT", "deepgram").lower() == "soniox" and not segments else "vad"
        options = {
            "stt": speech_to_text,
            "tts": text_to_speech,
            "vad": vad,
            "turn_handling": {
                "turn_detection": turn_detection,
                # The voice detector decides when the person cuts in, once the speech-to-text
                # service heard two words: a cough, a keyboard or the speakers' echo alone does
                # not stop the voice in the middle of a sentence.
                "interruption": {"mode": "vad", "min_words": 2},
                # Thinking ahead on half a sentence sends it to the talker's service: only while
                # a talker client chose to talk (switched on in `_apply_talker`).
                "preemptive_generation": {"enabled": False},
            },
            "stt_context_options": {"keyterms": [self.attention.name]},
        }
        if self.talker_llm is not None:
            options["llm"] = self.talker_llm
        self.session = AgentSession(**options)
        self.session.on("user_state_changed", self._on_user_state)
        self.session.on("user_input_transcribed", self._on_transcribed)
        self.session.on("agent_state_changed", lambda _event: self._publish_state())
        self.session.on("conversation_item_added", self._on_item)
        self.session.on("error", self._on_error)
        self.session.on("close", self._on_close)

        tools = talker.tools(self.call_device) if self.talker_llm is not None else []
        self.agent = Ears(self, tools)
        await self.session.start(
            agent=self.agent,
            room=self.room,
            # Nothing drives this session from outside; the room only carries the messages above.
            session_host=False,
            room_options=room_io.RoomOptions(
                # Typed chat would go to a language model; there is none.
                text_input=False,
                # More than one device may be in the room: one leaving must not end the session.
                close_on_disconnect=False,
            ),
        )

        self._pass_name_on()
        self.room.on("participant_connected", self._on_joined)
        self.room.on("participant_disconnected", self._on_left)
        for participant in self.room.remote_participants.values():
            self._on_joined(participant)

        self._apply_talker()
        self._spawn(self._ticker())
        self._publish_state(force=True)
        log.info(
            "in room %s, speech to text %s (%s, turns by %s), text to speech %s, talker %s, brain %s",
            self.room.name,
            env("MANAGER_STT", "deepgram"),
            STT_MODE,
            turn_detection,
            env("MANAGER_TTS", "cartesia"),
            env("MANAGER_TALKER") or "none",
            BRAIN,
        )

    def _spawn(self, coroutine) -> asyncio.Task:
        task = asyncio.create_task(coroutine)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
        return task

    # ----- who is listened to -------------------------------------------------------------------

    def _is_client(self, participant: rtc.RemoteParticipant) -> bool:
        return participant.identity != WORKER and participant.kind in DEFAULT_PARTICIPANT_KINDS

    def _on_joined(self, participant: rtc.RemoteParticipant) -> None:
        if not self._is_client(participant):
            return
        if participant.identity in self.clients:
            self.clients.remove(participant.identity)
        self.clients.append(participant.identity)
        self._choose()
        # A client that just came in has not seen any state yet.
        self._publish_state(force=True)

    def _on_left(self, participant: rtc.RemoteParticipant) -> None:
        if participant.identity not in self.clients:
            return
        self.clients.remove(participant.identity)
        self.asked.pop(participant.identity, None)
        self._choose()

    def _listen_of(self, identity: str) -> str:
        return self.asked.get(identity, {}).get("listen", self.default_listen)

    def _choose(self) -> None:
        """Decide whose microphone is listened to, and listen the way that client asked.

        A client that asked for "always" (the person chose to talk there) comes first, then
        one that listens for the name; among equals the one that joined last.
        """
        talking = [identity for identity in self.clients if self._listen_of(identity) == "always"]
        awake = [identity for identity in self.clients if self._listen_of(identity) != "never"]
        chosen = (talking or awake or self.clients or [None])[-1]
        if chosen != self.listened:
            self.listened = chosen
            self._listen_to(chosen)
        before = self.attention.name
        self.attention.name = self.asked.get(chosen or "", {}).get("name", self.last_name)
        self.attention.listen = self._listen_of(chosen) if chosen else self.default_listen
        if self.attention.name != before:
            self._pass_name_on()
        self._apply_talker()

    # ----- the talker ---------------------------------------------------------------------------

    def _talker_on(self) -> bool:
        """The talker answers: there is one, and the client listened to asked for it."""
        return self.talker_llm is not None and self.asked.get(self._listened(), {}).get("talker") == "1"

    def talker_instructions(self) -> str:
        asked = self.asked.get(self._listened(), {})
        return talker.instructions(self.attention.name, asked.get("place", ""), env("MANAGER_LANGUAGE", "en"), asked.get("now", ""))

    def _apply_talker(self) -> None:
        """Keep the talker's instructions and thinking-ahead in step with who is listened to."""
        if self.session is None or self.agent is None or self.talker_llm is None:
            return
        asked = self.asked.get(self._listened(), {})
        want = (self.attention.name, asked.get("place", ""), asked.get("now", ""))
        if want != self._instructions_for:
            self._instructions_for = want
            self._spawn(self.agent.update_instructions(self.talker_instructions()))
        # A talk started again carries on: the conversation's last turns, once per client.
        turns = asked.get("recent")
        if turns and self._talker_on() and self._listened() not in self._seeded:
            self._seeded.add(self._listened())
            self._spawn(self._seed(turns))
        # Thinking ahead sends half-heard words to the talker's service: only when everything
        # said counts anyway ("always"), never while it waits for its name.
        try:
            self.session.options.turn_handling["preemptive_generation"]["enabled"] = self._talker_on() and self.attention.listen == "always"
        except (AttributeError, KeyError, TypeError):
            log.debug("thinking ahead cannot be switched in this version")

    async def _seed(self, turns: list[tuple[str, str]]) -> None:
        """Put the conversation's last turns into the talker's history."""
        try:
            ctx = self.agent.chat_ctx.copy()
            for role, text in turns:
                ctx.add_message(role=role, content=text)
            await self.agent.update_chat_ctx(ctx)
            log.info("the talk carries on from %d turns of the conversation", len(turns))
        except Exception:
            log.exception("could not give the talker the conversation so far")

    async def call_device(self, name: str, arguments: dict) -> str:
        """Run one of the talker's tools on the client that is listened to, and wait for it."""
        to = self._listened()
        if not to:
            raise talker.ToolError("No device is in the room.")
        self._call_seq += 1
        call = f"t{self._call_seq}"
        future = asyncio.get_running_loop().create_future()
        self.calls[call] = (to, future)
        try:
            await self.room.local_participant.publish_data(
                json.dumps({"call": call, "name": name, "arguments": arguments, "to": to}), reliable=True, topic=TOOL, destination_identities=[to]
            )
            text = await asyncio.wait_for(future, TOOL_SECS)
        except asyncio.TimeoutError:
            raise talker.ToolError("The device did not answer in time.") from None
        finally:
            self.calls.pop(call, None)
        if name == "ask_manager" and text.strip().lower().startswith(talker.STARTED):
            # The manager works on it: "thinking" until its answer comes on `manager.say`.
            self.thinking_until = time.monotonic() + THINKING_SECS
            self._publish_state()
        log.info("tool %s on %s: %s", name, to, text[:200].replace("\n", " / "))
        return text

    def _on_result(self, message: dict, sender: str) -> None:
        waiting = self.calls.get(str(message.get("call") or ""))
        if waiting is None or waiting[0] != sender or waiting[1].done():
            return
        waiting[1].set_result(str(message.get("text") or ""))

    def _on_item(self, event) -> None:
        """What the talker itself said: for the captions, and to know its echo."""
        item = event.item
        if getattr(item, "role", None) != "assistant":
            return
        text = (getattr(item, "text_content", None) or "").strip()
        if not text:
            return
        self.last_spoken = (text, time.monotonic())
        self.attention.keep_open(time.monotonic())
        # Its own words, not the person's: to check later what it claimed.
        log.info("said %d words: %s", len(text.split()), text[:400])
        if any(said.startswith(text) for said in self.said_texts):
            # The manager's own answer (cut short, when the person talked over it): the
            # clients show it already.
            return
        self._spawn(self._publish(SAID, {"text": text}))

    def _listen_to(self, identity: str | None) -> None:
        if self.session is None:
            return
        try:
            self.session.room_io.set_participant(identity)
        except Exception:
            log.exception("could not switch to the microphone of %s", identity)

    def _listened(self) -> str:
        return self.listened or ""

    # ----- what comes in from the clients -------------------------------------------------------

    def _on_data(self, packet: rtc.DataPacket) -> None:
        if packet.topic not in (SAY, STOP, CONFIG, RESULT):
            return
        try:
            message = json.loads(packet.data.decode("utf-8")) if packet.data else {}
        except (UnicodeDecodeError, ValueError):
            log.warning("a message on %s was not JSON", packet.topic)
            return
        if not isinstance(message, dict):
            return
        sender = packet.participant.identity if packet.participant else self._listened()
        if packet.topic == SAY:
            self.say(str(message.get("id") or ""), str(message.get("text") or ""))
        elif packet.topic == STOP:
            self.stop()
        elif packet.topic == RESULT:
            self._on_result(message, sender)
        else:
            self._configure(message, sender)

    def _configure(self, message: dict, sender: str) -> None:
        """A client says the manager's name and how it wants to be listened to. Kept per client."""
        asked = self.asked.setdefault(sender, {})
        name, listen = message.get("name"), listen_value(message.get("listen"))
        if isinstance(name, str) and name.strip():
            asked["name"] = self.last_name = name.strip()
        if listen:
            asked["listen"] = listen
        # A client that runs the talker's tools says so; one that does not (an older build)
        # keeps asking the manager itself for every sentence.
        if "talker" in message:
            asked["talker"] = "1" if message.get("talker") is True else ""
        place = message.get("place")
        if isinstance(place, str) and place in talker.PLACES:
            asked["place"] = place
        # Where the person is, and the conversation so far (`docs/lens-everywhere.md`).
        if isinstance(message.get("now"), str):
            asked["now"] = message["now"]
        turns = talker.recent_turns(message.get("recent"))
        if turns:
            asked["recent"] = turns
        self._choose()
        log.info("%s asks for %s; listening to %s (%s)", sender or "a client", asked.get("listen", self.default_listen), self.listened or "nobody", self.attention.listen)

    def _pass_name_on(self) -> None:
        """Helps the speech-to-text service spell the name the way it is written."""
        if self.session is None:
            return
        try:
            self.session.update_options(keyterms=[self.attention.name])
        except Exception:
            log.exception("could not pass the name on to speech to text")

    # ----- hearing ------------------------------------------------------------------------------

    def _on_user_state(self, event) -> None:
        self.user_speaking = event.new_state == "speaking"
        if not self.user_speaking:
            self.addressed = False
        elif self.attention.listen != "wake" or self.attention.is_open(time.monotonic()):
            self.addressed = self.attention.listen != "never"
        self._publish_state()

    def _on_transcribed(self, event) -> None:
        # The first words tell whether the person is talking to the manager.
        if self.user_speaking and not self.addressed and self.attention.addressed(event.transcript, time.monotonic()):
            self.addressed = True
            self._publish_state()

    async def heard(self, text: str) -> str | None:
        """The person finished a sentence. Pass it on when it is meant for the manager.

        Returns what the talker should answer, or None when the talker does not answer it
        (it was not meant for the manager, or the client asks the manager itself)."""
        now = time.monotonic()
        self.addressed = False
        if self._is_own_voice(text, now):
            log.info("dropped the manager's own voice coming back through a microphone")
            return None
        accepted = self.attention.accept(text, now)
        if LOG_HEARD:
            log.info("understood %r: %s", text, "not for the manager, dropped" if accepted is None else f"passed on as {accepted!r}")
        log.info("heard a sentence of %d words from %s: %s", len(text.split()), self._listened() or "nobody", "not for the manager" if accepted is None else "the name only" if not accepted else "the talker answers" if self._talker_on() else "passed on")
        if accepted is None:
            return None
        if not accepted:
            # Only the name: the conversation is open, there is nothing to answer yet.
            self._publish_state()
            return None
        if self._talker_on():
            # The talker answers; the client shows what was heard.
            self.attention.keep_open(now)
            await self._publish(HEARD, {"text": accepted, "final": True, "from": self._listened(), "handled": True})
            self._publish_state()
            return accepted
        self.thinking_until = now + THINKING_SECS
        self._publish_state()
        if BRAIN == "server":
            if self._brain_task is not None:
                self._brain_task.cancel()
            self._brain_task = self._spawn(self._ask_server(accepted))
        else:
            await self._publish(HEARD, {"text": accepted, "final": True, "from": self._listened()})
        return None

    def _is_own_voice(self, text: str, now: float) -> bool:
        spoken = [said for _, _, said in self.speeches]
        if now - self.last_spoken[1] < 3.0:
            spoken.append(self.last_spoken[0])
        return any(is_echo(text, said) for said in spoken)

    async def _ask_server(self, text: str) -> None:
        """MANAGER_BRAIN=server: the manager on this machine answers, and the answer is spoken."""
        answer = ""
        try:
            process = await asyncio.create_subprocess_exec(
                *BRAIN_COMMAND, text, stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
            )
            try:
                out, err = await asyncio.wait_for(process.communicate(), BRAIN_TIMEOUT_SECS)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                process.kill()
                raise
            answer = out.decode("utf-8", "replace").strip()
            if process.returncode != 0:
                log.warning("%s failed (%s): %s", BRAIN_COMMAND[0], process.returncode, err.decode("utf-8", "replace").strip()[-400:])
                answer = ""
        except asyncio.CancelledError:
            return
        except asyncio.TimeoutError:
            log.warning("%s took longer than %.0f s", BRAIN_COMMAND[0], BRAIN_TIMEOUT_SECS)
        except OSError as error:
            log.warning("could not run %s: %s", BRAIN_COMMAND[0], error)
        self.say("", answer or "I could not get an answer on the server.")

    # ----- speaking -----------------------------------------------------------------------------

    def say(self, say_id: str, text: str) -> None:
        # Never an id aloud, whoever wrote the answer.
        text = talker.speakable(text).strip()
        self.thinking_until = 0.0
        if say_id and say_id in self.cut_ids:
            # The person talked over this answer: the rest of it is not spoken.
            self._publish_state()
            return
        if not text or self.session is None:
            self._publish_state()
            return
        talking = self._talker_on()
        if talking:
            # Spoken as the manager wrote it (it writes for the ear when asked by voice): a
            # retelling by the talker added guesses of its own. It goes into the talker's
            # conversation, so "and the second one?" makes sense, but not into the captions again.
            self.said_texts = (self.said_texts + [text])[-20:]
        try:
            handle = self.session.say(text, add_to_chat_ctx=talking)
        except RuntimeError as error:
            log.warning("cannot speak: %s", error)
            self._publish_state()
            return
        self.speeches.append((handle, say_id, text))
        handle.add_done_callback(self._on_spoken)
        log.info("speaking %s (%d characters)", say_id or "an answer", len(text))
        self._publish_state()

    def _on_spoken(self, handle) -> None:
        said_id = ""
        for spoken, say_id, text in self.speeches:
            if spoken is handle:
                self.last_spoken = (text, time.monotonic())
                said_id = say_id
        self.speeches = [speech for speech in self.speeches if speech[0] is not handle]
        log.info("spoke %s%s", said_id or "an answer", " (cut off)" if getattr(handle, "interrupted", False) else "")
        if said_id and getattr(handle, "interrupted", False):
            # Talked over: what is still queued of the same answer goes too.
            self.cut_ids.add(said_id)
            for queued, say_id, _ in list(self.speeches):
                if say_id == said_id:
                    try:
                        queued.interrupt(force=True)
                    except Exception:
                        log.debug("could not drop a queued sentence")
        # What the person says right after it belongs to the conversation.
        self.attention.keep_open(time.monotonic())
        self._publish_state()

    def stop(self) -> None:
        self.thinking_until = 0.0
        if self._brain_task is not None:
            self._brain_task.cancel()
            self._brain_task = None
        for _, say_id, _ in self.speeches:
            if say_id:
                self.cut_ids.add(say_id)
        if self.session is not None and (self.speeches or self._talker_on()):
            try:
                self.session.interrupt(force=True)
            except RuntimeError as error:
                log.warning("cannot stop speaking: %s", error)
        self._publish_state()

    # ----- telling the clients ------------------------------------------------------------------

    def _state(self) -> tuple[str, str | None]:
        agent_state = getattr(self.session, "agent_state", "") if self.session is not None else ""
        if self.speeches:
            return "speaking", self.speeches[0][1] or None
        if self._talker_on() and agent_state == "speaking":
            return "speaking", None
        if self._talker_on() and (agent_state == "thinking" or self.calls):
            return "thinking", None
        if time.monotonic() < self.thinking_until:
            return "thinking", None
        if self.user_speaking and self.addressed:
            return "listening", None
        return "idle", None

    def _publish_state(self, force: bool = False) -> None:
        state, say_id = self._state()
        current = (state, say_id, self.error)
        if not force and current == self._last_state:
            return
        self._last_state = current
        message: dict = {"state": state, "id": say_id}
        if self.error:
            message["error"] = self.error
        self._spawn(self._publish(STATE, message))

    async def _publish(self, topic: str, message: dict) -> None:
        try:
            await self.room.local_participant.publish_data(json.dumps(message), reliable=True, topic=topic)
        except Exception as error:
            if not self._closing:
                log.warning("could not publish on %s: %s", topic, error)

    async def _ticker(self) -> None:
        """Lets "thinking" run out when no answer comes."""
        while not self._closing:
            await asyncio.sleep(0.5)
            self._publish_state()

    # ----- when something breaks ----------------------------------------------------------------

    def _on_error(self, event) -> None:
        error = event.error
        log.warning("%s error (recoverable: %s): %s", getattr(error, "type", "speech"), getattr(error, "recoverable", "?"), getattr(error, "error", error))
        if getattr(error, "recoverable", True):
            return
        what = {"stt_error": "speech-to-text", "tts_error": "text-to-speech"}.get(getattr(error, "type", ""), "speech")
        self.error = f"The {what} service on the server failed: {getattr(error, 'error', error)}. Check its key in voice/manager/.env on the server."
        log.error(self.error)
        self._publish_state(force=True)

    def _on_close(self, event) -> None:
        if self._closing:
            return
        self._closing = True
        if getattr(event.reason, "value", event.reason) == "error":
            self._spawn(self._fail(self.error or "The voice worker on the server stopped after an error."))

    async def _fail(self, why: str) -> None:
        """Tell the clients why there is no voice, then empty the room so the next try starts fresh."""
        self.error = why
        log.error(why)
        await self._publish(STATE, {"state": "idle", "id": None, "error": why})
        await asyncio.sleep(0.5)
        try:
            await self.ctx.delete_room()
        except Exception:
            log.exception("could not close the room")
        self.ctx.shutdown("voice failed")


# ----- the agent server ---------------------------------------------------------------------------


def _no_load() -> float:
    # One person, one room: never refuse the job because the machine is busy building.
    return 0.0


# The health check answers on this machine only. Port 0: any free one (nothing here needs it).
server = AgentServer(num_idle_processes=1, load_fnc=_no_load, host="127.0.0.1", port=int(env("MANAGER_HEALTH_PORT", "0")))


def prewarm(process: JobProcess) -> None:
    """Load the voice detector once per process, before a job needs it."""
    process.userdata["vad"] = silero.VAD.load()


server.setup_fnc = prewarm


async def on_request(request: JobRequest) -> None:
    """Join the manager's room only, always under the same identity."""
    if request.room.name != ROOM:
        await request.reject()
        return
    await request.accept(name="Manager voice", identity=WORKER)


# No agent name: LiveKit sends this worker into every new room (it turns down all but the
# manager's). The LiveKit server next to it serves nothing else.
@server.rtc_session(on_request=on_request)
async def entrypoint(ctx: JobContext) -> None:
    await ManagerVoice(ctx).run()


if __name__ == "__main__":
    if BRAIN not in ("client", "server"):
        raise SystemExit(f"MANAGER_BRAIN={BRAIN!r}: use client or server")
    if STT_MODE not in ("stream", "segments"):
        raise SystemExit(f"MANAGER_STT_MODE={STT_MODE!r}: use stream or segments")
    cli.run_app(server)
