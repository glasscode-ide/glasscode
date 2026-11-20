"""Test only: the real worker with stand-in speech services (no keys, no network beyond LiveKit).

The stand-in speech-to-text answers every stretch of speech with the text in $MV_TRANSCRIPT;
the stand-in text-to-speech answers with a tone as long as the text would take to say.
"""
import math
import os
import struct
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "worker"))

from livekit.agents import stt, tts
from livekit.agents.types import DEFAULT_API_CONNECT_OPTIONS, NOT_GIVEN

import providers


class FakeSTT(stt.STT):
    def __init__(self):
        super().__init__(capabilities=stt.STTCapabilities(streaming=False, interim_results=False))

    async def _recognize_impl(self, buffer, *, language=NOT_GIVEN, conn_options=DEFAULT_API_CONNECT_OPTIONS):
        with open(os.environ["MV_TRANSCRIPT"]) as f:
            text = f.read().strip()
        return stt.SpeechEvent(type=stt.SpeechEventType.FINAL_TRANSCRIPT, alternatives=[stt.SpeechData(text=text, language="en")])


class FakeStream(tts.ChunkedStream):
    async def _run(self, output_emitter):
        rate = 24000
        output_emitter.initialize(request_id="fake", sample_rate=rate, num_channels=1, mime_type="audio/pcm")
        seconds = max(1.0, min(8.0, 0.07 * len(self.input_text)))
        total = int(rate * seconds)
        step = rate // 10
        for start in range(0, total, step):
            chunk = b"".join(struct.pack("<h", int(6000 * math.sin(2 * math.pi * 440 * (start + i) / rate))) for i in range(min(step, total - start)))
            output_emitter.push(chunk)
        output_emitter.flush()


class FakeTTS(tts.TTS):
    def __init__(self):
        super().__init__(capabilities=tts.TTSCapabilities(streaming=False), sample_rate=24000, num_channels=1)

    def synthesize(self, text, *, conn_options=DEFAULT_API_CONNECT_OPTIONS):
        return FakeStream(tts=self, input_text=text, conn_options=conn_options)


providers.STT["fake"] = FakeSTT
providers.TTS["fake"] = FakeTTS

import worker  # noqa: E402

if __name__ == "__main__":
    from livekit.agents import cli

    cli.run_app(worker.server)
