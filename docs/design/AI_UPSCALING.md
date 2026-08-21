# Design: AI upscaling — pack pipeline, cache keys, and what "reproducible" means

Ticket **W8-10**. Implements `docs/design/ENHANCEMENT_RUNTIME.md` §6 and
**FR-AI-001..004**. Written against Brad's 2026-08-20 ruling on the ticket,
which authorised adding an ONNX runtime and doing the inference.

## 1. Shape of the pipeline

```
extracted assets ──► animation::group ──► pipeline::build_pack ──► PackManifest
   (indexed pixels        (§6 grouping)      │                      + images
    + palette)                               ├─ sheet_of   (one sheet per set)
                                             ├─ Upscaler   (ONE call per sheet)
                                             ├─ split_sheet
                                             └─ CacheKey   (FR-AI-002)
```

Four modules, and the split is deliberate:

| Module | Owns | Needs a model? |
|---|---|---|
| `pack` | asset/settings hashing, `CacheKey`, all-or-nothing refusal | no |
| `animation` | §6's sprite → animation-set grouping | no |
| `pipeline` | sheeting, coherence, key assembly | no |
| `upscale` | the `Upscaler` seam + exact integer fallback | no |
| `onnx` | the ONNX-backed `Upscaler` | **yes**, and it is feature-gated off |

Only the last one needs a runtime. That is the point: everything the
honesty contract rests on is testable with no model, no runtime and no
network, so `cargo test --workspace` keeps CLAUDE.md's "no external deps"
promise and CI never grows an artifact.

## 2. Zero AI on the frame path

FR-AI-001 is explicit, and the structure enforces it rather than asking
politely: `Upscaler` is called only from `pipeline::build_pack`, which is
an offline pack builder. The composer looks assets up in the cache by
`CacheKey` and falls back to the original — it has no reference to a model,
a session, or this crate's `onnx` module, which in a default build does not
exist.

This also keeps the determinism invariant (ARCHITECTURE.md §3) untouched.
Nothing here runs on the core thread or mutates core state; a pack changes
what is *displayed*, downstream of a simulation that ran identically
whether the pack existed or not.

## 3. Animation sets are upscaled together

§6: "so a pack upscales a character **coherently instead of per-frame**".

Every member of an animation set is laid out on one sheet (`sheet_of`),
upscaled in a **single** `Upscaler::upscale` call, then cut back apart
(`split_sheet`). The motivation is concrete: upscaling each frame of an
eight-frame walk cycle independently gives eight subtly different
characters that flicker between one another in motion — invisible in a
screenshot, glaring in play.

An asset in no set becomes a one-member sheet and goes through the same
path. One code path, not a special case that only rare inputs exercise.

**The layout is a plain left-to-right row, not a packed atlas**, because it
must be reconstructible *exactly* by `split_sheet` from the member sizes
alone. A bin-packer's placements would have to be stored alongside the
sheet to be undone, which is state that can disagree with the image.

`split_sheet` re-derives geometry from the upscaler's declared `scale()`,
and the ONNX implementation **checks that declaration against the tensor
the model actually returned**. A silent mismatch would slice every frame at
the wrong offset rather than failing — the kind of bug that produces a pack
full of quietly misaligned sprites.

## 4. What "reproducible from its inputs" is allowed to mean

Criterion 1 asks for a pack "cache-keyed so a pack is reproducible from its
inputs". This section exists because the obvious reading is **not
achievable**, and shipping the phrase without saying so would be an
overclaim of exactly the species the honesty contract exists to prevent.

**Float inference is not bit-reproducible.** The same ONNX graph, run under
a different runtime version or a different execution provider (CPU vs
CoreML vs CUDA), can produce different pixels. Reassociation, fused
multiply-add, and differing kernel implementations are all permitted to
change the last bits. So `model_id` — which names the *weights* — does not
pin the output.

FR-AI-002's key is `(rom hash, asset hash, model id, settings hash)`. The
runtime is not in that tuple. Rather than widen a requirement's key or
quietly accept a key that fails to cover its output, the pipeline folds
`Upscaler::provenance()` into the **settings** map under the reserved key
`rf.provenance`:

- `NearestUpscaler` reports `exact/integer` — fixed, because integer pixel
  replication has no runtime, no float rounding and no execution provider.
- `OnnxUpscaler` reports `onnx/<dylib path>` — the runtime is an input to
  the pixels, so it is an input to the key.

A caller that supplies `rf.provenance` itself is **refused**
(`PipelineError::ReservedSetting`): being able to set it by hand would let
two different runtimes share one cache key, which is the silent
substitution this whole mechanism prevents.

### The resulting claim, stated exactly

**Guaranteed.** Identical inputs and identical provenance produce an
identical key, an identical manifest, and identical entry ordering. The key
covers everything that can change the bytes, so a changed runtime produces
a *different key* rather than different pixels behind an unchanged one. A
pack built with `NearestUpscaler` is additionally bit-identical on every
machine.

**Not guaranteed.** That two different machines running the same ONNX model
produce identical pixels. They may not, and nothing in this design pretends
otherwise.

This is why the exact integer upscaler is not a placeholder. It is the
reference the pipeline's tests are written against, and the only path that
can promise the strong form.

## 5. Runtime and model artifacts

Per the ruling: **no model weights in git**, fetch-and-pin like the ROM and
vector fetchers, gitignored, and CI has no artifacts.

`ort` is taken `default-features = false, features = ["load-dynamic"]`. The
runtime library is `dlopen`'d at run time from a path the caller supplies,
so there is no build-time download and no vendored blob. `OnnxUpscaler::load`
uses `ort::init_from`, which **returns an error** when the dylib is absent;
`ort`'s lazy default path `expect()`s and would abort the process
(`ort-2.0.0-rc.13/src/lib.rs:234`). A missing optional dependency must not
be a crash.

See `docs/TECH_STACK.md`'s "AI upscaling" row for the full dependency
record, including why `default-features = false` is load-bearing (the
default set pulls 73 crates and a CDLA-Permissive-2.0 licence that is not
on `deny.toml`'s allowlist; `load-dynamic` pulls 20 and needs no allowlist
change at all).

### Licence posture for model weights

NFR-011 governs *dependencies*. A fetched, hash-pinned, never-redistributed
weights file is an **artifact**, and the precedent for artifacts is
`tests/rom-manifest.toml`: each carries a `license_status`, including
`NoLicenseGrantFetchOnly` for "no licence grant from upstream — fetch from
origin only, never vendored, mirrored or re-hosted by us".

**No model is shipped, named or fetched by this ticket.** The pipeline is
model-agnostic — `model_id` is a string the caller supplies — so the choice
of weights is a decision this design deliberately leaves open rather than
one it makes silently. Whoever adopts a specific model must record its
licence status against the categories above, and Brad's STOP condition
applies: if the weights' licence fails NFR-011, record it rather than
shipping it.

## 6. What this ticket does not do

- **No fetcher for the runtime or a model.** `scripts/**` is outside
  W8-10's `write_scope`.
- **No `local-gate.sh` row and no `docs/evidence/local-gate.json` entry**
  for an inference test. Both files are outside `write_scope`. The ONNX
  path's behaviour against a real model is therefore **unproven by
  automation** — see the HANDOFF note on the ticket. This is stated plainly
  rather than left for a reader to infer from an absent test.
- **No pack editor / review UI.** §6's "user-reviewable" is a surface, and
  this is the format and pipeline underneath it. `BuiltPack` returns images
  in memory rather than writing them, precisely so a review step can sit
  between building and installing.
