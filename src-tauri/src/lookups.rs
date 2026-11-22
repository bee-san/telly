//! Looking words up on the phone, where Hachidori can't run (Android's WebView
//! loads no extensions): Hoshidicts, the engine inside Hachidori, built into
//! Telly for Android (build.rs), with your dictionaries as the PC has them
//! (phone.rs gets them).
//!
//! Each dictionary is installed once, into a folder of its own on the phone,
//! and its file from the PC is deleted. Lookups read those folders from the
//! disk, so a dictionary of any size costs the phone no memory to keep. One
//! that closes Telly twice while it's being installed, or that isn't a
//! dictionary, is left out until the PC has a newer one.

use crate::backend::Backend;
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Setting (phone): each dictionary installed, {revision, folder}, by its file's name on the PC.
const INSTALLED: &str = "phone.lookups";
/// Setting (phone): the dictionary being installed, {name, revision, times}.
const TRYING: &str = "phone.lookups.trying";
/// Setting (phone): each dictionary left out, by the revision that couldn't be installed.
const FAILED: &str = "phone.lookups.failed";
/// How often a dictionary may close Telly while it's being installed before it's left out.
const TRIES: i64 = 2;

/// What the phone's lookups keep while Telly is open.
#[derive(Default)]
pub struct Lookups {
    /// Your dictionaries, open; None until they're first wanted.
    engine: Mutex<Option<engine::Engine>>,
    /// How many dictionaries are open, the one being installed, and what went wrong last.
    progress: Mutex<Progress>,
    /// Held by the one install running, and by a sync while it changes the files to install.
    installing: Mutex<()>,
}

#[derive(Default)]
struct Progress {
    count: usize,
    installing: String,
    failed: String,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the phone keeps your dictionaries installed, each in a numbered folder.
fn dir(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("lookups")
}

fn map(backend: &Backend, key: &str) -> Map<String, Value> {
    backend.db.setting(key).ok().flatten().and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

/// Holds off installing while a sync changes the files the PC sent, and waits for an install that's running.
pub fn hold(backend: &Backend) -> std::sync::MutexGuard<'_, ()> {
    lock(&backend.lookups.installing)
}

/// Installs the dictionaries the phone got that it hasn't installed, in the
/// background: as Telly opens, and after each sync.
pub fn start(backend: Arc<Backend>) {
    std::thread::spawn(move || {
        let _one = hold(&backend);
        if let Err(e) = install(&backend) {
            lock(&backend.lookups.progress).failed = format!("{e:#}");
        }
        lock(&backend.lookups.progress).installing.clear();
    });
}

/// The numbered folder a dictionary's `folder` ("3/JMdict") is in.
fn slot(folder: &str) -> &str {
    folder.split('/').next().unwrap_or_default()
}

fn install(backend: &Backend) -> Result<()> {
    let dir = dir(backend);
    std::fs::create_dir_all(&dir)?;
    let wanted = crate::phone::dictionaries(backend);
    let mut installed = map(backend, INSTALLED);
    let mut failed = map(backend, FAILED);
    let mut said = String::new();

    // One that was being installed when Telly closed is tried again, then left out.
    let trying = backend.db.setting(TRYING)?.unwrap_or(Value::Null);
    let tried = |name: &str, revision: &str| if trying["name"] == name && trying["revision"] == revision { trying["times"].as_i64().unwrap_or(0) } else { 0 };
    if let (Some(name), Some(revision)) = (trying["name"].as_str(), trying["revision"].as_str()) {
        if tried(name, revision) >= TRIES {
            failed.insert(name.to_string(), json!(revision));
            said = format!("{name} closed Telly each time it was being installed, so it’s left out.");
        }
    }

    // Those the PC no longer has are let go, with any folder a stopped install left.
    let had = installed.len();
    installed.retain(|name, _| wanted.iter().any(|(n, _)| n == name));
    failed.retain(|name, revision| wanted.iter().any(|(n, r)| n == name && revision == r.as_str()));
    let mut changed = installed.len() != had;
    sweep(&dir, &installed);

    for (name, revision) in &wanted {
        let file = crate::phone::dictionaries_dir(backend).join(name);
        if installed.get(name).is_some_and(|d| d["revision"] == revision.as_str()) || failed.get(name).and_then(Value::as_str) == Some(revision) {
            let _ = std::fs::remove_file(&file);
            continue;
        }
        if !file.is_file() {
            // Its file went before it was installed: the next sync gets it again.
            crate::phone::forget_dictionary(backend, name)?;
            continue;
        }
        lock(&backend.lookups.progress).installing = name.clone();
        backend.db.set_setting(TRYING, &json!({ "name": name, "revision": revision, "times": tried(name, revision) + 1 }))?;
        let number = (1..).map(|n: u32| n.to_string()).find(|n| !dir.join(n).exists()).expect("a free folder");
        let made = engine::import(&file, &dir.join(&number)).and_then(|title| {
            let folder = format!("{number}/{title}");
            if engine::Engine::open(&[dir.join(&folder)]).1 == [true] {
                Ok(folder)
            } else {
                Err(anyhow::anyhow!("it has nothing a lookup can read"))
            }
        });
        match made {
            Ok(folder) => {
                installed.insert(name.clone(), json!({ "revision": revision, "folder": folder }));
                changed = true;
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(dir.join(&number));
                failed.insert(name.clone(), json!(revision));
                said = format!("{name} couldn’t be installed: {e:#}");
            }
        }
        backend.db.set_setting(INSTALLED, &Value::Object(installed.clone()))?;
        backend.db.set_setting(FAILED, &Value::Object(failed.clone()))?;
        backend.db.set_setting(TRYING, &Value::Null)?;
        let _ = std::fs::remove_file(&file);
    }
    backend.db.set_setting(INSTALLED, &Value::Object(installed))?;
    backend.db.set_setting(FAILED, &Value::Object(failed))?;
    backend.db.set_setting(TRYING, &Value::Null)?;
    lock(&backend.lookups.progress).failed = said;

    let mut engine = lock(&backend.lookups.engine);
    if changed || engine.is_none() {
        // The old ones are closed first: an older copy's folder may be about to go.
        *engine = None;
        *engine = Some(open(backend)?);
        sweep(&dir, &map(backend, INSTALLED));
    }
    Ok(())
}

/// Deletes every folder in `dir` that isn't an installed dictionary's: an older copy's, or a stopped install's.
fn sweep(dir: &Path, installed: &Map<String, Value>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !installed.values().any(|d| slot(d["folder"].as_str().unwrap_or_default()) == name) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Opens every dictionary installed, in the order lookups show them.
fn open(backend: &Backend) -> Result<engine::Engine> {
    let dir = dir(backend);
    let mut installed = map(backend, INSTALLED);
    let names: Vec<String> = crate::phone::dictionaries(backend).into_iter().map(|(name, _)| name).filter(|name| installed.contains_key(name)).collect();
    let folders: Vec<PathBuf> = names.iter().map(|name| dir.join(installed[name]["folder"].as_str().unwrap_or_default())).collect();
    let (engine, opened) = engine::Engine::open(&folders);
    // One this Telly can't read (an older Telly installed it) is got from the PC again at the next sync.
    for (name, _) in names.iter().zip(&opened).filter(|(_, opened)| !**opened) {
        installed.remove(name);
        crate::phone::forget_dictionary(backend, name)?;
    }
    backend.db.set_setting(INSTALLED, &Value::Object(installed))?;
    lock(&backend.lookups.progress).count = opened.iter().filter(|o| **o).count();
    Ok(engine)
}

/// The words `text` starts with, the longest first, with how your dictionaries'
/// installing is going: {found, count, installing, failed}.
pub fn look_up(backend: &Backend, text: &str, max: usize) -> Result<Value> {
    let found = if text.trim().is_empty() {
        Vec::new()
    } else {
        let mut engine = lock(&backend.lookups.engine);
        if engine.is_none() {
            *engine = Some(open(backend)?);
        }
        engine.as_ref().map(|e| e.look_up(text, max)).transpose()?.unwrap_or_default()
    };
    let progress = lock(&backend.lookups.progress);
    Ok(json!({ "found": found, "count": progress.count, "installing": progress.installing, "failed": progress.failed }))
}

/// Hoshidicts itself, through its C interface (vendor/hoshidicts/include/hoshidicts_c.h).
#[cfg(telly_lookups)]
mod engine {
    use anyhow::{anyhow, Result};
    use serde_json::{json, Value};
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::path::{Path, PathBuf};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Str {
        ptr: *const c_char,
        len: usize,
    }

    #[repr(C)]
    struct Glossary {
        dict_name: Str,
        glossary: Str,
        definition_tags: Str,
        term_tags: Str,
    }

    #[repr(C)]
    struct Term {
        expression: Str,
        reading: Str,
        rules: Str,
        score: f64,
        glossaries: *const Glossary,
        glossaries_count: usize,
        frequencies: *const c_void,
        frequencies_count: usize,
        pitches: *const c_void,
        pitches_count: usize,
    }

    #[repr(C)]
    struct Found {
        matched: Str,
        deinflected: Str,
        trace: *const c_void,
        trace_count: usize,
        term: Term,
        preprocessor_steps: i32,
    }

    unsafe extern "C" {
        fn hd_import(source_path: *const c_char, output_dir: *const c_char, low_ram: c_int) -> *mut c_void;
        fn hd_import_result_free(r: *mut c_void);
        fn hd_import_result_success(r: *const c_void) -> c_int;
        fn hd_import_result_title(r: *const c_void) -> *const c_char;
        fn hd_import_result_error(r: *const c_void) -> *const c_char;
        fn hd_query_new() -> *mut c_void;
        fn hd_query_free(q: *mut c_void);
        fn hd_query_add_term_dict(q: *mut c_void, path: *const c_char) -> c_int;
        fn hd_query_add_freq_dict(q: *mut c_void, path: *const c_char) -> c_int;
        fn hd_query_add_pitch_dict(q: *mut c_void, path: *const c_char) -> c_int;
        fn hd_query_add_kanji_dict(q: *mut c_void, path: *const c_char) -> c_int;
        fn hd_query_max_long_key_length(q: *const c_void) -> usize;
        fn hd_languages_new() -> *mut c_void;
        fn hd_languages_free(languages: *mut c_void);
        fn hd_lookup_new_with_languages(q: *mut c_void, languages: *const c_void) -> *mut c_void;
        fn hd_lookup_free(l: *mut c_void);
        fn hd_lookup_run(l: *const c_void, text: *const c_char, max_results: c_int, scan_length: usize, out: *mut *const Found, count: *mut usize) -> *mut c_void;
        fn hd_lookup_results_free(r: *mut c_void);
    }

    fn c_path(path: &Path) -> Result<CString> {
        CString::new(path.to_string_lossy().as_bytes()).map_err(|_| anyhow!("{} can't be a path here", path.display()))
    }

    fn text(s: Str) -> String {
        if s.ptr.is_null() || s.len == 0 {
            return String::new();
        }
        // SAFETY: Hoshidicts gives a string as its start and length, alive until its results are freed.
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(s.ptr.cast::<u8>(), s.len) }).into_owned()
    }

    /// Installs the dictionary in `file` under `into`, and says the folder it made there (the dictionary's title).
    pub fn import(file: &Path, into: &Path) -> Result<String> {
        std::fs::create_dir_all(into)?;
        let (file, into) = (c_path(file)?, c_path(into)?);
        // SAFETY: both paths are NUL-terminated and outlive the call; the result is freed once, below.
        unsafe {
            let result = hd_import(file.as_ptr(), into.as_ptr(), 1);
            if result.is_null() {
                return Err(anyhow!("the dictionary engine failed"));
            }
            let said = |s: *const c_char| CStr::from_ptr(s).to_string_lossy().into_owned();
            let made = if hd_import_result_success(result) != 0 { Ok(said(hd_import_result_title(result))) } else { Err(anyhow!("{}", said(hd_import_result_error(result)))) };
            hd_import_result_free(result);
            made
        }
    }

    /// Your dictionaries, open for lookups.
    pub struct Engine {
        query: *mut c_void,
        languages: *mut c_void,
        lookup: *mut c_void,
    }

    // SAFETY: the engine has no thread of its own and is only used behind Lookups' lock.
    unsafe impl Send for Engine {}

    impl Engine {
        /// Opens the dictionaries in `folders`, in that order, and says which of them opened.
        pub fn open(folders: &[PathBuf]) -> (Self, Vec<bool>) {
            // SAFETY: the query and the languages outlive the lookup made from them (see Drop), and each path is NUL-terminated.
            unsafe {
                let query = hd_query_new();
                let opened = folders
                    .iter()
                    .map(|folder| {
                        let Ok(path) = c_path(folder) else { return false };
                        // A dictionary holds one or more of these, and each that it holds is added.
                        [hd_query_add_term_dict, hd_query_add_freq_dict, hd_query_add_pitch_dict, hd_query_add_kanji_dict].iter().fold(false, |any, add| add(query, path.as_ptr()) == 0 || any)
                    })
                    .collect();
                let languages = hd_languages_new();
                let lookup = hd_lookup_new_with_languages(query, languages);
                (Self { query, languages, lookup }, opened)
            }
        }

        /// The words `text` starts with, the longest first.
        pub fn look_up(&self, text_: &str, max: usize) -> Result<Vec<Value>> {
            let asked = CString::new(text_.replace('\0', ""))?;
            let mut found: *const Found = std::ptr::null();
            let mut count = 0usize;
            // SAFETY: `found` points at `count` results that live until hd_lookup_results_free.
            unsafe {
                if self.lookup.is_null() {
                    return Err(anyhow!("the dictionary engine didn’t start"));
                }
                // As Hachidori asks: far enough to reach the longest word your dictionaries have.
                let scan = 16.max(hd_query_max_long_key_length(self.query) + 8);
                let results = hd_lookup_run(self.lookup, asked.as_ptr(), max as c_int, scan, &mut found, &mut count);
                if results.is_null() {
                    return Err(anyhow!("the dictionary engine failed"));
                }
                let list = (0..count)
                    .map(|i| {
                        let f = &*found.add(i);
                        let glossaries: Vec<Value> = (0..f.term.glossaries_count)
                            .map(|j| {
                                let g = &*f.term.glossaries.add(j);
                                json!({ "dictionary": text(g.dict_name), "glossary": text(g.glossary), "definitionTags": text(g.definition_tags), "termTags": text(g.term_tags) })
                            })
                            .collect();
                        json!({
                            "matched": text(f.matched),
                            "deinflected": text(f.deinflected),
                            "term": { "expression": text(f.term.expression), "reading": text(f.term.reading), "glossaries": glossaries },
                        })
                    })
                    .collect();
                hd_lookup_results_free(results);
                Ok(list)
            }
        }
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            // SAFETY: each was made by Hoshidicts in `open` and is freed once, the lookup before what it reads.
            unsafe {
                if !self.lookup.is_null() {
                    hd_lookup_free(self.lookup);
                }
                hd_languages_free(self.languages);
                hd_query_free(self.query);
            }
        }
    }
}

/// Telly on a PC has Hachidori, and is built without Hoshidicts.
#[cfg(not(telly_lookups))]
mod engine {
    use anyhow::{anyhow, Result};
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    pub fn import(_file: &Path, _into: &Path) -> Result<String> {
        Err(anyhow!("this Telly was built without its dictionary engine"))
    }

    pub struct Engine;

    impl Engine {
        pub fn open(folders: &[PathBuf]) -> (Self, Vec<bool>) {
            (Self, vec![false; folders.len()])
        }

        pub fn look_up(&self, _text: &str, _max: usize) -> Result<Vec<Value>> {
            Ok(Vec::new())
        }
    }
}

#[cfg(all(test, telly_lookups))]
mod tests {
    use super::*;
    use crate::backend::Dirs;
    use std::io::Write;

    /// A small Yomitan dictionary in `file`.
    fn dictionary(file: &Path, title: &str, terms: &str) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(file).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("index.json", options).unwrap();
        zip.write_all(format!(r#"{{"title":"{title}","format":3,"revision":"1"}}"#).as_bytes()).unwrap();
        zip.start_file("term_bank_1.json", options).unwrap();
        zip.write_all(terms.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    fn phone(name: &str) -> (Backend, PathBuf) {
        let dir = std::env::temp_dir().join(format!("telly-lookups-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        std::fs::create_dir_all(crate::phone::dictionaries_dir(&backend)).unwrap();
        (backend, dir)
    }

    const EAT: &str = r#"[["食べる","たべる","","v1",0,["to eat"],1,""]]"#;

    #[test]
    fn a_dictionary_is_installed_once_and_looked_up_from_its_folder() {
        let (backend, dir) = phone("install");
        let file = crate::phone::dictionaries_dir(&backend).join("Words.zip");
        dictionary(&file, "Words", EAT);
        backend.db.set_setting("phone.dictionaries", &json!({ "Words.zip": "r1" })).unwrap();
        install(&backend).unwrap();
        // Its file from the PC is gone, and the phone still has the dictionary.
        assert!(!file.exists());
        assert_eq!(crate::phone::dictionaries(&backend), [("Words.zip".to_string(), "r1".to_string())]);
        let said = look_up(&backend, "食べなかった", 6).unwrap();
        assert_eq!(said["count"], 1);
        assert_eq!(said["found"][0]["matched"], "食べなかった");
        assert_eq!(said["found"][0]["term"]["expression"], "食べる");
        assert_eq!(said["found"][0]["term"]["reading"], "たべる");
        assert_eq!(said["found"][0]["term"]["glossaries"][0]["dictionary"], "Words");
        assert!(said["found"][0]["term"]["glossaries"][0]["glossary"].as_str().unwrap().contains("to eat"));
        // Nothing is installed twice, and a Telly opened again reads the same folder.
        install(&backend).unwrap();
        *lock(&backend.lookups.engine) = None;
        assert_eq!(look_up(&backend, "食べる", 6).unwrap()["found"][0]["term"]["expression"], "食べる");
        assert_eq!(std::fs::read_dir(super::dir(&backend)).unwrap().count(), 1);
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_copy_takes_the_older_ones_place_and_one_the_pc_dropped_goes() {
        let (backend, dir) = phone("newer");
        let file = crate::phone::dictionaries_dir(&backend).join("Words.zip");
        dictionary(&file, "Words", EAT);
        backend.db.set_setting("phone.dictionaries", &json!({ "Words.zip": "r1" })).unwrap();
        install(&backend).unwrap();
        dictionary(&file, "Words", r#"[["飲む","のむ","","v5",0,["to drink"],1,""]]"#);
        backend.db.set_setting("phone.dictionaries", &json!({ "Words.zip": "r2" })).unwrap();
        install(&backend).unwrap();
        assert!(look_up(&backend, "食べる", 6).unwrap()["found"].as_array().unwrap().is_empty());
        assert_eq!(look_up(&backend, "飲んだ", 6).unwrap()["found"][0]["term"]["expression"], "飲む");
        assert_eq!(std::fs::read_dir(super::dir(&backend)).unwrap().count(), 1);
        backend.db.set_setting("phone.dictionaries", &json!({})).unwrap();
        install(&backend).unwrap();
        assert_eq!(look_up(&backend, "飲んだ", 6).unwrap()["count"], 0);
        assert_eq!(std::fs::read_dir(super::dir(&backend)).unwrap().count(), 0);
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_cant_be_installed_is_left_out_and_said() {
        let (backend, dir) = phone("left-out");
        let dictionaries = crate::phone::dictionaries_dir(&backend);
        std::fs::write(dictionaries.join("Not one.zip"), b"not a dictionary").unwrap();
        dictionary(&dictionaries.join("Closes.zip"), "Closes", EAT);
        dictionary(&dictionaries.join("Words.zip"), "Words", EAT);
        backend.db.set_setting("phone.dictionaries", &json!({ "Not one.zip": "r1", "Closes.zip": "r1", "Words.zip": "r1" })).unwrap();
        // Telly closed twice with this one being installed.
        backend.db.set_setting(TRYING, &json!({ "name": "Closes.zip", "revision": "r1", "times": 2 })).unwrap();
        install(&backend).unwrap();
        let said = look_up(&backend, "食べる", 6).unwrap();
        assert_eq!(said["count"], 1);
        assert_eq!(said["found"][0]["term"]["glossaries"].as_array().unwrap().len(), 1);
        assert!(said["failed"].as_str().unwrap().starts_with("Not one.zip couldn’t be installed"), "{said}");
        assert_eq!(map(&backend, FAILED).len(), 2);
        // Neither is tried again until the PC has a newer one.
        assert!(!dictionaries.join("Closes.zip").exists());
        dictionary(&dictionaries.join("Closes.zip"), "Closes", EAT);
        backend.db.set_setting("phone.dictionaries", &json!({ "Not one.zip": "r1", "Closes.zip": "r2", "Words.zip": "r1" })).unwrap();
        install(&backend).unwrap();
        assert_eq!(look_up(&backend, "食べる", 6).unwrap()["count"], 2);
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
