# The manager

The manager is one agent above all your sessions. You give it a name. It watches every
session, tells you what needs you, tells a session when its pull request was reviewed, keeps
track of what you asked of teammates in reviews, drafts short messages to teammates, and answers
when you type or talk to it. It keeps one memory per project that every agent shares, and it
follows each project's level: what it may do there without asking, and what waits for you on a
card.

It is off until you switch it on.

## Switch it on

Open **Settings › Manager** and switch it on. There you also choose:

- **Its name and yours.** Its name is how you call it; yours is used in the signature of a
  message ("AI Alex, on behalf of Alex").
- **The agent that thinks for it.** Claude Code or Codex, with one of your accounts.
- **The projects it manages, and each one's level.** All of them unless you take one out. Each
  project is Careful, Standard or Autopilot (below). It never touches a project it does not
  manage.
- **Teammates.** A name, a GitHub login, and how your messenger knows them.
- **Messages.** Whether every message waits for your OK, or routine pings may go by
  themselves. And the messenger program (see below).
- **Where it runs.** Automatic (this Mac while you use it, your server while the Mac sleeps),
  always on the server, or always this Mac.
- **The daily update.** The time of day it tells you what is going on.
- **Memory, Move in and Learning.** What memory keeps and where it came from; bringing in what
  is already on your computer; and the hourly upkeep of memory, off until you switch it on.

Then open the Manager screen and ask it anything: "what needs me?", "what is blocked?",
"walk me through today".

## What it does

- **Watches sessions.** It tells you when a session needs you, finished, is stuck, or has
  failing checks, and hands a session on when its account runs out of usage.
- **Follows reviews.** When you ask for changes on a teammate's pull request, it keeps that
  as a follow-up and tells you when they pushed a fix. When someone you trust reviews a pull
  request of one of your sessions, it tells that session to read the review.
- **Drafts messages.** Short messages to teammates, signed with its name and yours.
- **Remembers.** One memory per project, shared by every agent. A fact about you, your people
  or your decisions comes from your own typed words, or waits in the inbox for your tick.
- **Gives every turn one report.** Each turn of each session ends with exactly one report: how
  it ended (done, failed, stopped, crashed…) and the question it stopped on, if any. What a
  session says about its own work is a hint the manager checks against what it sees.
- **Runs your playbooks and schedules.** Small rules ("when CI fails on a draft, tell me") and
  checks at a time, each approved by you before it runs.
- **Shows and draws.** It takes you to the session or the part of the window it talks
  about, and draws small pictures while it explains.

Sessions and you can reach it with the `glasscode` command. `glasscode help manager` lists
everything, for example:

```
glasscode manager status
glasscode manager ask "what is blocked?"
glasscode memory search "how do we deploy"
glasscode memory propose "Sam owns the billing service"
glasscode manager follow-up add "the retry fix on #42" --person Sam
glasscode manager message list
glasscode approvals
```

## Levels: what it may do

- **Careful.** It asks before anything but reading, the board and its own notes.
- **Standard.** It starts and drives sessions, and asks before anything that leaves the Mac or
  removes work.
- **Autopilot.** It may also act on the forge, and only where the forge's own rules (branch
  protection, required reviews) hold. Settings › Projects says whether they do.

Anything a level does not allow waits on a card in Today and in the manager's conversation,
with Allow and Deny. Settings › Manager shows each level's table and previews any change before
it is made.

## What it cannot do

- It cannot approve, decide a pending card, or change its own level, playbooks or settings.
  Only you can, in the app (a paired phone may confirm an approval).
- It never sends a message you have not allowed. Every message waits for your OK. If you
  allow routine pings, a short plain fact about work ("PR #42 is ready for another look")
  goes by itself; anything with a promise, a date, an opinion or a decision still waits.
- A message that would approve, merge, close or accept something in your name is never sent,
  whatever you allow.
- It does not change code. Work goes to a session.
- Its brain has no shell and no file tools beyond its own folder. It runs in
  `~/.glasscode/manager/work`, never in a repository, so a cloned repository's hooks and MCP
  servers never run as you. It reads code through Glasscode's tools only.
- It cannot remember what nobody said. Text from a pull request, a tool's output or a pasted
  block is never a fact by itself, and what a session learns about code holds only while the
  lines it cites stay the same.
- It cannot send anything of a project marked "stays on this Mac" to the server: not its memory,
  sessions, words, journal or cards.
- It cannot run past its budget: at most six routine wakes an hour, a daily token cap, and
  nothing on paid overage unless you allow it. With no budget left, or signed out, it answers
  from what it knows and says why. When its plan's limit is used up, it holds your question and
  answers when the limit resets, or you ask the other agent (Claude or Codex) instead.
- It cannot start on a Claude Code older than 2.1.322 or a Codex older than 0.167.0. It says
  which version it needs.
- It cannot do the same thing twice. Each effect (a message, a report, a push) is done once,
  under its key, also after a crash or a restart.
- In its first round after you switch it on it says nothing about what was already there.
- It cannot keep a program that runs as you, outside any sandbox, from calling Glasscode as the
  manager. Its brain's Glasscode tools carry a signature, kept in a file only you can read
  (`~/.glasscode/manager/run/`), never on a command line; anything that runs as you can still read
  that file. Sessions cannot: their profiles deny that folder.

## Where its data lives

| What | Where |
| --- | --- |
| The conversation, by topic | `~/.glasscode/manager/conversation/` |
| Memory (a git repository of Markdown files) | `~/.glasscode/memory/` |
| The journal: what happened, a file per UTC day (30 kept) | `~/.glasscode/journal/` |
| Effects, each done once | `~/.glasscode/effects/` |
| Work items and follow-ups | `~/.glasscode/manager/work.json` |
| Messages to teammates, and the ones that came in | `~/.glasscode/manager/outbox.json`, `inbox.json` |
| The daily brief; questions held until a limit resets | `~/.glasscode/manager/brief.json`, `held.json` |
| Levels, playbooks, schedules and the cards that wait | `~/.glasscode/authority/` |
| The folder its agent runs in | `~/.glasscode/manager/work/` |
| Its settings | `~/.glasscode/state.json`, with your other settings |
| The voice keys | the macOS Keychain |
| The first manager's data, already moved into the files above | `~/.glasscode/manager/data.json` and its copy `data.v1.json` |

The files in `~/.glasscode/manager` are readable by you only. Nothing is sent to anyone but the agent you chose (Claude
Code or Codex, as in any session), GitHub through your own `gh`, your messenger program, and your
own server when you pair one.

## Running it on your server

On the Mac the manager stops when the Mac sleeps. If you have a server with `glasscoded`
(see the README), choose where it runs in Settings › Manager. Automatic hands it to the server
while the Mac sleeps or you switch to Server mode, and back when you use the Mac again; only one
of them is its home at a time. On the server it watches and drives the sessions there, follows
up on reviews, and makes cards the Mac shows. What only the Mac can do (moving the window,
sending a message through your messenger) waits for the Mac.

**Test** in Settings › Manager checks what the server needs: a folder it can write, `gh`
signed in, Claude Code or Codex signed in, and what decides whether the manager runs there.

On the server itself:

```
glasscoded manager status
```

## Voice

You can talk to the manager and hear it answer. That needs a small voice service on a machine
of your own. [`voice/manager/README.md`](../voice/manager/README.md) explains how to set it
up. Nothing of it is needed when you only type.

## Messages: the messenger program

Glasscode does not know any messaging service. Instead you give it a **messenger program**:
any program that can send a text to a person and list new messages. With one small script you
can connect Slack, email, or anything else.

Set it in Settings › Manager: a program name that is on your PATH, or a full path
(`~/bin/my-messenger`). Leave it empty and the manager still drafts messages, but none is
sent ("No messenger is set up. Choose one in Settings › Manager.") and no inbox is read.

[`script/messenger-example.sh`](../script/messenger-example.sh) is a working example that
needs no account: it writes what would be sent to a file. Start from it.

### The four calls

The program is called with the environment your sessions get. It prints JSON on stdout.

**1. Who you are**

```
my-messenger whoami
```

```json
{"id": "U123", "name": "Alex Example", "email": "alex@example.com"}
```

The manager uses this to tell your own messages from other people's. If this call fails, it
reads no inbox at all.

**2. Find a person**

```
my-messenger people "Sam"
```

```json
[{"id": "U456", "name": "Sam Example", "email": "sam@example.com"}]
```

An empty list when nobody matches.

**3. Send a message**

```
my-messenger send --to "U456" [--urgent]
```

The text comes on **stdin** (UTF-8, it may have several lines; the last line is the
signature). `--to` is what you entered for that teammate in Settings › Manager, or their
name when you entered nothing. `--urgent` marks a reply to an urgent message; what it means
is up to your program. Nothing needs to be printed.

This is only ever called for a message you allowed.

**4. New messages**

```
my-messenger inbox --since 1794735000
```

```json
[
  {
    "id": "m-1001",
    "chat": "D789",
    "from": "Sam Example",
    "text": "Can you look at #42?",
    "at": 1794735600,
    "urgent": false,
    "handled": false
  }
]
```

Messages other people wrote to you after `--since` (unix seconds).

| Field | Meaning |
| --- | --- |
| `id` | Stays the same for the same message, so it is not taken twice. |
| `chat` | Where it was written (optional). |
| `from` | Who wrote it: a name, or `{"id", "name", "email"}`. |
| `text` | The message as plain text. |
| `at` | When it was written: unix seconds, or an ISO time such as `2026-11-15T09:30:00Z`. |
| `urgent` | Your messenger marked it as urgent. |
| `handled` | You have read it already. |

A message whose `from` is your own `id`, `name` or `email` from `whoami` is left out, so the
manager never reacts to what you (or it, in your name) wrote. The manager asks about every
two minutes, and never looks further back than three days.

A message counts as urgent when `urgent` is true or its text says so ("urgent", "asap",
"prod is down"). An urgent message from a teammate you allowed to alert you notifies you
at once, and a holding reply is drafted for your OK. Nothing else is answered.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Done. |
| `3` | You are not signed in. The manager tells you so. |
| anything else | It failed. Write one line to stderr saying why; you see that line. |

A call that takes longer than 30 seconds is stopped. A message that failed to send waits for
you again; it is not retried by itself, because it may have gone out after all.

### On your server

The messenger program runs on the Mac. While the server is the manager's home, a message to a
teammate waits in the outbox until the Mac is home again.
