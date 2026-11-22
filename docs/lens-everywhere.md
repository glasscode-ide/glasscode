# One place everywhere: the Mac, the phone and the lens

What the owner asked for (2026-11-22): whatever is asked, from the glasses, the phone or the
Mac, does the same thing. Opening a session, a project, the tickets, the usage or anything else
shows it in the IDE **and** on the lens. Each place has a lens version, with less on it, and the
band's gestures move through it (the next ticket, the one below). A session that asks for an OK
shows it there. The voice knows where the person is and starts from there, and a voice that is
stopped and started again carries on the same conversation. The lens and the phone look modern
and use the whole screen.

## The shape

- **The Mac is the one place.** Its window's place (`lens_place`: the session on screen, Today,
  the board or a ticket in it, the pull requests, the usage) is the place. Whenever it changes,
  or what it shows changes, the Mac's door sends each phone a `place` event: the place's
  **lens page** (`glasscode_core::lens::Page`), built by the Mac so every device shows the same
  thing.
- **Opening from anywhere opens on the Mac.** The phone (a gesture on the lens, a tap on the
  phone, its voice) asks the door `place.open`. The Mac opens the place in its window, and the
  page comes back to every phone as a `place` event. The manager's `device.open` also opens the
  place in the Mac's window, and its `ui.open` reaches the lens through the same event.
- **The voice's tools run on the Mac.** The phone's talker tools `list_sessions` and
  `open_place` ask the door `voice.tool`, so the phone answers exactly as the Mac does
  (`voice_sessions`). Without the Mac (the server only) the phone answers from its own list.
- **The voice knows where the person is.** The voice's config carries `now` (the place on
  screen, in a sentence) and `recent` (the last turns of the manager's conversation). The worker
  puts `now` into the talker's instructions as it changes, and starts each talk with `recent`
  in the talker's history, so a talk started again carries on.

## The lens page

`kind` (session, today, tickets, ticket, prs, pr, usage, sessions, settings), `place` (the word
`place.open` takes), an `eyebrow` (what it is, its project), a `title`, a `status` with its tone,
an optional `meter` (0 to 1: the usage), a `body` (the last reply, a ticket's description), the
`items` (rows: title, detail, tone, the place each opens), and `prev`/`next` (the places beside
it in its list: a ticket's neighbours).

## The band on a page

- **Swipe down / up**: the next or the previous row. On a page without rows (a ticket), the next
  or the previous one in its list: the tickets one by one.
- **Swipe right** or **pinch** on a row: open it, on the lens and on the Mac.
- **Pinch** with no row chosen: talk about what is shown.
- **Swipe left**: back.

## Colours (research)

The Ray-Ban Display is a 600 × 600 full-colour LCoS projector in a geometric waveguide, up to
5000 nits, set off to the right of the eye. It is additive: black is clear glass, and bright
large areas leak light and wash the world out. Dark or low-luminance colours (deep blue, red,
magenta) vanish against a bright world; cyan and magenta wash out in sunlight. So:

- Black backgrounds (clear), never filled panels; one accent at a time, in thin strokes and
  small shapes.
- Text off-white (#EDEFF2), never pure white; grey (#9AA4AE) for the second line.
- High-luminance accents: mint #5CFFB0 (good), ice #9BE7FF (working), amber #FFC24D (needs
  you), coral #FF7A66 (failing), grey #8A94A0 (nothing).
- Glows and outlines, not shadows (a shadow looks like dirt on the glass).

The toolkit draws text only in its two colours; the colour is in small images the phone draws
(`LensArt`): the status orb, the meter, the accent rule. The phone uses the same palette on a
near-black ground, edge to edge.

## Slices

1. Core: `lens::Page` and its words (`glasscode_core/src/lens.rs`).
2. Mac: `lens_pages.rs` (the place, each place's page), the door's `place` event, `place.open`,
   `voice.tool`; `device.open` opens on the Mac too.
3. Voice: `now` and `recent` in the config (Mac app, helper, worker, phone).
4. Lens: the page card, its gestures, `LensArt`, following the Mac's place.
5. Phone: one full-screen view, the place's page and a talk button, in the same look.

## The voice knows, and stays quiet when asked

- **What it knows without asking.** `now` is the place on the screen, then what is open in
  Glasscode (each project: how many sessions are open, which work or wait for the person, and
  every open session's title), then the projects by name. The Mac sends it to its own voice and,
  with each `place` event, to the phone's. A name heard wrongly is matched to these.
- **One step to open.** `open_place` takes the person's own words for a session; the Mac finds
  the best fit (`session_find`), in the project they named.
- **No ids aloud.** Tool results carry short refs for `open_place` only, and the worker takes
  ids out of everything that is spoken (`talker.speakable`), whoever wrote it.
- **Silent.** On the phone "Make it silent" (and a swipe down on the lens while it listens)
  switches the microphone off and stops the voice; the talk stays open, and answers are shown,
  not spoken, until the person talks again. "End talk" leaves the room.
- **The list of sessions** has no view of its own in the Mac's window (it shows one session), so
  it is a page over the window's place until the window goes somewhere else.

## Checked against a whole Glasscode

`script/journeys.sh 9` runs a real Glasscode on a scratch home and a stand-in phone built from
the iPhone app's own link code. It checks, over the real link: the voice's tools answered by the
Mac, every page and a session (by its title and project, and by its id) opening in the window,
the page coming back to the phone, and that what is not there opens nothing. It found that the
door's list of what a phone may call (`caller::PHONE_ALLOWED`) lacked `place.open`, `place.get`
and `voice.tool`: every open from the phone had been refused.

## Status (2026-11-22)

All five slices are built. Not covered yet: a phone that reaches only the server (the Mac off)
gets no `place` events and answers the voice's tools from its own list, as before; the lens's
look has been checked on the toolkit's simulated Ray-Ban Display, not on the glasses.
