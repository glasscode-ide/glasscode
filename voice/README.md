# Voice

The manager's voice: you talk to it, it talks back. Two parts live here.

- [`manager/`](manager/README.md): the voice room on a machine of your own (a LiveKit server, and a
  worker that turns speech into text and text into speech). The manager's brain stays in
  Glasscode.
- [`helper/`](helper/): `manager-voice`, the Mac's headless helper that joins that room with the
  Mac's microphone and speaker. Glasscode starts it from the voice screen, the microphone buttons
  and the manager's orb (`crates/glasscode/src/manager_voice.rs`). Build it with
  `cd helper && swift build -c release --product manager-voice`; `script/bundle-mac.sh` bundles it.

**The glasses** do not use the room. The glasses app turns speech into text on the iPhone itself,
asks the manager over the device link, and speaks the answer with the phone's voices to the
glasses: see [`../docs/architecture.md`](../docs/architecture.md#the-glasses-app).

The ElevenLabs copilot that used to live here (`server.mjs` on port 4517, its shared token, its
web page and its five tools) was removed by the glasses plan (G16): the manager replaces it.
