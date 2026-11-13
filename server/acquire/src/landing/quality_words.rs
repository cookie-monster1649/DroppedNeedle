//! Fork: a file's quality and the quality settings, in plain words.
//!
//! What a person reads when files are held because the quality settings
//! do not list them: the file's real format, bit depth, sample rate or
//! bitrate, whether that is above or below what the settings allow, and
//! the setting that would let files like these in.

use droppedneedle_kernel::runtime_config::sections::{
    QualityRecipeEntry, RecipeFormat, RecipeQuality,
};

/// Where the quality settings live.
pub const QUALITY_SETTINGS: &str = "Settings → Download Client → Acquisition quality";

/// A sample rate in kHz, as people write it: "44.1 kHz", "192 kHz".
fn khz(rate: u32) -> String {
    let khz = f64::from(rate) / 1000.0;
    if (khz - khz.round()).abs() < 0.05 {
        format!("{} kHz", khz.round() as u32)
    } else {
        format!("{khz:.1} kHz")
    }
}

/// One file's quality as a person reads it: "FLAC 24-bit/192 kHz",
/// "MP3 128 kbps", "OGG". Only what is known is said.
pub fn describe(
    format: &str,
    bitrate_kbps: Option<u32>,
    bit_depth: Option<u8>,
    sample_rate: Option<u32>,
) -> String {
    let name = format.trim().to_uppercase();
    let lossless = ["FLAC", "WAV", "APE", "WV", "AIFF", "ALAC"].contains(&name.as_str())
        || (bit_depth.is_some() && ["M4A", "MP4"].contains(&name.as_str()));
    let detail = if lossless {
        match (bit_depth, sample_rate) {
            (Some(depth), Some(rate)) => Some(format!("{depth}-bit/{}", khz(rate))),
            (Some(depth), None) => Some(format!("{depth}-bit")),
            (None, Some(rate)) => Some(khz(rate)),
            (None, None) => None,
        }
    } else {
        bitrate_kbps.map(|rate| format!("{rate} kbps"))
    };
    match detail {
        Some(detail) => format!("{name} {detail}"),
        None => name,
    }
}

/// The narrowest FLAC setting that admits a depth and rate, as the
/// settings page names it.
fn flac_setting(depth: u8, rate: u32) -> &'static str {
    match (depth, rate) {
        (..=16, ..=48_000) => "CD quality FLAC",
        (..=24, ..=48_000) => "24-bit / 48 kHz FLAC",
        (..=24, ..=96_000) => "24-bit / 96 kHz FLAC",
        (..=24, ..=192_000) => "24-bit / 192 kHz FLAC",
        _ => "\"Any resolution\" FLAC",
    }
}

/// The highest depth and rate any FLAC entry admits, when one does.
fn flac_ceiling(recipe: &[QualityRecipeEntry]) -> Option<(i64, i64)> {
    recipe
        .iter()
        .filter(|entry| entry.format == RecipeFormat::Flac)
        .filter_map(|entry| match entry.quality {
            RecipeQuality::Cd => Some((16, 48_000)),
            RecipeQuality::Bit24Khz48 => Some((24, 48_000)),
            RecipeQuality::Bit24Khz96 => Some((24, 96_000)),
            RecipeQuality::Bit24Khz192 => Some((24, 192_000)),
            RecipeQuality::HiRes => Some((i64::MAX, i64::MAX)),
            RecipeQuality::Custom => Some((entry.bit_depth?, entry.sample_rate_hz?)),
            _ => None,
        })
        .max()
}

/// Why files of this quality are held for the quality settings, and the
/// setting that would take them, as one or two plain sentences.
pub fn not_listed_sentence(
    recipe: &[QualityRecipeEntry],
    format: &str,
    bitrate_kbps: Option<u32>,
    bit_depth: Option<u8>,
    sample_rate: Option<u32>,
) -> String {
    let quality = describe(format, bitrate_kbps, bit_depth, sample_rate);
    let is = |wanted: RecipeFormat| recipe.iter().any(|entry| entry.format == wanted);
    if format.eq_ignore_ascii_case("flac") {
        if !is(RecipeFormat::Flac) {
            return format!(
                "The files are {quality}, and your download settings don't include FLAC at all. \
                 Add {} in {QUALITY_SETTINGS} to import files like these automatically.",
                flac_setting(bit_depth.unwrap_or(16), sample_rate.unwrap_or(44_100))
            );
        }
        let depth = i64::from(bit_depth.unwrap_or(16));
        let rate = i64::from(sample_rate.unwrap_or(44_100));
        let higher = flac_ceiling(recipe)
            .is_some_and(|(most_depth, most_rate)| depth > most_depth || rate > most_rate);
        let setting = flac_setting(bit_depth.unwrap_or(16), sample_rate.unwrap_or(44_100));
        return if higher {
            format!(
                "The files are a higher quality format ({quality}) than your download settings allow. \
                 Add {setting} in {QUALITY_SETTINGS} to import these automatically."
            )
        } else {
            format!(
                "The files are {quality}, a FLAC quality your download settings don't list. \
                 Add {setting} in {QUALITY_SETTINGS} to import these automatically."
            )
        };
    }
    if format.eq_ignore_ascii_case("mp3") {
        let mins: Vec<i64> = recipe
            .iter()
            .filter(|entry| entry.format == RecipeFormat::Mp3)
            .map(|entry| entry.min_bitrate_kbps.unwrap_or(16))
            .collect();
        let Some(rate) = bitrate_kbps.map(i64::from) else {
            return format!(
                "The files are MP3, and your download settings don't list MP3 at this bitrate. \
                 Add an MP3 entry in {QUALITY_SETTINGS} to import these automatically."
            );
        };
        if mins.is_empty() {
            return format!(
                "The files are {quality}, and your download settings don't include MP3 at all. \
                 Add MP3 in {QUALITY_SETTINGS} if you want files like these imported automatically."
            );
        }
        if mins.iter().all(|min| rate < *min) {
            let lowest = mins.iter().min().copied().unwrap_or(16);
            return format!(
                "The files are a lower quality ({quality}) than your download settings allow \
                 (MP3 from {lowest} kbps). Import them by hand if you want them anyway, or add a \
                 lower MP3 bitrate in {QUALITY_SETTINGS}. DroppedNeedle keeps looking for a better copy."
            );
        }
        return format!(
            "The files are {quality}, a bitrate your download settings skip. \
             Add an MP3 entry covering {rate} kbps in {QUALITY_SETTINGS} to import these automatically."
        );
    }
    format!(
        "The files are {quality}. Only FLAC and MP3 can be imported automatically, so files in this \
         format always wait for you. Import them by hand if you want them; DroppedNeedle keeps looking \
         for a FLAC or MP3 copy."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flac(quality: RecipeQuality) -> QualityRecipeEntry {
        QualityRecipeEntry {
            format: RecipeFormat::Flac,
            quality,
            min_bitrate_kbps: None,
            max_bitrate_kbps: None,
            bit_depth: None,
            sample_rate_hz: None,
        }
    }

    fn mp3(min: i64) -> QualityRecipeEntry {
        QualityRecipeEntry {
            format: RecipeFormat::Mp3,
            quality: RecipeQuality::Custom,
            min_bitrate_kbps: Some(min),
            ..flac(RecipeQuality::Custom)
        }
    }

    #[test]
    fn qualities_read_as_people_write_them() {
        assert_eq!(
            describe("flac", None, Some(24), Some(192_000)),
            "FLAC 24-bit/192 kHz"
        );
        assert_eq!(
            describe("flac", None, Some(16), Some(44_100)),
            "FLAC 16-bit/44.1 kHz"
        );
        assert_eq!(describe("mp3", Some(128), None, None), "MP3 128 kbps");
        assert_eq!(describe("ogg", None, None, None), "OGG");
        assert_eq!(
            describe("m4a", None, Some(24), Some(96_000)),
            "M4A 24-bit/96 kHz"
        );
    }

    /// The case from the field: a CD-only recipe and a 24/192 download.
    #[test]
    fn hi_res_above_a_cd_recipe_names_the_setting() {
        let recipe = vec![flac(RecipeQuality::Cd), mp3(320)];
        let text = not_listed_sentence(&recipe, "flac", None, Some(24), Some(192_000));
        assert_eq!(
            text,
            format!(
                "The files are a higher quality format (FLAC 24-bit/192 kHz) than your download \
                 settings allow. Add 24-bit / 192 kHz FLAC in {QUALITY_SETTINGS} to import these \
                 automatically."
            )
        );
    }

    #[test]
    fn other_cases_say_what_to_do() {
        let recipe = vec![flac(RecipeQuality::Cd), mp3(320)];
        let low = not_listed_sentence(&recipe, "mp3", Some(128), None, None);
        assert!(low.contains("lower quality (MP3 128 kbps)"), "{low}");
        assert!(low.contains("from 320 kbps"), "{low}");
        let ogg = not_listed_sentence(&recipe, "ogg", Some(320), None, None);
        assert!(
            ogg.starts_with("The files are OGG 320 kbps. Only FLAC and MP3"),
            "{ogg}"
        );
        let no_flac = not_listed_sentence(&[mp3(320)], "flac", None, Some(16), Some(44_100));
        assert!(no_flac.contains("don't include FLAC at all"), "{no_flac}");
        assert!(no_flac.contains("CD quality FLAC"), "{no_flac}");
    }
}
