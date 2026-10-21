# kobodl-rs

A simple self-hosted web app for downloading the books you bought on [Kobo](https://www.kobo.com/) as DRM-free EPUBs,
written in Rust.

Status: the web app works; packaging (Docker, CI) is next. See [PLAN.md](PLAN.md).

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

The app has no login of its own. If you make it reachable from other machines, put it behind a reverse proxy that
handles authentication.

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
