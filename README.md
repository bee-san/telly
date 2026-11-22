<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-dark.svg">
    <img src="src-tauri/app-icon.svg" width="120" alt="Telly's logo: a television with a play button">
  </picture>
</p>

<h1 align="center">Telly</h1>

<p align="center">
  <b>Learn Japanese from the anime you already have.</b><br>
  An anime player for Windows. Point it at your rips, and every word on screen is a hover away.
</p>

<p align="center">
  <a href="https://github.com/bee-san/telly/releases/latest"><img src="https://img.shields.io/badge/Download_for_Windows-ffd84a?style=for-the-badge" alt="Download for Windows"></a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows-0078d4" alt="Platform: Windows">
  <a href="LICENSE"><img src="https://img.shields.io/badge/licence-GPL--3.0--or--later-blue" alt="Licence: GPL-3.0-or-later"></a>
  <img src="https://img.shields.io/badge/Tauri_2-24c8db?logo=tauri&logoColor=white" alt="Built with Tauri 2">
  <img src="https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/Svelte_5-ff3e00?logo=svelte&logoColor=white" alt="Svelte 5">
</p>

<br>

![Looking up ゴチャゴチャ in a subtitle line with Hachidori](docs/lookup.webp)

Telly finds Japanese subtitles for your rips and syncs them. Hover over a word to look it up, and mine it to Anki with the moving picture and the line's audio. Everything gets ready in the background, so you just press play.

## 📺 Previously on, like on TV

Come back to a show and Telly opens on a recap: the story so far, then the key moments from last time, cut together like a TV recap. Press **E** on any line and Claude explains it, grammar and all. The recap only uses what you've watched, so nothing ahead is spoiled.

![Previously on: the moments from the episode you left, each with a caption, then the episode starts](docs/previously-on.gif)

## 🎨 Make old rips look new

Anime4K upscaling, denoising and sharpening, stacked in the order you pick and remembered for each show. Or watch it on a Trinitron.

![Filters: clicking Anime4K, Restore and others on and off, each tile showing the frame through its filter](docs/filters.gif)

## 📚 Pick what you're ready for

Your library shows how much of each show's vocabulary you already know, so you can sort or filter by it and start with what you can follow.

![The Library: what you're watching, then every show with the share of its words you know](docs/library.webp)

## 🗂️ Every season in one place

Each anime is listed once, its seasons and movies inside, in the order they came out, with how far you are through each.

![K-ON!'s page: its two seasons and its movie, with how much of each you've watched and the next episode](docs/seasons.webp)

## 📛 Know who's who

Telly builds a dictionary of the characters in what you're watching, with their portraits, and colours their names in the subtitles.

![ハク looked up in a subtitle line: his name card from Telly characters, with his portrait](docs/names.webp)

## 📱 Take it with you

Telly for Android copies the episodes you pick from your PC over your Wi-Fi and plays them anywhere. Tap a word to look it up, add it to AnkiDroid, and your PC knows where you got to when you're home. A handheld's controller does all of it too. See [Telly on your phone](docs/manual.md#-telly-on-your-phone).

## 💡 Find what to watch next

Recommendations from AniList's users, weighed across your whole library and your AniList list.

![Recommended: anime to watch next, each with the shows in your library it's recommended from](docs/recommended.webp)

## 📊 Watch yourself get better

Time watched, cards mined and words known, over a week, a month, a year or all time.

![Stats for 7 days: time watched, episodes, lines, cards mined, each day's watching and words known](docs/stats.webp)

## ✨ And more

- 🎯 **Subtitles that fit:** found on Jimaku and synced to your rip, even files timed to a TV broadcast, ad breaks and all
- 🃏 **Cards that move:** every card you mine gets an animated picture of the line, and its audio
- 💬 **English when you need it:** hold **Tab** for the rip's own English line
- 🔊 **Boost dialogue:** quiet speech comes up and loud music comes down, so you catch every word
- 🎬 **Dramas and films too:** Japanese live action is found on TMDB, with its poster and subtitles
- 📝 **Premining:** learn the next episode's new words before you watch it
- 🗣️ **Every line a word is said in,** across your whole library, ready to play
- 🪟 **A small player** that stays on top of a game, lookups and all
- ⏭️ **Skip opening and outro,** from the rip's chapters or AniSkip
- 🤖 **No subtitles anywhere?** Telly makes them with Whisper
- ⬇️ **Downloads built in:** off until you add a search site you have the right to download from; then find releases of a show and download them with Telly's own torrent client, which picks up where it was when the connection drops. Each episode joins your library as it finishes. Download only what you have the right to.
- 📈 **AniList and Tidepool** log what you watch for you

## 🆚 Why Telly?

| Without Telly | With Telly |
|---|---|
| Find subtitles, download them, line them up with your rip | Found and synced for every episode, in the background |
| Set up a dictionary popup over your video player | Hover over a word, and the episode pauses while you read |
| Take a screenshot and cut the audio for every card | Every card gets the moving picture and the line's audio |
| Try to remember what happened last time | **Previously on** |

## 🚀 Get started

1. **[Download Telly](https://github.com/bee-san/telly/releases/latest)** and run the installer.
2. Tell it where your rips are. The setup finds ffmpeg, Anki, your dictionaries and Claude Code itself, and asks only for what it can't find.
3. Press play.

📖 Everything else, from every shortcut to how subtitles are matched, is in **[the manual](docs/manual.md)**.

<br>

<p align="center">
  <sub>Built on <a href="https://github.com/bee-san/hachidori">Hachidori</a>, <a href="https://jimaku.cc">Jimaku</a>, <a href="https://github.com/ikatson/rqbit">librqbit</a>, <a href="https://anilist.co">AniList</a>, <a href="https://github.com/bloc97/Anime4K">Anime4K</a>, <a href="https://github.com/Morgawr/michadame">michadame</a>, <a href="https://github.com/FireRedTeam/FireRedVAD">FireRedVAD</a> and <a href="https://claude.com/claude-code">Claude Code</a>. GPL-3.0-or-later.</sub>
</p>
