# Profiles

A profile teaches RetroForge facts about one game — memory addresses,
decode rules, HUD regions, on-screen text — as **data**, never code. It
is the first of P9's three deliverables and needs no Rust at all.

The format is specified in `docs/design/GAME_PROFILES.md`; this chapter
quotes the schema section and a real, validated profile.

## The schema

{{#include ../../design/GAME_PROFILES.md:schema}}

## A complete, validated profile

Below is `profiles/nes/rf-scroller-demo/profile.toml` in full — the same
file `retroforge-tool profile validate` checks in CI, so what you are
reading is known to parse and to use only keys the schema knows.

Note what it is **not**: it declares no `[text]` entries for RF-Scroller
itself, because that fixture draws no text. A profile that invented
strings a ROM never renders would be a profile that lies about what a
game says.

```toml
{{#include ../../../profiles/nes/rf-scroller-demo/profile.toml}}
```
