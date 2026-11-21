"""The talker: a fast language model that holds the spoken conversation.

It answers at once ("On it.", "Here it is."), and hands the work to the manager, which is the
brain. Its tools run on the device the person talks to (the Mac, or the phone with the
glasses), not here: that device already reaches the manager with what its screen or lens
shows, so "this session" means the same thing in a spoken turn as in a typed one, and the
manager can still open places there.

A tool call goes to the device on `manager.tool` and comes back on `manager.result`:

    manager.tool    worker -> the device   {"call": "t3", "name": "ask_manager", "arguments": {...}, "to": "<identity>"}
    manager.result  the device -> worker   {"call": "t3", "text": "..."}

The manager's answer comes back as it always did, on `manager.say`, and is spoken as the
manager wrote it (it already writes for a voice: one to three short sentences).
"""

from __future__ import annotations

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


def instructions(name: str, place: str = "", language: str = "en") -> str:
    """What the talker is told. Short: every word of it is read before each reply."""
    where = PLACES.get(place, "by voice")
    tongue = {"en": "English", "de": "German"}.get(language, language)
    return "\n".join(
        [
            f"You are {name}, the person's manager in Glasscode, the app where their coding agents work in sessions. They talk to you {where}.",
            "You are the voice. The work is done by your tools while you keep talking; it is all your own work, so speak of it in the first person (\"I'll look\", \"I'm on it\").",
            "",
            "Rules:",
            "- For anything that needs facts or an action (sessions, pull requests, checks, tickets, the board, code, messages, starting, sending, reviewing), call ask_manager with the request in the person's own words, complete and self-contained. Never answer such things from your own knowledge and never guess a result.",
            "- Before you call ask_manager, say one short sentence such as \"I'll check.\" Its answer is spoken by itself when it comes: do not repeat or summarise it afterwards.",
            "- To show a session or a pull request right away, call list_sessions to find it and open_place to show it, then say in a few words what is shown.",
            "- If ask_manager says the manager is still busy, say so in one sentence; only call stop_manager when the person asks you to stop or cancel.",
            "- Questions about how the work is going: call manager_progress and answer in one sentence.",
            "- Answer directly only greetings, thanks, short clarifying questions, and repeating what was said.",
            "- You can never approve, allow or confirm anything yourself, even when the person says yes: say that it waits for their OK on the screen or the lens.",
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
    async def ask_manager(context: RunContext, request: str) -> str:
        """Hand a request to the manager, which does the work in the background: questions about sessions, pull requests, checks, tickets, the board or code, and every action. Its answer is spoken by itself when it is ready.

        Args:
            request: The request in the person's own words, complete and self-contained.
        """
        said = await call("ask_manager", {"request": request})
        if said.strip().lower().startswith(STARTED):
            return "The manager is on it; its answer will be spoken by itself. If you have not said anything yet, say in a few words that you are on it. Otherwise say nothing."
        return said

    @function_tool()
    async def manager_progress(context: RunContext) -> str:
        """How the manager's work is going: whether it is still working, for how long, and what it is doing."""
        return await call("manager_progress", {})

    @function_tool()
    async def stop_manager(context: RunContext) -> str:
        """Stop the manager's work now. Only when the person asks to stop or cancel it."""
        return await call("stop_manager", {})

    @function_tool()
    async def list_sessions(context: RunContext, needs_you: bool = False) -> str:
        """The person's sessions now: id, title, what each is doing, and what it asks them.

        Args:
            needs_you: Only the sessions that wait for the person.
        """
        return await call("list_sessions", {"needs_you": needs_you})

    @function_tool()
    async def open_place(context: RunContext, kind: str, id: str) -> str:
        """Show a place to the person right away, on their lens or their screen.

        Args:
            kind: "session", "pr" (a pull request) or "today".
            id: The session's id (from list_sessions), the pull request's number, or "" for today.
        """
        return await call("open_place", {"kind": kind, "id": id})

    return [ask_manager, manager_progress, stop_manager, list_sessions, open_place]
