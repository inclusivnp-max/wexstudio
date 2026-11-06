//! Pure filename classification, adapted from WEXdesktop FileIconBadge colors.
//! No metadata/reads, project scanning, compiler work or global font fallback.
use crate::material_symbols as s;
use std::path::Path;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIcon {
    pub glyph: char,
    pub tint: [u8; 3],
    pub kind: &'static str,
}
const fn badge(glyph: char, tint: u32, kind: &'static str) -> FileIcon {
    FileIcon {
        glyph,
        tint: [(tint >> 16) as u8, (tint >> 8) as u8, tint as u8],
        kind,
    }
}
pub fn for_path(path: &Path, folder: bool, expanded: bool) -> FileIcon {
    if folder {
        return badge(
            if expanded { s::FOLDER_OPEN } else { s::FOLDER },
            0xd3bfff,
            "Folder",
        );
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "androidmanifest.xml" => {
            return badge(s::INTEGRATION_INSTRUCTIONS, 0x34d399, "Android manifest");
        }
        "wex.json" | "wex.lock.json" => return badge(s::TUNE, 0x5ec8e5, "WEX configuration"),
        "cmakelists.txt"
        | "makefile"
        | "dockerfile"
        | "build.gradle"
        | "build.gradle.kts"
        | "settings.gradle"
        | "settings.gradle.kts" => return badge(s::BUILD, 0x6bc5b1, "Build configuration"),
        _ => {}
    }
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "kt" | "kts" => badge(s::CODE, 0xe879f9, "Kotlin"),
        "java" => badge(s::CODE, 0xfb923c, "Java"),
        "class" => badge(s::COFFEE, 0xfb923c, "Java class"),
        "xml" => badge(s::CODE, 0x67e8f9, "XML resource"),
        "apk" | "apks" | "aab" => badge(s::ANDROID, 0x4ade80, "Android package"),
        "aar" => badge(s::WIDGETS, 0x9ccc65, "Android library"),
        "jar" | "zip" | "gz" | "tar" | "7z" | "rar" => badge(s::FOLDER_ZIP, 0xa78bfa, "Archive"),
        "dex" => badge(s::DATA_OBJECT, 0xc084fc, "DEX"),
        "arsc" => badge(s::STORAGE, 0xfcd34d, "Resource table"),
        "smali" => badge(s::INTEGRATION_INSTRUCTIONS, 0xf472b6, "Smali"),
        "rs" => badge(s::CODE, 0xfb923c, "Rust"),
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" => badge(s::CODE, 0x38bdf8, "C/C++"),
        "py" => badge(s::TERMINAL, 0x38bdf8, "Python"),
        "js" | "jsx" | "mjs" => badge(s::JAVASCRIPT, 0xfde047, "JavaScript"),
        "ts" | "tsx" => badge(s::CODE, 0x60a5fa, "TypeScript"),
        "css" => badge(s::CSS, 0x60a5fa, "CSS"),
        "html" | "htm" => badge(s::HTML, 0xfb7a45, "HTML"),
        "go" => badge(s::PETS, 0x00add8, "Go"),
        "rb" => badge(s::DIAMOND, 0xef4444, "Ruby"),
        "sql" => badge(s::SCHEMA, 0xeab308, "SQL"),
        "sh" | "bash" | "zsh" | "fish" => badge(s::TERMINAL, 0x86efac, "Shell script"),
        "bat" | "cmd" | "ps1" => badge(s::TERMINAL, 0x7dd3fc, "Shell script"),
        "json" | "yaml" | "yml" => badge(s::DATA_OBJECT, 0xa3e635, "Structured data"),
        "toml" | "properties" | "conf" | "ini" | "cfg" => badge(s::TUNE, 0x5ec8e5, "Configuration"),
        "pro" => badge(s::TUNE, 0xf59e0b, "ProGuard rules"),
        "md" | "markdown" => badge(s::ARTICLE, 0xff8b7d, "Markdown"),
        "txt" | "log" | "csv" => badge(s::TEXT_SNIPPET, 0xebe5f1, "Text"),
        "db" | "sqlite" | "sqlite3" => badge(s::DATABASE, 0x5eead4, "Database"),
        "png" | "webp" | "jpg" | "jpeg" | "gif" | "svg" | "ico" => {
            badge(s::IMAGE, 0x38bdf8, "Image")
        }
        "mp4" | "mkv" | "webm" | "mov" => badge(s::MOVIE, 0xf43f5e, "Video"),
        "mp3" | "wav" | "ogg" | "flac" => badge(s::AUDIOTRACK, 0xe879f9, "Audio"),
        "ttf" | "otf" | "woff" | "woff2" => badge(s::TEXT_FIELDS, 0xd8b4fe, "Font"),
        "jks" | "keystore" | "p12" => badge(s::KEY, 0xfbbf24, "Keystore"),
        "pem" | "crt" | "cer" => badge(s::VERIFIED_USER, 0xfcd34d, "Certificate"),
        "so" | "dll" | "dylib" => badge(s::MEMORY, 0xf87171, "Native library"),
        "bin" | "exe" => badge(s::DEVELOPER_BOARD, 0x9ca3af, "Binary"),
        "patch" | "diff" => badge(s::DIFFERENCE, 0x34d399, "Patch"),
        _ => badge(s::DESCRIPTION, 0xb4adc0, "File"),
    }
}
#[cfg(feature = "desktop")]
pub fn paint(
    ui: &eframe::egui::Ui,
    rect: eframe::egui::Rect,
    path: &Path,
    folder: bool,
    expanded: bool,
    enabled: bool,
) {
    let i = for_path(path, folder, expanded);
    let color = eframe::egui::Color32::from_rgb(i.tint[0], i.tint[1], i.tint[2]);
    s::paint(
        ui,
        rect,
        i.glyph,
        if enabled {
            color
        } else {
            color.gamma_multiply(0.45)
        },
        true,
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn android_and_source_types_keep_donor_glyphs_and_colors() {
        for (name, glyph, tint, kind) in [
            ("Main.kt", s::CODE, 0xe879f9, "Kotlin"),
            ("Main.JAVA", s::CODE, 0xfb923c, "Java"),
            ("screen.xml", s::CODE, 0x67e8f9, "XML resource"),
            ("demo.apk", s::ANDROID, 0x4ade80, "Android package"),
            ("lib.aar", s::WIDGETS, 0x9ccc65, "Android library"),
            ("a.dex", s::DATA_OBJECT, 0xc084fc, "DEX"),
        ] {
            assert_eq!(
                for_path(Path::new(name), false, false),
                badge(glyph, tint, kind)
            );
        }
    }
    #[test]
    fn semantic_names_win_over_generic_extension() {
        for (name, kind) in [
            ("AndroidManifest.XML", "Android manifest"),
            ("wex.json", "WEX configuration"),
            ("wex.lock.json", "WEX configuration"),
            ("build.gradle.kts", "Build configuration"),
            ("settings.gradle.kts", "Build configuration"),
            ("CMakeLists.txt", "Build configuration"),
            ("script.kts", "Kotlin"),
        ] {
            assert_eq!(for_path(Path::new(name), false, false).kind, kind);
        }
    }
    #[test]
    fn folders_ignore_extension_and_switch_to_actual_open_symbol() {
        assert_eq!(
            for_path(Path::new("some.apk"), true, false).glyph,
            s::FOLDER
        );
        assert_eq!(
            for_path(Path::new("some.apk"), true, true).glyph,
            s::FOLDER_OPEN
        );
    }
    #[test]
    fn hidden_unicode_unknown_and_extensionless_names_have_deterministic_fallback() {
        for name in [
            "/no/such/project/目录.unknown",
            ".gitignore",
            "LICENSE",
            "noextension",
        ] {
            assert_eq!(for_path(Path::new(name), false, false).kind, "File");
        }
        assert_eq!(
            for_path(Path::new(".config.JSON"), false, false).kind,
            "Structured data"
        );
        assert_eq!(
            for_path(Path::new("目录/😀.KT"), false, false).kind,
            "Kotlin"
        );
        assert_eq!(
            for_path(Path::new("archive.kt.zip"), false, false).kind,
            "Archive"
        );
    }
    #[test]
    fn non_utf8_filename_is_neutral_without_filesystem_access() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            for_path(
                Path::new(std::ffi::OsStr::from_bytes(b"bad\xff.kt")),
                false,
                false
            )
            .kind,
            "File"
        );
    }
}
