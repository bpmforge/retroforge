//! Unknown-key detection (FR-PROF-004, NFR-008: "warns on unknown keys").
//!
//! `serde(deny_unknown_fields)` turns an unknown key into a hard parse
//! *error*, which cannot satisfy "warns" — a profile with one typo'd key
//! would refuse to load at all instead of loading with a warning. Instead
//! this module hand-walks the parsed `toml::Value` against a small
//! whitelist tree (`known_shape`) built to mirror `docs/design/
//! GAME_PROFILES.md` §2 section-for-section, collecting every key path not
//! present in that tree. `crate::loader` runs this walk *before* the typed
//! `Profile` deserialize, so warnings are gathered independent of whether
//! the rest of the file is otherwise valid.

/// One level of the whitelist tree.
#[derive(Debug, Clone)]
enum Shape {
    /// A scalar/array-of-scalar value — nothing further to check.
    Leaf,
    /// Every element must match `inner`.
    Array(Box<Shape>),
    /// A table whose keys must all appear in `fields`.
    Object(Vec<(&'static str, Shape)>),
    /// An array of tables, each checked against `fields`.
    ArrayOfObjects(Vec<(&'static str, Shape)>),
    /// No further validation — used for `entities.fields`, whose key names
    /// are necessarily per-game and cannot be enumerated in a schema.
    Any,
}

fn axis_spec() -> Shape {
    Shape::Object(vec![
        ("addr", Shape::Leaf),
        ("type", Shape::Leaf),
        ("scale", Shape::Leaf),
        ("page", Shape::Object(vec![("addr", Shape::Leaf)])),
    ])
}

/// The schema v0 whitelist tree, matching GAME_PROFILES.md §2 top to
/// bottom.
fn known_shape() -> Shape {
    Shape::Object(vec![
        (
            "meta",
            Shape::Object(vec![
                ("profile_version", Shape::Leaf),
                ("title", Shape::Leaf),
                ("console", Shape::Leaf),
                ("region", Shape::Leaf),
                ("authors", Shape::Array(Box::new(Shape::Leaf))),
                ("sources", Shape::Array(Box::new(Shape::Leaf))),
                ("license", Shape::Leaf),
                (
                    "requires",
                    Shape::Object(vec![
                        ("mapper", Shape::Leaf),
                        ("chips", Shape::Array(Box::new(Shape::Leaf))),
                    ]),
                ),
            ]),
        ),
        (
            "identity",
            Shape::ArrayOfObjects(vec![
                ("sha256", Shape::Leaf),
                ("sha1", Shape::Leaf),
                ("md5", Shape::Leaf),
                ("crc32", Shape::Leaf),
                ("revision", Shape::Leaf),
            ]),
        ),
        (
            "capabilities",
            Shape::Object(vec![
                ("full_level", Shape::Leaf),
                ("hud_separation", Shape::Leaf),
                ("entity_overlay", Shape::Leaf),
                ("widescreen", Shape::Leaf),
                ("fast_load", Shape::Leaf),
                ("smooth_camera", Shape::Leaf),
            ]),
        ),
        (
            "memory_map",
            Shape::ArrayOfObjects(vec![
                ("addr", Shape::Leaf),
                ("len", Shape::Leaf),
                ("type", Shape::Leaf),
                ("label", Shape::Leaf),
                ("notes", Shape::Leaf),
                ("source", Shape::Leaf),
            ]),
        ),
        (
            "camera",
            Shape::Object(vec![
                ("mode", Shape::Leaf),
                ("x", axis_spec()),
                ("y", axis_spec()),
                (
                    "hud",
                    Shape::Object(vec![
                        ("region", Shape::Leaf),
                        ("scanlines", Shape::Array(Box::new(Shape::Leaf))),
                        ("detect", Shape::Leaf),
                    ]),
                ),
            ]),
        ),
        (
            "entities",
            Shape::Object(vec![
                (
                    "table",
                    Shape::Object(vec![
                        ("addr", Shape::Leaf),
                        ("stride", Shape::Leaf),
                        ("count", Shape::Leaf),
                    ]),
                ),
                ("fields", Shape::Any),
                ("offscreen_valid", Shape::Leaf),
            ]),
        ),
        (
            "rom_map",
            Shape::ArrayOfObjects(vec![
                ("offset", Shape::Leaf),
                ("len", Shape::Leaf),
                ("label", Shape::Leaf),
                ("type", Shape::Leaf),
                ("count", Shape::Leaf),
                // Not in GAME_PROFILES.md §2's own rom_map example, but
                // named by FR-PROF-003 for both memory_map and rom_map
                // rows alike — see RomMapEntry's doc comment.
                ("source", Shape::Leaf),
                ("notes", Shape::Leaf),
            ]),
        ),
        (
            "decode",
            Shape::Object(vec![
                ("kind", Shape::Leaf),
                (
                    "metatile",
                    Shape::Object(vec![
                        ("table", Shape::Leaf),
                        ("size", Shape::Leaf),
                        ("chr_bank_reg", Shape::Leaf),
                    ]),
                ),
                (
                    "screens",
                    Shape::Object(vec![
                        ("width", Shape::Leaf),
                        ("height", Shape::Leaf),
                        ("order", Shape::Leaf),
                    ]),
                ),
                (
                    "palettes",
                    Shape::Object(vec![("table", Shape::Leaf), ("per_area", Shape::Leaf)]),
                ),
                (
                    "collision",
                    Shape::Object(vec![("table", Shape::Leaf), ("bits", Shape::Leaf)]),
                ),
                // room_grid (ticket W9-08). shape.rs is a SECOND source of
                // truth beside the serde structs and Profile has no
                // deny_unknown_fields, so a table missing here still
                // deserialises and merely warns — the drift W8-12 found.
                (
                    "room_grid",
                    Shape::Object(vec![
                        ("rooms_across", Shape::Leaf),
                        ("rooms_down", Shape::Leaf),
                        ("room_width", Shape::Leaf),
                        ("room_height", Shape::Leaf),
                        ("indexed", Shape::Leaf),
                    ]),
                ),
                ("family_version", Shape::Leaf),
            ]),
        ),
        (
            "antiflicker",
            Shape::Object(vec![
                ("mode", Shape::Leaf),
                ("exclude_oam", Shape::Array(Box::new(Shape::Leaf))),
                ("blink_periods", Shape::Array(Box::new(Shape::Leaf))),
            ]),
        ),
        (
            "loading",
            Shape::Object(vec![(
                "wait_loops",
                Shape::Array(Box::new(Shape::Object(vec![
                    ("pc", Shape::Leaf),
                    (
                        "until",
                        Shape::Object(vec![("addr", Shape::Leaf), ("equals", Shape::Leaf)]),
                    ),
                    ("label", Shape::Leaf),
                ]))),
            )]),
        ),
        (
            "plugins",
            Shape::Object(vec![
                ("required", Shape::Array(Box::new(Shape::Leaf))),
                ("optional", Shape::Array(Box::new(Shape::Leaf))),
            ]),
        ),
        (
            "widescreen",
            Shape::Object(vec![
                ("bg1", Shape::Leaf),
                ("bg2", Shape::Leaf),
                ("bg3", Shape::Leaf),
                ("bg4", Shape::Leaf),
                ("obj", Shape::Leaf),
            ]),
        ),
        // `[text]` (ticket W8-12). This list is a SECOND source of truth
        // beside the serde structs, and it has to be kept in step: the
        // Profile struct has no `deny_unknown_fields`, so a table missing
        // from here still deserialises fine and merely warns — which is
        // exactly the silent drift `unknown_keys` exists to catch.
        (
            "text",
            Shape::Object(vec![
                (
                    "region",
                    Shape::ArrayOfObjects(vec![
                        ("id", Shape::Leaf),
                        ("x", Shape::Leaf),
                        ("y", Shape::Leaf),
                        ("width", Shape::Leaf),
                        ("height", Shape::Leaf),
                    ]),
                ),
                (
                    "entry",
                    Shape::ArrayOfObjects(vec![
                        ("region", Shape::Leaf),
                        ("original", Shape::Leaf),
                        ("translations", Shape::Leaf),
                        ("accessible", Shape::Leaf),
                    ]),
                ),
            ]),
        ),
        (
            "mods",
            Shape::Object(vec![(
                "patch",
                Shape::ArrayOfObjects(vec![
                    ("id", Shape::Leaf),
                    ("description", Shape::Leaf),
                    ("addr", Shape::Leaf),
                    ("replace", Shape::Array(Box::new(Shape::Leaf))),
                ]),
            )]),
        ),
    ])
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn walk(shape: &Shape, value: &toml::Value, path: &str, out: &mut Vec<String>) {
    match (shape, value) {
        (Shape::Any, _) | (Shape::Leaf, _) => {}
        (Shape::Array(inner), toml::Value::Array(items)) => {
            for item in items {
                walk(inner, item, path, out);
            }
        }
        (Shape::Object(fields), toml::Value::Table(table)) => {
            for (key, val) in table {
                let child_path = join(path, key);
                match fields.iter().find(|(name, _)| *name == key) {
                    Some((_, child_shape)) => walk(child_shape, val, &child_path, out),
                    None => out.push(child_path),
                }
            }
        }
        (Shape::ArrayOfObjects(fields), toml::Value::Array(items)) => {
            let as_object = Shape::Object(fields.clone());
            for item in items {
                walk(&as_object, item, path, out);
            }
        }
        // A shape/value kind mismatch (e.g. a string where a table was
        // expected) is a structural error the typed deserialize pass will
        // report; the unknown-key walk only classifies keys, not types.
        _ => {}
    }
}

/// Every key path in `value` (a parsed profile document) that is not part
/// of schema v0, as dotted paths (`decode.metatlie`, not `metatlie`).
pub(crate) fn unknown_keys(value: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(&known_shape(), value, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> toml::Value {
        toml::from_str(s).expect("valid toml")
    }

    #[test]
    fn no_warnings_for_a_fully_known_document() {
        let v = parse(
            r#"
            [meta]
            profile_version = "0.1"
            title = "t"
            console = "nes"
            region = "ntsc"

            [capabilities]
            full_level = true
            "#,
        );
        assert!(unknown_keys(&v).is_empty());
    }

    #[test]
    fn flags_an_unknown_top_level_key() {
        let v = parse(
            r#"
            [meta]
            profile_version = "0.1"
            title = "t"
            console = "nes"
            region = "ntsc"

            [bogus]
            x = 1
            "#,
        );
        let warnings = unknown_keys(&v);
        assert!(warnings.contains(&"bogus".to_string()), "{warnings:?}");
    }

    #[test]
    fn flags_a_nested_unknown_key_with_dotted_path() {
        let v = parse(
            r#"
            [meta]
            profile_version = "0.1"
            title = "t"
            console = "nes"
            region = "ntsc"

            [decode]
            kind = "metatile_screens"

            [decode.metatile]
            table = 1
            size = 4
            chr_bank_regg = "typo"
            "#,
        );
        let warnings = unknown_keys(&v);
        assert!(
            warnings.contains(&"decode.metatile.chr_bank_regg".to_string()),
            "{warnings:?}"
        );
    }

    #[test]
    fn does_not_flag_freeform_entity_field_names() {
        let v = parse(
            r#"
            [meta]
            profile_version = "0.1"
            title = "t"
            console = "nes"
            region = "ntsc"

            [entities]
            offscreen_valid = false

            [entities.table]
            addr = 1
            stride = 16
            count = 8

            [entities.fields]
            x = 0
            y = 4
            kind = 12
            whatever_the_game_calls_it = 7
            active = { offset = 15, mask = 0x80 }
            "#,
        );
        assert!(unknown_keys(&v).is_empty(), "{:?}", unknown_keys(&v));
    }
}
