# AGENTS.md

These instructions apply to the whole repository. The manual (`docs/manual.md`) says what Telly does; this file says how to work on it.

## Working agreement

- Commit to `main` and push. Telly has no pull requests or review gate: every change is a commit on `main`, one coherent change each.
- Other Claude Code sessions work on this checkout at the same time and keep their worktrees in `.claude/worktrees/`. Make your change in a git worktree of your own off `origin/main`, not in the main checkout, and fetch before you push: if `main` has moved, rebase onto it and check again.
- Make the smallest change that does what was asked and fits the code around it. Leave refactors, renames and reformatting out of it.
- When visible behaviour changes, change the manual's description of it (`docs/manual.md`) in the same commit. The manual describes what Telly does for the viewer, so keep it to that.
- The README is Telly's front page: a pitch, pictures and a line or two for each feature, with the details in the manual. Change it only when a feature it shows changes or a new one is worth showing there, and keep it short.
- Release only when asked (see Releases).

## Commit messages

- The subject is one sentence, in the present tense, saying what Telly does now, as the viewer meets it: "Leaving an episode writes only the recap you'll see next", "Esc on the countdown to the next episode keeps it stopped too, as Stop does". Say what changed for the viewer, not which code changed, and use no `feat:`/`fix:` prefixes.
- The body says what it did before and why that was wrong, then what it does now. When speed is the point, give the numbers before and after, with what was measured (`Finding where a word is said cost a common word 3 to 6% more instructions; now 0.4 to 2.3%`).
- End with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Writing

Telly's README, manual, comments and UI text are plain English, written to the person using it ("your rips", "the episode you left"). Keep to that:

- A doc comment says in a sentence what a thing is or does (`/** Whether moment i + 1 carries straight on from moment i, in the same episode. */`), not how it's built.
- Each backend module opens with a `//!` paragraph saying what it's for. Keep it true when the module changes.
- UI text says what will happen ("Start Episode 3", "Read recap"), and its keyboard shortcut goes in the title (`title="Previously on (P)"`).

## Where things are

- `src-tauri/src/` is the Rust backend. Everything the UI asks for goes through `Backend::call(cmd, args)` in `backend.rs` (the player's part of it is in `backend/player.rs`, and adding shows and looking in their folders in `backend/adding.rs`), and each module's `//!` comment says what it does: `db.rs` the library (telly.sqlite), `jobs.rs` the background work that gets shows ready, `media.rs` what WebView2 can play, `sync.rs` and `heard.rs` subtitle timing, `ask.rs` and `claude.rs` Explain and Previously on, `cards.rs`, `clip.rs` and `mining.rs` Anki cards, `search.rs` line search, `parts.rs` a chosen folder's seasons, movies and OVAs and the AniList entry of each, `series.rs` which anime a show's seasons and movies belong to, `hachidori.rs` and `own_hachidori.rs` the Hachidori inside Telly.
- `src/` is the SvelteKit frontend, a single-page app (adapter-static, no SSR). `src/lib/api.ts` has `call()` and a type for every answer. The routes are the Library (`+page.svelte`), `anime` (an anime's seasons and movies), `show`, `watch` (the player, Explain and Previously on; its end card and shortcuts list are components in `src/lib/player`, and how a subtitle line becomes HTML is `src/lib/subtitle-html.ts`), `settings`, `setup` (the first-time setup; its checks, in `src/lib/checks.ts`, also make Settings' list of what's not working), `premine` and `recommended`.
- Telly ships for Windows, where the UI runs in WebView2 (Chromium). `npm run tauri dev` on Linux runs it in WebKitGTK, and the media protocol differs between the two (`mediaUrl` in `api.ts`). A web API that only Chromium has needs checking on both, or avoiding.
- Hachidori is a separate project ([bee-san/hachidori](https://github.com/bee-san/hachidori)). `npm run prepare:hachidori` copies a pinned release into `src-tauri/extensions/` (ignored by git). Hachidori's own changes belong in its repository.

## What Telly must keep

- Your rips are never moved or changed. What Telly makes goes in its own folders (the manual's "Where things go").
- Tidepool's log counts what you watched and never keeps the text.
- Previously on tells the story only from the subtitles of what's been watched, never from what Claude knows of the show elsewhere, so nothing ahead is spoiled. Keep that instruction in its prompt (`recap_prompt` in `ask.rs`).

## Checks

Run what exercises the change, and say in the commit body what you ran when it isn't obvious:

- Frontend: `npm run check`, `npm test` (Vitest, the `*.test.ts` beside what they test in `src/lib`), and `npm run build` for anything that could break the build.
- Backend: `cargo test --manifest-path src-tauri/Cargo.toml --lib`. Four tests use Windows paths (`D:\Anime\...`) and fail on Linux: `a_season_folder_logs_to_its_own_entry_numbered_from_one`, `rips_on_one_drive_share_its_turns`, `season_folders_in_order` and `shows_are_listed_under_their_anime_in_the_order_they_came_out`.
- A push to `main` runs these on GitHub Actions, only for the side it changes: the backend's (`.github/workflows/checks.yml`) on Windows when `src-tauri/` changes, the frontend's (`.github/workflows/frontend.yml`) on Linux when the frontend does. Look at the run after you push.
- UI behaviour: `npm run devserver -- dev-data` runs the real backend over HTTP on port 1430, and `npm run dev` then works in an ordinary browser. `TELLY_CLAUDE`, `TELLY_YOMITAN_API` and `TELLY_JIMAKU_API` point Claude Code, Hachidori's API and Jimaku at stand-ins.
- For UI that needs data the backend is slow to make (a recap's moments, a show's episodes converted), a small stand-in on port 1430 will do: the page only sends `POST /call` with `{cmd, args}` (answered as `{ok}` or `{error}`) and `GET /media/...` with byte ranges, so canned answers and short test videos made with ffmpeg can drive the real page in headless Chromium.
- Speed: build with `--profile measure` and use the examples in `src-tauri/examples/` (`sync_real`, `recap_check`, `explain_check`, `names_check`, `jobs_check`). Compare before and after on the same input, and put the numbers in the commit body.

Ports: 1420 is Vite's, 1430 the dev server's, 8771 Hachidori Relay's (Telly runs its own while Anki is closed) and 8773 where Telly listens for Anki Beacon. Anki may be running on this computer, so never kill Anki's processes by pattern.

## Releases

Only when asked:

1. Set the new version in `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` and `src-tauri/tauri.conf.json`, and commit that alone as `Telly <version>`.
2. Push, tag that commit `v<version>`, and publish a GitHub release for the tag. `.github/workflows/release.yml` builds `Telly_<version>_x64-setup.exe` on Windows and attaches it to the release.
