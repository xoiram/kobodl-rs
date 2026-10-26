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
| `KOBODL_OUTPUT_DIR` | not set | Folder on the server that books can be saved to. Adds a "Save to server" button |
| `RUST_LOG` | `info` | Log level |

### Docker

The image is published to `ghcr.io/xoiram/kobodl-rs`. `latest` follows the main branch, and `sha-<commit>` tags pin
a specific build. Releases are tagged `X.Y.Z` and `X.Y`; prefer one of those for a stable deployment.

```sh
docker compose up -d
```

The example [docker-compose.yml](docker-compose.yml) publishes the app on `127.0.0.1:8080` only and keeps its data in
the `kobodl-data` volume. While the repository is private, so is the image: log in first with
`docker login ghcr.io -u <github-user>` and a personal access token with the `read:packages` scope. The image runs as a non-root user (UID 65532); if you bind-mount a host directory at `/data`
instead of using a volume, make that directory writable for that user.

### Releases

Versions follow [semantic versioning](https://semver.org). Commits to `main` use
[conventional commits](https://www.conventionalcommits.org): `fix:` bumps the patch version, `feat:` the minor
version, and `feat!:` or a `BREAKING CHANGE:` footer the major version (minor while the version is 0.x). Every other
type (`chore:`, `docs:`, `ci:` and so on) also counts as a patch release. A commit with `[skip ci]` in its message
doesn't run the release workflow, so it never opens or updates the release PR by itself. A release PR
stays open on `main` with the next version and changelog. Merging it tags `vX.Y.Z`, creates the GitHub release and
publishes the image with that version. The version lives in `[workspace.package]` in `Cargo.toml`, and release-please
updates it, along with `Cargo.lock`.

### Saving to a folder on the server

With `KOBODL_OUTPUT_DIR` set, each book gets a "Save to server" button next to "Download EPUB". Point it at the ingest
folder of Calibre-Web Automated or a similar app, and saved books are imported from there without any integration
between the two.

Books only appear in the folder once they're complete: they're written to hidden `.kobodl-*.part` files first and
then renamed. Saved files are readable by other users (mode `0644`), and a book that is saved again replaces the
earlier file.

The app has to be able to write to the folder, and the other app has to be able to delete files from it. The
simplest way is to run both as the same user. For Calibre-Web Automated, that's its `PUID`/`PGID`, usually 1000.

### Kubernetes

Mount a PersistentVolumeClaim at `/data`. The container runs as UID 65532, so set `fsGroup: 65532` in the pod's
`securityContext` to make the volume writable. Run a single replica with the `Recreate` deployment strategy: a pending
login is kept in memory, and two pods must not share `state.json`.

To save into another app's ingest volume, the volume must be mountable by both pods. A ReadWriteOnce volume, like a
default Longhorn volume, can only be attached to one node, so either:

- run kobodl-rs as a second container in the other app's pod, which shares its volumes. With the bjw-s `app-template`
  chart, volumes with `globalMounts` are mounted into every container, so the ingest folder is there automatically;
- or make the ingest volume ReadWriteMany (Longhorn supports this through an NFS share).

Then set `KOBODL_OUTPUT_DIR` to the mount path, for example `/cwa-book-ingest`, and run the container as the same user
as the other app (`runAsUser`/`runAsGroup`, plus `fsGroup` so `/data` stays writable).

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

The DRM removal in [`crates/kobo/src/drm.rs`](crates/kobo/src/drm.rs) is a Rust port of `koboDrmRemover.py` from
kobodl, which derives from `obok.py`. Both kobo-book-downloader projects are under the Unlicense. `obok.py` has no
license file in its repository that I could find. It is credited here because the algorithm comes from it. If you
are an author of one of these projects and want the credit changed or the code removed, please open an issue.

## License

[MIT](LICENSE). Anyone who copies or redistributes the code has to keep the copyright notice.
