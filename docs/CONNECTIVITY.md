# Connecting Glasscode

Glasscode has three pieces that talk to each other:

- **The IDE** (your Mac) — runs your coding sessions and the voice server on port **4517**.
- **The glasses app** (iPhone + Ray-Ban Display) — mirrors a session and lets you drive it by voice and a glance.
- *(Optional)* **A remote build server** — runs sessions over SSH.

The phone reaches the Mac's voice server at `http://<mac-address>:4517`. The only
question is what `<mac-address>` is on your network. There are three ways to set it,
from simplest to most involved.

---

## 1. Same Wi-Fi (LAN) — zero setup

If the Mac and the phone are on the same Wi-Fi, use the Mac's local IP.

1. On the Mac: **System Settings → Network** and note the IP (e.g. `192.168.1.20`),
   or run `ipconfig getifaddr en0`.
2. In `glasses/GlasscodeGlasses/Sources/GlasscodeConfig.swift`, set:
   ```swift
   static let controlBaseURL = "http://192.168.1.20:4517"
   ```
3. Build and run the glasses app.

This stops working the moment the phone leaves that Wi-Fi (cellular, a different
network). For "works anywhere," use Tailscale.

---

## 2. Tailscale — works on any network (recommended)

[Tailscale](https://tailscale.com) puts your Mac and phone on one private network
(a "tailnet") with stable addresses, over any internet connection, with no relay to
run and no ports to open. This is the recommended Glasscode setup.

1. Install Tailscale and sign in on **both** the Mac and the iPhone (and the remote
   build server, if you use one). A free personal account covers all your own devices.
2. On the Mac, find its tailnet address:
   ```sh
   tailscale ip -4
   ```
   It looks like `100.x.y.z` and does not change.
3. In `GlasscodeConfig.swift`, point Glasscode at it:
   ```swift
   static let controlBaseURL = "http://100.101.102.103:4517"
   ```
4. Build and run. The glasses now reach the Mac from anywhere — home Wi-Fi, office,
   or cellular — as long as both devices have Tailscale on.

For a remote build server, add it to the same tailnet; the IDE reaches it over SSH
on its tailnet address with no extra configuration.

**Why this is the default:** Tailscale is end-to-end encrypted, needs no server you
operate, and replaces what a relay would do for nearly everyone.

---

## 3. Self-hosted relay — optional, for when you can't use Tailscale

If you cannot run Tailscale (locked-down network, a device you don't control), you
can deploy the optional Glasscode relay: a small open-source WebSocket switchboard
you host, which forwards control traffic between the phone and the Mac using a
shared secret you set. It is **off by default** and not required for the LAN or
Tailscale paths above.

1. **Deploy the relay** and pick a shared secret — see [`relay/README.md`](../relay/README.md).
2. **On the Mac**, point the daemon at it:
   ```sh
   glasscoded relay wss://relay.example.com <the shared secret>
   ```
   It prints this Mac's **device id** (used in the next step) and starts keeping a
   `listen` socket open. `glasscoded relay status` shows whether it's connected, and
   `glasscoded relay off` stops using it.
3. **In the glasses app**, set the matching values in `GlasscodeConfig.swift` and
   flip the switch:
   ```swift
   static let useRelay = true
   static let relayURL = "wss://relay.example.com"
   static let relaySecret = "<the same shared secret>"
   static let macDeviceID = "<the device id glasscoded printed>"
   ```
   Build and run. The glasses now reach the Mac through the relay; `controlBaseURL`
   is ignored while `useRelay` is on.

Nothing in Glasscode depends on any hosted service; the relay is yours to run.

---

## Troubleshooting

- **Glasses show "Can't reach Glasscode."** Confirm the IDE is running and the Mac
  is awake and unlocked. Check the address in `GlasscodeConfig.swift` matches the Mac's
  current LAN or Tailscale IP. From the phone's browser, `http://<mac-address>:4517`
  should respond.
- **Works on Wi-Fi, not on cellular.** That's the LAN path — move to Tailscale.
- **Tailscale address changed.** It shouldn't, but if you reset Tailscale, re-run
  `tailscale ip -4` and update `GlasscodeConfig.swift`.
