/// Returns the creator name encoded in a VAR `package_id` (the part before the
/// first `.`). Empty creators are reported as `None` so callers can distinguish
/// "no creator" from a stored empty string.
pub(crate) fn creator_from_package_id(package_id: &str) -> Option<&str> {
    package_id
        .split_once('.')
        .map(|(creator, _)| creator)
        .filter(|creator| !creator.is_empty())
}

/// True when `seg` is a VAR version segment: all-ASCII-digits, or `latest`
/// (case-insensitive). This is the predicate that was inlined, identically, in
/// `hub::has_version_suffix`, `hub::strip_version`, `tasks::dependency_package_base`
/// and `tasks::dependency_version_segment` before they were folded in here.
fn is_version_segment(seg: &str) -> bool {
    !seg.is_empty()
        && (seg.eq_ignore_ascii_case("latest") || seg.chars().all(|c| c.is_ascii_digit()))
}

/// The version-stripped package family: `Creator.Name.3` and
/// `Creator.Name.latest` both yield `Creator.Name`; an id with no version
/// segment is returned unchanged. Borrows, so it costs nothing to call.
pub(crate) fn package_base(package_id: &str) -> &str {
    match package_id.rsplit_once('.') {
        Some((base, last)) if is_version_segment(last) => base,
        _ => package_id,
    }
}

/// The raw trailing version segment (`"3"`, `"007"`, `"latest"`), or `None`
/// when the id carries no version.
pub(crate) fn package_version_segment(package_id: &str) -> Option<&str> {
    package_id
        .rsplit_once('.')
        .map(|(_, last)| last)
        .filter(|last| is_version_segment(last))
}

/// True when the id already carries a trailing version or `.latest`.
pub(crate) fn has_package_version(package_id: &str) -> bool {
    package_version_segment(package_id).is_some()
}

/// The trailing version as a number — the comparator this codebase never had.
///
/// `None` for `.latest`, for a missing version, and for anything over 19 digits
/// (`u64::MAX` has 20 digits, so every <=19-digit decimal is guaranteed to fit
/// and the parse can never overflow). Leading zeros parse: `"007"` -> `Some(7)`.
///
/// Callers rely on `None` meaning "not comparable, so never the newest": a
/// `.latest` file must never win a family, because removing every numbered
/// version is exactly what `.latest` would then fail to resolve to.
pub(crate) fn package_version(package_id: &str) -> Option<u64> {
    let seg = package_version_segment(package_id)?;
    if seg.len() > 19 || !seg.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    seg.parse::<u64>().ok()
}

/// Windows-reserved DOS device names. A directory cannot be created with any of
/// these stems, so a creator literally named `CON` needs a suffix.
const RESERVED_DOS_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A creator name made safe to use as a folder component on Windows.
///
/// Strips `<>:"/\|?*` and control characters, then trims trailing dots and
/// spaces — Win32 removes those *silently*, so `"Name."` would land on disk as
/// `"Name"` and desync the planned destination from the real one. Reserved DOS
/// device names get a `_` suffix. Returns `None` when nothing usable is left.
///
/// Suffixing/renaming is safe here in a way it never is for a `.var` file:
/// VAM's package identity comes from the file stem alone, so the folder a
/// package sits in can be spelled however the filesystem will accept.
pub(crate) fn sanitize_creator_folder(creator: &str) -> Option<String> {
    let cleaned: String = creator
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .filter(|c| !c.is_control())
        .collect();

    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c == ' ');
    if trimmed.is_empty() {
        return None;
    }

    // The reserved check is on the whole name and on the part before the first
    // dot, matching Win32: `CON` and `CON.txt` are both refused.
    let stem = trimmed.split('.').next().unwrap_or(trimmed);
    if RESERVED_DOS_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Some(format!("{trimmed}_"));
    }

    Some(trimmed.to_string())
}

/// Maps an in-archive `internal_path` to one of the seeded category names in
/// the `categories` table. Path-based prefixes win over extension fallbacks so
/// e.g. a `.json` file under `Saves/scene/` is classified as a Scene rather
/// than a generic Preset.
pub(crate) fn category_name_for_path(internal_path: &str) -> &'static str {
    let lower = internal_path.to_ascii_lowercase();

    if lower.starts_with("saves/scene/") || lower.contains("/saves/scene/") {
        return "Scene";
    }
    // SubScene must win over the generic /Presets/ match below — VAM stores
    // sub-scene assets under Custom/SubScene/<creator>/.../Presets/...
    if lower.starts_with("custom/subscene/") || lower.contains("/subscene/") {
        return "SubScene";
    }
    if lower.contains("/morphs/") || lower.starts_with("custom/atom/person/morphs/") {
        return "Morph";
    }
    if lower.contains("/clothing/") || lower.starts_with("custom/clothing/") {
        return "Clothing";
    }
    if lower.contains("/hair/") || lower.starts_with("custom/hair/") {
        return "Hair";
    }
    if lower.contains("/textures/") || lower.starts_with("custom/atom/person/textures/") {
        return "Texture";
    }
    if lower.contains("/scripts/") || lower.starts_with("custom/scripts/") {
        return "Scripts";
    }
    if lower.contains("/sounds/") || lower.starts_with("custom/sounds/") {
        return "Audio";
    }
    if lower.contains("/assets/") || lower.starts_with("custom/assets/") {
        return "Asset";
    }
    if lower.contains("/presets/") {
        return "Preset";
    }

    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "vmi" | "vmb" => "Morph",
        "vam" | "vaj" | "vab" => "Asset",
        "vap" => "Preset",
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "tga" => "Texture",
        "wav" | "mp3" | "ogg" => "Audio",
        "cs" | "cslist" | "dll" => "Scripts",
        _ => "Other",
    }
}

/// The full set of seeded category names. Kept here so the migration in
/// `db.rs` and the classifier above cannot drift apart.
pub(crate) const SEEDED_CATEGORIES: &[&str] = &[
    "Scene",
    "SubScene",
    "Morph",
    "Clothing",
    "Hair",
    "Texture",
    "Asset",
    "Plugin",
    "Scripts",
    "Audio",
    "Preset",
    "Other",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_extraction() {
        assert_eq!(creator_from_package_id("CuaJoe.Morphs_for_JFF_V2"), Some("CuaJoe"));
        assert_eq!(creator_from_package_id("TrevorLake.Genitalia02.3"), Some("TrevorLake"));
        assert_eq!(creator_from_package_id(".LeadingDot"), None);
        assert_eq!(creator_from_package_id("NoDot"), None);
        assert_eq!(creator_from_package_id(""), None);
    }

    #[test]
    fn version_parsing() {
        assert_eq!(package_version("Creator.Pkg.3"), Some(3));
        // Leading zeros parse, but see `leading_zero_ids_stay_distinct` — the
        // planner must not treat .7 and .007 as the same package.
        assert_eq!(package_version("Creator.Pkg.007"), Some(7));
        // `.latest` is deliberately not comparable: it must never win a family.
        assert_eq!(package_version("Creator.Pkg.latest"), None);
        assert_eq!(package_version("Creator.Pkg.LATEST"), None);
        assert_eq!(package_version("Creator.Pkg.abc"), None);
        assert_eq!(package_version("Creator.Pkg"), None);
        assert_eq!(package_version("Creator.Pkg."), None);
        // 20 digits would overflow u64; the length guard rejects it rather than
        // silently wrapping or erroring.
        assert_eq!(package_version("Creator.Pkg.99999999999999999999"), None);
        // 19 digits is always safe.
        assert_eq!(package_version("Creator.Pkg.9999999999999999999"), Some(9_999_999_999_999_999_999));
    }

    #[test]
    fn base_and_segment_extraction() {
        assert_eq!(package_base("Creator.Pkg.3"), "Creator.Pkg");
        assert_eq!(package_base("Creator.Pkg.latest"), "Creator.Pkg");
        assert_eq!(package_base("Creator.Pkg"), "Creator.Pkg");
        assert_eq!(package_base("Creator.Pkg.abc"), "Creator.Pkg.abc");
        assert_eq!(package_base("NoDot"), "NoDot");

        assert_eq!(package_version_segment("Creator.Pkg.3"), Some("3"));
        assert_eq!(package_version_segment("Creator.Pkg.latest"), Some("latest"));
        assert_eq!(package_version_segment("Creator.Pkg"), None);

        assert!(has_package_version("Creator.Pkg.3"));
        assert!(has_package_version("Creator.Pkg.latest"));
        assert!(!has_package_version("Creator.Pkg"));
        assert!(!has_package_version("Creator.Pkg."));
    }

    /// `.7` and `.007` are different files to VAM — a ref to `.7` will not
    /// resolve to `.007` — even though both parse to 7. The planner keys
    /// families on the id, never on the parsed number alone.
    #[test]
    fn leading_zero_ids_stay_distinct() {
        assert_eq!(package_version("C.P.7"), package_version("C.P.007"));
        assert_ne!("c.p.7", "c.p.007");
    }

    /// Pins the primitives against the exact behavior of the five parsers they
    /// replaced (`hub::has_version_suffix`/`strip_version`/`base_lc`,
    /// `tasks::dependency_package_base`/`dependency_version_segment`), including
    /// the edges those implementations happened to have.
    #[test]
    fn wrappers_match_legacy_parsers() {
        // The legacy predicate, reproduced verbatim from the old hub.rs body.
        fn legacy_is_version(last: &str) -> bool {
            !last.is_empty()
                && (last.eq_ignore_ascii_case("latest") || last.chars().all(|c| c.is_ascii_digit()))
        }
        fn legacy_strip(id: &str) -> String {
            if let Some((base, last)) = id.rsplit_once('.') {
                if legacy_is_version(last) {
                    return base.to_string();
                }
            }
            id.to_string()
        }
        fn legacy_has_suffix(id: &str) -> bool {
            match id.rsplit_once('.') {
                Some((_, last)) => legacy_is_version(last),
                None => false,
            }
        }

        for id in [
            "Creator.Pkg.3",
            "Creator.Pkg.latest",
            "Creator.Pkg.LATEST",
            "Creator.Pkg",
            "Creator.Pkg.",
            "Creator.Pkg.abc",
            "Creator.Pkg.007",
            "NoDot",
            "",
            ".LeadingDot.1",
            "A.B.C.2",
        ] {
            assert_eq!(package_base(id), legacy_strip(id), "base mismatch for {id:?}");
            assert_eq!(
                has_package_version(id),
                legacy_has_suffix(id),
                "suffix mismatch for {id:?}",
            );
        }
    }

    #[test]
    fn sanitize_creator_folder_windows() {
        assert_eq!(sanitize_creator_folder("Qing").as_deref(), Some("Qing"));
        // Win32 silently strips these, so we must strip them first or the
        // planned dest won't match what lands on disk.
        assert_eq!(sanitize_creator_folder("Name ").as_deref(), Some("Name"));
        assert_eq!(sanitize_creator_folder("Name.").as_deref(), Some("Name"));
        assert_eq!(sanitize_creator_folder("  Name..  ").as_deref(), Some("Name"));
        // Reserved DOS device names cannot be directories.
        assert_eq!(sanitize_creator_folder("CON").as_deref(), Some("CON_"));
        assert_eq!(sanitize_creator_folder("con").as_deref(), Some("con_"));
        assert_eq!(sanitize_creator_folder("COM1").as_deref(), Some("COM1_"));
        assert_eq!(sanitize_creator_folder("CON.stuff").as_deref(), Some("CON.stuff_"));
        // Not reserved — only exact device stems are.
        assert_eq!(sanitize_creator_folder("CONTROL").as_deref(), Some("CONTROL"));
        // Illegal path characters are dropped.
        assert_eq!(sanitize_creator_folder("A/B:C*D").as_deref(), Some("ABCD"));
        // Nothing usable left.
        assert_eq!(sanitize_creator_folder(""), None);
        assert_eq!(sanitize_creator_folder("..."), None);
        assert_eq!(sanitize_creator_folder("///"), None);
        // Non-ASCII creators are common in VAM and must survive untouched.
        assert_eq!(sanitize_creator_folder("清水").as_deref(), Some("清水"));
    }

    #[test]
    fn category_classification() {
        assert_eq!(category_name_for_path("Saves/scene/MyScene.json"), "Scene");
        assert_eq!(
            category_name_for_path("Custom/Atom/Person/Morphs/female/foo.vmi"),
            "Morph",
        );
        assert_eq!(category_name_for_path("Custom/Clothing/female/Top.vam"), "Clothing");
        assert_eq!(category_name_for_path("Custom/Hair/female/Hair.vam"), "Hair");
        assert_eq!(
            category_name_for_path("Custom/Atom/Person/Textures/skin.png"),
            "Texture",
        );
        assert_eq!(category_name_for_path("Custom/Scripts/Plugin.cslist"), "Scripts");
        assert_eq!(category_name_for_path("Custom/Sounds/clip.wav"), "Audio");
        assert_eq!(category_name_for_path("Custom/Assets/AssetBundle.assetbundle"), "Asset");
        assert_eq!(
            category_name_for_path("Custom/SubScene/Creator/Pack/Sub.vap"),
            "SubScene",
        );
        // SubScene wins over the generic /Presets/ branch
        assert_eq!(
            category_name_for_path("Custom/SubScene/Some/Presets/x.vap"),
            "SubScene",
        );
        // Standalone /Presets/ paths still classify as Preset
        assert_eq!(category_name_for_path("Custom/Atom/Person/Presets/p.vap"), "Preset");
        // Extension fallback when path gives no hint
        assert_eq!(category_name_for_path("README.md"), "Other");
        assert_eq!(category_name_for_path("foo.dll"), "Scripts");
        assert_eq!(category_name_for_path("foo.cs"), "Scripts");
        assert_eq!(category_name_for_path("foo.vmi"), "Morph");
    }

    #[test]
    fn seeded_category_count() {
        assert_eq!(SEEDED_CATEGORIES.len(), 12);
    }
}
