//! The player's side of the backend: what an episode's page of the player is
//! given, the sitting each time one plays (what Tidepool's log counts), and
//! what each second's tick from the player keeps: where you are, what you've
//! watched, and when an episode is finished.

use super::*;

impl Backend {
    /// The episode playing (or paused) in the player, if it's open.
    pub fn now_playing(&self) -> Option<NowPlaying> {
        let p = self.playing.lock().ok()?;
        let p = p.as_ref()?;
        (p.at.elapsed() < Duration::from_secs(90)).then_some(NowPlaying { episode_id: p.episode_id, show_id: p.show_id, position: p.position })
    }

    pub fn playing_show(&self) -> Option<i64> {
        self.now_playing().map(|p| p.show_id)
    }

    /// How long since anything played.
    pub fn idle_for(&self) -> Option<Duration> {
        self.last_play.lock().ok().and_then(|t| t.map(|t| t.elapsed()))
    }

    pub(super) fn episode_view(&self, id: i64) -> Result<Value> {
        let episode = self.db.episode(id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
        let show = self.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let episodes = self.db.episodes(show.id)?;
        let i = episodes.iter().position(|e| e.id == id).unwrap_or(0);
        let lite = |e: &Episode| json!({ "id": e.id, "number": e.number, "ready": e.media_state == "ready", "mediaState": e.media_state, "mediaProgress": e.media_progress });
        // After a show's last episode comes the first of the show that carries on from it: the next season you have.
        let sequel = || {
            let next = series::next_show(self, &show)?;
            let its = self.db.episodes(next.id).ok()?;
            let mut first = lite(its.first()?);
            first["show"] = json!(next.title);
            first["movie"] = json!(is_movie(&next.format, next.anilist_id.is_some(), &its.iter().map(|e| e.duration).collect::<Vec<_>>()));
            Some(first)
        };
        let lines: Vec<Line> = jobs::read_lines(self, id);
        // From what ffprobe found in the rip: its chapters, its opening (else AniSkip's) and its audio tracks.
        let probe = jobs::stored_probe(self, id).unwrap_or_default();
        let audio: Vec<String> = probe
            .audio
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let language = match jobs::language_name(&a.language).trim() {
                    "" if a.language.is_empty() || a.language == "und" => format!("Track {}", i + 1),
                    "" => a.language.clone(),
                    name => name.to_string(),
                };
                if a.title.is_empty() || a.title.eq_ignore_ascii_case(&language) { language } else { format!("{language} · {}", a.title) }
            })
            .collect();
        Ok(json!({
            "show": show,
            "chapters": probe.chapters,
            "opening": media::opening(&probe).or(self.db.opening(id)?).map(|(start, end)| json!({ "start": start, "end": end })),
            "ending": media::ending(&probe).or(self.db.ending(id)?).map(|(start, end)| json!({ "start": start, "end": end })),
            "audio": audio,
            "audioTrack": jobs::playing_track(&episode, &probe),
            "listedAudio": jobs::listed_track(&episode, &probe),
            "src": format!("/ep/{id}?v={}-{}", episode.media_plan, episode.audio_track.unwrap_or(-1)),
            "episode": episode,
            "lines": lines,
            "prev": if i > 0 { Some(lite(&episodes[i - 1])) } else { None },
            "next": episodes.get(i + 1).map(lite).or_else(sequel),
            "count": episodes.len(),
            "movie": is_movie(&show.format, show.anilist_id.is_some(), &episodes.iter().map(|e| e.duration).collect::<Vec<_>>()),
            "tried": jobs::read_tried(self, id),
        }))
    }

    /// The seek bar's pictures of an episode, beside where Telly's copy goes.
    pub(super) fn sheet_path(&self, episode: &Episode) -> PathBuf {
        jobs::sheet_path(self, episode.show_id, episode.id)
    }

    /// The seek bar's pictures of an episode, made the first time it's asked for.
    pub(super) fn seek_sheet(&self, id: i64) -> Result<Value> {
        let episode = self.episode_or_err(id)?;
        let sheet = self.sheet_path(&episode);
        if !sheet.is_file() {
            let ffmpeg = self.tools().ffmpeg.ok_or_else(|| anyhow!("Telly needs ffmpeg for this. Choose it in Settings."))?;
            std::fs::create_dir_all(sheet.parent().expect("a folder"))?;
            media::make_sheet(&ffmpeg, Path::new(&episode.play_path), episode.duration, &sheet)?;
        }
        Ok(json!({
            "src": format!("/sheet/{id}"),
            "every": media::SHEET_EVERY,
            "columns": media::SHEET_COLUMNS,
            "rows": media::sheet_rows(episode.duration),
        }))
    }

    pub(super) fn open_player(&self, id: i64) -> Result<Value> {
        let episode = self.db.episode(id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
        // How long since the show was last open, for "Previously on".
        let away = self.db.show(episode.show_id)?.and_then(|s| ask::away_ms(s.last_watched_at));
        let first_time = self.db.show(episode.show_id)?.is_some_and(|s| s.last_watched_at.is_none());
        self.db.touch_show(episode.show_id)?;
        // A show starting for the first time joins the character dictionary.
        if first_time {
            self.names.poke();
        }
        self.begin_sitting(&episode);
        if let Some(m) = self.mining.get() {
            m.watch(true);
        }
        // Not ready to play (one you watched, whose copy was let go): it's got ready now.
        if episode.media_state == "pending" {
            self.jobs.ask_for(id);
        }
        self.jobs.poke();
        // Explain answers in a second or two from a Claude Code that's already started.
        if crate::claude::find().is_some() {
            crate::claude::warm_up(ask::model(self).as_deref(), Some(ask::EFFORT));
        }
        let mut view = self.episode_view(id)?;
        view["awayMs"] = json!(away);
        // Picking up part-way through: a recap that runs up to there.
        if ask::resuming(&episode, episode.position) {
            view["resume"] = ask::recap_state(self, &episode, Some(episode.position));
        }
        // The player uses the recap of the episode before only when there's no recap up to here.
        if view["resume"]["possible"] != true {
            view["recap"] = ask::recap_state(self, &episode, None);
        }
        Ok(view)
    }

    /// Plays the rip's audio track `track` in this episode from now on, and in
    /// the show's later episodes as they're got ready again. The Japanese
    /// track stays the default: picking it forgets the choice.
    pub(super) fn set_audio(&self, id: i64, track: usize) -> Result<Value> {
        let episode = self.episode_or_err(id)?;
        let probe = jobs::stored_probe(self, id).ok_or_else(|| anyhow!("Telly hasn’t read this episode’s audio tracks yet"))?;
        if track >= probe.audio.len() {
            return Err(anyhow!("This episode has no audio track {}", track + 1));
        }
        let ffmpeg = self.tools().ffmpeg.ok_or_else(|| anyhow!("Telly needs ffmpeg for this. Choose it in Settings."))?;
        let chosen = if track == media::japanese_audio(&probe) { Value::Null } else { json!(track) };
        self.db.set_setting(&jobs::audio_setting(episode.show_id), &chosen)?;
        if jobs::playing_track(&episode, &probe) != track {
            jobs::change_audio(self, &ffmpeg, &episode, &probe, track)?;
        }
        // Later episodes that are ready with another track follow: rips that play
        // it as they are at once, Telly's copies when they're got ready again.
        for later in self.db.episodes(episode.show_id)?.into_iter().filter(|e| e.number > episode.number && e.media_state == "ready") {
            if let Some(p) = jobs::stored_probe(self, later.id) {
                jobs::follow_audio(self, &ffmpeg, &later, &p)?;
            }
        }
        self.jobs.poke();
        self.episode_view(id)
    }

    /// The player opened `episode`: a sitting begins, and the last one ends.
    pub(super) fn begin_sitting(&self, episode: &Episode) {
        if let Ok(mut p) = self.playing.lock() {
            self.end_sitting(p.take());
            let now = Instant::now();
            let starting_midway = episode.watched_at.is_none()
                && episode.position > 5.0
                && (episode.duration <= 0.0 || episode.position < episode.duration * 0.95);
            if starting_midway {
                // Let the seek use the drive before any reader from the previous sitting.
                if let Ok(mut at) = self.ahead.at.lock() {
                    *at = (PathBuf::new(), 0, 0, at.3 + 1);
                }
            }
            *p = Some(Playing {
                episode_id: episode.id,
                show_id: episode.show_id,
                position: episode.position,
                playing: false,
                at: now,
                played_at: now,
                saved_at: now,
                session: None,
                starting_midway,
                from: None,
                reached_end: false,
            });
        }
    }

    pub(super) fn end_sitting(&self, p: Option<Playing>) {
        if let Some(p) = p {
            let _ = self.db.save_position(p.episode_id, p.position);
        }
    }

    pub(super) fn close_player(self: &Arc<Self>, id: i64, position: Option<f64>) -> Result<Value> {
        if let Some(position) = position {
            self.db.save_position(id, position)?;
        }
        if let Ok(mut p) = self.playing.lock() {
            if p.as_ref().is_some_and(|p| p.episode_id == id) {
                let mut taken = p.take();
                if let (Some(t), Some(pos)) = (taken.as_mut(), position) {
                    t.position = pos;
                }
                self.end_sitting(taken);
            }
        }
        if let Some(m) = self.mining.get() {
            m.watch(false);
        }
        self.jobs.poke();
        // Write now, so it's ready when you come back, the recap of what plays
        // next (`jobs::next_episode`): the next episode's after this one is watched
        // or near its end, else this one's up to here (in its first 5 minutes, the
        // one before it). Not both: one would go unused.
        if let Ok(e) = self.episode_or_err(id) {
            // Time away from the show is from now, when you left it, not from when it opened.
            let _ = self.db.touch_show(e.show_id);
            let seen = position.unwrap_or(e.position);
            let finished = e.watched_at.is_some() || (e.duration > 0.0 && seen >= self.watched_from(&e));
            ask::prepare_recaps(self, id, finished, (!finished && ask::resuming(&e, seen)).then_some(seen));
        }
        // Explain's Claude Code is kept only while the player is open: it goes a
        // minute after the player closes, unless another episode has opened.
        let backend = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(60));
            if backend.now_playing().is_none() && backend.playing.lock().is_ok_and(|p| p.is_none()) {
                crate::claude::end_warm();
            }
        });
        Ok(json!({}))
    }

    /// Where an episode counts as watched: where its ending starts (its
    /// chapters', else AniSkip's, as the player has it), else 90% of the way in.
    pub(super) fn watched_from(&self, episode: &Episode) -> f64 {
        let duration = episode.duration.max(0.0);
        let ending = episode.ending_at.or_else(|| self.db.ending(episode.id).ok().flatten().map(|(start, _)| start));
        ending.filter(|e| *e > duration * 0.5 && *e < duration).unwrap_or(duration * 0.9)
    }

    pub(super) fn tick(&self, a: TickArgs) -> Result<Value> {
        let episode = self.db.episode(a.id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
        let show = self.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let now = Instant::now();
        let mut finished = false;
        let duration = if episode.duration > 0.0 { episode.duration } else { 0.0 };
        let watched_from = self.watched_from(&episode);
        let at_end = duration > 0.0 && a.position >= watched_from;
        {
            let mut guard = self.playing.lock().map_err(|_| anyhow!("busy"))?;
            if guard.as_ref().is_none_or(|p| p.episode_id != a.id) {
                self.end_sitting(guard.take());
                *guard = Some(Playing {
                    episode_id: a.id,
                    show_id: show.id,
                    position: a.position,
                    playing: false,
                    at: now,
                    played_at: now,
                    saved_at: now,
                    session: None,
                    starting_midway: false,
                    from: None,
                    reached_end: false,
                });
            }
            let p = guard.as_mut().expect("just set");
            let gap = now.duration_since(p.at);
            let seconds = if a.playing && p.playing { gap.as_secs_f64().min(3.0) } else { 0.0 };
            // A new sitting after a long pause (or the computer asleep), so the log's times are real.
            let paused = now.duration_since(p.played_at).max(gap);
            if (a.playing || !a.lines.is_empty()) && (p.session.is_none() || paused > Duration::from_secs(600)) {
                let sitting = Sitting {
                    show_id: show.id,
                    title: &show.title,
                    native: &show.title_native,
                    anilist_id: show.anilist_id,
                    episode: episode.number,
                    episode_id: episode.id,
                    subs_file: &episode.subs_file,
                    subs_detail: &episode.subs_detail,
                };
                p.session = self.log.start(&sitting, now_ms(), a.subtitles).ok();
            }
            if let Some(session) = p.session {
                let lines: Vec<crate::log::Seen> = a
                    .lines
                    .iter()
                    .map(|l| crate::log::Seen {
                        at: l.at.unwrap_or_else(now_ms),
                        chars: l.chars,
                        episode_id: episode.id,
                        line: l.line,
                        start: l.start,
                        end: l.end,
                        nudge_ms: episode.subs_nudge_ms,
                        position: l.position,
                        rate: a.rate,
                        text: l.text.clone(),
                        speaker: l.speaker.clone(),
                        ruby: if l.ruby.is_null() { "[]".into() } else { l.ruby.to_string() },
                        subtitles: a.subtitles,
                    })
                    .collect();
                if seconds > 0.0 || !lines.is_empty() {
                    let _ = self.log.extend(session, now_ms(), seconds, &lines, a.subtitles);
                }
            }
            if a.playing && p.from.is_none() {
                p.from = Some(a.position);
            }
            p.position = a.position;
            // Played up to here (it was playing, or it is): how far you've got in the episode.
            let played_to = a.playing || p.playing;
            p.playing = a.playing;
            p.at = now;
            if a.playing {
                p.played_at = now;
            }
            if a.playing {
                if let Ok(mut l) = self.last_play.lock() {
                    *l = Some(now);
                }
            }
            if now.duration_since(p.saved_at) >= Duration::from_secs(5) || !a.playing {
                self.db.save_position(a.id, a.position)?;
                if played_to {
                    self.db.reach(a.id, a.position)?;
                }
                p.saved_at = now;
            }
            if at_end && (!p.reached_end || episode.watched_at.is_none()) {
                let played = !p.reached_end && p.from.is_some_and(|from| from < watched_from);
                finished = self.db.finish_episode(a.id, played)?;
                p.reached_end = true;
            }
        }
        if finished {
            self.jobs.poke();
        }
        Ok(json!({ "events": self.events_since(a.since), "watched": at_end || episode.watched_at.is_some() }))
    }
}
