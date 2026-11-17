"""Test only: drive the real Swift helper (no microphone, no sound) the way the app does."""
import array, asyncio, json, os, subprocess, sys, threading, time, wave
from livekit import rtc

HERE = os.environ["MV_WORK"]
TOKENS = dict(line.split()[1:3] for line in open(os.path.join(HERE, "tokens.txt")))
HELPER = os.environ["MV_HELPER"]
URL = "ws://127.0.0.1:17880"
T0 = time.monotonic()
lines, failures, room_events = [], [], []

def log(t): print(f"[{time.monotonic()-T0:6.2f}] {t}", flush=True)

def start_helper(token):
    env = dict(os.environ, MANAGER_VOICE_URL=URL, MANAGER_VOICE_TOKEN=token, MANAGER_VOICE_NAME="AI Mira", MANAGER_VOICE_LISTEN="wake",
               DYLD_FRAMEWORK_PATH=os.path.join(os.path.dirname(os.path.dirname(HELPER)), "out/Products/Release"))
    p = subprocess.Popen([HELPER, "--no-audio"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(os.path.join(HERE, "helper.err"), "w"), env=env, text=True)
    def read():
        for line in p.stdout:
            lines.append(json.loads(line)); log(f"      helper: {line.strip()}")
    threading.Thread(target=read, daemon=True).start()
    return p

def tell(p, obj):
    log(f"      app -> helper: {obj}")
    p.stdin.write(json.dumps(obj) + "\n"); p.stdin.flush()

async def expect(what, pred, timeout, since=0, source=None):
    source = lines if source is None else source
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if any(pred(l) for l in source[since:]):
            log(f"ok    {what}"); return True
        await asyncio.sleep(0.05)
    failures.append(what); log(f"FAIL  {what}"); return False

async def expect_none(what, pred, seconds, since):
    await asyncio.sleep(seconds)
    if any(pred(l) for l in lines[since:]): failures.append(what); log(f"FAIL  {what}")
    else: log(f"ok    {what}")

async def main():
    log("--- a wrong token is refused, in words")
    bad = start_helper(TOKENS["mac-helper"][:-6] + "AAAAAA")
    await expect("error line for a bad token", lambda l: "error" in l, 20)
    code = bad.wait(timeout=10); log(f"      exit code {code}")
    if code != 3: failures.append("bad token exit code")
    mark = len(lines)

    log("--- the helper joins, the worker follows")
    p = start_helper(TOKENS["mac-helper"])
    await expect("ready", lambda l: l.get("ready") is True, 20, mark)
    await expect("state from the worker", lambda l: l.get("state") == "idle", 20, mark)

    log("--- a second device joins and speaks: the worker knows the name from the helper's config")
    room = rtc.Room()
    @room.on("data_received")
    def on_data(packet):
        room_events.append((packet.topic, json.loads(packet.data.decode())))
    await room.connect(URL, TOKENS["mac-test1"])
    source = rtc.AudioSource(48000, 1)
    track = rtc.LocalAudioTrack.create_audio_track("microphone", source)
    await room.local_participant.publish_track(track, rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_MICROPHONE))
    pending = array.array("h")
    async def feed():
        nonlocal pending
        silence = array.array("h", [0] * 480)
        while True:
            chunk = silence
            if len(pending) >= 480: chunk, pending = pending[:480], pending[480:]
            await source.capture_frame(rtc.AudioFrame(chunk.tobytes(), 48000, 1, 480))
    asyncio.create_task(feed())
    await asyncio.sleep(1.5)
    mark = len(lines)
    open(os.path.join(HERE, "transcript.txt"), "w").write("AI Mira, what is running right now?")
    with wave.open(os.path.join(HERE, "u1.wav")) as w: pending.extend(array.array("h", w.readframes(w.getnframes())))
    await expect("the other device gets heard (from itself)", lambda e: e[0] == "manager.heard" and e[1] == {"text": "what is running right now?", "final": True, "from": "mac-test1"}, 20, 0, room_events)
    await expect_none("the helper does not pass on another device's heard", lambda l: "heard" in l, 2, mark)
    await expect("the helper reports thinking", lambda l: l.get("state") == "thinking", 5, mark)

    log("--- heard addressed to the helper is passed to the app")
    mark = len(lines)
    await room.local_participant.publish_data(json.dumps({"text": "open the last session", "final": True, "from": "mac-helper"}), reliable=True, topic="manager.heard")
    await expect("heard line", lambda l: l.get("heard") == "open the last session", 5, mark)

    log("--- say and stop from the app")
    mark = len(lines)
    tell(p, {"say": {"id": "say-9", "text": "Hello from the app. " * 12}})
    await expect("speaking", lambda l: l.get("state") == "speaking", 8, mark)
    await asyncio.sleep(1.5)
    t = time.monotonic(); mark = len(lines)
    tell(p, {"stop": True})
    await expect("idle after stop", lambda l: l.get("state") == "idle", 4, mark)
    log(f"      stopped {time.monotonic()-t:.2f}s after the stop line")

    log("--- a new name from the app reaches the worker")
    tell(p, {"config": {"name": "Sam", "listen": "wake"}})
    await asyncio.sleep(float(os.environ.get("MANAGER_WINDOW_SECS", "6")) + 1.5)
    n = len(room_events)
    open(os.path.join(HERE, "transcript.txt"), "w").write("Sam, are you there?")
    with wave.open(os.path.join(HERE, "u3.wav")) as w: pending.extend(array.array("h", w.readframes(w.getnframes())))
    await expect("heard with the new name", lambda e: e[0] == "manager.heard" and e[1].get("text") == "are you there?", 20, n, room_events)

    log("--- listening is kept per device: this one asks for always, the helper stays on wake")
    await asyncio.sleep(float(os.environ.get("MANAGER_WINDOW_SECS", "6")) + 1.5)
    await room.local_participant.publish_data(json.dumps({"listen": "always"}), reliable=True, topic="manager.config")
    await asyncio.sleep(0.5)
    n = len(room_events)
    open(os.path.join(HERE, "transcript.txt"), "w").write("what is the time")
    with wave.open(os.path.join(HERE, "u2.wav")) as w: pending.extend(array.array("h", w.readframes(w.getnframes())))
    await expect("always: heard without the name, from the device that spoke", lambda e: e[0] == "manager.heard" and e[1] == {"text": "what is the time", "final": True, "from": "mac-test1"}, 20, n, room_events)

    log("--- the device that asked for always leaves: the helper (wake) is listened to again")
    await room.disconnect()
    await asyncio.sleep(1.0)
    mark = len(lines)
    tell(p, {"config": {"name": "Sam", "listen": "always"}})
    tell(p, {"say": {"id": "say-10", "text": "Still here."}})
    await expect("the helper still speaks through the worker", lambda l: l.get("state") == "speaking", 8, mark)
    await expect("idle again", lambda l: l.get("state") == "idle", 8, mark)

    log("--- closing stdin ends the helper")
    t = time.monotonic(); p.stdin.close()
    try:
        code = p.wait(timeout=6); log(f"ok    helper ended {time.monotonic()-t:.2f}s after stdin closed, exit code {code}")
        if code != 0: failures.append("helper exit code")
    except subprocess.TimeoutExpired:
        failures.append("helper did not end"); log("FAIL  helper still running"); p.kill()

asyncio.run(main())
print("RESULT:", "ALL OK" if not failures else f"FAILED: {failures}")
sys.exit(1 if failures else 0)
