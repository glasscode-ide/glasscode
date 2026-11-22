"""Tests for the talker's tools and rules. Run in this folder with the worker's Python:
`~/.glasscode/voice/venv/bin/python -m unittest test_talker`."""

import asyncio
import unittest

from talker import instructions, recent_turns, tools


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
        self.assertIn("call list_sessions with search set to the person's own words", text)
        self.assertIn("A worktree without a session is closed", text)
        self.assertIn("only as a Count line of a tool result says", text)

    def test_more_requests_go_on_at_once(self):
        text = instructions("glasscode", "mac")
        self.assertIn("call do_task for it at once", text)
        self.assertIn("Never keep a request back to send later", text)

    def test_a_named_project_is_passed_on_by_its_exact_name(self):
        text = instructions("glasscode", "mac", now="On the Mac's screen now: Today. The projects in Glasscode: hub2.0, glasscode.")
        self.assertIn("pass its exact name as project to list_sessions", text)
        self.assertIn("The projects in Glasscode: hub2.0, glasscode.", text)


class WhereAndWhatWasSaid(unittest.TestCase):
    def test_the_place_on_the_screen_is_in_the_instructions(self):
        text = instructions("glasscode", "mac", now="On the Mac's screen now: Session · glasscode: \"Voice\" (working), id s1.")
        self.assertIn("Where the person is: On the Mac's screen now: Session · glasscode", text)
        self.assertNotIn("Where the person is", instructions("glasscode", "mac"))

    def test_a_talk_carries_on_from_the_last_turns(self):
        recent = [{"who": "person", "text": "open the voice session"}, {"who": "manager", "text": "It is open.\nIt works."}, {"who": "robot", "text": "x"}, "junk", {"who": "person", "text": "  "}]
        self.assertEqual(recent_turns(recent), [("user", "open the voice session"), ("assistant", "It is open. It works.")])
        many = [{"who": "person", "text": f"turn {i}"} for i in range(20)]
        self.assertEqual(len(recent_turns(many)), 8)
        self.assertEqual(recent_turns(many)[-1], ("user", "turn 19"))
        self.assertEqual(recent_turns(None), [])


if __name__ == "__main__":
    unittest.main()
