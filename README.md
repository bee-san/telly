# Telly

An anime player for learning Japanese, for Windows. Point it at a folder of rips and it:

- works out the show on AniList and puts the episodes in order
- gets each episode playable, converting what WebView2 can't play (MPEG-2 DVD rips, 10-bit HEVC) in the background, next episode first
- finds Japanese subtitles on [Jimaku](https://jimaku.cc) and syncs them to your rip, the way alass does
- shows them as lines you can look up with [Hachidori](https://github.com/bee-san/hachidori), pausing while you read and carrying on when you move back to the picture
- finishes the cards you mine with an animated AVIF of the line and its audio as Opus
- explains the line on screen when you press **E**, and when you come back to a show, opens on **Previously on**: a recap of the episode before and its key moments to watch again (both by Claude, through the Claude Code on your computer)
- makes a dictionary of the characters in what you're watching, the way [characterdictionary.tokyo](https://characterdictionary.tokyo) does, adds it to Hachidori and colours their names in the subtitles
- logs what you watch for [Tidepool](https://github.com/bee-san/tidepool)

The design, plans and build order are in the [design artifact](https://claude.ai/artifact/VbgPYuAenU9pXpGiVTWGjn).

## Installing

`npm run release` builds `src-tauri/target/release/bundle/nsis/Telly_<version>_x64-setup.exe`, an installer for your Windows user. Run it as `Telly_<version>_x64-setup.exe /P /D=%LOCALAPPDATA%\Programs\Telly` to keep the app apart from Telly's data: without `/D` it installs to `%LOCALAPPDATA%\Telly`, the folder that also holds your library. Later installs reuse the folder the first one used.

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

Hachidori in Telly links to the Hachidori in your browser, so lookups use your dictionaries, settings and card format. That goes through Hachidori Relay in Anki, or Telly's own relay while Anki is closed. Turn on sharing in your browser's Hachidori (Settings → Sharing), and Telly's Settings → Hachidori says "Linked".

## Explain and Previously on

Both use [Claude Code](https://claude.com/claude-code), installed and signed in on this computer: Telly runs `claude -p` with no tools, the way Gromit Reader does, so it uses your own sign-in and needs no API key. Settings → Claude picks the model and whether answers are in English or simple Japanese.

- **Explain** (E, or Explain on a transcript line) pauses and explains the line: a translation, then the words and grammar a learner might trip on. The answer shows as it's written and is saved, so asking again is instant.
- **Previously on** (P, or Recap) recaps the episode before from its subtitles, with the story so far from the recap before it, and picks up to 6 lines whose moments play back as clips (Watch the Moments) before the episode starts. It opens by itself when you come back to a show after 8 hours (Settings → Claude), unless you're picking up in the middle of an episode. It's written when you leave after most of an episode, so it's ready next time.

## Character names

Telly fetches the casts of the 6 shows you watched last from AniList, with their portraits, and builds a Yomitan dictionary with [characterdictionary.tokyo](https://github.com/bee-san/Japanese_Character_Name_Dictionary)'s own builder (copied into `src-tauri/src/names`): each character's full name, its parts, kana spellings and honorifics (ナツさん, ルーシィ様…), with a card showing who they are. Names written with ・ (ナツ・ドラグニル) get each part. To keep a cast of hundreds a sensible size, main characters get every honorific, supporting characters the common ones, and background characters none.

It goes into Hachidori as **Telly characters** the way SubMiner sends its own: through Hachidori's upload requests, from Telly's Hachidori, which passes it on to the Hachidori in your browser once they're linked. A new copy replaces the last. Settings → Character names shows where it is, and can leave spoilers out (the default) or in.

`cargo run --release --example names_check -- <AniList id> <folder>` builds one show's dictionary on its own, to see what it holds.

## Whisper

When Jimaku has nothing for an episode (or there's no key yet), its show's page offers **Make Subtitles With Whisper**. It uses the whisper.cpp Tidepool downloads for voice input, with whisper.cpp's small multilingual model (190 MB, from Settings → Whisper). Whisper's subtitles are marked as machine-made.

## Where things go

| | |
|---|---|
| `%LOCALAPPDATA%\Telly\telly.sqlite` | your shows, episodes, where you are, settings, the card queue |
| `%LOCALAPPDATA%\Telly\media` | Telly's copies of episodes WebView2 can't play as they are |
| `%LOCALAPPDATA%\Telly\subtitles` | synced subtitles, one file per episode |
| `%LOCALAPPDATA%\Telly\names` | casts from AniList, their portraits, and the character dictionary |
| `%APPDATA%\Telly\telly-log.sqlite` | what you watched, for Tidepool (counts only, never the text) |

Your rips are never moved or changed.

## Developing

- `cargo test --manifest-path src-tauri/Cargo.toml --lib` runs the backend's tests.
- `npm run check` type-checks the frontend.
- `npm run devserver -- dev-data` runs the backend over HTTP on port 1430, and `npm run dev` then works in an ordinary browser, so the UI can be driven without the app (no Hachidori there).

Ports: 8771 is Hachidori Relay's (Telly runs its own while Anki is closed), 8773 is where Telly listens for Anki Beacon.

## Licence

GPL-3.0-or-later, as Gromit Reader (whose Hachidori, relay and Beacon code Telly copies) and alass are.
