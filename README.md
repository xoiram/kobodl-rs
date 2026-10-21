# kobodl-rs

A simple self-hosted web app for downloading the books you bought on [Kobo](https://www.kobo.com/) as DRM-free EPUBs,
written in Rust.

Status: planning. See [PLAN.md](PLAN.md).

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
