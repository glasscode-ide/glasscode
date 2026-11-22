"""The speech services and the talker the worker can use, chosen by environment variables.

    MANAGER_STT        soniox | deepgram | openai | elevenlabs          (default: deepgram)
    MANAGER_TTS        soniox | cartesia | elevenlabs | openai | deepgram (default: cartesia)
    MANAGER_VOICE      the voice, as the text-to-speech service names it (empty: its default)
    MANAGER_LANGUAGE   the language spoken, as the services name it ("en", "de", ...; default: en)
    MANAGER_STT_MODEL / MANAGER_TTS_MODEL   another model than the default
    SONIOX_STT_URL / SONIOX_TTS_URL   Soniox's addresses (empty: its global ones; the EU ones
                       are wss://stt-rt.eu.soniox.com/transcribe-websocket and
                       wss://tts-rt.eu.soniox.com/tts-websocket)

The talker: a fast language model that holds the conversation and hands the work to the
manager (see worker.py). Off when MANAGER_TALKER is empty.

    MANAGER_TALKER        anthropic | openai | mistral | groq | custom   (empty: no talker)
    MANAGER_TALKER_MODEL  the model (anthropic: claude-haiku-5-5; mistral: mistral-small-latest;
                          groq: llama-3.3-70b-versatile; openai and custom: required)
    MANAGER_TALKER_URL    custom only: an OpenAI-compatible address (https://…/v1)
    MANAGER_TALKER_KEY    its key; else the service's own variable (ANTHROPIC_API_KEY,
                          OPENAI_API_KEY, MISTRAL_API_KEY, GROQ_API_KEY)
    MANAGER_TALKER_EFFORT anthropic only: low (default) | medium | high

Each service reads its own key from the environment (SONIOX_API_KEY, DEEPGRAM_API_KEY,
OPENAI_API_KEY, CARTESIA_API_KEY, ELEVEN_API_KEY). One Soniox key does both listening and
speaking.

To add a service: write one small function that returns the plugin's object and put it in
`STT`, `TTS` or `TALKERS` below. Written against livekit-agents 1.8.12 and the plugins pinned
in requirements.txt.
"""

from __future__ import annotations

import os
from collections.abc import Callable

from livekit.agents import llm, stt, tts

# Plugins register themselves when they are imported, and that must happen on the main
# thread of the process, so they are imported here and not inside the functions.
from livekit.plugins import anthropic, cartesia, deepgram, elevenlabs, openai, soniox


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


def _stt_soniox() -> stt.STT:
    # Soniox finds the end of a sentence itself (`<end>`), so the worker lets it decide when
    # the person finished (turn detection "stt").
    params = soniox.STTOptions(
        model=env("MANAGER_STT_MODEL", "stt-rt-v6"),
        language_hints=[_language()],
        language_hints_strict=True,
        max_endpoint_delay_ms=int(env("SONIOX_ENDPOINT_DELAY_MS", "1500")),
    )
    url = env("SONIOX_STT_URL")
    return soniox.STT(params=params, **({"base_url": url} if url else {}))


STT: dict[str, Callable[[], stt.STT]] = {
    "soniox": _stt_soniox,
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


def _tts_soniox() -> tts.TTS:
    options = {"model": env("MANAGER_TTS_MODEL", "tts-rt-v2"), "language": _language()}
    voice = env("MANAGER_VOICE")
    if voice:
        options["voice"] = voice
    url = env("SONIOX_TTS_URL")
    if url:
        options["websocket_url"] = url
    return soniox.TTS(**options)


TTS: dict[str, Callable[[], tts.TTS]] = {
    "soniox": _tts_soniox,
    "cartesia": _tts_cartesia,
    "elevenlabs": _tts_elevenlabs,
    "openai": _tts_openai,
    "deepgram": _tts_deepgram,
}


# ----- the talker -------------------------------------------------------------------------------


def _talker_key(own: str) -> str:
    return env("MANAGER_TALKER_KEY") or env(own)


class _QuickClaude(anthropic.LLM):
    """Claude with little thinking: in a spoken conversation the first word counts most."""

    def chat(self, **kwargs):
        effort = env("MANAGER_TALKER_EFFORT", "low")
        extra = dict(kwargs.pop("extra_kwargs", None) or {})
        body = dict(extra.get("extra_body") or {})
        body.setdefault("output_config", {"effort": effort})
        extra["extra_body"] = body
        return super().chat(extra_kwargs=extra, **kwargs)


def _talker_anthropic() -> llm.LLM:
    # No temperature: current Claude models refuse one that is not the default.
    return _QuickClaude(model=env("MANAGER_TALKER_MODEL", "claude-haiku-5-5"), api_key=_talker_key("ANTHROPIC_API_KEY"), max_tokens=1024, caching="ephemeral")


def _openai_compatible(base_url: str, own_key: str, default_model: str) -> llm.LLM:
    model = env("MANAGER_TALKER_MODEL", default_model)
    if not model:
        raise ValueError("MANAGER_TALKER_MODEL is not set")
    key = _talker_key(own_key)
    if not key:
        raise ValueError(f"no key: set MANAGER_TALKER_KEY or {own_key}")
    options = {"model": model, "api_key": key}
    if base_url:
        options["base_url"] = base_url
    return openai.LLM(**options)


TALKERS: dict[str, Callable[[], llm.LLM]] = {
    "anthropic": _talker_anthropic,
    "openai": lambda: _openai_compatible("", "OPENAI_API_KEY", ""),
    "mistral": lambda: _openai_compatible("https://api.mistral.ai/v1", "MISTRAL_API_KEY", "mistral-small-latest"),
    "groq": lambda: _openai_compatible("https://api.groq.com/openai/v1", "GROQ_API_KEY", "llama-3.3-70b-versatile"),
    "custom": lambda: _openai_compatible(env("MANAGER_TALKER_URL"), "MANAGER_TALKER_KEY", ""),
}


def make_talker() -> llm.LLM | None:
    """The talker named by MANAGER_TALKER, or None when there is none."""
    name = env("MANAGER_TALKER").lower()
    if not name or name in ("off", "none"):
        return None
    if name not in TALKERS:
        raise ValueError(f"MANAGER_TALKER={name!r} is not a talker this worker knows. Use one of: {', '.join(sorted(TALKERS))}.")
    if name == "custom" and not env("MANAGER_TALKER_URL"):
        raise ValueError("MANAGER_TALKER=custom needs MANAGER_TALKER_URL, an OpenAI-compatible address.")
    try:
        return TALKERS[name]()
    except Exception as error:
        raise ValueError(f"The talker {name!r} could not be set up: {error}. Set its key in Settings › Manager › Voice and start the voice again.") from error


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
