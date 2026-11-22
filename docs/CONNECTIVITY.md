# Connecting Glasscode

Glasscode has three pieces that talk to each other:

- **The IDE** (your Mac): runs your coding sessions and the manager while you work there.
- *(Optional)* **Your server**: keeps sessions running, and is the manager's home while the Mac
  sleeps.
- **The glasses app** (iPhone with Meta Ray-Ban Display and the Neural Band): you talk to the
  manager from it. See [`mobile-glasses-plan.md`](mobile-glasses-plan.md) and
  [`architecture.md`](architecture.md#the-glasses-app).

They reach each other over **Tailscale**: one private network (a tailnet) of your own devices,
encrypted end to end (WireGuard), on any internet connection, with no port to open and no server
to run. On the same Wi-Fi, Tailscale goes direct.

> **Windows:** this guide is for the Mac. On Windows the glasses' door, pairing and the server
> features are not ported yet. See [`windows.md`](windows.md#what-is-not-ported).

---

## 1. Put your devices on one tailnet

1. Install Tailscale and sign in with the same account on the **Mac**, the **iPhone** and the
   **server** if you have one. A free personal account covers them.
2. On the Mac, `tailscale ip -4` prints its tailnet address (`100.x.y.z`). Glasscode finds it by
   itself; you never type it.

## 2. Pair the glasses app with the Mac

1. On the Mac: **Settings › Server › Glasses › Pair glasses**. A code shows for ten minutes.
2. On the iPhone: open Glasscode and scan it (or paste the pairing link).
3. That is all. The phone made its own key in its Secure Enclave; the Mac pinned it, and the phone
   pinned the Mac's door key. With your server connected, the phone is paired with it too, in the
   same step.

The Mac's **device door** opens on its tailnet address (port 4519) while a phone is paired, and
answers only a phone it paired, from the tailnet. The title bar's glasses icon shows a green dot
while the glasses are connected.

## 3. Away from the Mac

The phone talks to wherever the manager is home:

- **The Mac is awake**: through its door, from anywhere on the tailnet.
- **The Mac sleeps**: through your server, over SSH with the same key, into
  `glasscoded gate --device <id>`. The server's manager answers, in the same conversation. What
  only the Mac can do (its window, your messenger) waits for it, and the manager says so.

Press **Server mode** before you leave, so your open sessions move to the server and the manager
can drive them while the Mac sleeps.

---

## Self-hosted relay (optional, the Mac and the server only)

When the Mac cannot reach the server over Tailscale or SSH directly (a locked-down network), the
optional relay joins them: see [`relay/README.md`](../relay/README.md) and `glasscoded relay`. It
carries SSH only. The glasses app does not use it.

---

## Troubleshooting

- **The phone says the Mac cannot be reached.** Is the Mac awake, with Glasscode open, and
  Tailscale on, on both? Settings › Server › Glasses on the Mac says "Not reachable" when
  Tailscale is not running there.
- **"This phone is not paired with this Mac."** It was removed in Settings › Glasses, or the
  pairing was made on another Mac: pair again.
- **"This is not the Mac you paired."** Another machine answered on that address. Pair again
  on the right Mac.
- **The server cannot be reached.** Is it on the tailnet, and does `ssh` to it work from the Mac?
  Pairing again copies its current host keys.
