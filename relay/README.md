# Glasscode Relay

A small, self-hosted WebSocket switchboard that joins your Glasscode devices — your
Mac, your phone, an optional build server — so they reach each other **from any
network**, without a VPN and without opening a port on your Mac.

It is **optional**. Most people should use [Tailscale](../docs/CONNECTIVITY.md)
instead: it needs no server you operate. Run the relay only when you can't use
Tailscale (a locked-down network, a device you don't control).

Nothing in Glasscode depends on any hosted service. This relay is yours to run, and
it is licensed Apache-2.0 like the rest of Glasscode.

---

## How it works

Every device dials **out** to the relay over one WebSocket, so a phone on cellular
reaches a Mac behind NAT with no inbound ports.

```
  phone  ──connect──▶  relay  ◀──listen──  Mac (voice server :4517)
                         │
               joins the two sockets and
             passes bytes through, blind to
                   what's inside them
```

- The **Mac** keeps a `listen` socket open and waits.
- The **phone** opens `connect?to=<mac>`; the relay tells the Mac, the Mac opens an
  `accept` socket, and the relay splices the two into one byte pipe.
- The relay sees *who* talks to *whom* and *how much*, never *what*.

Authorisation is a **single shared secret** you set. Every device that presents it
belongs to one group and can reach the others. There is no account server and no
per-device registry. **Rotate the secret to lock every device out at once.**

> Anyone who has the secret can reach every device on your relay. Use a long random
> secret, keep it private, and serve the relay over HTTPS/WSS (see below).

---

## Run it

### 1. Make a secret

```sh
openssl rand -base64 32
```

Keep this value; every device uses the same one.

### 2. Start the relay

**With Docker** (from the repo root):

```sh
docker build -t glasscode-relay ./relay
docker run -d --name glasscode-relay -p 8080:8080 \
  -e RELAY_SECRET='<the secret from step 1>' \
  glasscode-relay
```

**From source:**

```sh
cd relay
RELAY_SECRET='<the secret>' cargo run --release
```

Configuration (environment):

| Variable        | Default        | Meaning                                         |
| --------------- | -------------- | ----------------------------------------------- |
| `RELAY_SECRET`  | *(required)*   | The shared secret; at least 16 characters.      |
| `LISTEN_ADDR`   | `0.0.0.0:8080` | Address and port to listen on.                  |
| `RUST_LOG`      | `glasscode_relay=info` | Log level.                              |

Check it's up: `curl http://<host>:8080/healthz` → `ok`.

### 3. Put it behind HTTPS

The relay speaks plain HTTP so it can sit behind a TLS terminator. Point a reverse
proxy (Caddy, nginx, a cloud load balancer) at it and give it a domain, so devices
connect over `wss://`. With [Caddy](https://caddyserver.com):

```
relay.example.com {
    reverse_proxy localhost:8080
}
```

Devices then use `https://relay.example.com` (the client upgrades it to `wss://`).
WebSockets stay open for a long time — if your proxy has an idle timeout, raise it
past a minute (the relay pings every 20 s to help).

### 4. Point your devices at it

- **The Mac** (`glasscoded`): point the daemon at the relay —

  ```sh
  glasscoded relay wss://relay.example.com '<the secret>'
  ```

  It prints the Mac's **device id** and keeps a `listen` socket open.
  `glasscoded relay status` reports the connection; `glasscoded relay off` stops.

- **The glasses app**: in `glasses/GlasscodeGlasses/Sources/GlasscodeConfig.swift`,
  set `useRelay = true`, `relayURL`, `relaySecret`, and `macDeviceID` (the id the
  command above printed), then build. It reaches the Mac through the relay instead
  of a LAN/Tailscale address.

Each device picks its own **device id** (any unique label); the Mac's is set for you
by `glasscoded relay …`, and the phone's is `phoneDeviceID` in `GlasscodeConfig.swift`.

---

## Protocol

All endpoints require two things: `Authorization: Bearer <RELAY_SECRET>` and an
`x-device-id` header (plus optional `x-device-kind` and `x-device-name`).

| Endpoint                         | Who    | Purpose                                                        |
| -------------------------------- | ------ | ------------------------------------------------------------- |
| `GET /v1/listen` (WS)            | host   | Keep open; receive `{"type":"incoming",conn,from,channel}` and presence. |
| `GET /v1/connect?to=&channel=` (WS) | client | Reach a host; the relay notifies it, then splices the sockets. |
| `GET /v1/accept?conn=` (WS)      | host   | Pick up an incoming connection by its `conn` id.              |
| `GET /v1/online`                 | any    | `{"online":[{device,kind,name}]}` — the hosts listening now.  |
| `GET /healthz`                   | —      | `ok` (no auth).                                                |

The host chooses what each `channel` maps to locally — Glasscode uses `control` for
the voice server (`:4517`) and `ssh` for a build server (`:22`). Frames are binary
and passed through unchanged (up to 1 MiB each).

Limits: up to 256 open sockets per relay; connections that no host answers within
15 s are dropped.

---

## Security notes

- The secret is the only credential. Treat it like a password: long, random, private.
- Serve over TLS (step 3) so the secret and traffic aren't exposed on the wire.
- To revoke a lost device, **rotate the secret** on the relay and your other devices.
- The relay never stores traffic and never reads inside the byte pipe.
