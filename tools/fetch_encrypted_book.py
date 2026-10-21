"""Downloads one Kobo book without removing the DRM, for testing the Rust DRM code against a real book.

This uses TnS-hun/kobo-book-downloader and its existing login. Run it with the Python environment that tool uses:

    python tools/fetch_encrypted_book.py <path to kobo-book-downloader/kobo-book-downloader> <book-id> <output-dir>

Get the book ID from that tool's `list` command. The script writes two files to the output directory:

- `encrypted.epub`: the book as Kobo serves it, still encrypted.
- `keys.json`: the device ID, user ID and content keys needed to decrypt it.

keys.json contains account identifiers, so keep both files out of the repository.
"""

import json
import os
import sys


def main() -> None:
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    tool_dir, book_id, output_dir = sys.argv[1:]
    sys.path.insert(0, os.path.abspath(tool_dir))

    from Globals import Globals
    from Kobo import Kobo
    from Settings import Settings

    import logging

    Globals.Logger = logging.getLogger()
    Globals.Kobo = Kobo()
    Globals.Settings = Settings()
    if not Globals.Settings.IsLoggedIn():
        sys.exit("Not logged in. Log in with kobo-book-downloader first.")

    kobo = Globals.Kobo
    kobo.LoadInitializationSettings()

    # These are private methods, so use their mangled names.
    contentAccessBook = kobo._Kobo__GetContentAccessBook(book_id, Kobo.DisplayProfile)
    contentKeys = Kobo._Kobo__GetContentKeys(contentAccessBook)
    downloadUrl, hasDrm = Kobo._Kobo__GetDownloadInfo(book_id, contentAccessBook)
    if not hasDrm:
        sys.exit("This book has no Kobo DRM, pick another one.")

    os.makedirs(output_dir, exist_ok=True)
    kobo._Kobo__DownloadToFile(downloadUrl, os.path.join(output_dir, "encrypted.epub"))

    keysPath = os.path.join(output_dir, "keys.json")
    with open(os.open(keysPath, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as f:
        json.dump(
            {
                "device_id": Globals.Settings.DeviceId,
                "user_id": Globals.Settings.UserId,
                "content_keys": contentKeys,
            },
            f,
            indent=4,
        )

    print(f"Wrote encrypted.epub and keys.json to {output_dir}")


if __name__ == "__main__":
    main()
