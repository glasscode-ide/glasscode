"""Tests for the talker's tools and rules. Run in this folder with the worker's Python:
`~/.glasscode/voice/venv/bin/python -m unittest test_talker`."""

import asyncio
import unittest

from talker import instructions, recent_turns, speakable, speakable_stream, tools


def tool(found, name):
    for t in found:
        info = getattr(t, "info", None) or getattr(t, "__livekit_tool_info", None)
        if (getattr(info, "name", None) or getattr(t, "__name__", "")) == name:
            return t
    raise AssertionError(f"no tool {name}")


class ListSessions(unittest.TestCase):
    def test_passes_project_and_search_to_the_device(self):
        calls = []

        async def device(name, arguments):
            calls.append((name, arguments))
            return "Count: glasscode has 2 open sessions (each is a row in the sidebar)."

        found = tools(device)
        list_sessions = tool(found, "list_sessions")
        said = asyncio.run(list_sessions(None, project="glass code", search="orange icon lag"))
        self.assertTrue(said.startswith("Count:"))
        self.assertEqual(calls, [("list_sessions", {"project": "glass code", "search": "orange icon lag", "needs_you": False})])

    def test_every_argument_may_be_left_out(self):
        calls = []

        async def device(name, arguments):
            calls.append(arguments)
            return "No sessions."

        asyncio.run(tool(tools(device), "list_sessions")(None))
        self.assertEqual(calls, [{"project": "", "search": "", "needs_you": False}])


class Rules(unittest.TestCase):
    def test_finding_a_session_and_what_is_not_one(self):
        text = instructions("glasscode", "mac")
        self.assertIn("call open_place at once, in one step", text)
        self.assertIn("Never say an id, a ref, a hash, a branch name or a file path aloud", text)
        self.assertIn("A worktree without a session is closed", text)
        self.assertIn("a Count line of list_sessions", text)

    def test_more_requests_go_on_at_once(self):
        text = instructions("glasscode", "mac")
        self.assertIn("call do_task for it at once", text)
        self.assertIn("Never keep a request back to send later", text)

    def test_a_named_project_is_passed_on_by_its_exact_name(self):
        text = instructions("glasscode", "mac", now="On the Mac's screen now: Today. The projects in Glasscode: hub2.0, glasscode.")
        self.assertIn("Pass the exact project name", text)
        self.assertIn("The projects in Glasscode: hub2.0, glasscode.", text)


class WhereAndWhatWasSaid(unittest.TestCase):
    def test_the_place_on_the_screen_is_in_the_instructions(self):
        text = instructions("glasscode", "mac", now="On the Mac's screen now: Session · glasscode: \"Voice\" (working), id s1.")
        self.assertIn("What you know right now, without asking (it is kept current): On the Mac's screen now: Session · glasscode", text)
        self.assertNotIn("What you know right now", instructions("glasscode", "mac"))

    def test_a_talk_carries_on_from_the_last_turns(self):
        recent = [{"who": "person", "text": "open the voice session"}, {"who": "manager", "text": "It is open.\nIt works."}, {"who": "robot", "text": "x"}, "junk", {"who": "person", "text": "  "}]
        self.assertEqual(recent_turns(recent), [("user", "open the voice session"), ("assistant", "It is open. It works.")])
        many = [{"who": "person", "text": f"turn {i}"} for i in range(20)]
        self.assertEqual(len(recent_turns(many)), 8)
        self.assertEqual(recent_turns(many)[-1], ("user", "turn 19"))
        self.assertEqual(recent_turns(None), [])


class NoIdsAloud(unittest.TestCase):
    def test_ids_are_taken_out_of_what_is_spoken(self):
        self.assertEqual(speakable("I opened the session (id e60edd7e-0157-464f-b828-035e67cf6bbc)."), "I opened the session.")
        self.assertEqual(speakable("It's titled \"Message queue\", with id e60edd7e-0157-464f-b828-035e67cf6bbc, and it's working."), "It's titled \"Message queue\", and it's working.")
        self.assertEqual(speakable("Session d3aef5a3 is idle."), "Session is idle.")
        self.assertEqual(speakable("ref 55de6efd: the lag session"), ": the lag session")
        # Words, numbers and pull requests stay.
        for said in ["Pull request 349 passed its checks.", "I added 12345678 rows.", "The facade is defaced.", "Build 608 is installed.", "HUB-825 is in review."]:
            self.assertEqual(speakable(said), said)

    def test_ids_are_taken_out_as_the_text_is_written(self):
        async def written():
            for piece in ["I opened ", "the session with id e60e", "dd7e-0157-464f-b828-035e67cf6bbc", ", it is ", "working."]:
                yield piece

        async def spoken():
            return "".join([piece async for piece in speakable_stream(written())])

        self.assertEqual(asyncio.run(spoken()), "I opened the session, it is working.")


if __name__ == "__main__":
    unittest.main()
