# musort

Organizes a flat music library (`Artist/Song.mp3` + `.lrc`) so it looks right in Jellyfin and on a portable player (tested with a FiiO JM21), and keeps the player's card in sync with the library.

It's a single binary. Nothing is written without `--apply`, and whatever is removed or changed can be reverted with `musort undo`.

## Build

You need Rust (stable):

```sh
brew install rustup && rustup-init -y   # if you don't have it
cargo build --release
```

The binary ends up in `target/release/musort`.

## Install on your PATH

```sh
mkdir -p ~/.local/bin
install -m 755 target/release/musort ~/.local/bin/musort
```

If `~/.local/bin` isn't on your PATH, add it to your shell config (`~/.zshrc` or `~/.bashrc`) and reload:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

To update, rebuild and run the `install` again.

## Usage

```sh
musort doctor /path/to/Music            # problem report (read-only)
musort clean  /path/to/Music            # empty folders, macOS/Android junk, orphan lyrics
musort fix    /path/to/Music            # tags and file/folder names
musort review /path/to/Music            # screen for what fix can't decide on its own
musort sync   /path/to/Music /Volumes/CARD
musort undo   /path/to/Music            # reverts the last operation
```

`clean`, `fix` and `sync` show what they would do; add `--apply` to do it. `doctor`, `fix` and `sync` accept `--all` to list everything instead of the first few items.

### doctor

Lists what's wrong: songs without `album_artist`, several artists packed into one value (`"A, B"`), folders that don't match the tag's artist, possible aliases of the same artist, duplicates, URLs in tags, songs missing genre, year, artwork or lyrics, and system junk.

### clean

Moves to the trash: folders with no music, folders and files created by macOS and Android (`.DS_Store`, `._*`, `.Spotlight-V100`, `Android/`…) and `.lrc` lyrics whose song no longer exists. A folder holding files that aren't music is left alone.

### fix

- Splits collaborations into separate artists (ID3v2.4, one value per artist) and fills `album_artist` with the main artist. Jellyfin then links the song to every artist, and the FiiO groups it under the main one without creating fake artists like `"A, B"`.
- Unifies how the artist is spelled within each folder.
- Strips ads from tags (`- www.site.com`, `Title | Band 2016`, repeated `(feat. X)`).
- Renames files after the title and folders after the artist, using names valid on FAT/exFAT.
- Songs whose tag names a different artist than their folder are left untouched; resolve them with `review`.
- `--unicode` also converts names stored as NFD to NFC. It's cosmetic, and Jellyfin will see those paths as new songs (losing play history and favorites), so it's off by default.

### review

A terminal screen with two tabs (Tab to switch):

- **Wrong artist**: `c` uses the folder's artist, `t` uses the tag's artist and moves the song to that folder, `e` to type the name.
- **Possible aliases**: `1` merges everything into the left folder, `2` into the right one, `e` to type the final name, `d` if they're different artists (remembered in `.musort.json` and not asked again).

`x` clears a decision, `a` twice applies, `q` quits. Afterwards run `musort fix` to clean up titles.

### sync

Copies only new or changed music and lyrics to the card, and deletes from the card whatever is no longer in the library. Keeps an index, `.musort-sync.json`, on the card so it doesn't have to compare the whole card again.

- Only deletes music, lyrics and hidden files; anything else is kept and reported. Never touches `Android/`, `DCIM/`, `Music/` or the hidden folders at the card's root.
- If the card has lyrics the library lacks (the player can download them), they're copied into the library instead of deleted.
- Removes the `._*` files macOS creates when writing to exFAT.
- Use a card reader. Mounting the player over MTP (MacDroid and similar) is slow and fails when reading many folders.

### undo

Every `clean`, `fix --apply` or change made from `review` keeps a log in `.musort-trash/<timestamp>/` inside the library: whatever was removed is moved there instead of deleted, and renames and tag changes are recorded. `undo` reverts the most recent operation; run it again to go further back. Once you're sure, delete `.musort-trash` by hand. Jellyfin ignores folders that start with a dot.

## Notes

- The first scan of a network library reads every file's tags and can take a couple of minutes; after that an index in `~/Library/Caches/musort/` (or `~/.cache/musort/`) is used and only changed files are re-read.
- Writing tags may rewrite the whole file the first time (when the tag has no free space). Over the network that's slow; later writes are fast.
- Formats: reads mp3, flac, m4a, ogg, opus and wav; writes tags to mp3 and flac.
- macOS and Linux.
