# Telly

An anime player for learning Japanese, for Windows. Point it at a folder of rips and it:

- works out the show on AniList and puts the episodes in order
- gets each episode playable, converting what WebView2 can't play (MPEG-2 DVD rips, 10-bit HEVC) in the background, next episode first
- finds Japanese subtitles on [Jimaku](https://jimaku.cc) and syncs them to your rip, the way alass does
- shows them as lines you can look up with [Hachidori](https://github.com/bee-san/hachidori), pausing while you read and carrying on when you move back to the picture
- finishes the cards you mine with an animated AVIF of the line and its audio as Opus
- logs what you watch for [Tidepool](https://github.com/bee-san/tidepool)

The design, plans and build order are in the [design artifact](https://claude.ai/artifact/VbgPYuAenU9pXpGiVTWGjn).

## Running it

```
npm install
npm run prepare:hachidori   # once: downloads Hachidori and sets it up for Telly
npm run tauri dev
```

Telly needs ffmpeg (with libsvtav1 and libopus for cards). It uses the one you choose in Settings, else one on PATH, else GameSentenceMiner's copy.

Then in Settings:

- add your Jimaku API key (jimaku.cc → Account)
- check the card fields (Kiku's `Picture`, `SentenceAudio` and `MiscInfo` by default)
- add `http://127.0.0.1:8773/anki/events` to Anki Beacon's addresses, so cards are finished the moment you add them (without Beacon, Telly asks AnkiConnect every 2 seconds while you watch)

## Where things go

| | |
|---|---|
| `%LOCALAPPDATA%\Telly\telly.sqlite` | your shows, episodes, where you are, settings, the card queue |
| `%LOCALAPPDATA%\Telly\media` | Telly's copies of episodes WebView2 can't play as they are |
| `%LOCALAPPDATA%\Telly\subtitles` | synced subtitles, one file per episode |
| `%APPDATA%\Telly\telly-log.sqlite` | what you watched, for Tidepool (counts only, never the text) |

Your rips are never moved or changed.

## Developing

- `cargo test --manifest-path src-tauri/Cargo.toml --lib` runs the backend's tests.
- `npm run check` type-checks the frontend.
- `npm run devserver -- dev-data` runs the backend over HTTP on port 1430, and `npm run dev` then works in an ordinary browser, so the UI can be driven without the app (no Hachidori there).

Ports: 8771 is Hachidori Relay's (Telly runs its own while Anki is closed), 8773 is where Telly listens for Anki Beacon.

## Licence

GPL-3.0-or-later, as Gromit Reader (whose Hachidori, relay and Beacon code Telly copies) and alass are.
