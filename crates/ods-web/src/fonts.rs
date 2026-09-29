//! IBM Plex Sans (400, 500, 600) and Mono (400, 500), Latin-1 subsets, vendored as
//! released in the `@ibm/plex-sans` 1.1.0 and `@ibm/plex-mono` 2.5.0 packages
//! (SIL Open Font License 1.1, `assets/vendor/fonts/LICENSE-IBM-Plex-OFL-1.1.txt`).
//!
//! The dashboard serves them itself, so it needs no font CDN: it works offline and under
//! a CSP that only adds `font-src 'self'` (ADR-0009, amended).

use std::fmt::Write as _;

/// A vendored font: its file name and bytes.
pub(crate) struct Font {
    pub(crate) file: &'static str,
    pub(crate) bytes: &'static [u8],
    family: &'static str,
    weight: u16,
}

macro_rules! font {
    ($file:literal, $family:literal, $weight:literal) => {
        Font {
            file: $file,
            bytes: include_bytes!(concat!("../assets/vendor/fonts/", $file)),
            family: $family,
            weight: $weight,
        }
    };
}

pub(crate) const FONTS: [Font; 5] = [
    font!("IBMPlexSans-Regular-Latin1.woff2", "IBM Plex Sans", 400),
    font!("IBMPlexSans-Medium-Latin1.woff2", "IBM Plex Sans", 500),
    font!("IBMPlexSans-SemiBold-Latin1.woff2", "IBM Plex Sans", 600),
    font!("IBMPlexMono-Regular-Latin1.woff2", "IBM Plex Mono", 400),
    font!("IBMPlexMono-Medium-Latin1.woff2", "IBM Plex Mono", 500),
];

/// The characters the Latin-1 subsets cover, as IBM's own stylesheets declare them;
/// anything else (✓, ▾) falls back to the system fonts.
const LATIN1: &str = "U+0000, U+000D, U+0020-007E, U+00A0-00A3, U+00A4-00FF, U+0131, \
    U+0152-0153, U+02C6, U+02DA, U+02DC, U+2013-2014, U+2018-201A, U+201C-201E, \
    U+2020-2022, U+2026, U+2030, U+2039-203A, U+2044, U+2074, U+20AC, U+2122, U+2212, \
    U+FB01-FB02";

/// `@font-face` rules for the fonts, fetched from `assets/fonts/` next to the page. An
/// installed copy is used first.
pub(crate) fn font_faces() -> String {
    FONTS.iter().fold(String::new(), |mut css, f| {
        let _ = writeln!(
            css,
            "@font-face {{ font-family: '{family}'; font-style: normal; font-weight: {weight}; \
             font-display: swap; src: local('{family}'), url('assets/fonts/{file}') format('woff2'); \
             unicode-range: {LATIN1}; }}",
            family = f.family,
            weight = f.weight,
            file = f.file,
        );
        css
    })
}

/// The font called `file`, if it is one of ours.
pub(crate) fn font(file: &str) -> Option<&'static Font> {
    FONTS.iter().find(|f| f.file == file)
}
