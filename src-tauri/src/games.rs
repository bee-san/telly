//! Whether a game is running, so getting shows ready (jobs.rs) starts no new
//! episode's work until it closes: converting video, syncing subtitles and
//! making the seek bar's pictures use the processor and the drive the game
//! wants. A game is a program in a folder the game stores install into
//! (Steam's, Epic's, GOG's, Riot's, the Xbox app's) or World of Warcraft's,
//! or one named in Settings. The launchers that sit in the background in
//! those folders don't count, or the work would never run.

use crate::backend::Backend;
use std::sync::Arc;
use std::time::Duration;

/// Settings: whether work waits while a game is running (on unless turned off).
pub const WAIT: &str = "games.wait";
/// Settings: more games, by their program's name, separated by commas.
pub const NAMES: &str = "games.names";

/// How often the running programs are looked at.
const LOOK_EVERY: Duration = Duration::from_secs(10);

/// The folders games are installed in, as they appear in a program's path.
const GAME_FOLDERS: [&str; 6] = [r"\steamapps\common\", r"\epic games\", r"\gog games\", r"\riot games\", r"\xboxgames\", r"\world of warcraft\"];

/// The folders among those that hold launchers and tools that stay running,
/// not games: Epic Games Launcher and the Epic Online Services it installs
/// beside it, Riot Client, and Wallpaper Engine.
const NOT_GAME_FOLDERS: [&str; 4] = [r"\epic games\launcher\", r"\epic games\epic online services\", r"\riot games\riot client\", r"\steamapps\common\wallpaper_engine\"];

/// Games known by their program's name, wherever they're installed (see `program`).
const GAME_NAMES: [&str; 2] = ["wow", "wowclassic"];

/// A program's name as names are compared: its file's name, in any case, without ".exe".
fn program(path: &str) -> String {
    let file = path.trim().rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
    file.strip_suffix(".exe").map(str::to_string).unwrap_or(file)
}

/// Whether a program is a game: one in a folder games are installed in that
/// isn't a launcher's, or one known by its name, `extra` (see `extra_names`)
/// included. `name` is its program's name and `exe` its path, empty when
/// Windows won't say.
pub fn is_game(name: &str, exe: &str, extra: &[String]) -> bool {
    let path = exe.replace('/', "\\").to_lowercase();
    let within = |folders: &[&str]| folders.iter().any(|folder| path.contains(folder));
    if within(&GAME_FOLDERS) && !within(&NOT_GAME_FOLDERS) {
        return true;
    }
    [program(name), program(exe)].iter().any(|n| !n.is_empty() && (GAME_NAMES.contains(&n.as_str()) || extra.contains(n)))
}

/// The games Settings adds (`NAMES`, from `text`), as `program` names them.
fn extra_names(text: &str) -> Vec<String> {
    text.split([',', '\n']).map(program).filter(|n| !n.is_empty()).collect()
}

/// The program's name of a game running now (see `is_game`), if one is. Only
/// the list of processes is refreshed, and each one's path read once, when it's new.
fn game_running(system: &mut sysinfo::System, extra: &[String]) -> Option<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet));
    system
        .processes()
        .values()
        .filter(|p| is_game(&p.name().to_string_lossy(), &p.exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default(), extra))
        .map(|p| p.name().to_string_lossy().into_owned())
        .min()
}

/// Looks for a game every 10 seconds while Settings has work wait for one,
/// and keeps what it finds for the workers (`running`).
pub fn watch(backend: Arc<Backend>) {
    let watching = move || {
        let mut system = sysinfo::System::new().ok();
        loop {
            let found = match system.as_mut() {
                Some(system) if backend.db.setting_bool(WAIT, true) => game_running(system, &extra_names(&backend.db.setting_str(NAMES))),
                _ => None,
            };
            *backend.jobs.game.lock().unwrap_or_else(|e| e.into_inner()) = found;
            std::thread::sleep(LOOK_EVERY);
        }
    };
    std::thread::Builder::new().name("telly-games".into()).spawn(watching).ok();
}

/// The game running now, by its program's name, unless Settings has work go on regardless.
pub fn running(backend: &Backend) -> Option<String> {
    if !backend.db.setting_bool(WAIT, true) {
        return None;
    }
    backend.jobs.game.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(exe: &str) -> bool {
        let name = exe.rsplit('\\').next().unwrap();
        is_game(name, exe, &[])
    }

    #[test]
    fn a_game_is_a_program_in_a_game_folder() {
        assert!(game(r"C:\Program Files (x86)\World of Warcraft\_retail_\Wow.exe"));
        assert!(game(r"C:\Program Files (x86)\World of Warcraft\_classic_era_\WowClassic.exe"));
        assert!(game(r"C:\Program Files (x86)\Steam\steamapps\common\ELDEN RING\Game\eldenring.exe"));
        assert!(game(r"D:\SteamLibrary\steamapps\common\Baldurs Gate 3\bin\bg3_dx11.exe"));
        assert!(game(r"C:\Program Files\Epic Games\Fortnite\FortniteGame\Binaries\Win64\FortniteClient-Win64-Shipping.exe"));
        assert!(game(r"C:\GOG Games\The Witcher 3 Wild Hunt\bin\x64\witcher3.exe"));
        assert!(game(r"C:\Riot Games\VALORANT\live\ShooterGame\Binaries\Win64\VALORANT-Win64-Shipping.exe"));
        assert!(game(r"C:\Riot Games\League of Legends\Game\League of Legends.exe"));
        assert!(game(r"E:\XboxGames\Starfield\Content\Starfield.exe"));
        // In any case, with either slash.
        assert!(game(r"c:\program files (x86)\STEAM\SteamApps\Common\Hades\x64\Hades.exe"));
        assert!(is_game("hades", "/home/you/.local/share/Steam/steamapps/common/Hades/Hades", &[]));
    }

    #[test]
    fn the_launchers_and_tools_that_stay_running_are_not_games() {
        assert!(!game(r"C:\Program Files (x86)\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe"));
        assert!(!game(r"C:\Program Files (x86)\Epic Games\Launcher\Engine\Binaries\Win64\EpicWebHelper.exe"));
        assert!(!game(r"C:\Program Files (x86)\Epic Games\Epic Online Services\service\EpicOnlineServices.exe"));
        assert!(!game(r"C:\Riot Games\Riot Client\RiotClientServices.exe"));
        assert!(!game(r"C:\Riot Games\Riot Client\UX\RiotClientUx.exe"));
        assert!(!game(r"C:\Program Files (x86)\Steam\steamapps\common\wallpaper_engine\wallpaper64.exe"));
        assert!(!game(r"C:\Program Files (x86)\Steam\steamapps\common\wallpaper_engine\bin\webwallpaper32.exe"));
        // Steam, GOG Galaxy and Battle.net themselves are outside the games' folders.
        assert!(!game(r"C:\Program Files (x86)\Steam\steam.exe"));
        assert!(!game(r"C:\Program Files (x86)\Steam\bin\cef\cef.win7x64\steamwebhelper.exe"));
        assert!(!game(r"C:\Program Files (x86)\GOG Galaxy\GalaxyClient.exe"));
        assert!(!game(r"C:\Program Files (x86)\Battle.net\Battle.net.exe"));
        assert!(!game(r"C:\Windows\explorer.exe"));
    }

    #[test]
    fn a_game_is_known_by_its_name_where_its_folder_cant_be_read() {
        assert!(is_game("Wow.exe", "", &[]));
        assert!(is_game("WowClassic.exe", "", &[]));
        assert!(!is_game("WowUp.exe", "", &[]));
        assert!(!is_game("", "", &[]));
        // Names added in Settings, with or without ".exe", in any case.
        let extra = extra_names(" FFXIV_DX11.exe, eldenring ,,");
        assert_eq!(extra, ["ffxiv_dx11", "eldenring"]);
        assert!(is_game("ffxiv_dx11.exe", r"C:\Program Files (x86)\SquareEnix\FINAL FANTASY XIV - A Realm Reborn\game\ffxiv_dx11.exe", &extra));
        assert!(is_game("", r"C:\Games\ELDEN RING\Game\eldenring.exe", &extra));
        assert!(!is_game("ffxiv_dx11.exe", "", &[]));
        // A launcher named in Settings counts too.
        assert!(is_game("RiotClientServices.exe", r"C:\Riot Games\Riot Client\RiotClientServices.exe", &["riotclientservices".into()]));
    }
}
