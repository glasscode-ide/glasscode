"""The wake word: does a sentence start with the manager's name?

The name is whatever the person called their manager ("AI Mira", "Manager"). Speech-to-text
spells names its own way ("A.I. Mira", "I Meera"), so the match is a little loose: a short
greeting in front is fine, the name's last words alone are fine ("Mira, ..."), and so is a
small spelling difference. Only the start of the sentence counts.

No LiveKit in this file, so it can be tested alone: `python3 -m unittest test_wake`.
"""

from __future__ import annotations

import re
import unicodedata
from difflib import SequenceMatcher

# Words people put in front of a name. They are skipped, never required.
GREETINGS = {"hey", "hi", "hello", "ok", "okay", "yo", "hallo", "hei", "so", "and", "oh"}

# How alike the heard word(s) and the name must be (1.0 = the same letters).
CLOSE_ENOUGH = 0.82

_WORD = re.compile(r"[^\W_]+(?:'[^\W_]+)*")
_AFTER_NAME = re.compile(r"^[\s,.:;!?\-–—]+")


def _plain(word: str) -> str:
    """Lower case, without accents."""
    folded = unicodedata.normalize("NFKD", word.casefold())
    return "".join(c for c in folded if not unicodedata.combining(c))


def _words(text: str) -> list[tuple[str, int]]:
    """The words of `text`, each with where it ends."""
    return [(_plain(m.group()), m.end()) for m in _WORD.finditer(text)]


def name_forms(name: str) -> list[str]:
    """The ways the name may be said, letters only, longest first.

    "AI Mira" gives "aimira" and "mira": the whole name, and the name without its
    first words. A form shorter than three letters would match far too much and is left out.
    """
    tokens = [word for word, _ in _words(name)]
    forms: list[str] = []
    for start in range(len(tokens)):
        form = "".join(tokens[start:])
        if len(form) >= 3 and form not in forms:
            forms.append(form)
    return forms


def _alike(heard: str, form: str) -> float:
    """How alike two spellings are. Short names must be exact."""
    if heard == form:
        return 1.0
    if len(form) <= 3:
        return 0.0
    return SequenceMatcher(None, heard, form).ratio()


def strip_name(text: str, name: str) -> str | None:
    """`text` without the manager's name at its start, or None when it does not start with it.

    The result may be empty: the person said only the name.
    """
    words = _words(text)
    forms = name_forms(name)
    if not words or not forms:
        return None

    first = 0
    while first < len(words) and first < 2 and words[first][0] in GREETINGS:
        first += 1

    best_score = 0.0
    best_end = -1
    for form in forms:
        # A name of two words may come out as three ("A.I. Mira"): try a few lengths.
        longest = len(_words(name)) + 2
        for count in range(1, min(len(words) - first, longest) + 1):
            heard = "".join(word for word, _ in words[first : first + count])
            if len(heard) > len(form) + 2:
                break
            score = _alike(heard, form)
            # Longer forms come first, so an equal score never replaces the whole name.
            if score >= CLOSE_ENOUGH and score > best_score:
                best_score = score
                best_end = words[first + count - 1][1]
        if best_end >= 0:
            break

    if best_end < 0:
        return None
    return _AFTER_NAME.sub("", text[best_end:]).strip()


def is_echo(heard: str, spoken: str) -> bool:
    """Whether `heard` is the manager's own voice coming back through a microphone.

    A client without echo cancelling would make the manager answer itself for ever. A
    sentence of four words or more whose word pairs nearly all occur in what is being
    spoken is taken for its echo. Short replies ("yes", "stop that") never are.
    """
    heard_words = [word for word, _ in _words(heard)]
    spoken_words = [word for word, _ in _words(spoken)]
    if len(heard_words) < 4 or len(spoken_words) < 2:
        return False
    spoken_pairs = set(zip(spoken_words, spoken_words[1:]))
    heard_pairs = list(zip(heard_words, heard_words[1:]))
    found = sum(1 for pair in heard_pairs if pair in spoken_pairs)
    return found / len(heard_pairs) >= 0.7


# How a client asks to be listened to.
LISTEN = ("wake", "always", "never")


def listen_value(value: object) -> str | None:
    """A client's "listen" as the worker knows it, or None when it is not one of them.

    "hold" is an older word for "always" (a client that only sends its microphone while a
    key is held).
    """
    if value == "hold":
        return "always"
    return value if value in LISTEN else None


class Attention:
    """Whether the worker should pass on what it hears right now.

    `listen` is "wake" (after the name, and for a while after the manager spoke),
    "always" (an open microphone: the person chose to talk, so everything counts) or
    "never".
    """

    def __init__(self, name: str = "Manager", listen: str = "wake", window_secs: float = 20.0) -> None:
        self.name = name
        self.listen = listen
        self.window_secs = window_secs
        self._open_until = 0.0

    def configure(self, name: str | None, listen: str | None) -> None:
        if name and name.strip():
            self.name = name.strip()
        if listen_value(listen):
            self.listen = listen_value(listen)

    def keep_open(self, now: float) -> None:
        """The manager spoke, or was spoken to: what follows soon belongs to the conversation."""
        self._open_until = max(self._open_until, now + self.window_secs)

    def is_open(self, now: float) -> bool:
        return now < self._open_until

    def addressed(self, text: str, now: float) -> bool:
        """Whether speech that begins like `text` is meant for the manager (for the state only)."""
        if self.listen == "never":
            return False
        if self.listen == "always":
            return True
        return self.is_open(now) or strip_name(text, self.name) is not None

    def accept(self, text: str, now: float) -> str | None:
        """What to pass on of a finished sentence, or None to drop it.

        An empty string means the person only said the name: nothing to pass on, but the
        conversation is open now.
        """
        text = text.strip()
        if not text or self.listen == "never":
            return None
        if self.listen == "wake":
            rest = strip_name(text, self.name)
            if rest is None:
                if not self.is_open(now):
                    return None
            else:
                text = rest
        self.keep_open(now)
        return text
