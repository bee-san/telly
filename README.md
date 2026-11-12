# Telly

An anime player for learning Japanese, for Windows. Point it at a folder of rips and it:

- works out the show on AniList and puts the episodes in order
- lists your library as movies, series or both, sorted by when it was added, name, year released or the share of its words you know; what you're watching is one row that scrolls sideways
- gives what AniList doesn't know (live action, a folder of specials) a cover too: the poster from Wikipedia's article of that very name, else a frame from the video, taken at random when it's added, with the title drawn over it as on a poster. Every cover gets a small copy when it's got, which is what the app draws, so the Library scrolls without waiting for pictures. A film is listed as a movie, with its length
- plays each episode straight from the rip when WebView2 can (H.264, HEVC and AV1, 8-bit or 10-bit, in MKV or MP4), and converts what it can't (MPEG-2 DVD rips; DTS and TrueHD audio, to lossless FLAC) in the background, next episode first
- finds Japanese subtitles on [Jimaku](https://jimaku.cc) and syncs them to your rip: to the rip's own subtitles when it has any (in any language, pictures included), or to its speech, heard by [FireRedVAD](https://github.com/FireRedTeam/FireRedVAD)
- shows them as lines you can look up with [Hachidori](https://github.com/bee-san/hachidori), pausing while you read and carrying on when you move back to the picture
- plays the Japanese audio track unless you pick another for the show (**Audio ▾**, on rips with more than one), offers **Skip opening** and **Skip ending** from the rip's chapters, or from where [AniSkip](https://aniskip.com)'s users marked them when the chapters don't say (skipping an ending goes on to what comes after it, a preview or a scene after the credits, or to the next episode), and marks the chapters on the seek bar
- shows the rip's own English subtitles for the line while you hold **Tab**, when the rip has them as text (**?** lists every key)
- finishes the cards you mine with an animated AVIF of the line and its audio as Opus
- explains the line on screen when you press **E**, and when you come back to a show, opens on **Previously on**: the story so far in its arcs, the episode you left, and key moments to watch again, each with a caption (both by Claude, through the Claude Code on your computer)
- premines: before you watch, it lists the words in the next episodes that you don't know yet and makes them into cards in your Hachidori card format
- makes a dictionary of the characters in what you're watching, the way [characterdictionary.tokyo](https://characterdictionary.tokyo) does, adds it to Hachidori and colours their names in the subtitles
- finds a word in every line it's said in across your library, the lines you can read first, and plays the scene around each
- recommends anime to watch next, from what AniList's users recommend to people who liked the shows in your library
- logs what you watch for [Tidepool](https://github.com/bee-san/tidepool), and on your AniList list once you've signed in

The design, plans and build order are in the [design artifact](https://claude.ai/artifact/VbgPYuAenU9pXpGiVTWGjn).

## Installing

Each [release](https://github.com/bee-san/telly/releases) has the installer, `Telly_<version>_x64-setup.exe`, an installer for your Windows user. Publishing a release tagged `v<version>` (the version in `src-tauri/tauri.conf.json`) builds it on GitHub Actions and adds it to the release (`.github/workflows/release.yml`); run the workflow by hand to build one without a release. `npm run release` builds the same installer locally, in `src-tauri/target/release/bundle/nsis/`. Run it as `Telly_<version>_x64-setup.exe /P /D=%LOCALAPPDATA%\Programs\Telly` to keep the app apart from Telly's data: without `/D` it installs to `%LOCALAPPDATA%\Telly`, the folder that also holds your library. Later installs reuse the folder the first one used.

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
- **Pronunciation:** local recordings come first, for lookups and word audio on cards. Telly tries [Yomitan Audio Fast](https://github.com/bee-san/yomitan-audio-fast)'s standalone server on `127.0.0.1:5052`, then the Local Audio Server Anki add-on on `127.0.0.1:5050` (Yomitan Audio Fast's drop-in add-on uses that same address), then your other sources. Install and run either server with your audio collection; the add-on needs Anki open. These defaults are set even if the server isn't running yet. Reorder, disable or remove sources in Hachidori's Audio Settings; Telly sets them up only once.
- **Anki:** when a show starts playing, Hachidori's card template is set to the note type in Settings → Cards (kiku_typed) and the deck there (例文マイニング; `{show}` in its name stands for the show's title), with the tags `Telly` and the show's title (`Fairy_Tail`). Telly doesn't make the deck: only premining makes decks in Anki. The fields come from Hachidori's own mapping for Kiku, as its first-run setup makes them.

Settings → Hachidori can link it to the Hachidori in your browser instead, so lookups use that one's dictionaries, settings and card format, through Hachidori Relay in Anki (or Telly's own relay while Anki is closed), with sharing on in the browser's Hachidori (Settings → Sharing).

## Explain and Previously on

Both use [Claude Code](https://claude.com/claude-code), installed and signed in on this computer: Telly runs `claude -p` with no tools, the way Gromit Reader does, so it uses your own sign-in and needs no API key. It asks Haiku 5.5 unless Settings → Claude picks another model, and answers in English or simple Japanese.

Claude Code takes 3 to 15 seconds to start, longer than Haiku takes to answer. So while the player is open Telly keeps one Claude Code started and past its first turn, and Explain asks in it: the answer starts 1 to 3 seconds after you press E and is complete within 4 (measured over 12 questions: first words after 1.1 to 3.3 s, the whole answer after 1.8 to 4.1 s, against 24 s starting Claude Code for each question with its default model). What's left is Haiku's own time to answer. Telly also starts Claude Code with its non-essential traffic off (update checks, telemetry), which saves a few seconds more.

- **Explain** (E, or Explain on a transcript line) pauses and explains the line: a translation, then the words and grammar a learner might trip on. The answer shows as it's written and is saved, so asking again is instant. The box under it asks a follow-up question about the line, in the same warm Claude Code; each question carries the line, its explanation and the questions before, and follow-ups aren't saved.
- **Previously on** (P, or Recap) tells the story so far: the story cut into its arcs, each with what happens in it and the episodes it spans, then **Last time** for the episode you left, then up to 6 moments to play back as clips, each with a caption saying what's happening in it. Each recap carries on from the one kept for the episode before, so only the episode you just left is read (up to six when there's no recap to carry on from). When you come back to a show after 8 hours (Settings → Claude), the moments play first and the episode starts after them; Read recap shows the text instead. Back within 3 days, it's only Last time and that episode's moments, with the rest behind Show story so far. Picking up in the middle of an episode, the recap runs up to where you stopped. It's written when you leave the player, so it's ready next time (Haiku takes about 15 seconds for one episode); if it isn't, the episode starts and Telly says when the recap is ready.

## Filters

**Filters** in the player (V) opens a panel of tiles, each showing the frame through one filter. Click filters to put them on, and they run one after another in the order you clicked them, each on the picture the one before left: with two or more, each tile says where it comes, 1, 2, 3. Click one again to take it out, or None to take them all off. The filters are drawn over the picture only, so the subtitles stay sharp, and they're remembered for each show. When an episode fills well over its own size on screen (1.5× or more) and its show has no filter picked, a note suggests Anime4K, once.

- **Denoise** smooths out grain, as on DVD-era rips, and keeps lines sharp. **Deband** smooths the bands in skies and dark gradients.
- **Restore** undoes blur and compression blocks, and **Restore Soft** does less, for a cleaner rip.
- **Anime4K** S, M and L are [Anime4K](https://github.com/bloc97/Anime4K)'s 2× upscalers, for a rip smaller than the screen. L looks best and needs the most from the graphics card. **FSR** is AMD's FidelityFX Super Resolution 1, an upscaler to the screen's size with its own sharpening: much lighter than Anime4K. **Lanczos** scales the picture to the screen plainly.
- **Thin Lines** and **Darken Lines** make outlines slimmer or bolder.
- **Sharpen** crisps lines, and **FSR Sharpen** is FSR's sharpening alone: stronger, and gentler on grain.
- **Trinitron**, **PVM-20M2** and **Dorm TV** are three of [michadame](https://github.com/Morgawr/michadame)'s televisions: its Halo shaders and presets, unchanged. They're made for a 4K screen: on a smaller one the scanlines fade and the picture comes out brighter. A television draws the whole screen, so it always comes last, and picking another takes its place.

Denoise, Restore, Thin Lines and Darken Lines are Anime4K's too. An upscaler works from the picture as it stands, and is passed over once the picture fills the screen: Anime4K after Anime4K doesn't double it again, and Anime4K does nothing to a rip that's already the screen's size. Lanczos shrinks a picture too, so after an upscaler that left it bigger than the screen, it does the fitting. For an old rip, for example: Denoise, Restore, Anime4K M, Sharpen.

The shaders and where they come from are in `src/lib/filters/shaders`.

## Character names

Telly fetches the casts of the 6 shows you watched last from AniList, with their portraits, and builds a Yomitan dictionary with [characterdictionary.tokyo](https://github.com/bee-san/Japanese_Character_Name_Dictionary)'s own builder (copied into `src-tauri/src/names`): each character's full name, its parts, kana spellings and honorifics (ナツさん, ルーシィ様…), with a card showing who they are. Names written with ・ (ナツ・ドラグニル) get each part. To keep a cast of hundreds a sensible size, main characters get every honorific, supporting characters the common ones, and background characters none.

It goes into Hachidori as **Telly characters** the way SubMiner sends its own: through Hachidori's upload requests, from Telly's Hachidori, which passes it on to the Hachidori in your browser once they're linked. A new copy replaces the last. Settings → Character names shows where it is, and can leave spoilers out (the default) or in.

`cargo run --profile measure --example names_check -- <AniList id> <folder>` builds one show's dictionary on its own, to see what it holds.

## Premining

A show's page has **Premine**: the words in the next episode (or the next 3 or 10) that you have no card for, most useful first, each with the first line it comes up in. Tick the ones you want, or take the top 20, 40 or 80, and **Make cards**.

- **Words** come from the synced subtitles, split by [Lindera](https://github.com/lindera/lindera) with IPADIC (built into Telly). Particles, auxiliaries, pronouns, words only there for the grammar (わけ, こと, ている's いる), numbers, names and the cast are left out. So is what closed captions add that nobody says: sounds in brackets （爆発音）, the speaker's name and song lyrics (♪), so a card's sentence and audio are someone speaking. Where nouns run together, the longest stretch Hachidori knows is one word (魔導士 in 魔導士たち, not 魔 and 導).
- **Hachidori** gives each word's headword, reading and rank in your frequency dictionary: Telly's own, whose engine page looks the words up and sends back only those, or, when Telly is linked to the Hachidori in your browser, that one, through the Yomitan API Hachidori Relay serves on port 19633.
- **Anki** says what you have. A note whose word field (the one your card format fills with `{expression}`) holds the word, or its reading (する for 為る), with a card you've studied, is a card you have, so the word isn't listed. Which cards count as known is chosen in Settings → Known words: a deck, the note types, the field, and whether a card need only be studied or must be mature (21 days or more from its next review, as Anki counts it). That's the share of known words at the top.
- **Most useful first:** the words that come up most in those episodes, then the most common anywhere.
- **Cards** are made in your Hachidori card format (the first Template in its Settings → Anki), word audio included, in a **Premined** subdeck of that deck (`例文マイニング::Premined`), so you can set how many new ones you see a day. The sentence, its furigana and the cloze come from the word's line. Telly then adds the moving picture, the line's audio and MiscInfo as it does for the cards you mine while watching, and tags them `telly::premine`.

A show's page also shows, for each episode with subtitles, the share of its words that are known, counted the same way, and the Library shows each show's. Telly keeps the shares, and a copy of the words Anki says you have, so both are there at once and with Anki closed. With Anki open they're brought up to date when Telly starts, when the Library or a show's page opens, and once a day while Telly stays open: two searches for note ids, then only the notes Telly hasn't read. The show whose page is open goes first, then the shows you're watching, the latest first, then the rest.

An episode's words are worked out when its subtitles sync, and kept. An episode whose subtitles synced while Telly's Hachidori couldn't be asked (it was starting, or installing a dictionary) gets its words when the Library or a show's page next opens, that show's episodes first, or once Hachidori can be asked. Nothing is worked out while a dictionary in your folder is still to be installed, so no episode keeps words that miss its entries.

The 10 episodes of Cyberpunk: Edgerunners season 2 (5,300 lines, 3,300 different words and runs of nouns to look up) took 26 to 32 s to work out with six dictionaries, and take a few seconds now, with the same words (runs taken in turn, Telly's Hachidori in Chromium on Linux):

- Each lookup's answer carried every dictionary's definitions of up to 32 matches, 230 MB for the 10 episodes, where Telly reads only the headword, reading and frequencies of the first. The engine page now sends only those (0.6 MB), and has 16 lookups going at once: 6 to 8.5 s.
- The engine is asked for its best 2 matches, not 32, so it reads the definitions of 2 (asked for its best match alone, the 10 episodes took 2.7 s; the 2 and the 1 lookup in 20 asked again haven't been timed on a quiet computer). It ranks every match from every dictionary before it keeps any, so its first is the same, unless the two rank the same (Hachidori keeps no order between matches that tie, so which comes first can depend on how many it keeps): when the two match the same length, preprocessing and score, the 32 are asked for, as for 1 lookup in 20.
- Redoing every episode's share when your known words change reads only the saved words: 0.4 s for 270 episodes.

`cargo run --profile measure --example words_check -- <data folder> <port> <dictionaries folder> <subtitle files>` times it against a copy of Telly's Hachidori open in a browser (see the example for how; `--shares` times the Library's pass).

## Searching lines

Text typed in the Library's search that has Japanese in it is also looked for in the subtitles. Lines are listed under their shows, each with its picture and episode; pressing one plays its scene over the Library (the line with the one before and after it), and **Watch from here** opens the player there.

- **A word** finds every form of itself: 食べる, たべる and 食べた all find 食べた, 食べて and 食べられる, and the whole form is marked. It goes by the words premining saves for each episode. A name finds itself with an honorific after it too (フリーレン finds フリーレン様), which Telly's character dictionary makes one word.
- **Anything else** (a phrase, a name no episode has as a word) is looked for in the lines' text. So are the lines of episodes whose words aren't saved yet, listed after the rest. A single kana isn't: it's in nearly every line.
- **The lines you can read first** lead: those that are said (not sung, not a caption's note), then those with the fewest other words you don't know. A show lists up to 100 of its lines, with how many it has.
- The lines are kept in `telly-lines.sqlite`, made from the files in `subtitles` when they sync and in a pass as Telly starts (about three minutes for 2,700 episodes the first time).

On 850,000 lines in 2,700 episodes (made up, a tenth of them without their words saved), a word said in 42,000 lines took 0.27 s to answer, and 0.95 s to be listed on the page. Before the 100 a show, the same word sent 7.6 MB in 0.77 s, and a single kana 64 MB in 6.3 s.

## Recommended

**Recommended**, in the library's toolbar, lists anime to watch next. It goes on the shows in your library and needs no AniList sign-in. On AniList, people pair shows ("liked this, try that") and vote each pair up or down; Telly asks for the 25 most voted pairs of every show AniList knows, 50 shows to a request. It asks when Telly starts and when a show is added, about the shows it hasn't asked about that week, so the page opens with its answers.

- **Adding up:** each show has a say on a pick, counted by the size of its votes (log2(1 + votes)): 6,000 votes count about twice as much as 80. So a pick liked from many of your shows beats one that a single well-known show carries. A show watched to the end counts double one not started.
- **Series:** a show's seasons, films and side stories (AniList's relations) have one say between them, the most votes any of them gives. The rest of a series you have isn't offered as a pick.
- **A pick** opens with its synopsis, the shows it's recommended from with their votes, and **Open on AniList**. **Hide** leaves it out from now on, for one you've seen elsewhere.

AniList's answers are kept for a week. A show is asked about again after that, and a show you add is asked about the next time the page opens.

## Your AniList list

In **Settings → AniList**, **Get a token** opens AniList, which shows an access token once you approve Telly. Paste it and press **Sign in**. From then on, each episode you watch raises that anime's progress on your list, and its last episode marks it completed.

- Only episodes watched after signing in count. Progress only goes up: rewatching an earlier episode, or marking one unwatched, changes nothing.
- Watching a show your list has as completed (finished in Telly or anywhere) is a rewatch: it's marked **rewatching** at the episode you're on, goes up as you watch, and is completed again at its last episode, its rewatch count one more. An episode counts when it's played to its end again; rewatching only the last episode starts no rewatch, and nor does marking episodes watched by hand.
- A show added as one folder holding several seasons: the first season folder goes to the show's own entry, and a later one to the entry AniList finds for its folder's name, numbered from 1 within that folder. When AniList isn't fairly sure which entry a folder is, or a number is past the entry's last episode, nothing is logged.
- Shows AniList doesn't know aren't logged.
- If AniList can't be reached, Telly tries again every 10 minutes, and Settings says why it failed.
- **Recommended** leaves out what's on your list, except what you only plan to watch. A copy of the list is kept, fetched again when Telly starts, when you sign in and after Telly updates it.

## How subtitles are matched

Telly tries every file Jimaku has that could be the episode's, syncs each to the episode, and keeps the one that fits best.

- **Your own subtitles:** a Japanese subtitle file beside the rip and named after it (`Episode 01.ja.srt`, `Episode 01.srt`) is used as it is, with nothing of the rip read and no sync to move it: you put it there. Every show's episodes get theirs before any rip is read. Of several, the one re-timed with alass (`.alass.srt`) is taken, then one marked Japanese (`.ja.srt`, `.jpn.srt`), then one named just as the rip, and a machine-made one (`.ai.srt`) last; the episode's page can switch to another. A Japanese track inside the rip comes first among the rest: when it's in sync already (a sync would move it less than half a second), Telly keeps it as it is and doesn't ask Jimaku. Otherwise it's synced along with Jimaku's files.
- **What files are matched to:** the rip's own subtitles when it has any, in any language. Picture subtitles (PGS, as on most Blu-ray rips) are read from the container's packets: no OCR is needed, only when each subtitle shows. They're timed to this very video, so they place a file far more precisely than the speech does, and in a few seconds. Lines showing at once (a song's karaoke) count once. A rip without subtitles is matched to its speech, heard by FireRedVAD (FireRedTeam's detector, which finds 96% of speech where WebRTC's finds 36%, without music fooling it).
- **How sure a fit is:** by how much of the timing the file's lines cover, against the same lines moved 20 seconds to 5 minutes away, and by how often they start where the timing's lines start (within 0.3 s), against the same moved 23 seconds to 3¼ minutes away. Where the timing has lines most of the time, in long stretches of talk, a file moved a minute covers nearly as much, so coverage says little; where the lines start still says where the file belongs. So a file fits with one shift when its line starts stand 6.5 spreads above chance, and each quarter of it above chance on its own: the lines of a TV broadcast's file between two ad breaks stand far above chance at their shift, but that shift doesn't fit the rest. Line starts tell a file's speed (PAL) too, where coverage can't.
- **Which number:** a show added as one folder holding several seasons gets its files looked for under Telly's number, the number in the file's name, and that number after the season folders before it, since Jimaku's packs often number a whole run from 1. A season folder's own AniList entry is looked up on Jimaku too. What fits teaches the folder its numbering, and when nothing expected fits, every file is screened and the best few synced in full: a release whose order differs (Judas's Fairy Tail 2014 runs in another order than Netflix's from its 48th episode) still finds each episode's file.
- **Which file:** one synced in one piece beats one that fits only cut into pieces (below); otherwise one that fits beats one that doesn't; then clean Japanese text (not furigana or English set as lines); then a file made for this very release (the same release tag); then the smaller shift; then the tighter fit. A Japanese track inside the rip counts as made for it, and as an exact fit when the rip is matched to that track. When nothing fits, Telly keeps nothing rather than the wrong episode's lines.
- **Files timed to a TV broadcast:** a recording of the broadcast has the ads in it, so its subtitles are a minute or more out after each break and don't fit with one shift. When no file syncs with one shift, Telly cuts each file where alass cuts it, but only where the broadcast's lines pause for 20 seconds or more (an ad break, the opening): elsewhere alass moves an episode's last scene onto the closing song, whose sung lines, thick and fast, agree with anything. Each piece goes where its lines start with the timing's. Coverage can't judge pieces, since another episode's file cut up covers the timing as well as one that belongs, but line starts can: the pieces are kept only if, together, their lines start with the timing's far more often than when moved 23 seconds to 3¼ minutes away (6.5 spreads above chance). A piece of under 10 lines, or no better than chance, follows its neighbour, so a short recap or cold open that can't be placed is left out rather than shown at the wrong time.

Measured on Fairy Tail's 286 Judas Blu-ray episodes: every one of the 277 TV episodes synced (files that belong stood 6.3 to 17 spreads above chance, files that don't at most 5.4), Judas-timed files chosen where they exist (1 to 40), Netflix's elsewhere, and the 9 OVAs, for which Jimaku has nothing, left without. Synced by speech instead, the same files came within 0.15 s.

Files timed to the TV broadcast, measured on episodes 1 to 40: the 23 that don't fit with one shift came apart into 2 or 3 pieces (episode 3's at −5, −75 and −165 s: a 70-second ad break, then a 90-second one), each piece's lines a median of 0.6 s or less from where the Judas files have them, and 81% of lines within a second (77 to 89% for the TV files that fit with one shift). Against the rip's English subtitles they stood 8.1 to 20 spreads above chance; 508 other episodes' files (TV and Netflix-timed, 1, 2, 5 and 11 episodes away) stood at most 4.4, and none was kept. Matched to the speech instead, 18 of the 23 were kept (one with one shift), and none of the other episodes' files (at most 3.6).

Measured on Cyberpunk: Edgerunners season 2 (10 episodes, each rip with 10 full subtitle tracks, and a Japanese file beside it in time with it): against each of the rip's tracks, the files that belong had their line starts stand 29 to 43 spreads above chance (23 to 36 against the speech); the other 9 episodes' files at most 4.7 (3.3), and none of them was kept, against the tracks (900 tries) or the speech (90). By coverage alone, the files that belong stood only 4.7 to 5.4 in episodes 4 and 7, whose subtitles show 57 and 59% of the time, and those two got none; matched to the speech, 4 of the 10 were sure and 2 got none. Now every one is kept and synced, within 0.07 s, against the rip's subtitles or its speech, and so is each thrown 3.2 s late or 40 s early, sped up or slowed down for PAL (matched to the speech, 1 of the 10 was before), with its lines split, merged or started up to 0.3 s out, or with a scene 4 s out (8 of the 10 are cut back where it moved). Timed to a broadcast with ads of 90 and 75 seconds where the talk pauses about a third and two thirds in, each came apart into its 3 pieces, every line within half a second.

`cargo run --profile measure --example sync_real -- <telly.sqlite> <jimaku folder> 1-286` runs this over a library, read-only, and says what Telly would keep for each episode.

## Machine-made subtitles

When Jimaku has nothing for an episode (or there's no key yet), its show's page offers **Make subtitles with Whisper**. It uses the whisper.cpp Tidepool downloads for voice input, with one of whisper.cpp's multilingual models, chosen and downloaded in Settings → Machine-made subtitles: small (190 MB, the default), medium (539 MB), large v3 turbo (574 MB) or large v3 (1.1 GB). How much better the bigger ones hear Japanese hasn't been measured here. Whisper's subtitles are marked as machine-made.

With an OpenRouter API key in Settings, a model on [OpenRouter](https://openrouter.ai) makes them instead, and the button names it: MAI-Transcribe 2 (the default), ElevenLabs Scribe v2 or Whisper large v3, chosen in Settings. Each episode's Japanese audio is sent to OpenRouter in pieces of about 5 minutes, cut where the audio is quietest, because a provider gives a request 60 seconds. A 24-minute episode costs about $0.04 with MAI-Transcribe 2, $0.09 with ElevenLabs and $0.01 with Whisper (what OpenRouter charged for 90 seconds of each in November 2026). The audio goes as WAV, which all three take. On 90 seconds of one episode, MAI-Transcribe 2 and ElevenLabs heard every line and timed each within a quarter of a second of the episode's own subtitles; Whisper added lines nobody said. More than that hasn't been measured here.

With that key, Telly also asks OpenRouter by itself, without the button. An episode no subtitle file fits has its subtitles made this way. And when a file's timing fit is doubtful, four 90-second stretches of the episode are sent: if at least 12 of the lines heard, from at least 3 stretches, read like the file's lines and agree on one shift, the file is moved there (the episode's details say “matched to what MAI-Transcribe heard”, naming the model chosen); if not, the timing's own answer stands. On one episode, with whisper.cpp's small model listening, a file thrown out by 1.1, 7.3 and 42 seconds came back within 0.08 s.

## Getting a library ready

Telly works through a library in the background: each show's next episode first, then the 10 after it, then the rest. Several episodes are worked on at once, and the title bar says which show is being got ready, or how many shows. Click it for what each worker is doing and how far it's got, and for the work waiting of each kind (video, subtitles, the seek bar's pictures, words), the next to be taken first.

- **Video:** one conversion at a time. A rip that plays as it is doesn't wait behind one: a second worker reads what's in the rips nobody has looked at yet, about a second each.
- **Subtitles:** 3 episodes at once. A rip is read through once for all its subtitle tracks together, text and pictures, however many it has; a rip without any is heard in that read, to match files to its speech. Jimaku is asked by one episode at a time, and a show's files are fetched once.
- **For the player:** that one read also makes the seek bar's pictures, when the rip itself is what plays, and keeps the rip's English lines, so the player doesn't read the rip from end to end when an episode first plays. On an SSD, the subtitle tracks are read on their own first, in a fraction of a second where the pictures take seconds, and the pictures made just after, from what that read left in Windows' file cache. Telly's own copy of an episode gets its pictures when the copy is made. Episodes got ready before this are caught up in the background, next episodes first.
- **One rip at a time from each hard drive:** the rest of an episode's work (asking Jimaku, syncing its files) goes on while the next rip is read, but two rips aren't read from one hard drive at once, and the episodes that come first (each show's next) are read first. A hard drive reads two at once slower than one after the other. An SSD, which Windows tells apart, is read three rips at a time; a drive Windows can't say (a network share, some USB enclosures) is read as a hard drive.

Measured on a 4 TB USB hard drive (Seagate Expansion, exFAT) nothing else was reading, each run on rips that weren't in Windows' file cache:

- **A film** (Princess Mononoke: one file of 18 GB with five text subtitle tracks and one of pictures, and a Japanese subtitle file beside it): 26.8 minutes when each track was read in a pass of its own (six passes of 233 to 277 s), 4.4 minutes now (261 and 267 s). The subtitles kept are the same, byte for byte.
- **8 ordinary episodes** (four of Fullmetal Alchemist: Brotherhood and four of Sword Art Online II, 2.9 GB, 2 or 3 text tracks each): 67 and 84 s before, 60 and 62 s now. A small rip's second and third passes came from Windows' file cache, so they cost little; a film is bigger than the cache.
- **Two rips at once:** 8 episodes of one show were read at 37 and 40 MB/s one at a time, 26 and 31 MB/s two at a time, 25 and 35 MB/s four at a time. 8 episodes of four shows, which lie further apart on the drive: 38 and 48 MB/s one at a time, 23 MB/s two at a time, 21 and 23 MB/s four at a time.
- **The seek bar's pictures:** each keyframe is decoded on several cores, since keyframes are too far apart for ffmpeg's usual threads to share: 4.6 s for a 24-minute 1080p HEVC episode, where it took 17.7 s, for the same pictures. Made in the same read as the subtitle tracks, they took a 10-bit H.264 episode's read from 7 s to about 10 s (its keyframes decode on one core), and 12 Fairy Tail episodes on an SSD from 17 s to 52 s. The whole pipeline wasn't timed on the hard drive with them, as the drive was busy.
- **How many at once:** on an SSD, where nothing waits long for the drive, 24 episodes of Fairy Tail took 81 and 103 s with one worker, 54 and 59 s with two, 34 s with three, 27 and 47 s with four, 26 and 39 s with six. On the hard drive the drive is the limit whatever the number.

`cargo run --profile measure --example jobs_check -- <data folder>` runs the background work over a scratch copy of a library and says when each episode was done, and what the title bar said.

## Where things go

| | |
|---|---|
| `%LOCALAPPDATA%\Telly\telly.sqlite` | your shows, episodes, where you are, settings, the card queue |
| `%LOCALAPPDATA%\Telly\media` | Telly's copies of episodes WebView2 can't play as they are. Settings → Disk space can move them to another folder, on an external drive, say |
| `%LOCALAPPDATA%\Telly\subtitles` | synced subtitles, one file per episode |
| `%LOCALAPPDATA%\Telly\names` | casts from AniList, their portraits, and the character dictionary |
| `%LOCALAPPDATA%\Telly\known-words.json` | Telly's copy of the words Anki says you have a studied card for |
| `%LOCALAPPDATA%\Telly\telly-lines.sqlite` | every spoken line and its words, for searching lines; made again from `subtitles` if it's deleted |
| `%LOCALAPPDATA%\Telly\recommended.json` | what AniList's users recommend for each of your shows, kept for a week |
| `%LOCALAPPDATA%\Telly\anilist-list.json` | which anime are on your AniList list, once you've signed in |
| `%APPDATA%\Telly\telly-log.sqlite` | what you watched, for Tidepool (counts only, never the text) |

Your rips are never moved or changed.

## Developing

- `cargo test --manifest-path src-tauri/Cargo.toml --lib` runs the backend's tests.
- `npm run check` type-checks the frontend.
- `npm run devserver -- dev-data` runs the backend over HTTP on port 1430, and `npm run dev` then works in an ordinary browser, so the UI can be driven without the app (no Hachidori there).
- `TELLY_YOMITAN_API` points premining at a stand-in for Hachidori's API, `TELLY_CLAUDE` Explain and Previously on at a stand-in for Claude Code, and `TELLY_JIMAKU_API` the subtitle search at a stand-in for Jimaku, for testing without your own.
- `--profile measure` builds optimised but without the release build's link-time optimisation, for the measuring examples: `sync_real` (subtitle matching over a library), `recap_check` (one Previously on from real subtitles), `explain_check` (how soon Explain answers in the warm session), `names_check` (one show's character dictionary) and `jobs_check` (the background work over a scratch library).

Ports: 8771 is Hachidori Relay's (Telly runs its own while Anki is closed), 8773 is where Telly listens for Anki Beacon.

## Licence

GPL-3.0-or-later, as Gromit Reader (whose Hachidori, relay and Beacon code Telly copies) and alass are.
