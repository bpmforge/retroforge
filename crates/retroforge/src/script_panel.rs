//! The Lua console panel's pure half (ticket W4-04; DEBUGGER.md §5,
//! FR-PLUG-003).
//!
//! Everything a human would otherwise have to *look at* to check —
//! what the capability list says, what the ledger renders as, what the
//! console shows — is computed here as plain strings, so it is testable
//! without driving egui. `crate::debug_dock`'s tab body only paints these.
//!
//! Same split as `crate::enhanced_view` and `rf_renderer::compare`: the
//! branching is what has bugs, so the branching is what gets tested.

use rf_plugin_sdk::{ScriptHost, ScriptState};

/// The capability list shown at enable time (FR-PLUG-003), including the
/// heading a user actually reads.
#[must_use]
pub fn capability_lines(host: &ScriptHost) -> Vec<String> {
    let mut out = vec![format!(
        "{} {} (api {}) — {}",
        host.manifest.id, host.manifest.version, host.manifest.api, host.manifest.license
    )];
    out.push(format!("provenance: {}", host.manifest.provenance));
    let granted = host.manifest.capabilities.granted_summary();
    if granted.is_empty() {
        out.push("requests no capabilities".to_string());
    } else {
        out.push("requests:".to_string());
        for cap in granted {
            out.push(format!("  • {cap}"));
        }
    }
    out
}

/// One line describing the script's current state, for the panel header.
#[must_use]
pub fn status_line(host: &ScriptHost) -> String {
    match host.state() {
        ScriptState::Running => "running".to_string(),
        ScriptState::Throttled { last_cost } => {
            format!("THROTTLED — last frame cost {last_cost:?} (over budget)")
        }
        // The error is included rather than a bare "paused": a user who
        // cannot see why has to go looking in a log for the one fact that
        // matters.
        ScriptState::Paused { error } => format!("PAUSED after a fault — {error}"),
    }
}

/// The write ledger, rendered (FR-PLUG-003: "write ledger visible in UI").
#[must_use]
pub fn ledger_lines(host: &ScriptHost) -> Vec<String> {
    let entries = host.ledger().entries();
    if entries.is_empty() {
        // Saying "no writes" is not the same as showing nothing: an empty
        // panel reads as "not implemented", which is exactly the doubt a
        // mod-tier audit surface must not create.
        return vec!["no memory writes recorded this session".to_string()];
    }
    entries
        .iter()
        .map(|e| {
            format!(
                "frame {:>8}  ${:04X} = ${:02X}  ({})",
                e.frame, e.addr, e.value, e.plugin
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_plugin_sdk::{Budget, Capabilities, Manifest};

    fn host(caps: Capabilities, source: &str) -> ScriptHost {
        ScriptHost::load(
            Manifest {
                id: "demo".into(),
                version: "1.0".into(),
                api: "0.1".into(),
                license: "MIT".into(),
                provenance: "test fixture".into(),
                capabilities: caps,
            },
            source,
            Budget::default(),
        )
        .expect("test script loads")
    }

    #[test]
    fn the_capability_list_names_licence_provenance_and_every_grant() {
        let lines = capability_lines(&host(
            Capabilities {
                read_memory: true,
                write_memory: true,
                ..Capabilities::default()
            },
            "",
        ));
        let joined = lines.join("\n");
        assert!(joined.contains("MIT"), "{joined}");
        assert!(joined.contains("test fixture"), "provenance must show");
        assert!(joined.contains("WRITE MEMORY"), "{joined}");
        assert!(joined.contains("read emulated memory"), "{joined}");
    }

    /// A plugin that asks for nothing must SAY so, not render an empty
    /// list a user could read as "the list failed to load".
    #[test]
    fn a_plugin_requesting_nothing_says_so_explicitly() {
        let lines = capability_lines(&host(Capabilities::default(), ""));
        assert!(lines.iter().any(|l| l.contains("requests no capabilities")));
    }

    /// The status line must carry the REASON, not just the state — a user
    /// who cannot see why has to go hunting for the one fact that matters.
    #[test]
    fn the_status_line_carries_the_fault_reason() {
        let mut h = host(
            Capabilities::default(),
            "function on_frame(f) error('kaboom') end",
        );
        assert_eq!(status_line(&h), "running");
        h.on_frame(1);
        let line = status_line(&h);
        assert!(line.starts_with("PAUSED"), "{line}");
        assert!(
            line.contains("kaboom"),
            "the reason must be in the line: {line}"
        );
    }

    #[test]
    fn the_ledger_renders_writes_and_says_so_when_there_are_none() {
        let mut h = host(Capabilities::default(), "");
        assert!(ledger_lines(&h)[0].contains("no memory writes"));

        let mut modder = host(
            Capabilities {
                write_memory: true,
                ..Capabilities::default()
            },
            "",
        );
        modder.record_write(42, 0x0300, 0x7F);
        let lines = ledger_lines(&modder);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("$0300 = $7F"), "{}", lines[0]);
        assert!(
            lines[0].contains("frame") && lines[0].contains("42"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("demo"), "the ledger must name the plugin");

        // A denied write leaves the ledger empty — the gate and the
        // display agree.
        assert!(!h.record_write(1, 0x0300, 0xFF));
        assert!(ledger_lines(&h)[0].contains("no memory writes"));
    }
}
