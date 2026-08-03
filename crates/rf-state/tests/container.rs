//! Container-level acceptance tests (ticket W0-05).
//!
//! Covers: roundtrip; unknown-chunk-skip (surfaced, not silently
//! dropped); missing-required-chunk (named); chunk version bump ⇒
//! migration hook or explicit error; wrong magic rejected; newer
//! container_version refused; rom_sha256 mismatch refused; truncated/
//! corrupt input rejected without panic; oversized `len` rejected without
//! a huge allocation; same-state-different-timestamp hashes identical
//! (but bytes differ); bincode-2 payload roundtrip through a real chunk.

mod support;

use rf_state::{
    decode_payload, encode_payload, Container, ContainerError, LoadWarning, MigrationRegistry,
    ReplayCursor,
};
use support::{golden_container, GOLDEN_ROM_SHA256};

#[test]
fn roundtrip_preserves_header_and_chunks() {
    let original = golden_container().unwrap();
    let bytes = original.encode().unwrap();
    let (decoded, warnings) = Container::decode_default(&bytes).unwrap();

    assert!(
        warnings.is_empty(),
        "golden container should load with zero warnings"
    );
    assert_eq!(decoded.header, original.header);
    assert_eq!(decoded.chunks(), original.chunks());
    assert_eq!(decoded.state_hash(), original.state_hash());
}

#[test]
fn unknown_chunk_is_skipped_with_a_surfaced_warning_and_retained() {
    let mut c = golden_container().unwrap();
    c.add_chunk(*b"ZZZZ", 1, vec![9, 9]).unwrap();
    let bytes = c.encode().unwrap();

    let (decoded, warnings) = Container::decode_default(&bytes).unwrap();
    assert_eq!(warnings, vec![LoadWarning::UnknownChunk { tag: *b"ZZZZ" }]);
    // "Skipped" means "not interpreted", not "discarded" — the bytes are
    // still there for a caller that wants to inspect/preserve them.
    let kept = decoded
        .chunk(*b"ZZZZ")
        .expect("unknown chunk must be retained");
    assert_eq!(kept.payload, vec![9, 9]);
}

#[test]
fn missing_required_chunk_fails_loudly_and_names_the_chunk() {
    // Build every required chunk except CART, by hand (bypassing the
    // golden helper) so exactly one is missing.
    let mut c = Container::new(0, GOLDEN_ROM_SHA256, "0.1.0", 1_700_000_000);
    for tag in [
        *b"CPU_", *b"PPU_", *b"APU_", *b"WRAM", *b"VRAM", *b"OAM_", *b"CGRM", *b"MAPR",
    ] {
        c.add_chunk(tag, 1, vec![0]).unwrap();
    }
    let bytes = c.encode().unwrap();

    let err = Container::decode_default(&bytes).unwrap_err();
    assert_eq!(err, ContainerError::MissingRequiredChunk { tag: *b"CART" });
    assert!(
        err.to_string().contains("CART"),
        "error message must name the chunk"
    );
}

#[test]
fn chunk_version_bump_without_migration_is_an_explicit_error() {
    let mut c = golden_container().unwrap();
    // Overwrite CPU_ at a version this build does not expect.
    let chunks_without_cpu: Vec<_> = c
        .chunks()
        .iter()
        .filter(|ch| ch.tag != *b"CPU_")
        .cloned()
        .collect();
    let mut rebuilt = Container::new(0, GOLDEN_ROM_SHA256, "0.1.0", 1_700_000_000);
    rebuilt.add_chunk(*b"CPU_", 2, vec![1, 2, 3]).unwrap();
    for ch in chunks_without_cpu {
        rebuilt.add_chunk(ch.tag, ch.version, ch.payload).unwrap();
    }
    c = rebuilt;
    let bytes = c.encode().unwrap();

    let err = Container::decode_default(&bytes).unwrap_err();
    assert_eq!(
        err,
        ContainerError::CannotMigrate {
            tag: *b"CPU_",
            found: 2,
            expected: 1,
        }
    );
    assert!(err.to_string().contains("CPU_"));
}

#[test]
fn chunk_version_bump_with_registered_migration_succeeds() {
    let mut c = golden_container().unwrap();
    let chunks_without_cpu: Vec<_> = c
        .chunks()
        .iter()
        .filter(|ch| ch.tag != *b"CPU_")
        .cloned()
        .collect();
    let mut rebuilt = Container::new(0, GOLDEN_ROM_SHA256, "0.1.0", 1_700_000_000);
    rebuilt.add_chunk(*b"CPU_", 2, vec![1, 2, 3]).unwrap();
    for ch in chunks_without_cpu {
        rebuilt.add_chunk(ch.tag, ch.version, ch.payload).unwrap();
    }
    c = rebuilt;
    let bytes = c.encode().unwrap();

    fn migrate_cpu_v2_to_v1(found: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
        assert_eq!(found, 2);
        // Trivial migration: v1 just drops the last byte of v2.
        Ok(payload[..payload.len() - 1].to_vec())
    }
    let mut registry = MigrationRegistry::new();
    registry.register(*b"CPU_", migrate_cpu_v2_to_v1);

    let (decoded, warnings) = Container::decode(&bytes, &registry).unwrap();
    assert_eq!(
        warnings,
        vec![LoadWarning::Migrated {
            tag: *b"CPU_",
            from: 2,
            to: 1,
        }]
    );
    let migrated = decoded.chunk(*b"CPU_").unwrap();
    assert_eq!(migrated.version, 1);
    assert_eq!(migrated.payload, vec![1, 2]);
}

#[test]
fn wrong_magic_is_rejected() {
    let mut bytes = golden_container().unwrap().encode().unwrap();
    bytes[0] = b'X';
    let err = Container::decode_default(&bytes).unwrap_err();
    assert!(matches!(err, ContainerError::BadMagic { .. }));
}

#[test]
fn newer_container_version_is_refused() {
    let mut bytes = golden_container().unwrap().encode().unwrap();
    // container_version lives at header offset 4..6 (see header.rs).
    let bumped = u16::from_le_bytes([bytes[4], bytes[5]]) + 1;
    bytes[4..6].copy_from_slice(&bumped.to_le_bytes());
    let err = Container::decode_default(&bytes).unwrap_err();
    assert!(matches!(
        err,
        ContainerError::UnsupportedContainerVersion { .. }
    ));
}

#[test]
fn rom_sha256_mismatch_is_refused() {
    let c = golden_container().unwrap();
    let (decoded, _) = Container::decode_default(&c.encode().unwrap()).unwrap();
    let wrong = [0xFF; 32];
    let err = decoded.verify_rom(wrong).unwrap_err();
    assert_eq!(
        err,
        ContainerError::RomMismatch {
            expected: wrong,
            found: GOLDEN_ROM_SHA256,
        }
    );
    assert!(decoded.verify_rom(GOLDEN_ROM_SHA256).is_ok());
}

#[test]
fn every_truncated_prefix_is_rejected_without_panicking() {
    let bytes = golden_container().unwrap().encode().unwrap();
    for len in 0..bytes.len() {
        let result = std::panic::catch_unwind(|| Container::decode_default(&bytes[..len]));
        assert!(result.is_ok(), "prefix len {len} must not panic");
        assert!(
            result.unwrap().is_err(),
            "prefix len {len} must be rejected"
        );
    }
    // The full, untruncated stream must still decode successfully.
    assert!(Container::decode_default(&bytes).is_ok());
}

/// Independently re-implements the header byte layout documented in
/// `header.rs`'s module doc comment, using only `Header`'s public fields.
/// Used to hand-assemble corrupt-body test inputs without reaching into
/// the crate's private `Header::write` — which also means this doubles as
/// an external conformance check of the documented wire layout.
fn build_header_bytes(h: &rf_state::Header) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"RFST");
    out.extend_from_slice(&h.container_version.to_le_bytes());
    out.push(h.console);
    out.extend_from_slice(&h.flags.to_le_bytes());
    out.extend_from_slice(&h.rom_sha256);
    let ev = h.emu_version.as_bytes();
    out.extend_from_slice(&(u16::try_from(ev.len()).unwrap()).to_le_bytes());
    out.extend_from_slice(ev);
    out.extend_from_slice(&h.timestamp.to_le_bytes());
    out
}

#[test]
fn oversized_chunk_len_in_hand_built_body_is_rejected_without_huge_allocation() {
    // Hand-assemble a body claiming a payload of u32::MAX bytes, well
    // beyond what actually follows, then wrap it in a real header + real
    // zstd stream so this exercises the full Container::decode path (not
    // just decode_chunks in isolation).
    let header_bytes = build_header_bytes(&golden_container().unwrap().header);

    let mut body = Vec::new();
    body.extend_from_slice(b"CPU_");
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&u32::MAX.to_le_bytes());
    body.extend_from_slice(&[1, 2, 3]); // far short of u32::MAX bytes

    let compressed = zstd::encode_all(&body[..], 3).unwrap();
    let mut bytes = header_bytes;
    bytes.extend_from_slice(&compressed);

    let err = Container::decode_default(&bytes).unwrap_err();
    assert_eq!(
        err,
        ContainerError::ChunkTruncated {
            tag: *b"CPU_",
            needed: u32::MAX as usize,
            got: 3,
        }
    );
}

#[test]
fn same_logical_state_different_timestamp_hashes_identically_but_bytes_differ() {
    let required_tags = [
        *b"CPU_", *b"PPU_", *b"APU_", *b"WRAM", *b"VRAM", *b"OAM_", *b"CGRM", *b"MAPR", *b"CART",
    ];
    let mut early = Container::new(0, GOLDEN_ROM_SHA256, "0.1.0", 1_700_000_000);
    let mut late = Container::new(0, GOLDEN_ROM_SHA256, "0.1.0", 1_800_000_000);
    for state in [&mut early, &mut late] {
        for tag in required_tags {
            state.add_chunk(tag, 1, vec![0x42; 8]).unwrap();
        }
    }

    let early_bytes = early.encode().unwrap();
    let late_bytes = late.encode().unwrap();

    // The trap this test guards against: if timestamp injection silently
    // did nothing, early_bytes == late_bytes and the hash-equality
    // assertion below would pass vacuously.
    assert_ne!(
        early_bytes, late_bytes,
        "different timestamps must produce different container bytes"
    );
    assert_eq!(early.header.timestamp, 1_700_000_000);
    assert_eq!(late.header.timestamp, 1_800_000_000);
    assert_eq!(
        early.state_hash(),
        late.state_hash(),
        "state hash must be independent of timestamp (FR-STATE-002/NFR-001)"
    );
}

#[test]
fn encode_is_byte_deterministic_within_a_run() {
    let c = golden_container().unwrap();
    assert_eq!(c.encode().unwrap(), c.encode().unwrap());
}

#[test]
fn rply_chunk_payload_round_trips_through_bincode_2() {
    let mut c = golden_container().unwrap();
    let cursor = ReplayCursor { frame: 4242 };
    c.add_chunk(*b"RPLY", 1, encode_payload(&cursor).unwrap())
        .unwrap();

    let bytes = c.encode().unwrap();
    let (decoded, warnings) = Container::decode_default(&bytes).unwrap();
    assert!(warnings.is_empty());

    let rply = decoded.chunk(*b"RPLY").unwrap();
    let back: ReplayCursor = decode_payload(&rply.payload).unwrap();
    assert_eq!(back, cursor);
}

#[test]
fn required_chunk_with_empty_payload_is_accepted() {
    // rf-core-api's StateView docs an empty mapper_state slice "for
    // mappers with no persistent state" — a required chunk's presence
    // check must not confuse "present with a zero-length payload" for
    // "absent".
    let mut c = Container::new(0, GOLDEN_ROM_SHA256, "0.1.0", 1_700_000_000);
    for tag in [
        *b"CPU_", *b"PPU_", *b"APU_", *b"WRAM", *b"VRAM", *b"OAM_", *b"CGRM", *b"CART",
    ] {
        c.add_chunk(tag, 1, vec![0]).unwrap();
    }
    c.add_chunk(*b"MAPR", 1, vec![]).unwrap(); // no persistent mapper state
    let bytes = c.encode().unwrap();

    let (decoded, warnings) = Container::decode_default(&bytes).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(decoded.chunk(*b"MAPR").unwrap().payload, Vec::<u8>::new());
}

#[test]
fn decompression_bomb_is_rejected_via_the_safety_cap() {
    // A small compressed input claiming an unbounded decompressed size
    // must fail fast (DecompressedTooLarge), not exhaust memory.
    let header_bytes = build_header_bytes(&golden_container().unwrap().header);
    let huge_zeros = vec![0u8; 65 * 1024 * 1024]; // 65 MiB, over the 64 MiB cap
    let compressed = zstd::encode_all(&huge_zeros[..], 3).unwrap();
    assert!(
        compressed.len() < 1024 * 1024,
        "a run of zeros should compress to well under 1 MiB"
    );

    let mut bytes = header_bytes;
    bytes.extend_from_slice(&compressed);

    let err = Container::decode_default(&bytes).unwrap_err();
    assert_eq!(
        err,
        ContainerError::DecompressedTooLarge {
            max: 64 * 1024 * 1024
        }
    );
}
