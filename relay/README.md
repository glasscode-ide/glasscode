# Glasscode Relay

A small, self-hosted WebSocket switchboard that joins your Mac and your Glasscode server so they
reach each other over SSH **from any network**, without a VPN and without opening a port. The
glasses app does not use it: it reaches the Mac's device door and the server over Tailscale
([`docs/CONNECTIVITY.md`](../docs/CONNECTIVITY.md)).

It is **optional**. Most people should use [Tailscale](../docs/CONNECTIVITY.md)
instead: it needs no server you operate. Run the relay only when you can't use
Tailscale (a locked-down network, a device you don't control).

Nothing in Glasscode depends on any hosted service. This relay is yours to run, and
it is licensed Apache-2.0 like the rest of Glasscode.

---

## How it works

Every device dials **out** to the relay over one WebSocket, so the Mac reaches a server behind
NAT with no inbound ports.

```
  Mac  ──connect──▶  relay  ◀──listen──  server (sshd :22)
                         │
               joins the two sockets and
             passes bytes through, blind to
                   what's inside them
```

- The **server** keeps a `listen` socket open and waits.
- The **Mac** opens `connect?to=<server>&channel=ssh`; the relay tells the server, the server opens
  an `accept` socket, and the relay splices the two into one byte pipe.
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

- **The server** (`glasscoded`): point the daemon at the relay —

  ```sh
  glasscoded relay wss://relay.example.com '<the secret>'
  ```

  It prints the server's **device id** and keeps a `listen` socket open. The Mac reaches it
  through the relay from Glasscode's Server dialog.
  `glasscoded relay status` reports the connection; `glasscoded relay off` stops.

Each device picks its own **device id** (any unique label); the server's is set for you by
`glasscoded relay …`.

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

The host chooses what each `channel` maps to locally. Glasscode uses `ssh` (`:22`) only: the
`control` channel to the old voice server (`:4517`) was closed (the glasses plan's G02), because a
relayed connection reached it from loopback, where it asked for no token. Frames are binary
and passed through unchanged (up to 1 MiB each).

Limits: up to 256 open sockets per relay; connections that no host answers within
15 s are dropped.

---

## Security notes

- The secret is the only credential. Treat it like a password: long, random, private.
- Serve over TLS (step 3) so the secret and traffic aren't exposed on the wire.
- To revoke a lost device, **rotate the secret** on the relay and your other devices.
- The relay never stores traffic and never reads inside the byte pipe.
