# Telly

An anime player for learning Japanese, for Windows. Point it at a folder of rips and it:

- works out the show on AniList and puts the episodes in order
- gets each episode playable, converting what WebView2 can't play (MPEG-2 DVD rips, 10-bit HEVC) in the background, next episode first
- finds Japanese subtitles on [Jimaku](https://jimaku.cc) and syncs them to your rip: to the rip's own subtitles when it has any (in any language, pictures included), or to its speech, heard by [FireRedVAD](https://github.com/FireRedTeam/FireRedVAD)
- shows them as lines you can look up with [Hachidori](https://github.com/bee-san/hachidori), pausing while you read and carrying on when you move back to the picture
- plays the Japanese audio track unless you pick another for the show (**Audio ▾**, on rips with more than one), offers **Skip opening** from the rip's chapters, and marks the chapters and the lines on the seek bar
- shows the rip's own English subtitles for the line while you hold **Tab**, when the rip has them as text (**?** lists every key)
- finishes the cards you mine with an animated AVIF of the line and its audio as Opus
- explains the line on screen when you press **E**, and when you come back to a show, opens on **Previously on**: the story so far in its arcs, the episode you left, and key moments to watch again, each with a caption (both by Claude, through the Claude Code on your computer)
- premines: before you watch, it lists the words in the next episodes that you don't know yet and makes them into cards in your Hachidori card format
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

Hachidori in Telly is ready to mine on its own, in overlay mode (lookups on hover, no first-run page):

- **Dictionaries:** it installs every Yomitan dictionary (.zip) in `Documents\dictionaries`, or the folder chosen in Settings → Hachidori, one at a time, and again when a file changes. The word dictionaries go in first (Jitendex, JMdict, then 大辞林 and 三省堂国語辞典), so they lead the popup and the card's definition; the rest follow, smallest first. Hachidori's own Settings (Settings → Hachidori → Open Hachidori Settings) can reorder them.
- **Anki:** when a show starts playing, Hachidori's card template is set to the note type in Settings → Cards (kiku_typed) and the show's deck (`Type::{show}`, so `Type::Fairy Tail`), which Telly makes in Anki if it's missing. The fields come from Hachidori's own mapping for Kiku, as its first-run setup makes them.

Settings → Hachidori can link it to the Hachidori in your browser instead, so lookups use that one's dictionaries, settings and card format, through Hachidori Relay in Anki (or Telly's own relay while Anki is closed), with sharing on in the browser's Hachidori (Settings → Sharing).

## Explain and Previously on

Both use [Claude Code](https://claude.com/claude-code), installed and signed in on this computer: Telly runs `claude -p` with no tools, the way Gromit Reader does, so it uses your own sign-in and needs no API key. It asks Haiku 5.5 unless Settings → Claude picks another model, and answers in English or simple Japanese.

Claude Code takes 3 to 15 seconds to start, longer than Haiku takes to answer. So while the player is open Telly keeps one Claude Code started and past its first turn, and Explain asks in it: the answer starts 1 to 3 seconds after you press E and is complete within 4 (measured over 12 questions: first words after 1.1 to 3.3 s, the whole answer after 1.8 to 4.1 s, against 24 s starting Claude Code for each question with its default model). What's left is Haiku's own time to answer. Telly also starts Claude Code with its non-essential traffic off (update checks, telemetry), which saves a few seconds more.

- **Explain** (E, or Explain on a transcript line) pauses and explains the line: a translation, then the words and grammar a learner might trip on. The answer shows as it's written and is saved, so asking again is instant. The box under it asks a follow-up question about the line, in the same warm Claude Code; each question carries the line, its explanation and the questions before, and follow-ups aren't saved.
- **Previously on** (P, or Recap) tells the story so far: the story cut into its arcs, each with what happens in it and the episodes it spans, then **Last time** for the episode you left, then up to 6 moments to play back as clips, each with a caption saying what's happening in it. Each recap carries on from the one kept for the episode before, so only the episode you just left is read (up to six when there's no recap to carry on from). When you come back to a show after 8 hours (Settings → Claude), the moments play first and the episode starts after them; Read recap shows the text instead. Back within 3 days, it's only Last time and that episode's moments, with the rest behind Show story so far. Picking up in the middle of an episode, the recap runs up to where you stopped. It's written when you leave the player, so it's ready next time (Haiku takes about 15 seconds for one episode); if it isn't, the episode starts and Telly says when the recap is ready.

## Filters

**Filters** in the player (V) opens a panel of tiles, each showing the frame through one filter. The filter is drawn over the picture only, so the subtitles stay sharp, and it's remembered for each show. When an episode fills well over its own size on screen (1.5× or more) and its show has no filter picked, a note suggests Anime4K, once.

- **Deband** smooths the bands in skies and dark gradients, and **Sharpen** crisps lines.
- **Anime4K** S, M and L are [Anime4K](https://github.com/bloc97/Anime4K)'s 2× upscalers, for a rip smaller than the screen. L looks best and needs the most from the graphics card.
- **Trinitron**, **PVM-20M2** and **Dorm TV** are three of [michadame](https://github.com/Morgawr/michadame)'s televisions: its Halo shaders and presets, unchanged. They're made for a 4K screen: on a smaller one the scanlines fade and the picture comes out brighter.

The shaders and where they come from are in `src/lib/filters/shaders`.

## Character names

Telly fetches the casts of the 6 shows you watched last from AniList, with their portraits, and builds a Yomitan dictionary with [characterdictionary.tokyo](https://github.com/bee-san/Japanese_Character_Name_Dictionary)'s own builder (copied into `src-tauri/src/names`): each character's full name, its parts, kana spellings and honorifics (ナツさん, ルーシィ様…), with a card showing who they are. Names written with ・ (ナツ・ドラグニル) get each part. To keep a cast of hundreds a sensible size, main characters get every honorific, supporting characters the common ones, and background characters none.

It goes into Hachidori as **Telly characters** the way SubMiner sends its own: through Hachidori's upload requests, from Telly's Hachidori, which passes it on to the Hachidori in your browser once they're linked. A new copy replaces the last. Settings → Character names shows where it is, and can leave spoilers out (the default) or in.

`cargo run --profile measure --example names_check -- <AniList id> <folder>` builds one show's dictionary on its own, to see what it holds.

## Premining

A show's page has **Premine…**: the words in the next episode (or the next 3 or 10) that you have no card for, most useful first, each with the first line it comes up in. Tick the ones you want, or take the top 20, 40 or 80, and **Make cards**.

- **Words** come from the synced subtitles, split by [Lindera](https://github.com/lindera/lindera) with IPADIC (built into Telly). Particles, auxiliaries, pronouns, words only there for the grammar (わけ, こと, ている's いる), numbers, names and the cast are left out. So is what closed captions add that nobody says: sounds in brackets （爆発音）, the speaker's name and song lyrics (♪), so a card's sentence and audio are someone speaking. Where nouns run together, the longest stretch Hachidori knows is one word (魔導士 in 魔導士たち, not 魔 and 導).
- **Hachidori** gives each word's headword, reading and rank in your frequency dictionary: Telly's own, through Hachidori's own API code running in its engine page, or, when Telly is linked to the Hachidori in your browser, that one, through the Yomitan API Hachidori Relay serves on port 19633.
- **Anki** says what you have. A note whose word field (the one your card format fills with `{expression}`) holds the word, or its reading (する for 為る), is a card you have, so the word isn't listed. A card 21 days or more apart is mature, and its word is known: that's the share of known words at the top.
- **Most useful first:** the words that come up most in those episodes, then the most common anywhere.
- **Cards** are made in your Hachidori card format (the first Template in its Settings → Anki), word audio included, in a **Premined** subdeck of the show's deck (`Type::Fairy Tail::Premined`), so you can set how many new ones you see a day. The sentence, its furigana and the cloze come from the word's line. Telly then adds the moving picture, the line's audio and MiscInfo as it does for the cards you mine while watching, and tags them `telly::premine`.

A show's page also shows, for each episode with subtitles, the share of its words that are on cards you've studied in Anki, counted the same way. It's worked out when the page opens, with Anki open.

## How subtitles are matched

Telly tries every file Jimaku has that could be the episode's, syncs each to the episode, and keeps the one that fits best.

- **What files are matched to:** the rip's own subtitles when it has any, in any language. Picture subtitles (PGS, as on most Blu-ray rips) are read from the container's packets: no OCR is needed, only when each subtitle shows. They're timed to this very video, so they place a file far more precisely than the speech does, and in a few seconds. A rip without subtitles is matched to its speech, heard by FireRedVAD (FireRedTeam's detector, which finds 96% of speech where WebRTC's finds 36%, without music fooling it).
- **Which number:** a show added as one folder holding several seasons gets its files looked for under Telly's number, the number in the file's name, and that number after the season folders before it, since Jimaku's packs often number a whole run from 1. A season folder's own AniList entry is looked up on Jimaku too. What fits teaches the folder its numbering, and when nothing expected fits, every file is screened and the best few synced in full: a release whose order differs (Judas's Fairy Tail 2014 runs in another order than Netflix's from its 48th episode) still finds each episode's file.
- **Which file:** one synced in one piece beats one that fits only cut into pieces (below); otherwise one that fits beats one that doesn't; then clean Japanese text (not furigana or English set as lines); then a file made for this very release (the same release tag); then the smaller shift; then the tighter fit. A Japanese track inside the rip counts as made for it, and as an exact fit when the rip is matched to that track. When nothing fits, Telly keeps nothing rather than the wrong episode's lines.
- **Files timed to a TV broadcast:** a recording of the broadcast has the ads in it, so its subtitles are a minute or more out after each break and don't fit with one shift. When no file syncs with one shift, Telly cuts each file where alass cuts it, but only where the broadcast's lines pause for 20 seconds or more (an ad break, the opening): elsewhere alass moves an episode's last scene onto the closing song, whose sung lines, thick and fast, agree with anything. Each piece goes where its lines start with the timing's. Coverage can't judge pieces, since another episode's file cut up covers the timing as well as one that belongs, but line starts can: the pieces are kept only if, together, their lines start with the timing's far more often than when moved 23 seconds to 3¼ minutes away (6.5 spreads above chance). A piece of under 10 lines, or no better than chance, follows its neighbour, so a short recap or cold open that can't be placed is left out rather than shown at the wrong time.

Measured on Fairy Tail's 286 Judas Blu-ray episodes: every one of the 277 TV episodes synced (files that belong stood 6.3 to 17 spreads above chance, files that don't at most 5.4), Judas-timed files chosen where they exist (1 to 40), Netflix's elsewhere, and the 9 OVAs, for which Jimaku has nothing, left without. Synced by speech instead, the same files came within 0.15 s.

Files timed to the TV broadcast, measured on episodes 1 to 40: the 23 that don't fit with one shift came apart into 2 or 3 pieces (episode 3's at −5, −75 and −165 s: a 70-second ad break, then a 90-second one), each piece's lines a median of 0.6 s or less from where the Judas files have them, and 81% of lines within a second (77 to 89% for the TV files that fit with one shift). Against the rip's English subtitles they stood 8.1 to 20 spreads above chance; 508 other episodes' files (TV and Netflix-timed, 1, 2, 5 and 11 episodes away) stood at most 4.4, and none was kept. Matched to the speech instead, 18 of the 23 were kept (one with one shift), and none of the other episodes' files (at most 3.6).

`cargo run --profile measure --example sync_real -- <telly.sqlite> <jimaku folder> 1-286` runs this over a library, read-only, and says what Telly would keep for each episode.

## Whisper

When Jimaku has nothing for an episode (or there's no key yet), its show's page offers **Make subtitles with Whisper**. It uses the whisper.cpp Tidepool downloads for voice input, with one of whisper.cpp's multilingual models, chosen and downloaded in Settings → Whisper: small (190 MB, the default), medium (539 MB), large v3 turbo (574 MB) or large v3 (1.1 GB). How much better the bigger ones hear Japanese hasn't been measured here. Whisper's subtitles are marked as machine-made.

## Where things go

| | |
|---|---|
| `%LOCALAPPDATA%\Telly\telly.sqlite` | your shows, episodes, where you are, settings, the card queue |
| `%LOCALAPPDATA%\Telly\media` | Telly's copies of episodes WebView2 can't play as they are. Settings → Disk space can move them to another folder, on an external drive, say |
| `%LOCALAPPDATA%\Telly\subtitles` | synced subtitles, one file per episode |
| `%LOCALAPPDATA%\Telly\names` | casts from AniList, their portraits, and the character dictionary |
| `%APPDATA%\Telly\telly-log.sqlite` | what you watched, for Tidepool (counts only, never the text) |

Your rips are never moved or changed.

## Developing

- `cargo test --manifest-path src-tauri/Cargo.toml --lib` runs the backend's tests.
- `npm run check` type-checks the frontend.
- `npm run devserver -- dev-data` runs the backend over HTTP on port 1430, and `npm run dev` then works in an ordinary browser, so the UI can be driven without the app (no Hachidori there).
- `TELLY_YOMITAN_API` points premining at a stand-in for Hachidori's API, and `TELLY_CLAUDE` Explain and Previously on at a stand-in for Claude Code, for testing without your own.
- `--profile measure` builds optimised but without the release build's link-time optimisation, for the measuring examples: `sync_real` (subtitle matching over a library), `recap_check` (one Previously on from real subtitles), `explain_check` (how soon Explain answers in the warm session) and `names_check` (one show's character dictionary).

Ports: 8771 is Hachidori Relay's (Telly runs its own while Anki is closed), 8773 is where Telly listens for Anki Beacon.

## Licence

GPL-3.0-or-later, as Gromit Reader (whose Hachidori, relay and Beacon code Telly copies) and alass are.
