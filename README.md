# kobodl-rs

A simple self-hosted web app for downloading the books you bought on [Kobo](https://www.kobo.com/) as DRM-free EPUBs,
written in Rust.

Status: v1 is done. See [PLAN.md](PLAN.md) for how it was built and what's left open.

## Running

```sh
cargo run --release -p kobodl-web
```

Then open <http://127.0.0.1:8080>, log in with the code the app shows you, and download books from the library page.
If you already use kobo-book-downloader or kobodl, you can import the login from its settings file instead.

Configuration is through environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `KOBODL_LISTEN` | `127.0.0.1:8080` | Address to listen on |
| `KOBODL_DATA_DIR` | `data` | Directory for `state.json`, which holds the Kobo login tokens |
| `RUST_LOG` | `info` | Log level |

### Docker

The image is published to `ghcr.io/xoiram/kobodl-rs`. `latest` follows the main branch, and `sha-<commit>` tags pin
a specific build.

```sh
docker compose up -d
```

The example [docker-compose.yml](docker-compose.yml) publishes the app on `127.0.0.1:8080` only and keeps its data in
the `kobodl-data` volume. While the repository is private, so is the image: log in first with
`docker login ghcr.io -u <github-user>` and a personal access token with the `read:packages` scope. The image runs as a non-root user (UID 65532); if you bind-mount a host directory at `/data`
instead of using a volume, make that directory writable for that user.

### Kubernetes

Mount a PersistentVolumeClaim at `/data`. The container runs as UID 65532, so set `fsGroup: 65532` in the pod's
`securityContext` to make the volume writable. Run a single replica with the `Recreate` deployment strategy: a pending
login is kept in memory, and two pods must not share `state.json`.

### Staying logged in

The login is stored in `state.json` in the data directory: the device registration Kobo knows this app by, and the
access and refresh tokens. It's read at startup and saved whenever Kobo issues new tokens, so restarts and new
containers stay logged in on the same device as long as the data directory is kept. Logging out keeps the device
registration too, so logging in again doesn't add another device to your Kobo account.

The file holds account tokens and is only readable by the user the app runs as. Back it up like a password.

### Security

The app has no login of its own. If you make it reachable from other machines, put it behind a reverse proxy that
handles authentication. See [docs/reverse-proxy.md](docs/reverse-proxy.md) for Caddy and nginx examples.

## Scope

- One Kobo account.
- Web UI only, no CLI.
- No built-in authentication. Run it behind a reverse proxy that handles auth.

## Prior art

This is a reimplementation of these projects. All of the Kobo API knowledge comes from them.

- [subdavis/kobo-book-downloader](https://github.com/subdavis/kobo-book-downloader) (`kobodl`): the fork with the
  web UI and multi-user support, which is the primary reference for this port.
- [TnS-hun/kobo-book-downloader](https://github.com/TnS-hun/kobo-book-downloader): the original CLI tool.
- [obok.py](https://github.com/apprenticeharper/DeDRM_tools/blob/master/Other_Tools/Kobo/obok.py) by Physisticated,
  from DeDRM_tools: the original Kobo DRM removal algorithm that both projects are based on.

## License

Not decided yet.
