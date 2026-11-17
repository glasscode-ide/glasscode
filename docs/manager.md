# The manager

The manager is one agent above all your sessions. You give it a name. It watches every
session, tells you what needs you, keeps track of what you asked of teammates in pull-request
reviews, tells a session when its pull request was reviewed, drafts short messages to
teammates, keeps a memory, and answers when you type or talk to it.

It is off until you switch it on.

## Switch it on

Open **Settings › Manager** and switch it on. There you also choose:

- **Its name and yours.** Its name is how you call it; yours is used in the signature of a
  message ("AI Alex, on behalf of Alex").
- **The agent that thinks for it.** Claude Code or Codex, with one of your accounts.
- **The projects it manages.** All of them unless you take one out. It never touches a
  project it does not manage.
- **Teammates.** A name, a GitHub login, and how your messenger knows them.
- **Messages.** Whether every message waits for your OK, or routine pings may go by
  themselves. And the messenger program (see below).
- **Where it runs.** On this Mac or on your server.
- **The daily update.** The time of day it tells you what is going on.

Then open the Manager screen and ask it anything: "what needs me?", "what is blocked?",
"walk me through today".

## What it does

- **Watches sessions.** It tells you when a session needs you, finished, or has failing
  checks.
- **Follows reviews.** When you ask for changes on a teammate's pull request, it keeps that
  as a follow-up and tells you when they pushed a fix. When someone reviews a pull request of
  one of your sessions, it tells that session to read the review.
- **Drafts messages.** Short messages to teammates, signed with its name and yours.
- **Remembers.** Facts you tell it, and facts it learns that still matter next week.
- **Shows and draws.** It takes you to the session or the part of the window it talks
  about, and draws small pictures while it explains.

Sessions and you can reach it with the `glasscode` command. `glasscode help manager` lists
everything, for example:

```
glasscode manager status
glasscode manager ask "what is blocked?"
glasscode manager remember "Sam owns the billing service"
glasscode manager follow-up add "the retry fix on #42" --person Sam
glasscode manager message list
```

## What it will not do

- It never approves, merges, closes or comments on a pull request, and never posts anything
  on GitHub. Its agent may use `gh` and `git` only to read.
- It never sends a message you have not allowed. Every message waits for your OK. If you
  allow routine pings, a short plain fact about work ("PR #42 is ready for another look")
  goes by itself; anything with a promise, a date, an opinion or a decision still waits.
- A message that would approve, merge, close or accept something in your name is never sent,
  whatever you allow.
- It does not change code. Work goes to a session.
- Everything it does to sessions goes through the `glasscode` command, so Settings ›
  Automation applies to it as to any agent.
- In its first round after you switch it on it says nothing about what was already there.

## Where its data lives

| What | Where |
| --- | --- |
| Follow-ups, messages, memory, the conversation | `~/.glasscode/manager/data.json` |
| The folder its agent runs in | `~/.glasscode/manager/work/` |
| On the server: what waits for the Mac | `~/.glasscode/manager/server.json` |
| Its settings | `~/.glasscode/state.json`, with your other settings |
| The voice keys | the macOS Keychain |

The files in `~/.glasscode/manager` are readable by you only. Nothing is sent to anyone but the agent you chose (Claude
Code or Codex, as in any session), GitHub through your own `gh`, and your messenger program.

## Running it on your server

On the Mac the manager stops when the Mac sleeps. If you have a server with `glasscoded`
(see the README), choose **Server** as its home in Settings › Manager. Then its rounds run
there; the Mac shows, speaks and types into its own sessions. The two copies of its data are kept in step; only
the copy at its home sends messages. When the server cannot be reached, the Mac takes over.

**Test** in Settings › Manager checks what the server needs: a folder it can write, `gh`
signed in, Claude Code or Codex signed in, your messenger program (when you set one), and
that the Mac and the server are in step.

On the server itself:

```
glasscoded manager status
glasscoded manager ask "what needs me?"
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

When the manager's home is the server, the messenger program runs there. Install it on the
server too (same name, on the PATH of your login shell) and sign it in there.
