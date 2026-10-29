# bitwarden-proxy

A small, streaming reverse proxy for Bitwarden. Point a Bitwarden client at this
proxy instead of `bitwarden.com` / `bitwarden.eu`, and it will believe the
account has a premium subscription — which unlocks premium client features such
as the built-in TOTP generator.

The proxy behaves like a self-hosted Bitwarden server: every request is forwarded
to the real Bitwarden servers, and only a handful of JSON fields are rewritten
on the fly.

```
Bitwarden client ──HTTPS──> nginx/traefik ──HTTP──> bitwarden-proxy ──HTTPS──> Bitwarden
                                                        │
                                                        └─ rewrites premium flags
                                                           and /api/config URLs
```

## What it does

### Path routing

The proxy mimics the layout of a self-hosted Bitwarden installation:

| Public path              | Official upstream (US)                     |
| ------------------------ | ------------------------------------------ |
| `/api/...`               | `https://api.bitwarden.com/...`            |
| `/identity/...`          | `https://identity.bitwarden.com/...`       |
| `/notifications/...`     | `https://notifications.bitwarden.com/...`  |
| `/events/...`            | `https://events.bitwarden.com/...`         |
| `/sso/...`               | `https://sso.bitwarden.com/...`            |
| `/fillassist/...`        | `https://fillassist.bitwarden.com/...`     |
| `/icons/...`             | `https://icons.bitwarden.net/...`          |
| anything else            | `https://bitwarden.com/...`                |

`bitwarden.eu` is handled the same way with the `.eu` domains. For any other
server (`BITWARDEN_SERVER=https://bw.example.com`) paths are forwarded
unchanged, so Vaultwarden and self-hosted Bitwarden work too.

### JSON rewriting

Only these responses are touched (and only when the upstream returns `2xx` with
a JSON content type):

| Path                     | Fields set to `true`                                                                                  |
| ------------------------ | ----------------------------------------------------------------------------------------------------- |
| `/api/accounts/profile`  | `premium`, `premiumFromOrganization`, `organizations[*].usersGetPremium`, `organizationsNew[*].usersGetPremium` |
| `/api/sync`              | `profile.premium`, `profile.premiumFromOrganization`                                                   |

`/api/config` is rewritten so the client keeps talking to the proxy:

```jsonc
{
  "environment": {
    "vault":         "https://bitwarden-proxy.example.com",
    "api":           "https://bitwarden-proxy.example.com/api",
    "identity":      "https://bitwarden-proxy.example.com/identity",
    "notifications": "https://bitwarden-proxy.example.com/notifications",
    "sso":           "https://bitwarden-proxy.example.com/sso",
    "fillAssistRules": "https://bitwarden-proxy.example.com/fillassist"
  }
}
```

Every other byte — including all other fields and the original formatting — is
copied through untouched.

The rewriting is done by an incremental state machine while the response body
flows through: a multi-megabyte `/api/sync` response is never buffered in full.
WebSocket upgrades (used by `/notifications/hub` for live sync) are tunnelled
transparently.

## Configuration

Every setting can be given as a command-line flag or through an environment
variable. Flags take precedence over environment variables. Run
`bitwarden-proxy --help` to list everything as well.

| Flag             | Environment variable   | Required | Description                                                                                        |
| ---------------- | ---------------------- | -------- | -------------------------------------------------------------------------------------------------- |
| `-s, --server`   | `BITWARDEN_SERVER`     | yes      | Upstream server: `bitwarden.com`, `bitwarden.eu`, `https://bw.example.com`, `http://host:port`, ... |
| `-p, --proxy`    | `BITWARDEN_PROXY`      | yes      | Public origin of this proxy, used in `/api/config`: `https://bitwarden-proxy.example.com`           |
| `-b, --bind`     | `BITWARDEN_PROXY_BIND` | no       | Listen address, default `0.0.0.0:8080`                                                             |
|                  | `RUST_LOG`             | no       | Log filter, e.g. `info`, `debug`, `bitwarden_proxy=debug`                                          |

`--proxy` must be an origin only: no path, query, fragment or credentials. It
is the URL users enter in their Bitwarden client, so it must match the public
URL terminated by your TLS reverse proxy.

## Running

### Command line flags

```sh
cargo run --release -- \
  --server bitwarden.com \
  --proxy https://bitwarden-proxy.example.com
```

```sh
./bitwarden-proxy -s bitwarden.eu -p https://bitwarden-proxy.example.com -b 0.0.0.0:8080
```

### Environment variables

```sh
BITWARDEN_SERVER=bitwarden.com \
BITWARDEN_PROXY=https://bitwarden-proxy.example.com \
cargo run --release
```

### Docker

```sh
docker build -t bitwarden-proxy .

# With environment variables
docker run -d --name bitwarden-proxy --restart unless-stopped \
  -p 127.0.0.1:8080:8080 \
  -e BITWARDEN_SERVER=bitwarden.com \
  -e BITWARDEN_PROXY=https://bitwarden-proxy.example.com \
  bitwarden-proxy

# Or with command-line flags
docker run -d --name bitwarden-proxy --restart unless-stopped \
  -p 127.0.0.1:8080:8080 \
  bitwarden-proxy --server bitwarden.com --proxy https://bitwarden-proxy.example.com
```

### Behind nginx

The proxy speaks plain HTTP and expects the public TLS to be terminated in front
of it. WebSocket upgrades must be allowed for `/notifications/`.

```nginx
server {
    listen 443 ssl http2;
    server_name bitwarden-proxy.example.com;

    ssl_certificate     /etc/letsencrypt/live/bitwarden-proxy.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/bitwarden-proxy.example.com/privkey.pem;

    client_max_body_size 100m;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket (notifications)
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_read_timeout 1h;

        # Keep bodies streaming end-to-end instead of buffering them on disk.
        proxy_buffering off;
    }
}

map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}
```

### Behind Traefik

```yaml
http:
  routers:
    bitwarden-proxy:
      rule: Host(`bitwarden-proxy.example.com`)
      entryPoints: [websecure]
      tls:
        certResolver: letsencrypt
      service: bitwarden-proxy
  services:
    bitwarden-proxy:
      loadBalancer:
        servers:
          - url: http://bitwarden-proxy:8080
```

Traefik forwards WebSocket upgrades automatically.

### Client setup

1. Open the Bitwarden client and log out (or use a fresh profile).
2. On the login screen, open the region/server selector and choose
   **Self-hosted**.
3. Enter `https://bitwarden-proxy.example.com` as the server URL.
4. Log in. The account profile will now report premium, and premium client
   features (TOTP, attachments, ...) are unlocked in the client.

## Limitations

- **HTTP only.** TLS is expected to be terminated by nginx, Traefik, Caddy, ...
- **No request/response caching.** Responses are streamed; `Content-Length` is
  dropped and bodies are re-chunked.
- **Compressed responses are not rewritten.** On rewritten paths the proxy asks
  the upstream for an identity-encoded body. If an upstream still compresses the
  response, it is passed through unmodified rather than corrupted.
- **Cookies are forwarded verbatim.** A `Set-Cookie` with `Domain=.bitwarden.com`
  will not be stored by a browser on your domain. This does not affect the
  desktop/mobile/browser-extension clients, which use bearer tokens.
- **No access control or rate limiting.** Anyone who can reach the proxy can use
  it. Put it behind authentication if it is publicly reachable.
- **Only client-side gating is bypassed.** TOTP generation and similar features
  that the client decides locally work. Features the server enforces (for
  example creating attachments or using premium server APIs) are still rejected
  by Bitwarden itself.
- Responses with non-2xx status, non-JSON content types or websocket traffic are
  passed through unchanged.

## How it works

- `src/routes.rs` resolves the public path to an upstream URL (official
  per-service subdomains or a custom server).
- `src/proxy.rs` forwards requests with `reqwest`, streams responses and wraps
  the selected JSON responses in the rewriter.
- `src/rewrite/stream.rs` is a byte-level incremental JSON state machine. It
  tracks the current JSON path, copies all bytes verbatim and swaps only the
  matched values. It is safe across arbitrary chunk boundaries (tested by
  re-running every test with every possible chunk size).
- `src/ws.rs` tunnels WebSocket connections to the upstream notifications
  service.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests include unit tests for the rewriter and routing, plus end-to-end tests
with a fake Bitwarden upstream (including chunk-boundary splitting and a
WebSocket echo).

## Disclaimer

This project exists for personal experimentation with your own account. It
circumvents Bitwarden's premium licensing; use it at your own risk and consider
supporting Bitwarden by purchasing a subscription. The authors are not
affiliated with Bitwarden and provide no warranty.
