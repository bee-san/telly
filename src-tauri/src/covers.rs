//! Covers for what AniList doesn't know (live action, a folder of specials):
//! the poster from Wikipedia's article of that very name, else a frame from
//! the video. Wikipedia needs no key, and its posters are small (about 260
//! pixels across), which is the size a cover shows at.
//!
//! And the Library's small copy of every cover, made when the cover is got.

use crate::util::command;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const USER_AGENT: &str = concat!("Telly/", env!("CARGO_PKG_VERSION"), " (personal anime player)");

/// Whether Wikipedia's article `article`, with its one-line `description`, is
/// about the film or series called `title`: the same name, give or take
/// punctuation and a "(2016 film)" after the article's, and something to
/// watch. The name alone isn't enough: "Monster" is also a drink.
pub fn is_the_article(article: &str, description: &str, title: &str) -> bool {
    let plain = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let name = article.split(" (").next().unwrap_or(article);
    let description = description.to_lowercase();
    plain(name) == plain(title) && ["film", "series", "anime", "drama"].iter().any(|word| description.contains(word))
}

/// Where the poster in Wikipedia's article about `title` is, when it has one.
pub fn wikipedia_poster(title: &str) -> Result<Option<String>> {
    let q = percent_encoding::utf8_percent_encode(title.trim(), percent_encoding::NON_ALPHANUMERIC).to_string();
    // The first articles a search for the title finds, each with its lead picture
    // and one-line description. A poster is "non-free" there, hence pilicense=any.
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&format=json&generator=search&gsrsearch={q}&gsrlimit=5&prop=pageimages%7Cdescription&piprop=thumbnail&pithumbsize=600&pilicense=any"
    );
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
    let mut response = agent.get(&url).header("User-Agent", USER_AGENT).call().map_err(|e| anyhow!("Wikipedia isn’t reachable ({e})"))?;
    let json: Value = response.body_mut().read_json().context("Wikipedia sent something unexpected")?;
    let pages = json.pointer("/query/pages").and_then(Value::as_object);
    Ok(pages.into_iter().flat_map(|pages| pages.values()).find_map(|page| {
        let text = |key: &str| page.get(key).and_then(Value::as_str).unwrap_or("");
        let poster = page.pointer("/thumbnail/source").and_then(Value::as_str)?;
        is_the_article(text("title"), text("description"), title).then(|| poster.to_string())
    }))
}

/// ffmpeg's arguments for a cover from the picture of `video` at `at` seconds:
/// the most telling of the next hundred frames (not a fade to black), its
/// middle cut to a cover's shape, 600 pixels tall.
pub fn frame_args(video: &Path, at: f64, output: &Path) -> Vec<String> {
    [
        "-nostdin", "-hide_banner", "-v", "error", "-y", "-ss", &format!("{at:.1}"), "-i", &video.to_string_lossy(),
        "-map", "0:v:0", "-an", "-sn", "-vf", "thumbnail,crop=ih*135/190:ih,scale=-2:600", "-frames:v", "1", "-q:v", "4", "-f", "mjpeg", &output.to_string_lossy(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Makes a cover at `output` from a frame of `video` (`duration` seconds
/// long), taken at random from its middle four fifths: past the logos, before
/// the credits.
pub fn frame(ffmpeg: &Path, video: &Path, duration: f64, output: &Path) -> Result<()> {
    let random = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos() % 1000).unwrap_or(500) as f64 / 1000.0;
    let at = duration * (0.1 + 0.8 * random);
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = output.with_extension("jpg.part");
    let out = command(ffmpeg).args(frame_args(video, at, &partial)).stdin(Stdio::null()).output().context("starting ffmpeg")?;
    if !out.status.success() || !partial.is_file() {
        let _ = std::fs::remove_file(&partial);
        let text = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("ffmpeg couldn’t take a frame for the cover: {}", text.lines().last().unwrap_or("")));
    }
    std::fs::rename(&partial, output).context("saving the cover")
}

/// How wide the Library's copy of a cover is, in pixels: the widest a cover
/// is drawn there (about 160), at twice a screen's scale.
pub const SMALL_WIDTH: u32 = 320;

/// Makes the Library's copy of the cover at `cover`, at `small`: no wider than
/// SMALL_WIDTH, and a JPEG of ordinary quality. A cover as AniList sends it is
/// 460 across and up to 750 KB, which is slow to draw a hundred of.
pub fn make_small(cover: &Path, small: &Path) -> Result<()> {
    // A cover's name says .jpg whatever was sent: what it is, is in its first bytes.
    let image = image::ImageReader::open(cover)?.with_guessed_format()?.decode().context("reading the cover")?;
    let image = if image.width() > SMALL_WIDTH {
        let height = (image.height() as u64 * SMALL_WIDTH as u64 / image.width() as u64).max(1) as u32;
        image.resize_exact(SMALL_WIDTH, height, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let partial = small.with_extension("jpg.part");
    let file = std::io::BufWriter::new(std::fs::File::create(&partial)?);
    // A JPEG has no see-through parts.
    image.to_rgb8().write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(file, 85)).context("saving the small cover")?;
    std::fs::rename(&partial, small).context("saving the small cover")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_cover_is_no_wider_than_the_library_draws_it() {
        let dir = std::env::temp_dir().join(format!("telly-covers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // As AniList sends one: 460 across. And a PNG under a .jpg name, as a download is saved.
        let (cover, small) = (dir.join("1.jpg"), dir.join("1.small.jpg"));
        image::RgbaImage::from_pixel(460, 650, image::Rgba([200, 40, 40, 255])).save_with_format(&cover, image::ImageFormat::Png).unwrap();
        make_small(&cover, &small).unwrap();
        assert_eq!(image::image_dimensions(&small).unwrap(), (320, 452));
        assert_eq!(image::ImageReader::open(&small).unwrap().with_guessed_format().unwrap().format(), Some(image::ImageFormat::Jpeg));
        // A poster narrower than that stays its size.
        let (cover, small) = (dir.join("2.jpg"), dir.join("2.small.jpg"));
        image::RgbImage::from_pixel(230, 325, image::Rgb([40, 40, 200])).save_with_format(&cover, image::ImageFormat::Jpeg).unwrap();
        make_small(&cover, &small).unwrap();
        assert_eq!(image::image_dimensions(&small).unwrap(), (230, 325));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn takes_only_the_article_of_that_very_name() {
        assert!(is_the_article("Shin Godzilla", "2016 film by Hideaki Anno and Shinji Higuchi", "Shin Godzilla"));
        assert!(is_the_article("Call Me Chihiro", "2023 Japanese film", "call me chihiro"));
        // Wikipedia adds what it is when the name is shared; punctuation differs between a folder and an article.
        assert!(is_the_article("Liar Game (2007 TV series)", "Japanese television drama", "Liar Game"));
        assert!(is_the_article("Your Name", "2016 Japanese animated film", "Your Name."));
        // The character, the band and the cereal a search also finds.
        assert!(!is_the_article("Shin Godzilla (character)", "Fictional monster, or kaiju", "Shin Godzilla"));
        assert!(!is_the_article("The Specials", "British ska band from Coventry", "K-On! specials"));
        assert!(!is_the_article("Sword Art Online (TV series)", "Japanese anime television series", "Sword Art Online specials"));
    }

    #[test]
    fn cuts_a_frame_to_a_cover_s_shape() {
        let args = frame_args(Path::new("film.mkv"), 1234.56, Path::new("7.jpg.part")).join(" ");
        assert!(args.contains("-ss 1234.6 -i film.mkv") && args.contains("thumbnail,crop=ih*135/190:ih,scale=-2:600") && args.ends_with("-f mjpeg 7.jpg.part"), "{args}");
    }
}
