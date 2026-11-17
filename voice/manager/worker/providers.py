"""The speech services the worker can use, chosen by environment variables.

    MANAGER_STT        deepgram | openai | elevenlabs          (default: deepgram)
    MANAGER_TTS        cartesia | elevenlabs | openai | deepgram (default: cartesia)
    MANAGER_VOICE      the voice, as the text-to-speech service names it (empty: its default)
    MANAGER_LANGUAGE   the language spoken, as the services name it ("en", "de", ...; default: en)
    MANAGER_STT_MODEL / MANAGER_TTS_MODEL   another model than the default

Each service reads its own key from the environment (DEEPGRAM_API_KEY, OPENAI_API_KEY,
CARTESIA_API_KEY, ELEVEN_API_KEY).

To add a service: write one small function that returns the plugin's object and put it in
`STT` or `TTS` below. Written against livekit-agents 1.8.12 and the plugins pinned in
requirements.txt.
"""

from __future__ import annotations

import os
from collections.abc import Callable

from livekit.agents import stt, tts

# Plugins register themselves when they are imported, and that must happen on the main
# thread of the process, so they are imported here and not inside the functions.
from livekit.plugins import cartesia, deepgram, elevenlabs, openai


def env(name: str, default: str = "") -> str:
    """An environment variable, trimmed; `default` when it is unset or empty."""
    return os.environ.get(name, "").strip() or default


def _language() -> str:
    return env("MANAGER_LANGUAGE", "en")


# ----- speech to text ---------------------------------------------------------------------------


def _stt_deepgram() -> stt.STT:
    # Streams the microphone as it comes; "multi" as the language follows mixed languages.
    return deepgram.STT(model=env("MANAGER_STT_MODEL", "nova-3"), language=_language())


def _stt_openai() -> stt.STT:
    # Not a streaming service: the worker sends each sentence once it has ended.
    return openai.STT(model=env("MANAGER_STT_MODEL", "gpt-4o-mini-transcribe"), language=_language())


def _stt_elevenlabs() -> stt.STT:
    model = env("MANAGER_STT_MODEL")
    if model:
        return elevenlabs.STT(language_code=_language(), model=model)
    return elevenlabs.STT(language_code=_language())


STT: dict[str, Callable[[], stt.STT]] = {
    "deepgram": _stt_deepgram,
    "openai": _stt_openai,
    "elevenlabs": _stt_elevenlabs,
}


# ----- text to speech ---------------------------------------------------------------------------


def _tts_cartesia() -> tts.TTS:
    voice = env("MANAGER_VOICE")
    options = {"model": env("MANAGER_TTS_MODEL", "sonic-3"), "language": _language()}
    if voice:
        options["voice"] = voice
    return cartesia.TTS(**options)


def _tts_elevenlabs() -> tts.TTS:
    voice = env("MANAGER_VOICE")
    options = {"model": env("MANAGER_TTS_MODEL", "eleven_flash_v2_5")}
    if voice:
        options["voice_id"] = voice
    return elevenlabs.TTS(**options)


def _tts_openai() -> tts.TTS:
    return openai.TTS(model=env("MANAGER_TTS_MODEL", "gpt-4o-mini-tts"), voice=env("MANAGER_VOICE", "ash"))


def _tts_deepgram() -> tts.TTS:
    # With Deepgram the voice is the model ("aura-2-andromeda-en").
    return deepgram.TTS(model=env("MANAGER_VOICE") or env("MANAGER_TTS_MODEL", "aura-2-andromeda-en"))


TTS: dict[str, Callable[[], tts.TTS]] = {
    "cartesia": _tts_cartesia,
    "elevenlabs": _tts_elevenlabs,
    "openai": _tts_openai,
    "deepgram": _tts_deepgram,
}


# ----- choosing ---------------------------------------------------------------------------------


def _pick(kind: str, variable: str, default: str, known: dict[str, Callable[[], object]]):
    name = env(variable, default).lower()
    if name not in known:
        raise ValueError(f"{variable}={name!r} is not a {kind} service this worker knows. Use one of: {', '.join(sorted(known))}.")
    try:
        return known[name]()
    except Exception as error:  # a missing key, mostly
        raise ValueError(f"The {kind} service {name!r} could not be set up: {error}. Set its key in voice/manager/.env on the server and start the worker again.") from error


def make_stt() -> stt.STT:
    """The speech-to-text service named by MANAGER_STT."""
    return _pick("speech-to-text", "MANAGER_STT", "deepgram", STT)


def make_tts() -> tts.TTS:
    """The text-to-speech service named by MANAGER_TTS."""
    return _pick("text-to-speech", "MANAGER_TTS", "cartesia", TTS)
