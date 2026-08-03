//! `Container`: the assembled `.rfstate` file — header plus chunk list,
//! encode/decode, and the determinism-safe state hash.

use std::io::Read;

use sha2::{Digest, Sha256};

use crate::chunk::{decode_chunks, encode_chunks, Chunk};
use crate::cursor::Cursor;
use crate::error::{ContainerError, LoadWarning};
use crate::header::Header;
use crate::migrate::MigrationRegistry;
use crate::tags::{is_required, tag_info, TAG_REGISTRY};

const ZSTD_LEVEL: i32 = 3;

/// Safety cap on the decompressed chunk body. Not a wire-format field —
/// SAVE_STATES.md §2 does not specify a decompressed-size prefix, so this
/// is enforced by bounding how many bytes we will ever read out of the
/// zstd decoder, independent of what the file claims. A full SNES state
/// is well under 1 MiB; 64 MiB leaves generous headroom while still
/// making an inflate-bomb (small compressed input claiming unbounded
/// output) fail fast instead of exhausting memory.
const MAX_DECOMPRESSED_BODY_BYTES: usize = 64 * 1024 * 1024;

/// An assembled `.rfstate` container: header plus chunk list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// The container header.
    pub header: Header,
    chunks: Vec<Chunk>,
}

impl Container {
    /// Start a new, empty container. `timestamp` is caller-injected
    /// (never sampled internally) — see `header.rs`'s module doc comment
    /// for why that matters for determinism and golden-fixture stability.
    pub fn new(
        console: u8,
        rom_sha256: [u8; 32],
        emu_version: impl Into<String>,
        timestamp: u64,
    ) -> Self {
        Self {
            header: Header {
                container_version: crate::header::CONTAINER_VERSION,
                console,
                flags: 0,
                rom_sha256,
                emu_version: emu_version.into(),
                timestamp,
            },
            chunks: Vec::new(),
        }
    }

    /// Add a chunk. Errors if `payload` is too large to fit the wire
    /// format's `u32` length field.
    pub fn add_chunk(
        &mut self,
        tag: [u8; 4],
        version: u16,
        payload: Vec<u8>,
    ) -> Result<(), ContainerError> {
        u32::try_from(payload.len()).map_err(|_| ContainerError::PayloadTooLarge {
            tag,
            len: payload.len(),
        })?;
        self.chunks.push(Chunk {
            tag,
            version,
            payload,
        });
        Ok(())
    }

    /// All chunks currently in the container, in insertion/parse order.
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// The first chunk with the given tag, if present.
    pub fn chunk(&self, tag: [u8; 4]) -> Option<&Chunk> {
        self.chunks.iter().find(|c| c.tag == tag)
    }

    /// Refuse a state whose `rom_sha256` does not match `expected` (the
    /// normalized hash of the ROM the caller is trying to load this state
    /// against). `rf-state` does not compute ROM hashes itself (no
    /// dependency on `rf-cart` — see the ticket's layering note); callers
    /// pass in a hash they computed elsewhere.
    pub fn verify_rom(&self, expected_sha256: [u8; 32]) -> Result<(), ContainerError> {
        if self.header.rom_sha256 != expected_sha256 {
            return Err(ContainerError::RomMismatch {
                expected: expected_sha256,
                found: self.header.rom_sha256,
            });
        }
        Ok(())
    }

    /// Serialize to bytes: header (uncompressed) followed by a single
    /// zstd stream over the encoded chunk list.
    pub fn encode(&self) -> Result<Vec<u8>, ContainerError> {
        let mut out = Vec::new();
        self.header.write(&mut out)?;

        let mut body = Vec::new();
        encode_chunks(&self.chunks, &mut body)?;

        let compressed = zstd::encode_all(&body[..], ZSTD_LEVEL)
            .map_err(|e| ContainerError::Decompression(e.to_string()))?;
        out.extend_from_slice(&compressed);
        Ok(out)
    }

    /// Parse `bytes`, applying `migrations` to any known chunk whose
    /// version does not match this build's expectation.
    ///
    /// Behavior (SAVE_STATES.md §2 "Rules", FR-STATE-004):
    /// - Unknown chunk tags are kept (not discarded) and reported as
    ///   [`LoadWarning::UnknownChunk`] rather than causing a load failure.
    /// - A missing *required* (core-set) chunk is a hard, named error.
    /// - A known chunk at the wrong version is migrated if a function is
    ///   registered for its tag, or fails with a hard, named error if not.
    pub fn decode(
        bytes: &[u8],
        migrations: &MigrationRegistry,
    ) -> Result<(Container, Vec<LoadWarning>), ContainerError> {
        let mut cur = Cursor::new(bytes);
        let header = Header::read(&mut cur)?;
        let compressed = cur.into_rest();

        let body = decompress_capped(compressed)?;
        let mut chunks = decode_chunks(&body)?;

        for info in TAG_REGISTRY.iter().filter(|t| t.required) {
            if !chunks.iter().any(|c| c.tag == info.tag) {
                return Err(ContainerError::MissingRequiredChunk { tag: info.tag });
            }
        }

        let mut warnings = Vec::new();
        for c in &mut chunks {
            match tag_info(c.tag) {
                Some(info) if c.version == info.current_version => {}
                Some(info) => match migrations.migrate(c.tag, c.version, &c.payload) {
                    Some(Ok(migrated)) => {
                        warnings.push(LoadWarning::Migrated {
                            tag: c.tag,
                            from: c.version,
                            to: info.current_version,
                        });
                        c.payload = migrated;
                        c.version = info.current_version;
                    }
                    Some(Err(e)) => return Err(e),
                    None => {
                        return Err(ContainerError::CannotMigrate {
                            tag: c.tag,
                            found: c.version,
                            expected: info.current_version,
                        })
                    }
                },
                None => warnings.push(LoadWarning::UnknownChunk { tag: c.tag }),
            }
        }

        Ok((Container { header, chunks }, warnings))
    }

    /// [`Container::decode`] with an empty [`MigrationRegistry`] — any
    /// version mismatch will be a hard `CannotMigrate` error.
    pub fn decode_default(bytes: &[u8]) -> Result<(Container, Vec<LoadWarning>), ContainerError> {
        Self::decode(bytes, &MigrationRegistry::new())
    }

    /// Deterministic hash of machine state, for FR-STATE-002/007 roundtrip
    /// and cross-mode comparisons.
    ///
    /// Scope is deliberate: only **required (core-set)** chunks
    /// contribute, sorted by tag, each as `tag || version_le(u16) ||
    /// len_le(u32) || payload`. This means:
    /// - Header fields (`timestamp`, `emu_version`, `rom_sha256`,
    ///   `console`, `flags`, `container_version`) are **never** part of
    ///   the input — two states with identical core machine state hash
    ///   identically regardless of when/how they were saved (see
    ///   `header.rs`'s determinism note).
    /// - Optional chunks (`ENHC`, `PROF`, `INPT`, `RPLY`) are excluded, so
    ///   an Enhanced-mode save and an Accuracy-mode save of the same
    ///   logical machine state hash identically (FR-STATE-007) even
    ///   though only the former carries enhancement chunks.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut required: Vec<&Chunk> = self.chunks.iter().filter(|c| is_required(c.tag)).collect();
        required.sort_by_key(|c| c.tag);

        let mut hasher = Sha256::new();
        for c in required {
            hasher.update(c.tag);
            hasher.update(c.version.to_le_bytes());
            let len = u32::try_from(c.payload.len()).expect(
                "payload length invariant: add_chunk/decode_chunks guarantee this fits u32",
            );
            hasher.update(len.to_le_bytes());
            hasher.update(&c.payload);
        }
        hasher.finalize().into()
    }
}

fn decompress_capped(compressed: &[u8]) -> Result<Vec<u8>, ContainerError> {
    let decoder =
        zstd::Decoder::new(compressed).map_err(|e| ContainerError::Decompression(e.to_string()))?;
    let mut limited = decoder.take(MAX_DECOMPRESSED_BODY_BYTES as u64 + 1);
    let mut body = Vec::new();
    limited
        .read_to_end(&mut body)
        .map_err(|e| ContainerError::Decompression(e.to_string()))?;
    if body.len() > MAX_DECOMPRESSED_BODY_BYTES {
        return Err(ContainerError::DecompressedTooLarge {
            max: MAX_DECOMPRESSED_BODY_BYTES,
        });
    }
    Ok(body)
}
