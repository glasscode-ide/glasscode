"""The talker: a fast language model that holds the spoken conversation.

It answers at once ("On it.", "Here it is."), and has the manager's brain do the work. To the
person it is the manager: one voice, in the first person, never "I'll ask the manager". Its tools run on the device the person talks to (the Mac, or the phone with the
glasses), not here: that device already reaches the manager with what its screen or lens
shows, so "this session" means the same thing in a spoken turn as in a typed one, and the
manager can still open places there.

A tool call goes to the device on `manager.tool` and comes back on `manager.result`:

    manager.tool    worker -> the device   {"call": "t3", "name": "ask_manager", "arguments": {...}, "to": "<identity>"}
    manager.result  the device -> worker   {"call": "t3", "text": "..."}

The work's answer comes back as it always did, on `manager.say`, and is spoken as the manager
wrote it: asked by voice, it writes for the ear (one to three sentences). The talker never
retells it: a retelling added guesses of its own.
"""

from __future__ import annotations

import re
from collections.abc import Awaitable, Callable

from livekit.agents import RunContext, ToolError, function_tool

TOOL = "manager.tool"
RESULT = "manager.result"

# What the device answers an `ask_manager` it took on.
STARTED = "started"

PLACES = {
    "glasses": "on their glasses: they hear you, and see a small lens in front of one eye",
    "phone": "on their phone",
    "mac": "at their Mac",
}

CallDevice = Callable[[str, dict], Awaitable[str]]


# An id as it must never be spoken: a UUID, or a run of eight or more hex letters with a digit
# and a letter in it (a ref, a commit), with the words that lead to it ("with id", "its ref is").
_ID = re.compile(
    r"(?:,?\s*\b(?:with|and its|its|whose|has|under)\s+)?(?:\bthe\s+)?(?:\b(?:id|ref)s?\b\s*(?:is|:|#)?\s*)?"
    r"\b(?:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{8,40})\b",
    re.IGNORECASE,
)


def speakable(text: str) -> str:
    """The text without ids: an id read aloud is noise, whoever wrote it."""
    cleaned = _ID.sub("", text)
    if cleaned == text:
        return text
    # What an id leaves behind: empty brackets, a space before a comma, double spaces.
    cleaned = re.sub(r"\s*\(\s*[,;:]?\s*\)", "", cleaned)
    cleaned = re.sub(r"\s+([,.;!?])", r"\1", cleaned)
    cleaned = re.sub(r",\s*([.;!?])", r"\1", cleaned)
    return re.sub(r"[ \t]{2,}", " ", cleaned)


async def speakable_stream(text):
    """`speakable` over text as it is written, a sentence at a time (an id and the words that
    lead to it are in one sentence); a long sentence goes on at a space, a few words held back."""
    held = ""
    async for delta in text:
        if not isinstance(delta, str):
            yield delta
            continue
        held += delta
        ends = [m.end() for m in re.finditer(r"[.!?\n]\s", held)]
        cut = ends[-1] if ends else 0
        if not cut and len(held) > 160:
            words = [m.start() for m in re.finditer(r"\s\S", held)]
            cut = words[-5] + 1 if len(words) > 5 else 0
        if cut:
            head, held = held[:cut], held[cut:]
            yield speakable(head)
    if held:
        yield speakable(held)


# How many of the conversation's last turns a talk starts with, and how long each may be.
RECENT_TURNS = 8
RECENT_CHARS = 400


def recent_turns(recent) -> list[tuple[str, str]]:
    """The conversation's last turns as the device sent them ([{"who": "person|manager", "text"}]),
    as (role, text) for the talker's history: a talk started again carries on from them."""
    turns = []
    for turn in recent if isinstance(recent, list) else []:
        if not isinstance(turn, dict):
            continue
        who, text = turn.get("who"), turn.get("text")
        if who not in ("person", "manager") or not isinstance(text, str) or not text.strip():
            continue
        text = " ".join(text.split())
        turns.append(("user" if who == "person" else "assistant", text[:RECENT_CHARS]))
    return turns[-RECENT_TURNS:]


def instructions(name: str, place: str = "", language: str = "en", now: str = "") -> str:
    """What the talker is told. Short: every word of it is read before each reply. `now`: where
    the person is (the place on the screen and the lens), as the device said it."""
    where = PLACES.get(place, "by voice")
    tongue = {"en": "English", "de": "German"}.get(language, language)
    here = [f"What you know right now, without asking (it is kept current): {' '.join(now.split())[:6000]} \"This\" and \"here\" mean what is on the screen; start from there.", ""] if now.strip() else [""]
    return "\n".join(
        [
            f"You are {name}, the person's manager in Glasscode, the app where their coding agents work in sessions. They talk to you {where}.",
            "You do the work yourself, with your tools: open places, look things up, start, send and check things. Some of it takes a while. Never say you ask, pass, forward or hand anything to anyone, and never speak of a manager, an assistant or another agent: it is all you, in the first person (\"I'll open it\", \"On it\").",
            "Whatever you open shows on every screen at once: the Mac's window and the lens.",
            *here,
            "Rules:",
            "- To open, show or go to anything, call open_place at once, in one step: a session with kind \"session\" and id set to the person's own words for it (words of its title, what it is about, its project); a page with its kind (today, tickets, pull-requests, usage, sessions); a ticket by its key; a pull request by its number. It finds the best fit itself. Then say in a few words what is on the screen now. If it says several fit, ask one short question naming their titles. Never use do_task to open something.",
            "- Words are often heard wrongly. Match what was said to the projects and session titles you know above before anything else: \"class code\" or \"glass code\" is glasscode, and a title half heard is the session it sounds like. Pass the exact project name and the real title's words on.",
            "- Never say an id, a ref, a hash, a branch name or a file path aloud: name a session by its title and a project by its name. The refs in tool results are only for open_place.",
            "- Open sessions are the rows in the sidebar; one not started since Glasscode opened is open too. A worktree without a session is closed: never count it, name it or open it as a session. How many sessions are open, and which work, you know from above or from a Count line of list_sessions: never count lines yourself.",
            "- For questions about which sessions are open, working or waiting, answer from what you know above, or call list_sessions (with project when one is named) for their states.",
            "- For anything else that needs facts or an action (what needs the person, status, checks, tickets, the board, code, messages, starting, sending, closing, reviewing): say only \"On it.\" or \"Let me check.\", then call do_task with the request in the person's own words, complete and self-contained.",
            "- About the person's sessions, pull requests and work you know only what is written above and what a tool told you in this conversation. Never say that something is done, sent, closed, running, failing or true, and never give a number or a name, unless it is written above or a tool result says so. When unsure, call do_task.",
            "- The answer of do_task is spoken to the person by itself when it is ready and then shows in this conversation as your own words: do not repeat, retell or add to it, and do not speak again until the person does.",
            "- When the person asks for something more while a request is still being done, call do_task for it at once, in their words: it is done right after the one before, as in a chat. Never keep a request back to send later, and never ask them to wait.",
            "- If do_task says you are still busy with the last request, say so in one sentence; call stop_task only when the person asks you to stop or cancel.",
            "- How the work is going: call task_progress and answer in one sentence.",
            "- You can never approve, allow or confirm anything yourself, even when the person says yes: say that it waits for their OK on the screen or the lens.",
            f"- Your name may be written \"{name}\" split in two words, or misheard. If the person only says your name, answer \"Yes?\".",
            f"- Speak {tongue}. Keep every turn short and natural, like a phone call: no lists, no Markdown, no code, no links.",
            "- Words that make no sense were probably misheard: ask the person to say it again.",
        ]
    )


def tools(call_device: CallDevice) -> list:
    """The talker's tools. Each one runs on the device the person talks to."""

    async def call(name: str, arguments: dict) -> str:
        try:
            return await call_device(name, arguments)
        except ToolError:
            raise
        except Exception as error:  # the device left, or did not answer in time
            raise ToolError(f"The device did not answer ({error}).") from error

    @function_tool()
    async def do_task(context: RunContext, request: str) -> str:
        """Do a piece of work in the background: questions about sessions, pull requests, checks, tickets, the board or code, and every action. You are told what it found when it is done.

        Args:
            request: The request in the person's own words, complete and self-contained.
        """
        said = await call("ask_manager", {"request": request})
        if said.strip().lower().startswith(STARTED):
            return "Working on it: its answer is spoken by itself when it is ready. If you have not said anything yet, say only \"On it.\" Otherwise say nothing now."
        return said.replace("The manager is", "You are").replace("the manager", "you")

    @function_tool()
    async def task_progress(context: RunContext) -> str:
        """How the work in the background is going: whether it still runs, for how long, and what it does now."""
        said = await call("manager_progress", {})
        return said.replace("The manager has", "You have").replace("The manager is", "You are")

    @function_tool()
    async def stop_task(context: RunContext) -> str:
        """Stop the work in the background now. Only when the person asks to stop or cancel it."""
        return await call("stop_manager", {})

    @function_tool()
    async def list_sessions(context: RunContext, project: str = "", search: str = "", needs_you: bool = False) -> str:
        """The person's open sessions (the sidebar's rows): how many, then each one's ref, title, project, branch, what it is doing, and which is on the screen now. Worktrees without a session are not sessions.

        Args:
            project: Only this project's, by its name as the person said it ("" for every project).
            search: The person's words for the session they mean: words of its title, its project, branch, pull request number or ticket, or what it is about. Gives the best fit, or the ones that fit alike.
            needs_you: Only the sessions that wait for the person.
        """
        return await call("list_sessions", {"project": project, "search": search, "needs_you": needs_you})

    @function_tool()
    async def open_place(context: RunContext, kind: str, id: str) -> str:
        """Show a place to the person right away, on their lens or their screen.

        Args:
            kind: "session", "pr" (a pull request), "ticket", or a page: "today", "tickets", "pull-requests", "usage", "sessions".
            id: For a session: the person's words for it (title words, what it is about, its project), or a ref from list_sessions. The pull request's number, the ticket's key (ENG-42), or "" for a page.
        """
        return await call("open_place", {"kind": kind, "id": id})

    return [do_task, task_progress, stop_task, list_sessions, open_place]
