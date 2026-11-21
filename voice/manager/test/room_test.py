"""Test only: a stand-in for the Mac helper that plays recorded speech as its microphone and
checks what the worker does, step by step."""
import array
import asyncio
import json
import os
import sys
import time
import wave

from livekit import rtc

HERE = os.environ["MV_WORK"]
URL = os.environ.get("MV_URL", "ws://127.0.0.1:17880")
TOKENS = dict(line.split()[1:3] for line in open(os.path.join(HERE, "tokens.txt")))
ME = "mac-test1"
TRANSCRIPT = os.path.join(HERE, "transcript.txt")
T0 = time.monotonic()
events = []  # (seconds, topic, message)
tone_frames = 0
failures = []


def log(text):
    print(f"[{time.monotonic() - T0:6.2f}] {text}", flush=True)


def load(name):
    with wave.open(os.path.join(HERE, name)) as w:
        assert w.getframerate() == 48000 and w.getnchannels() == 1 and w.getsampwidth() == 2
        return array.array("h", w.readframes(w.getnframes()))


class Mic:
    def __init__(self):
        self.source = rtc.AudioSource(48000, 1)
        self.pending = array.array("h")

    def play(self, samples):
        self.pending.extend(samples)

    async def run(self):
        silence = array.array("h", [0] * 480)
        while True:
            if len(self.pending) >= 480:
                chunk, self.pending = self.pending[:480], self.pending[480:]
            else:
                chunk = silence
            await self.source.capture_frame(rtc.AudioFrame(chunk.tobytes(), 48000, 1, 480))


async def wait_for(what, predicate, timeout, since=None):
    start = len(events) if since is None else since
    deadline = time.monotonic() + timeout
    seen = start
    while time.monotonic() < deadline:
        for event in events[seen:]:
            if predicate(event):
                log(f"ok    {what}: {event[1]} {event[2]}")
                return event
        seen = len(events)
        await asyncio.sleep(0.05)
    failures.append(what)
    log(f"FAIL  {what} (nothing in {timeout}s)")
    return None


async def expect_none(what, predicate, seconds, since):
    await asyncio.sleep(seconds)
    hits = [e for e in events[since:] if predicate(e)]
    if hits:
        failures.append(what)
        log(f"FAIL  {what}: got {hits[0][1]} {hits[0][2]}")
    else:
        log(f"ok    {what}")


def is_state(state, say_id=None):
    return lambda e: e[1] == "manager.state" and e[2].get("state") == state and (say_id is None or e[2].get("id") == say_id)


def is_heard(text=None):
    return lambda e: e[1] == "manager.heard" and (text is None or e[2].get("text") == text)


async def main():
    global tone_frames
    room = rtc.Room()
    mic = Mic()

    @room.on("data_received")
    def on_data(packet):
        try:
            message = json.loads(packet.data.decode())
        except ValueError:
            message = {"raw": repr(packet.data)}
        who = packet.participant.identity if packet.participant else "?"
        events.append((time.monotonic() - T0, packet.topic, message))
        log(f"      <- {packet.topic} from {who}: {message}")
        if packet.topic == "manager.tool" and message.get("to") == ME:
            # As a talker client: the manager takes the request on and answers later.
            answers = {"ask_manager": "started", "list_sessions": "s1: Voice mode, working"}
            asyncio.create_task(send("manager.result", {"call": message.get("call"), "text": answers.get(message.get("name"), "ok")}))

    async def listen(track):
        global tone_frames
        async for event in rtc.AudioStream(track):
            samples = array.array("h", bytes(event.frame.data))
            if samples and max(abs(s) for s in samples) > 1000:
                tone_frames += 1

    @room.on("track_subscribed")
    def on_track(track, publication, participant):
        log(f"      subscribed to {participant.identity}'s {'audio' if track.kind == rtc.TrackKind.KIND_AUDIO else 'video'} track")
        if track.kind == rtc.TrackKind.KIND_AUDIO:
            asyncio.create_task(listen(track))

    @room.on("participant_connected")
    def on_join(participant):
        log(f"      joined: {participant.identity} (kind {participant.kind})")

    async def send(topic, message):
        log(f"      -> {topic}: {message}")
        await room.local_participant.publish_data(json.dumps(message), reliable=True, topic=topic)

    def transcript(text):
        with open(TRANSCRIPT, "w") as f:
            f.write(text)

    await room.connect(URL, TOKENS[ME])
    log(f"connected as {room.local_participant.identity} to room {room.name!r}")
    track = rtc.LocalAudioTrack.create_audio_track("microphone", mic.source)
    await room.local_participant.publish_track(track, rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_MICROPHONE))
    asyncio.create_task(mic.run())
    await send("manager.config", {"name": "AI Mira", "listen": "wake"})

    log("--- 1. the worker joins as manager-voice and reports a state")
    deadline = time.monotonic() + 30
    while "manager-voice" not in room.remote_participants and time.monotonic() < deadline:
        await asyncio.sleep(0.1)
    if "manager-voice" not in room.remote_participants:
        log("FAIL  the worker did not join")
        failures.append("worker joins")
        await room.disconnect()
        return
    log(f"ok    worker joined; in the room: {sorted(room.remote_participants)}")
    await wait_for("first state", lambda e: e[1] == "manager.state", 15, since=0)
    await send("manager.config", {"name": "AI Mira", "listen": "wake"})
    await asyncio.sleep(1.0)

    log("--- 2. wake word: the name is taken off and the sentence comes back")
    mark = len(events)
    transcript("A.I. Mira, what is running right now?")
    mic.play(load("u1.wav"))
    heard = await wait_for("heard after the name", is_heard(), 20, mark)
    if heard and heard[2] != {"text": "what is running right now?", "final": True, "from": ME}:
        failures.append("heard payload")
        log(f"FAIL  heard payload is {heard[2]}")
    await wait_for("thinking after heard", is_state("thinking"), 5, mark)

    log("--- 3. say: speaking with the id, audio arrives, then idle")
    mark, before = len(events), tone_frames
    await send("manager.say", {"id": "say-1", "text": "Two sessions are running and one of them needs you."})
    await wait_for("speaking say-1", is_state("speaking", "say-1"), 8, mark)
    await wait_for("idle after speaking", is_state("idle"), 15, mark)
    log(f"{'ok   ' if tone_frames - before > 20 else 'FAIL '} audio frames with sound from the worker: {tone_frames - before}")
    if tone_frames - before <= 20:
        failures.append("worker audio")

    log("--- 4. right after it spoke, no name is needed")
    mark = len(events)
    transcript("and what about the reviews?")
    mic.play(load("u2.wav"))
    await wait_for("heard without the name", is_heard("and what about the reviews?"), 20, mark)
    mark = len(events)
    await send("manager.say", {"id": "say-2", "text": "No reviews."})
    await wait_for("speaking say-2", is_state("speaking", "say-2"), 8, mark)
    await wait_for("idle after say-2", is_state("idle"), 10, mark)

    log("--- 5. later, without the name, nothing is passed on")
    await asyncio.sleep(float(os.environ.get("MANAGER_WINDOW_SECS", "6")) + 2)
    mark = len(events)
    transcript("and what about the reviews?")
    mic.play(load("u2.wav"))
    await expect_none("no heard outside the conversation", is_heard(), 9, mark)

    log("--- 6. stop: a long sentence is cut off")
    mark = len(events)
    await send("manager.say", {"id": "say-3", "text": "This is a very long answer. " * 6})
    spoke = await wait_for("speaking say-3", is_state("speaking", "say-3"), 8, mark)
    await asyncio.sleep(1.5)
    await send("manager.stop", {})
    idle = await wait_for("idle after stop", is_state("idle"), 4, mark)
    if spoke and idle:
        log(f"      spoke for {idle[0] - spoke[0]:.1f}s of about 8s")

    log("--- 7. the person talks over it: it stops and the sentence is passed on")
    await asyncio.sleep(1.0)
    mark = len(events)
    await send("manager.say", {"id": "say-4", "text": "This is another very long answer. " * 5})
    spoke = await wait_for("speaking say-4", is_state("speaking", "say-4"), 8, mark)
    await asyncio.sleep(1.5)
    transcript("Mira, please stop talking now.")
    mic.play(load("u3.wav"))
    heard = await wait_for("heard while it spoke", is_heard("please stop talking now."), 20, mark)
    ended = await wait_for("no longer speaking", lambda e: e[1] == "manager.state" and e[2].get("state") != "speaking", 6, mark)
    if spoke and ended:
        log(f"      spoke for {ended[0] - spoke[0]:.1f}s of about 8s")

    log("--- 8. its own voice coming back is dropped")
    await asyncio.sleep(1.0)
    mark = len(events)
    await send("manager.say", {"id": "say-5", "text": "Two sessions need you. The checks on the billing pull request failed, and Sam asked for a review."})
    await wait_for("speaking say-5", is_state("speaking", "say-5"), 8, mark)
    await asyncio.sleep(1.0)
    transcript("the checks on the billing pull request failed")
    mic.play(load("u2.wav"))
    await expect_none("echo is not passed on", is_heard(), 9, mark)
    await wait_for("idle again", is_state("idle"), 12, mark)

    log("--- 9. listen: never passes nothing, always passes everything")
    await asyncio.sleep(float(os.environ.get("MANAGER_WINDOW_SECS", "6")) + 1)
    await send("manager.config", {"name": "AI Mira", "listen": "never"})
    await asyncio.sleep(0.5)
    mark = len(events)
    transcript("AI Mira, hello there")
    mic.play(load("u1.wav"))
    await expect_none("never: no heard", is_heard(), 8, mark)
    await send("manager.config", {"listen": "always"})
    await asyncio.sleep(0.5)
    mark = len(events)
    transcript("what is the time")
    mic.play(load("u2.wav"))
    await wait_for("always: heard without the name", is_heard("what is the time"), 20, mark)

    if os.environ.get("MANAGER_TALKER"):
        log("--- 10. a talker client: the worker answers itself and hands the work over")
        await send("manager.say", {"id": "x", "text": "ok"})  # ends the "thinking" of step 9
        await wait_for("idle before the talker", is_state("idle"), 12)
        await send("manager.config", {"listen": "always", "talker": True, "place": "glasses"})
        await asyncio.sleep(0.5)
        mark, before = len(events), tone_frames
        transcript("what needs me")
        mic.play(load("u2.wav"))
        heard = await wait_for("heard, marked as handled", lambda e: is_heard("what needs me")(e) and e[2].get("handled") is True, 20, mark)
        await wait_for("the talker's own words", lambda e: e[1] == "manager.said" and e[2].get("text") == "I'll check.", 15, mark)
        tool = await wait_for("its tool runs here", lambda e: e[1] == "manager.tool" and e[2].get("name") == "ask_manager", 15, mark)
        if tool and tool[2].get("arguments") != {"request": "what needs me"}:
            failures.append("tool arguments")
            log(f"FAIL  tool arguments are {tool[2].get('arguments')}")
        await wait_for("thinking while the manager works", is_state("thinking"), 10, mark)
        mark = len(events)
        await send("manager.say", {"id": "ask-1", "text": "Two sessions need you."})
        await wait_for("the manager's answer is spoken", is_state("speaking", "ask-1"), 8, mark)
        await wait_for("idle after the answer", is_state("idle"), 15, mark)
        if tone_frames - before <= 20:
            failures.append("talker audio")
        await expect_none("the manager's answer is not told as the talker's", lambda e: e[1] == "manager.said" and "Two sessions" in e[2].get("text", ""), 1, mark)

        log("--- 11. talked over: the rest of that answer is not spoken")
        await asyncio.sleep(1.0)
        mark = len(events)
        await send("manager.say", {"id": "ask-2", "text": "This is a long answer from the manager. " * 5})
        await wait_for("speaking ask-2", is_state("speaking", "ask-2"), 8, mark)
        await asyncio.sleep(1.5)
        transcript("wait, stop")
        mic.play(load("u3.wav"))
        await wait_for("no longer speaking ask-2", lambda e: e[1] == "manager.state" and e[2].get("id") != "ask-2", 8, mark)
        await asyncio.sleep(1.0)
        mark = len(events)
        await send("manager.say", {"id": "ask-2", "text": "And here is more of the same answer."})
        await expect_none("a later sentence of it is dropped", is_state("speaking", "ask-2"), 3, mark)

    await room.disconnect()
    log("disconnected")


asyncio.run(main())
print("RESULT:", "ALL OK" if not failures else f"{len(failures)} FAILED: {failures}")
sys.exit(1 if failures else 0)
