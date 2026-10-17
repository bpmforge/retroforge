//! Patch-file naming, pure (no filesystem): which file names beside a ROM
//! are patches for it, and in what order they stack. RetroArch-style:
//! `Game.sfc` -> `Game.bps` / `Game.ips` / `Game.ups`, and numbered stacks
//! `Game.ips1`, `Game.ips2`, ... applied in order.
use super::PatchKind;

/// A recognised patch file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchName {
    pub kind: PatchKind,
    /// 0 for the plain extension, N for `ipsN`.
    pub index: u32,
}

/// Classify an extension (`"ips"`, `"bps2"`, any case).
#[must_use]
pub fn classify_ext(ext: &str) -> Option<PatchName> {
    let ext = ext.to_ascii_lowercase();
    let (kind, digits) = [PatchKind::Bps, PatchKind::Ips, PatchKind::Ups]
        .into_iter()
        .find_map(|k| ext.strip_prefix(k.ext()).map(|rest| (k, rest.to_owned())))?;
    if digits.is_empty() {
        return Some(PatchName { kind, index: 0 });
    }
    if digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(PatchName {
        kind,
        index: digits.parse().ok()?,
    })
}

/// Is `file_name` a patch for the ROM whose file stem is `rom_stem`?
/// The stem must match in full (`Game2.ips` is not `Game`'s), and the
/// remainder must be exactly one patch extension (`Game.ips.bak` is not).
#[must_use]
pub fn classify_beside(rom_stem: &str, file_name: &str) -> Option<PatchName> {
    let head = file_name.get(..rom_stem.len())?;
    let ext = file_name.get(rom_stem.len()..)?.strip_prefix('.')?;
    if !head.eq_ignore_ascii_case(rom_stem) || ext.contains('.') {
        return None;
    }
    classify_ext(ext)
}

/// Application order: stack number, then format (BPS, IPS, UPS), then name.
#[must_use]
pub fn order_key(name: &PatchName, file_name: &str) -> (u32, PatchKind, String) {
    (name.index, name.kind, file_name.to_ascii_lowercase())
}

/// Is `file_name` the `.bso` for the ROM with this stem?
#[must_use]
pub fn is_bso_for(rom_stem: &str, file_name: &str) -> bool {
    file_name.len() == rom_stem.len() + 4
        && file_name
            .get(..rom_stem.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(rom_stem))
        && file_name
            .get(rom_stem.len()..)
            .is_some_and(|s| s.eq_ignore_ascii_case(".bso"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_numbered_extensions() {
        assert_eq!(
            classify_ext("bps"),
            Some(PatchName {
                kind: PatchKind::Bps,
                index: 0
            })
        );
        assert_eq!(
            classify_ext("IPS2"),
            Some(PatchName {
                kind: PatchKind::Ips,
                index: 2
            })
        );
        assert_eq!(
            classify_ext("ups10"),
            Some(PatchName {
                kind: PatchKind::Ups,
                index: 10
            })
        );
        for bad in ["", "ipsx", "ips-1", "ips12345", "sfc", "bpss"] {
            assert_eq!(classify_ext(bad), None, "{bad}");
        }
    }

    #[test]
    fn partial_names_are_ignored_and_stacks_order() {
        let stem = "Game";
        let names = [
            "Game.ips2",
            "Game.bps",
            "Game.ips",
            "Game.ips1",
            "Game2.ips",
            "Game.ips.bak",
            "xGame.ips",
            "Gam.ips",
            "Game.sfc",
            "GAME.UPS",
            "Game.bso",
            "Game..ips",
        ];
        let mut found: Vec<_> = names
            .iter()
            .filter_map(|n| classify_beside(stem, n).map(|c| (order_key(&c, n), *n)))
            .collect();
        found.sort();
        let order: Vec<&str> = found.into_iter().map(|(_, n)| n).collect();
        assert_eq!(
            order,
            ["Game.bps", "Game.ips", "GAME.UPS", "Game.ips1", "Game.ips2"]
        );
        assert!(is_bso_for("Game", "game.BSO"));
        assert!(!is_bso_for("Game", "Game2.bso"));
        assert!(!is_bso_for("Game", "Game.bso.txt"));
    }

    #[test]
    fn non_ascii_names_do_not_panic() {
        assert_eq!(
            classify_beside("Gäme", "Gäme.ips"),
            Some(PatchName {
                kind: PatchKind::Ips,
                index: 0
            })
        );
        assert_eq!(classify_beside("Game", "Gä.ips"), None);
        assert_eq!(classify_beside("Game", "Gaméé.ips"), None);
        assert_eq!(classify_beside("Game", ""), None);
    }
}
