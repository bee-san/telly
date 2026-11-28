//! Shows coming into Telly: reading a folder for Add media…, matching what's
//! in it to AniList, adding its shows and parts, and the look in a show's
//! folder for new episodes, rips that moved and new releases of ones that
//! are gone.

use super::*;

impl Backend {
    /// Reads a folder for Add media: its files, the anime it is, and, when it
    /// holds more than one show (a folder for each season, one of movies, a
    /// loose OVA), its parts with the AniList entry of each (parts.rs). With
    /// `base`, the anime is the one you chose, and the folder just read isn't
    /// read again. With `sure` (the first-time setup, where many folders are
    /// read and few looked at), an anime whose names aren't much like the
    /// folder's isn't taken for it. `asRead` is the shows it's added as when
    /// nothing is changed.
    pub(super) fn scan(&self, folder: &str, base: Option<i64>, sure: bool) -> Result<Value> {
        let kept = self.scanned.lock().ok().and_then(|s| s.clone()).filter(|s| base.is_some() && s.folder == folder);
        let scan = match kept {
            Some(scan) => scan,
            None => self.read_folder(folder)?,
        };
        if let Ok(mut kept) = self.scanned.lock() {
            *kept = Some(scan.clone());
        }
        if let Ok(mut sheet) = self.sheet.lock() {
            *sheet = Some((folder.to_string(), std::time::Instant::now()));
        }
        let found = self.matched(scan, base, sure)?;
        let as_read: Vec<Value> = found.shows().iter().map(|(show, shared)| as_read(show, *shared)).collect();
        Ok(json!({ "scan": found.scan, "matches": found.matches, "parts": found.parts, "entries": found.entries, "anime": found.anime, "error": found.error, "ffprobe": self.tools().ffprobe.is_some(), "asRead": as_read }))
    }

    /// Whether Add media's sheet has been open on `path`, a folder in it or one
    /// it's in, in the last half hour: what's there is yours to add, as you
    /// match it, not the libraries' look's.
    pub(crate) fn sheet_open(&self, path: &str) -> bool {
        let sheet = self.sheet.lock().ok().and_then(|sheet| sheet.clone());
        sheet.is_some_and(|(folder, since)| since.elapsed() < Duration::from_secs(1800) && (relink::below(&folder, path).is_some() || relink::below(path, &folder).is_some()))
    }

    /// What's done once Add media's sheet has added from the folder it read:
    /// the libraries' look may look there again, and takes none of what you
    /// left out for new. `added` are the rips of the shows it added.
    pub(super) fn sheet_done(&self, scan: Option<&scan::Scan>, added: &[&str]) {
        if let Ok(mut sheet) = self.sheet.lock() {
            *sheet = None;
        }
        // Only the folder those shows were added from: the one read last may be another.
        let Some(scan) = scan.filter(|scan| !added.is_empty() && added.iter().all(|path| scan.files.iter().any(|f| f.path == *path))) else { return };
        let rest: Vec<(String, f64)> = scan.files.iter().filter(|f| f.duration > 0.0 && !added.contains(&f.path.as_str())).map(|f| (f.path.clone(), f.duration)).collect();
        crate::library::pass_over(self, &rest);
    }

    /// The length of each video, through ffprobe. One still downloading has none.
    pub(crate) fn lengths(&self, paths: &[PathBuf]) -> Vec<Option<f64>> {
        let tools = self.tools();
        self.downloads.unless_downloading(paths, |paths| scan::probe_durations(tools.ffprobe.as_deref(), paths))
    }

    /// Reads a folder's videos: which are episodes, and the show's name.
    pub(crate) fn read_folder(&self, folder: &str) -> Result<scan::Scan> {
        scan::scan(Path::new(folder), |paths| self.lengths(paths))
    }

    /// The anime a folder that's been read is, and its parts with the AniList
    /// entry of each. With `base`, the anime is the one you chose. With
    /// `sure`, an anime none of whose names is much like the folder's isn't
    /// taken for it.
    pub(super) fn matched(&self, scan: scan::Scan, base: Option<i64>, sure: bool) -> Result<Found> {
        let split = parts::split(&scan);
        // Of several seasons, the first one's episodes say which entry the folder's name means.
        let count = if split.len() > 1 { split.iter().find(|p| p.kind == parts::Kind::Season).map_or(0, parts::episodes) } else { scan.files.iter().filter(|f| f.number.is_some()).count() };
        let (mut matches, mut error) = match anilist::search_loosely(&scan.guess) {
            Ok(mut found) => {
                anilist::rank(&mut found, &scan.guess, count);
                if sure && !found.first().is_some_and(|a| a.is_called(&scan.guess)) {
                    found.clear();
                }
                let error = anilist::follow_sequels(&mut found, &scan.guess).err().map(|e| format!("{e:#}"));
                (found, error)
            }
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        };
        if let Some(id) = base {
            let chosen = anilist::by_id(id)?;
            matches.retain(|a| a.id != id);
            matches.insert(0, chosen);
        }
        // The rest of the anime: its other seasons, its movies and its OVAs.
        let graph = matches.first().and_then(|found| match series::around(self, found.id) {
            Ok(graph) => Some(graph),
            Err(e) => {
                error.get_or_insert(format!("{e:#}"));
                None
            }
        });
        let (parts, entries, anime) = match (&graph, matches.first()) {
            (Some(graph), Some(found)) => {
                let root = graph.root(found.id).unwrap_or(found.id);
                let entries: Vec<Anime> = graph.parts(root).into_iter().map(|e| e.anime.clone()).collect();
                (parts::assign(split, graph, found), entries, Some(graph.entry(root).map_or(found, |e| &e.anime).clone()))
            }
            _ => (parts::unassigned(split, &scan.guess), Vec::new(), matches.first().cloned()),
        };
        // One part is one show, added as ever.
        let parts = if parts.len() > 1 { parts } else { Vec::new() };
        Ok(Found { scan, matches, parts, entries, anime, error })
    }

    /// Adds a folder that's been read as Add media… does when nothing in its
    /// sheet is changed: one show, or each of its parts as a show, with the
    /// AniList entries its name finds. Nobody checks these, so an anime whose
    /// names aren't much like the folder's isn't taken for it: the folder is
    /// added under its own name, better that than as the wrong show.
    ///
    /// Only what isn't in Telly is added: a show (or part) with a video that
    /// `new` says is new, and no episode of a show in Telly. So a folder some
    /// of whose seasons are in Telly gets the ones that turned up since; with
    /// `base`, they're matched as parts of that anime, the one the seasons in
    /// Telly are. A part with an episode `passed` over (a show you removed, or
    /// left out when it was added) stays out, its new episodes with it.
    /// Returns the shows added, or why AniList couldn't be asked.
    pub(crate) fn add_as_read(self: &Arc<Self>, scan: &scan::Scan, base: Option<i64>, new: impl Fn(&str) -> bool, passed: impl Fn(&str) -> bool) -> Result<Vec<i64>> {
        let found = self.matched(scan.clone(), base, true)?;
        if let Some(error) = &found.error {
            return Err(anyhow!("{error}"));
        }
        let mut shows = found.shows();
        shows.retain(|(show, _)| to_add(&show.episodes, &new, &passed));
        if shows.is_empty() {
            return Ok(Vec::new());
        }
        self.add_parts(shows, found.anime.map(|a| a.id), Some(scan), true)
    }

    /// The lengths of videos as ffprobe reads them, each read once while
    /// Telly is open and it stays the size and age it was. A show's folder is
    /// looked in every 10 minutes, and what isn't an episode there (an extra,
    /// a second copy, a file you left out) would be read again each time. One
    /// that can't be read yet (still being copied there) is read again.
    pub(super) fn lengths_once(&self, ffprobe: Option<&Path>, paths: &[PathBuf]) -> Vec<Option<f64>> {
        self.lengths_once_read(paths, |unread| scan::probe_durations(ffprobe, unread))
    }

    /// `lengths_once`, with `read` reading the lengths of those not kept.
    pub(super) fn lengths_once_read(&self, paths: &[PathBuf], read: impl FnOnce(&[PathBuf]) -> Vec<Option<f64>>) -> Vec<Option<f64>> {
        let stamp = |p: &PathBuf| std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().ok()));
        let stamps: Vec<_> = paths.iter().map(stamp).collect();
        let mut lengths: Vec<Option<f64>> = {
            let kept = self.looked_at.lock().unwrap_or_else(|e| e.into_inner());
            paths.iter().zip(&stamps).map(|(p, stamp)| kept.get(p).filter(|(size, age, _)| Some((*size, *age)) == *stamp).map(|k| k.2)).collect()
        };
        let unread: Vec<usize> = (0..paths.len()).filter(|i| lengths[*i].is_none()).collect();
        let read = if unread.is_empty() { Vec::new() } else { read(&unread.iter().map(|i| paths[*i].clone()).collect::<Vec<_>>()) };
        let mut kept = self.looked_at.lock().unwrap_or_else(|e| e.into_inner());
        for (i, length) in unread.into_iter().zip(read) {
            if let (Some(length), Some((size, age))) = (length, stamps[i]) {
                kept.insert(paths[i].clone(), (size, age, length));
            }
            lengths[i] = length;
        }
        lengths
    }

    /// Looks in a show's folder again for episodes that weren't there, or
    /// couldn't be read (still downloading), when it was added. Returns how
    /// many it added. An episode's rip moved within the folder is found where
    /// it is now, keeping everything (relink.rs).
    pub fn rescan(&self, id: i64) -> Result<usize> {
        let _one_look_at_a_time = self.rescanning.lock().unwrap_or_else(|e| e.into_inner());
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let existing = self.db.episodes(id)?;
        // Its folder holds other shows' rips too (one of several movies, a loose OVA beside season folders, half a
        // season folder): nothing new there is its own, so the folder is read only to find a rip of its own that moved.
        let shared = self.db.setting_bool(&shared_setting(id), false);
        if shared && existing.iter().all(|e| Path::new(&e.path).is_file()) {
            return Ok(0);
        }
        let known: std::collections::HashMap<String, f64> = existing.iter().map(|e| (e.path.clone(), e.duration)).collect();
        let taken: std::collections::HashSet<i64> = existing.iter().map(|e| e.number).collect();
        let tools = self.tools();
        // A file still downloading is left for the next look.
        let scan = scan::scan(Path::new(&show.folder), |paths| self.downloads.unless_downloading(paths, |paths| {
            // Only files Telly doesn't know yet need ffprobe, and none where the folder holds other shows' rips:
            // nothing new there is this show's, and a rip of its own that moved is found by its name and size.
            let unknown: Vec<PathBuf> = paths.iter().filter(|p| !shared && !known.contains_key(p.to_string_lossy().as_ref())).cloned().collect();
            let probed = self.lengths_once(tools.ffprobe.as_deref(), &unknown);
            let mut by_path: std::collections::HashMap<PathBuf, Option<f64>> = unknown.into_iter().zip(probed).collect();
            paths
                .iter()
                .map(|p| match known.get(p.to_string_lossy().as_ref()) {
                    // Known episodes count as readable whatever their length.
                    Some(d) => Some(if *d > 0.0 { *d } else { 600.0 }),
                    None => by_path.remove(p).flatten(),
                })
                .collect()
        }))?;
        let mut next_number = existing.iter().map(|e| e.number).max().unwrap_or(0) + 1;
        // Numbered as their names say: the folder as a whole is, or the show's
        // episodes are (a second release of one makes the folder's numbers
        // repeat, but its own number is still in its name).
        let parsed: std::collections::HashMap<&str, Option<i64>> = scan.files.iter().map(|f| (f.path.as_str(), f.parsed)).collect();
        let by_names = scan.numbering == "names" || {
            // (One without a number in its name, a recap say, says nothing either way.)
            let mut named = existing.iter().filter_map(|e| parsed.get(e.path.as_str()).copied().flatten().map(|p| p == e.number)).peekable();
            named.peek().is_some() && named.all(|by_name| by_name)
        };
        // Episodes whose rip isn't there any more. A file of its name and size
        // elsewhere in the folder is that rip, moved; else a new file of the
        // same number replaces it.
        let gone_episodes: Vec<Episode> = existing.iter().filter(|e| !Path::new(&e.path).is_file()).cloned().collect();
        let unknown: Vec<(String, i64)> = scan.files.iter().filter(|f| !known.contains_key(&f.path)).map(|f| (f.path.clone(), f.size)).collect();
        let moved = relink::find(&show.folder, &show.folder, &gone_episodes, &unknown);
        if !moved.is_empty() {
            relink::relink_episodes(self, &[], &moved.iter().collect::<Vec<_>>())?;
        }
        let moved_to: std::collections::HashSet<&str> = moved.iter().map(|m| m.to.as_str()).collect();
        let mut gone: std::collections::HashMap<i64, i64> =
            gone_episodes.iter().filter(|e| !moved.iter().any(|m| m.id == e.id)).map(|e| (e.number, e.id)).collect();
        // Not this show's: another show's episodes, what's in another show's folder
        // inside this one (a season of its own), and what you left out when adding it.
        let others: Vec<Show> = self.db.shows()?.into_iter().filter(|s| s.id != id && s.folder != show.folder).collect();
        let theirs: std::collections::HashSet<String> = self.db.all_episodes()?.into_iter().filter(|e| e.show_id != id).map(|e| e.path).collect();
        let left_out: std::collections::HashSet<String> =
            self.db.setting(&left_out_setting(id))?.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        let not_its_own = |path: &str| {
            theirs.contains(path)
                || in_folder(&show.folder, path).is_some_and(|p| left_out.contains(&p))
                || others.iter().any(|s| Path::new(&s.folder).starts_with(&show.folder) && Path::new(path).starts_with(&s.folder))
        };
        // A film whose rip is gone, and one new video in its folder: that's its rip now, whatever it's called.
        // A film's name has no episode number to tell by, and it may run past two hours.
        let film = existing.len() == 1 && show.anilist_episodes.unwrap_or(1) == 1 && is_movie(&show.format, show.anilist_id.is_some(), &[existing[0].duration]);
        if film && !shared && gone.len() == 1 {
            let videos: Vec<&scan::ScannedFile> = scan
                .files
                .iter()
                .filter(|f| (f.reason.is_empty() || f.reason.starts_with("Over 2 hours")) && !known.contains_key(&f.path) && !not_its_own(&f.path))
                .collect();
            if let [rip] = videos.as_slice() {
                self.replace_rip(id, existing[0].id, rip)?;
                self.jobs.poke();
                return Ok(1);
            }
        }
        // A film is its one rip: another video in its folder (the anime's next film, put beside it) is no
        // episode of it, and its library's look adds that as a part of the anime.
        if film {
            return Ok(0);
        }
        let news: Vec<&scan::ScannedFile> = scan
            .files
            .iter()
            .filter(|f| !shared && f.reason.is_empty() && !known.contains_key(&f.path) && !moved_to.contains(f.path.as_str()) && !not_its_own(&f.path))
            .collect();
        // A folder in the show's own that none of its episodes are in, with two or more files numbered as
        // episodes it has: another season, whose numbers start again (as parts.rs tells seasons apart when a
        // folder is added). None of it is this show's, the files numbered past its last episode neither.
        let top = |path: &str| in_folder(&show.folder, path).and_then(|rel| rel.split_once('/').map(|(top, _)| top.to_string())).unwrap_or_default();
        let its_folders: std::collections::HashSet<String> = existing.iter().map(|e| top(&e.path)).collect();
        let there: std::collections::HashSet<i64> = existing.iter().filter(|e| !gone.contains_key(&e.number)).map(|e| e.number).collect();
        let mut again: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for f in news.iter().filter(|f| by_names && f.parsed.is_some_and(|n| there.contains(&n))) {
            *again.entry(top(&f.path)).or_default() += 1;
        }
        let another_season = |path: &str| !its_folders.contains(&top(path)) && again.get(&top(path)).is_some_and(|&count| count >= 2);
        let mut new: Vec<crate::db::NewEpisode> = Vec::new();
        let mut unnumbered = Vec::new();
        let mut replaced = 0;
        for f in news.into_iter().filter(|f| !another_season(&f.path)) {
            match f.parsed {
                Some(n) if by_names => {
                    if let Some(episode) = gone.remove(&n) {
                        self.replace_rip(id, episode, f)?;
                        replaced += 1;
                        continue;
                    }
                    if taken.contains(&n) || new.iter().any(|e| e.number == n) {
                        continue; // another copy of an episode Telly has
                    }
                    new.push(crate::db::NewEpisode { number: n, path: f.path.clone(), size: f.size, duration: f.duration });
                }
                _ => unnumbered.push(f),
            }
        }
        // Files without a number of their own come after, on numbers no other episode has.
        for f in unnumbered {
            while taken.contains(&next_number) || new.iter().any(|e| e.number == next_number) {
                next_number += 1;
            }
            new.push(crate::db::NewEpisode { number: next_number, path: f.path.clone(), size: f.size, duration: f.duration });
        }
        if !new.is_empty() {
            self.db.add_episodes(id, &new)?;
        }
        if replaced + new.len() > 0 {
            self.jobs.poke();
        }
        Ok(new.len() + replaced)
    }

    /// Puts a new rip in place of an episode's that's gone. What Telly made
    /// from the old one (its copy, seek-bar pictures, the files in subtitles)
    /// goes, so the new one is read and synced from the start.
    pub(super) fn replace_rip(&self, show_id: i64, episode_id: i64, file: &scan::ScannedFile) -> Result<()> {
        self.db.replace_rip(episode_id, &file.path, file.size, file.duration)?;
        let _ = std::fs::remove_file(jobs::copy_path(self, show_id, episode_id));
        let _ = std::fs::remove_file(jobs::sheet_path(self, show_id, episode_id));
        self.forget_episode_files(&[episode_id]);
        let _ = self.lines.forget(episode_id);
        Ok(())
    }

    /// Looks for new episodes in every show's folder, then for new shows in
    /// your libraries' folders (library.rs), now and every 10 minutes.
    pub(super) fn watch_folders(self: &Arc<Self>) {
        let backend = self.clone();
        std::thread::Builder::new()
            .name("telly-folders".into())
            .spawn(move || loop {
                for show in backend.db.shows().unwrap_or_default() {
                    if Path::new(&show.folder).is_dir() {
                        let _ = backend.rescan(show.id);
                    }
                }
                crate::library::look(&backend);
                std::thread::sleep(Duration::from_secs(600));
            })
            .ok();
    }

    pub(super) fn new_show_from(anime: Option<&Anime>, mut base: NewShow) -> NewShow {
        if let Some(a) = anime {
            base.title = a.short_title();
            base.title_native = a.native.clone();
            base.title_english = a.english.clone();
            base.title_romaji = a.romaji.clone();
            base.anilist_id = Some(a.id);
            base.anilist_episodes = a.episodes;
            base.format = a.format.clone();
            base.year = a.year;
            base.cover_url = a.cover.clone();
            base.cover_color = a.color.clone();
            // A film is the one episode of its entry, whatever number its name has ("Jujutsu Kaisen 0").
            if a.format == "MOVIE" && a.episodes == Some(1) && base.episodes.len() == 1 {
                base.episodes[0].number = 1;
            }
        }
        base
    }

    pub(super) fn add_show(self: &Arc<Self>, args: &Value) -> Result<Value> {
        let mut new: NewShow = serde_json::from_value(args.clone()).context("the show to add")?;
        if new.episodes.is_empty() {
            return Err(anyhow!("There are no episodes to add"));
        }
        let anime = match new.anilist_id {
            Some(id) => Some(anilist::by_id(id)?),
            None => None,
        };
        if new.title.trim().is_empty() {
            new.title = scan::clean_title(Path::new(&new.folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().as_str());
        }
        let new = Self::new_show_from(anime.as_ref(), new);
        let id = self.db.add_show(&new)?;
        let scan = self.scanned.lock().ok().and_then(|s| s.clone());
        self.keep_left_out(id, &new, scan.as_ref());
        self.sheet_done(scan.as_ref(), &new.episodes.iter().map(|e| e.path.as_str()).collect::<Vec<_>>());
        self.fetch_cover(id, new.cover_url.clone());
        series::gather(self);
        recommend::gather(self);
        self.jobs.poke();
        Ok(json!({ "id": id }))
    }

    /// Notes the files Add media listed in a new show's folder that you left
    /// out, so Look for new episodes doesn't add them after all.
    pub(super) fn keep_left_out(&self, id: i64, show: &NewShow, scan: Option<&scan::Scan>) {
        let Some(scan) = scan else { return };
        let chosen: std::collections::HashSet<&str> = show.episodes.iter().map(|e| e.path.as_str()).collect();
        let left: Vec<String> = scan.files.iter().filter(|f| f.reason.is_empty() && !chosen.contains(f.path.as_str())).map(|f| f.path.clone()).collect();
        self.leave_out(id, &show.folder, &left);
    }

    /// Adds `paths` (those in `folder`, the show's) to what Look for new
    /// episodes leaves out of show `id`.
    pub(super) fn leave_out(&self, id: i64, folder: &str, paths: &[String]) {
        let mut left: Vec<String> = self.db.setting(&left_out_setting(id)).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        for p in paths.iter().filter_map(|p| in_folder(folder, p)) {
            if !left.contains(&p) {
                left.push(p);
            }
        }
        if !left.is_empty() {
            let _ = self.db.set_setting(&left_out_setting(id), &json!(left));
        }
    }

    /// A new show's download as the show to add: `anime`, with the episodes
    /// Add media would give its folder. None for a folder with no episode
    /// finished yet, and for one that's several shows (parts.rs), which Add
    /// media's sheet is for.
    pub(super) fn show_in_download(&self, folder: &str, anime: &Anime) -> Result<Option<NewShow>> {
        let tools = self.tools();
        let scan = scan::scan(Path::new(folder), |paths| {
            self.downloads.unless_downloading(paths, |paths| scan::probe_durations(tools.ffprobe.as_deref(), paths))
        })?;
        let episodes: Vec<crate::db::NewEpisode> = scan
            .files
            .iter()
            .filter_map(|f| f.number.map(|number| crate::db::NewEpisode { number, path: f.path.clone(), size: f.size, duration: f.duration }))
            .collect();
        if episodes.is_empty() || parts::split(&scan).len() > 1 {
            return Ok(None);
        }
        Ok(Some(Self::new_show_from(Some(anime), NewShow { folder: folder.to_string(), episodes, ..Default::default() })))
    }

    /// Adds a new show's download as the AniList entry you said it was when
    /// it started (torrents.rs). Returns whether it was added.
    pub fn add_download(self: &Arc<Self>, folder: &str, anilist_id: i64) -> Result<bool> {
        // Kept from when the download's place was worked out.
        let anime = match series::graph(self).entry(anilist_id) {
            Some(entry) => entry.anime.clone(),
            None => anilist::by_id(anilist_id)?,
        };
        let Some(new) = self.show_in_download(folder, &anime)? else { return Ok(false) };
        // Its rips are a show's already (a library's look came by first): it's in Telly.
        let _one_at_a_time = self.rescanning.lock().unwrap_or_else(|e| e.into_inner());
        let in_telly: std::collections::HashSet<String> = self.db.all_episodes()?.iter().map(|e| crate::library::key(&e.path)).collect();
        if new.episodes.iter().any(|e| in_telly.contains(&crate::library::key(&e.path))) {
            return Ok(true);
        }
        let id = self.db.add_show(&new)?;
        self.fetch_cover(id, new.cover_url.clone());
        series::gather(self);
        recommend::gather(self);
        self.jobs.poke();
        Ok(true)
    }

    /// Adds a folder's parts, each as a show of its own: an anime's seasons,
    /// movies and OVAs, from one sheet. Nothing is added unless AniList
    /// answers for every entry. A part with no entry is listed under `anime`
    /// (the anime's key) with the rest.
    pub(super) fn add_shows(self: &Arc<Self>, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Part {
            #[serde(flatten)]
            show: NewShow,
            /// Its folder holds other parts' rips too.
            #[serde(default)]
            shared: bool,
        }
        let parts: Vec<Part> = serde_json::from_value(args.get("shows").cloned().unwrap_or_default()).context("the shows to add")?;
        let anime: Option<i64> = arg(args, "anime")?;
        if parts.is_empty() || parts.iter().any(|p| p.show.episodes.is_empty()) {
            return Err(anyhow!("There are no episodes to add"));
        }
        let scan = self.scanned.lock().ok().and_then(|s| s.clone());
        let rips: Vec<String> = parts.iter().flat_map(|p| p.show.episodes.iter().map(|e| e.path.clone())).collect();
        // The first-time setup adds from folders that are libraries by then: what a look there added meanwhile isn't added twice.
        let new_only = arg::<Option<bool>>(args, "newOnly")?.unwrap_or(false);
        let added = self.add_parts(parts.into_iter().map(|p| (p.show, p.shared)).collect(), anime, scan.as_ref(), new_only)?;
        self.sheet_done(scan.as_ref(), &rips.iter().map(String::as_str).collect::<Vec<_>>());
        Ok(json!({ "ids": added }))
    }

    /// Adds each of `parts` as a show, with whether its folder holds other
    /// parts' rips too. `scan` is the folder as it was read: what's in a
    /// part's folder and isn't its own is left out of later looks for new
    /// episodes. With `new_only` (nobody chose these: a library's look did),
    /// a part with a rip that's a show's episode by now isn't added.
    pub(crate) fn add_parts(self: &Arc<Self>, parts: Vec<(NewShow, bool)>, anime: Option<i64>, scan: Option<&scan::Scan>, new_only: bool) -> Result<Vec<i64>> {
        let ids: Vec<i64> = parts.iter().filter_map(|(show, _)| show.anilist_id).collect();
        let entries = anilist::by_ids(&ids)?;
        if let Some(missing) = ids.iter().find(|id| !entries.iter().any(|a| a.id == **id)) {
            return Err(anyhow!("AniList has no anime {missing}"));
        }
        // Held from the look at what's in Telly to the adding, so a look for new episodes and this don't both take a rip.
        let _one_at_a_time = new_only.then(|| self.rescanning.lock().unwrap_or_else(|e| e.into_inner()));
        let in_telly: std::collections::HashSet<String> = if new_only { self.db.all_episodes()?.iter().map(|e| crate::library::key(&e.path)).collect() } else { Default::default() };
        let mut added = Vec::new();
        for (mut show, shared) in parts {
            if show.episodes.iter().any(|e| in_telly.contains(&crate::library::key(&e.path))) {
                continue;
            }
            if show.title.trim().is_empty() {
                show.title = scan::clean_title(Path::new(&show.folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().as_str());
            }
            let entry = show.anilist_id.and_then(|id| entries.iter().find(|a| a.id == id));
            let new = Self::new_show_from(entry, show);
            let id = self.db.add_show(&new)?;
            self.keep_left_out(id, &new, scan);
            if shared {
                self.db.set_setting(&shared_setting(id), &json!(true))?;
            }
            if let (None, Some(key)) = (new.anilist_id, anime) {
                series::move_show(self, id, Some(key))?;
            }
            self.fetch_cover(id, new.cover_url.clone());
            added.push(id);
        }
        if added.is_empty() {
            return Ok(added);
        }
        series::gather(self);
        recommend::gather(self);
        self.jobs.poke();
        Ok(added)
    }

    pub(super) fn rematch(self: &Arc<Self>, id: i64, anilist_id: i64) -> Result<Value> {
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let anime = anilist::by_id(anilist_id)?;
        let new = Self::new_show_from(Some(&anime), NewShow { folder: show.folder.clone(), ..Default::default() });
        self.db.set_show_anilist(id, &new)?;
        self.remove_cover(id);
        self.fetch_cover(id, new.cover_url.clone());
        series::gather(self);
        recommend::gather(self);
        // Subtitles were found, or found missing and queued to be made, under the old show's name.
        self.db.requeue_subs(Some(id), &["none", "failed", "check", "synced", "nokey", "whisper"])?;
        // And those being worked on, from the old match.
        self.jobs.find_again(&self.db, Some(id));
        self.jobs.forget_jimaku();
        self.jobs.poke();
        self.show_view(id)
    }

    /// Change match… to a series or film on TMDB, for a show AniList doesn't know.
    pub(super) fn rematch_tmdb(self: &Arc<Self>, id: i64, tmdb_id: &str) -> Result<Value> {
        self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let title = tmdb::by_id(tmdb_id)?;
        self.db.set_show_tmdb(id, &title)?;
        // Chosen by hand: it isn't looked up again.
        self.db.set_setting(&format!("tmdb.asked.{id}"), &Value::Bool(true))?;
        self.remove_cover(id);
        self.fetch_cover(id, title.cover.clone());
        series::gather(self);
        recommend::gather(self);
        self.db.requeue_subs(Some(id), &["none", "failed", "check", "synced", "nokey", "whisper"])?;
        self.jobs.find_again(&self.db, Some(id));
        self.jobs.forget_jimaku();
        self.jobs.poke();
        self.show_view(id)
    }
}

/// Whether a show (or part) read in a library's folder is added: it has an
/// episode that's new, and none that's passed over, as a show you removed has,
/// or one you left out when its folder was added: that stays out, new
/// episodes and all.
fn to_add(episodes: &[crate::db::NewEpisode], new: impl Fn(&str) -> bool, passed: impl Fn(&str) -> bool) -> bool {
    episodes.iter().any(|e| new(&e.path)) && !episodes.iter().any(|e| passed(&e.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_show_you_removed_stays_out_when_a_new_episode_turns_up_in_its_folder() {
        let episodes: Vec<crate::db::NewEpisode> =
            (1..=13).map(|n| crate::db::NewEpisode { number: n, path: format!("Series/Show/Show - {n:02}.mkv"), size: 1, duration: 1400.0 }).collect();
        let new = |path: &str| path.ends_with("13.mkv");
        // Episodes 1 to 12 were the show you removed: passed over, and 13 with them.
        assert!(!to_add(&episodes, new, |path: &str| !path.ends_with("13.mkv")));
        // A season that's new from start to end is added.
        assert!(to_add(&episodes, |_: &str| true, |_: &str| false));
        // Nothing new: nothing to add.
        assert!(!to_add(&episodes, |_: &str| false, |_: &str| false));
    }
}
