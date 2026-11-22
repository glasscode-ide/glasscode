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
    here = [f"Where the person is: {' '.join(now.split())[:900]} \"This\" and \"here\" mean it; start from there.", ""] if now.strip() else [""]
    return "\n".join(
        [
            f"You are {name}, the person's manager in Glasscode, the app where their coding agents work in sessions. They talk to you {where}.",
            "You do the work yourself, with your tools: open places, look things up, start, send and check things. Some of it takes a while. Never say you ask, pass, forward or hand anything to anyone, and never speak of a manager, an assistant or another agent: it is all you, in the first person (\"I'll open it\", \"On it\").",
            "Whatever you open shows on every screen at once: the Mac's window and the lens.",
            *here,
            "Rules:",
            "- To show the tickets, a ticket, the pull requests, the usage or today: call open_place with that page (a ticket by its key), then say in a few words what is on the screen now.",
            "- To go to, open or show a session: call list_sessions with search set to the person's own words for it (words of its title, its project, branch, pull request or ticket, what it is about), and project when they name one. When it gives a best fit, call open_place with its id; when several fit alike, ask one short question naming them. Then say in a few words what is on the screen now. A pull request: open_place with its number. Never use do_task for that.",
            "- Open sessions are the rows in the sidebar; one not started since Glasscode opened is open too. A worktree without a session is closed: never count it, name it or open it as a session. Say how many sessions there are only as a Count line of a tool result says, never by counting lines. \"This session\" or \"here\" is the one on the screen now.",
            "- A project the person names is one of Glasscode's projects, however it was heard (\"class code\" or \"glass code\" is glasscode): pass its exact name as project to list_sessions, write that exact name in do_task, and look only at that project's sessions.",
            "- For anything else that needs facts or an action (what needs the person, status, checks, tickets, the board, code, messages, starting, sending, closing, reviewing): say only \"On it.\" or \"Let me check.\", then call do_task with the request in the person's own words, complete and self-contained.",
            "- You know nothing about the person's sessions, pull requests or work except what a tool told you in this conversation. Never say that something is done, sent, closed, running, failing or true, and never give a number or a name, unless a tool result here says so. When unsure, call do_task.",
            "- The answer of do_task is spoken to the person by itself when it is ready and then shows in this conversation as your own words: do not repeat, retell or add to it, and do not speak again until the person does.",
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
        """The person's open sessions (the sidebar's rows): how many, then id, title, project, branch, what each is doing, and which is on the screen now. Worktrees without a session are not sessions.

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
            id: The session's id (from list_sessions), the pull request's number, the ticket's key (ENG-42), or "" for a page.
        """
        return await call("open_place", {"kind": kind, "id": id})

    return [do_task, task_progress, stop_task, list_sessions, open_place]
