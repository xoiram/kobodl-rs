# Implementation plan

The port is based on `subdavis/kobo-book-downloader` at commit `d096092` (2026-09-19). File references below
(`kobo.py`, `actions.py`, `app.py`) point into that repo's `kobodl/` directory.

## Goals for v1

- A simple web UI to log in to Kobo, browse the library, and download a book as a DRM-free EPUB to the browser.
- One Kobo account. One static binary and a small Docker image.
- No built-in authentication. The reverse proxy in front of it handles that.

## Non-goals for v1

- CLI, multiple accounts, wishlist, user-configurable filename templates.
- Saving books to a server-side folder, bulk downloads, download history, or integration with other tools.
- Audiobooks. `kobo.py` supports them, so they can come later.
- Adobe DRM (`AdobeDrm`). Like upstream, report these books as unsupported instead of downloading them.

## Architecture

A Cargo workspace with two crates:

- `kobo`: a library crate with the Kobo API client, the data model, and DRM removal. It has no web dependencies, so
  it can be tested on its own.
- `kobodl-web`: a binary crate with the `axum` server, HTML templates, and state persistence.

Main dependencies:

| Purpose | Crate |
|---|---|
| Async runtime | `tokio` |
| HTTP client | `reqwest` with `rustls`, `json` and `stream` features; no OpenSSL, so musl static builds stay simple |
| Web server | `axum`, `tower-http` |
| Templates | `askama`, with `htmx` served from a vendored static file |
| JSON | `serde`, `serde_json` |
| Crypto | `aes`, `sha2`, `base64`, `hex`; PKCS7 unpadding by hand or with `block-padding` |
| EPUB rewrite | `zip` |
| Errors and logging | `thiserror` in `kobo`, `anyhow` in the binary, `tracing` |
| Tests | `wiremock` for HTTP fakes |

### Configuration

Environment variables, so it works the same in Docker and systemd:

- `KOBODL_LISTEN`: bind address. Default `127.0.0.1:8080`, so it's never exposed without a proxy by accident.
- `KOBODL_DATA_DIR`: holds `state.json`.

### State

`state.json` replaces upstream's `kobodl.json`, with one account instead of a list: `device_id`, `serial_number`,
`access_token`, `refresh_token`, `user_id`, `user_key`, `email`.

Write the file atomically (temp file plus rename) with mode `0600`, because it holds account tokens.

## Kobo client (`kobo` crate)

A port of `kobo.py`. Details that have to match exactly:

- Constants: `Affiliate`, `ApplicationVersion`, `DefaultPlatformId`, `DisplayProfile`, the device fields, and the
  Kobo e-reader `User-Agent` on every request.
- `ClientKey` is the base64 of `DefaultPlatformId`.
- Device and serial IDs are random lowercase hex strings of length 64 and 32.

Methods to port:

1. `authenticate_device(user_key: Option<&str>)`: `POST https://storeapi.kobo.com/v1/auth/device`. Check that
   `TokenType == "Bearer"`.
2. `refresh_authentication()`: `POST https://storeapi.kobo.com/v1/auth/refresh`.
3. Retry on 401: replaces upstream's `requests` response hook. Wrap authenticated requests in a helper that refreshes
   once and retries once. Put the refresh behind a `tokio::sync::Mutex`, so concurrent requests that all get a 401
   only trigger one refresh. Persist new tokens through a callback or a channel, so the library crate doesn't need to
   know about the state file.
4. `load_initialization_settings()`: `GET https://storeapi.kobo.com/v1/initialization`. It returns the `Resources`
   map of endpoint URLs (`library_sync`, `content_access_book`, and so on). Cache it in memory.
5. `activate_on_web()`: `GET https://auth.kobobooks.com/ActivateOnWeb` with the `pws*` query parameters. Scrape the
   poll endpoint (`data-poll-endpoint="..."`, then HTML-unescape it) and the activation code (from the
   `qrcodegenerator` URL) from the HTML response. This is the most fragile part, so keep the regexes in one place and
   return a specific error when they don't match.
6. `check_activation(poll_url)`: `POST` to the poll URL. When `Status == "Complete"`, parse `userId`, `userKey` and
   `email` from the query string of `RedirectUrl`.
7. `get_library()`: paginate `library_sync` with the `x-kobo-synctoken` request header. Continue while the response
   header `x-kobo-sync` is `continue`.
8. `get_content_access_book(product_id)`: `content_access_book` with `{ProductId}` substituted and
   `DisplayProfile=Android`.
9. Download URL selection, ported from `__GetDownloadInfo`:
   - Accept both `ContentUrls` and `DownloadUrls`, both `DrmType` and `DRMType`, and both `DownloadUrl` and `Url`.
   - Strip the `b` query parameter unless the host is `amazonaws.com`.
   - An empty URL list means the book is archived. Return an error that says so.
10. File download: always send `x-amz-request-payer: requester` so it survives the redirect to S3. Send the bearer
    token only to `kobo.com` hosts.

### Library model

Port the filtering in `actions.py` (`__GetBookList`, `__GetBookMetadata`) into typed structs:

- Skip entries without `NewEntitlement`, previews (`Accessibility == "Preview"`), and refunded books (`IsLocked`).
- Classify entries as ebook (`BookMetadata`), audiobook (`AudiobookMetadata`) or subscription
  (`BookSubscriptionEntitlement`).
- Keep `archived` (`IsRemoved`) and `read` (`ReadingState.StatusInfo.Status == "Finished"`) as fields, and let the UI
  filter on them.
- Use the author logic from `__GetBookAuthor`: authors with `Role == "Author"`, falling back to the first contributor.

Keep the raw JSON for each book next to the typed fields (`serde_json::Value`), because Kobo adds fields over time.

### DRM removal

A port of `koboDrmRemover.py`:

1. User key = the last 16 bytes of `sha256(device_id + user_id)`. In the Python code this is the second half of the
   hex digest.
2. For each zip entry that has a content key: base64-decode the key, AES-128-ECB decrypt it with the user key, then
   AES-128-ECB decrypt the entry with the result and strip the PKCS7 padding.
3. Write a new zip with `mimetype` as the first entry, stored without compression. Compress everything else with
   deflate.

The `aes` crate has no ECB mode type, so loop over 16-byte blocks with `decrypt_block`. Return a proper error on bad
padding instead of panicking, since that is what a wrong key looks like.

## Web app (`kobodl-web` crate)

Server-rendered HTML with `askama` and a little `htmx` for the login polling. No SPA and no JS build step.

| Route | Purpose |
|---|---|
| `GET /` | Redirects to `/login` if not activated, otherwise to `/library` |
| `GET /login`, `POST /login/start` | Starts activation. Shows the code and a link to `https://www.kobo.com/activate` |
| `GET /login/poll` | htmx polling every 5 s. Calls `check_activation`, then `authenticate_device(user_key)`, then saves state |
| `POST /logout` | Clears the credentials |
| `GET /library` | Book table with filters for unread/all and hide/show archived. Marks Adobe DRM books and audiobooks as unsupported |
| `POST /library/refresh` | Re-syncs the library cache |
| `GET /books/{id}/epub` | Downloads and decrypts the book, then sends it to the browser as an attachment |
| `GET /healthz` | For Docker and the proxy |

### Book downloads

- Download to a temp file (`tempfile` crate), decrypt to a second temp file, stream that to the browser, and let both
  get deleted afterwards. Nothing is kept on the server.
- A book takes a few seconds, so a plain request is fine. Show a "preparing download" state on the button and an
  error message if it fails. A background job queue isn't needed for v1.
- Filename: upstream's default `{Author} - {Title} {ShortRevisionId}.epub`, using its character whitelist for
  sanitizing. Set it with `Content-Disposition`, including the `filename*` form for non-ASCII titles.

### Security, given there's no built-in auth

- Bind to localhost by default, and document a reverse proxy setup with auth (Caddy or nginx examples).
- State-changing routes are `POST`. Reject requests whose `Origin` or `Sec-Fetch-Site` header shows they came from
  another site. Proxy basic auth doesn't protect against CSRF, because browsers attach the credentials anyway.
- Never log tokens. Add a `Debug` impl for the credentials struct that redacts them.

## Testing

- DRM: build a fixture EPUB in the test, encrypted with known keys, and check that it decrypts byte for byte and that
  `mimetype` comes out first and stored. Known-answer tests compare against values computed with the Python
  implementation. For a real book, `tools/fetch_encrypted_book.py` saves an encrypted book and its keys using the
  Python tool's login, and the `decrypt_book` example decrypts it.
- Library parsing: JSON fixtures for each entitlement shape (ebook, audiobook, subscription, preview, refunded,
  archived, read). Scrub personal data from any fixture taken from a real account before committing it.
- Client: `wiremock` tests for the 401-refresh-retry, sync-token pagination, the `ActivateOnWeb` scraping (with a
  saved HTML fixture), and the download URL selection rules.
- Web: `axum` handler tests with `tower::ServiceExt::oneshot` against a fake client, for the login state machine and
  the CSRF checks.

## Phases

Each phase ends with something that works.

1. **Library crate, DRM first.** Workspace setup, DRM removal, and its fixture test. Done when a real encrypted EPUB
   (downloaded with the Python tool's settings file) decrypts and opens in an e-reader app.
2. **Kobo client.** Auth, refresh, initialization, library sync, and download, plus a `kobo_cli` example that uses
   the Python tool's settings JSON to list the library and download one book. Done when it downloads and decrypts a
   book without the Python tool.
3. **Web app.** Login flow, library page, download to the browser, persistent state. Done when the whole flow works
   from a fresh `state.json`.
4. **Packaging.** Multi-stage Dockerfile (musl static binary on `scratch` or distroless), example docker-compose,
   reverse proxy docs, GitHub Actions CI (fmt, clippy, test).

## Risks

- **Kobo API changes.** The activation scraping and the download URL format have changed before (see the S3
  requester-pays handling upstream). Keep an eye on the upstream repos and port their fixes.
- **Kobo account devices.** The tool registers as a fake Kobo device. Each new device ID shows up as a new device on
  the account, so reuse the stored device ID when logging in again instead of generating a new one.
- **Testing against the real API.** It needs a real account and real purchases, so CI can only run the fakes.

## Open decisions

- License. Both upstream projects are released under the Unlicense, so any license is compatible with them.
- Whether audiobooks are worth supporting after v1.
