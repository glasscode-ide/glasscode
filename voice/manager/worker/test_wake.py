"""Tests for the wake word. Run in this folder: `python3 -m unittest test_wake`."""

import unittest

from wake import Attention, is_echo, listen_value, name_forms, strip_name


class StripName(unittest.TestCase):
    def test_plain_name_in_front(self):
        self.assertEqual(strip_name("Manager, what is running?", "Manager"), "what is running?")
        self.assertEqual(strip_name("manager what is running", "Manager"), "what is running")

    def test_greeting_in_front_is_skipped(self):
        self.assertEqual(strip_name("Hey Manager, any reviews?", "Manager"), "any reviews?")
        self.assertEqual(strip_name("OK, Manager: status", "Manager"), "status")

    def test_two_word_name_and_its_last_word(self):
        self.assertEqual(strip_name("AI Mira, give me the update", "AI Mira"), "give me the update")
        self.assertEqual(strip_name("Mira, give me the update", "AI Mira"), "give me the update")

    def test_spelled_differently_by_speech_to_text(self):
        self.assertEqual(strip_name("A.I. Mira, what's new?", "AI Mira"), "what's new?")
        self.assertEqual(strip_name("Hey I Mira what's new", "AI Mira"), "what's new")
        self.assertEqual(strip_name("Samira, are you there", "Samirah"), "are you there")

    def test_only_the_name(self):
        self.assertEqual(strip_name("AI Mira.", "AI Mira"), "")
        self.assertEqual(strip_name("Hey Manager!", "Manager"), "")

    def test_name_not_at_the_start(self):
        self.assertIsNone(strip_name("I told the manager about it", "Manager"))
        self.assertIsNone(strip_name("what is running?", "Manager"))
        self.assertIsNone(strip_name("", "Manager"))

    def test_other_words_do_not_match(self):
        self.assertIsNone(strip_name("Mirror the display please", "AI Mira"))
        self.assertIsNone(strip_name("Sam, come here", "Samirah"))

    def test_short_names_must_be_exact(self):
        self.assertEqual(strip_name("Sam, come here", "Sam"), "come here")
        self.assertIsNone(strip_name("Sat down", "Sam"))

    def test_accents_do_not_matter(self):
        self.assertEqual(strip_name("René, the checks?", "Rene"), "the checks?")

    def test_forms(self):
        self.assertEqual(name_forms("AI Mira"), ["aimira", "mira"])
        self.assertEqual(name_forms("Manager"), ["manager"])
        self.assertEqual(name_forms("AI"), [])


class Echo(unittest.TestCase):
    SPOKEN = "Two sessions need you. The checks on the billing pull request failed, and Sam asked for a review."

    def test_its_own_words_coming_back(self):
        self.assertTrue(is_echo("the checks on the billing pull request failed", self.SPOKEN))
        self.assertTrue(is_echo("Two sessions need you the checks on the building pull request failed", self.SPOKEN))

    def test_the_person_answering(self):
        self.assertFalse(is_echo("open the billing pull request for me", self.SPOKEN))
        self.assertFalse(is_echo("which sessions need me right now", self.SPOKEN))

    def test_short_replies_are_never_echo(self):
        self.assertFalse(is_echo("stop", self.SPOKEN))
        self.assertFalse(is_echo("need you", self.SPOKEN))
        self.assertFalse(is_echo("the checks failed", ""))


class Listening(unittest.TestCase):
    def test_wake_needs_the_name(self):
        ears = Attention("AI Mira", "wake")
        self.assertIsNone(ears.accept("what is running?", now=100.0))
        self.assertEqual(ears.accept("AI Mira, what is running?", now=100.0), "what is running?")

    def test_conversation_stays_open_for_a_while(self):
        ears = Attention("AI Mira", "wake", window_secs=20.0)
        self.assertEqual(ears.accept("Mira, what is running?", now=100.0), "what is running?")
        self.assertEqual(ears.accept("and the reviews?", now=110.0), "and the reviews?")
        self.assertIsNone(ears.accept("and the reviews?", now=140.0))

    def test_open_after_the_manager_spoke(self):
        ears = Attention("AI Mira", "wake", window_secs=20.0)
        ears.keep_open(now=200.0)
        self.assertEqual(ears.accept("yes, do that", now=215.0), "yes, do that")
        self.assertIsNone(ears.accept("yes, do that", now=250.0))

    def test_only_the_name_opens_the_conversation(self):
        ears = Attention("AI Mira", "wake")
        self.assertEqual(ears.accept("AI Mira", now=10.0), "")
        self.assertEqual(ears.accept("what is running?", now=15.0), "what is running?")

    def test_always_passes_everything_and_never_nothing(self):
        self.assertEqual(Attention("AI Mira", "always").accept("what is running?", now=1.0), "what is running?")
        self.assertIsNone(Attention("AI Mira", "never").accept("AI Mira, hello", now=1.0))

    def test_listen_values(self):
        self.assertEqual([listen_value(v) for v in ("wake", "always", "never", "hold")], ["wake", "always", "never", "always"])
        self.assertIsNone(listen_value("sometimes"))
        self.assertIsNone(listen_value(None))

    def test_configure_keeps_what_is_not_given(self):
        ears = Attention("Manager", "wake")
        ears.configure("AI Mira", None)
        ears.configure(None, "always")
        ears.configure("  ", "sometimes")
        self.assertEqual((ears.name, ears.listen), ("AI Mira", "always"))

    def test_addressed_for_the_state(self):
        ears = Attention("AI Mira", "wake")
        self.assertFalse(ears.addressed("what is", now=1.0))
        self.assertTrue(ears.addressed("AI Mira what", now=1.0))
        ears.keep_open(now=1.0)
        self.assertTrue(ears.addressed("what is", now=5.0))


if __name__ == "__main__":
    unittest.main()
