# Running behind a reverse proxy

kobodl-rs has no login of its own. Anyone who can reach it can download your books and log your Kobo account out, so
if it's reachable from anywhere but your own machine, put a reverse proxy with authentication in front of it.

Things the proxy setup needs:

- **Authentication.** Basic auth is enough for one user. Forward auth with Authelia, Authentik or similar works too.
- **Pass the `Host` header through.** The app rejects form posts from other sites. Browsers send `Sec-Fetch-Site`,
  which is checked first, but for older browsers the app compares the `Origin` header with `X-Forwarded-Host` or
  `Host`. Caddy does this by default; nginx needs `proxy_set_header Host $host;`.
- **Don't buffer downloads for long.** A book download takes a few seconds while the app fetches and decrypts it. The
  default timeouts of Caddy and nginx are fine.
- **HTTPS**, since basic auth sends the password with every request.

Keep the app itself unreachable except through the proxy: bind it to `127.0.0.1` (the default outside Docker, and
what the example `docker-compose.yml` does), or put both containers on a private Docker network.

`/healthz` returns `ok` and can be used for health checks. It reveals nothing, but it sits behind the same
authentication as everything else unless you exclude it.

## Caddy

Create a password hash with `caddy hash-password`, then:

```caddyfile
books.example.com {
	basic_auth {
		reader $2a$14$replace.with.the.hash.from.caddy.hash-password
	}
	reverse_proxy 127.0.0.1:8080
}
```

## nginx

Create the password file with `htpasswd -c /etc/nginx/kobodl.htpasswd reader` (from `apache2-utils`), then:

```nginx
server {
    listen 443 ssl;
    server_name books.example.com;
    # ssl_certificate and ssl_certificate_key as for your other sites

    auth_basic "Kobo books";
    auth_basic_user_file /etc/nginx/kobodl.htpasswd;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

## Docker Compose with Caddy

Instead of publishing the app's port, put Caddy on the same network and proxy to the service name:

```yaml
services:
  kobodl:
    build: .
    restart: unless-stopped
    volumes:
      - kobodl-data:/data

  caddy:
    image: caddy:2
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy-data:/data

volumes:
  kobodl-data:
  caddy-data:
```

with `reverse_proxy kobodl:8080` in the Caddyfile.
