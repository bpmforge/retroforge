use rf_cheats::*;
use rf_core_api::{RamWrite, RomOverride};

fn rom(c: Cheat) -> RomOverride {
    match c.effect {
        CheatEffect::Rom(r) => r,
        CheatEffect::Ram(_) => panic!("expected ROM override"),
    }
}
fn ram(c: Cheat) -> RamWrite {
    match c.effect {
        CheatEffect::Ram(r) => r,
        CheatEffect::Rom(_) => panic!("expected RAM write"),
    }
}

// Vectors from https://tuxnes.sourceforge.net/gamegenie.html
#[test]
fn nes_gg_vectors() {
    let c = parse(Console::Nes, "GOSSIP").unwrap();
    assert_eq!(c.format, CodeFormat::NesGameGenie);
    assert_eq!(
        rom(c),
        RomOverride {
            addr: 0xD1DD,
            value: 0x14,
            compare: None
        }
    );
    let c = parse(Console::Nes, " zexpygla ").unwrap();
    assert_eq!(
        rom(c),
        RomOverride {
            addr: 0x94A7,
            value: 0x02,
            compare: Some(0x03)
        }
    );
    assert_eq!(
        parse(Console::Nes, "ZEXP-YGLA").unwrap(),
        parse(Console::Nes, "ZEXPYGLA").unwrap()
    );
    // The 8-letter vector re-encodes to itself.
    let o = rom(parse(Console::Nes, "ZEXPYGLA").unwrap());
    assert_eq!(encode_nes_game_genie(o), "ZEXPYGLA");
}

#[test]
fn nes_gg_round_trip() {
    for addr in (0x8000..=0xFFFFu32).step_by(0x1D) {
        for value in (0..=255u8).step_by(7) {
            for compare in [None, Some(0u8), Some(value), Some(0xFF), Some(value ^ 0x5A)] {
                let o = RomOverride {
                    addr,
                    value,
                    compare,
                };
                let s = encode_nes_game_genie(o);
                assert_eq!(s.len(), if compare.is_some() { 8 } else { 6 });
                assert_eq!(rom(parse(Console::Nes, &s).unwrap()), o, "{s}");
            }
        }
    }
}

#[test]
fn nes_gg_errors() {
    let e = parse(Console::Nes, "ZEXPYGLB").unwrap_err();
    assert!(matches!(e, CheatError::BadChar { ch: 'B', .. }), "{e}");
    assert!(e.to_string().contains('B'));
    assert!(matches!(
        parse(Console::Nes, "ZEXPY").unwrap_err(),
        CheatError::BadCodeLength { got: 5, .. }
    ));
    assert_eq!(parse(Console::Nes, "   ").unwrap_err(), CheatError::Empty);
}

#[test]
fn nes_par_and_raw() {
    let c = parse(Console::Nes, "0075FF").unwrap();
    assert_eq!(c.format, CodeFormat::NesActionReplay);
    assert_eq!(
        ram(c),
        RamWrite {
            addr: 0x0075,
            value: 0xFF,
            compare: None
        }
    );
    let c = parse(Console::Nes, "6000:0a").unwrap();
    assert_eq!(c.format, CodeFormat::Raw);
    assert_eq!(ram(c).addr, 0x6000);
    let c = parse(Console::Nes, "0075?03:09").unwrap();
    assert_eq!(
        ram(c),
        RamWrite {
            addr: 0x75,
            value: 9,
            compare: Some(3)
        }
    );
    let c = parse(Console::Nes, "C123?4F:99").unwrap();
    assert_eq!(
        rom(c),
        RomOverride {
            addr: 0xC123,
            value: 0x99,
            compare: Some(0x4F)
        }
    );
    assert!(matches!(
        parse(Console::Nes, "2000:01").unwrap_err(),
        CheatError::AddressOutOfRange { .. }
    ));
    assert!(matches!(
        parse(Console::Nes, "8000FF").unwrap_err(),
        CheatError::AddressOutOfRange { .. }
    ));
    assert!(matches!(
        parse(Console::Nes, "0075:1").unwrap_err(),
        CheatError::BadLength { field: "value", .. }
    ));
    assert!(matches!(
        parse(Console::Nes, "0075:01:02").unwrap_err(),
        CheatError::Malformed { .. }
    ));
}

#[test]
fn snes_gg_round_trip_and_shape() {
    for addr in (0..0x100_0000u32).step_by(0x3_0F7) {
        for value in [0u8, 1, 0x7F, 0x80, 0xFF, (addr & 0xFF) as u8] {
            let s = encode_snes_game_genie(addr, value);
            assert_eq!(s.len(), 9);
            assert_eq!(s.as_bytes().get(4), Some(&b'-'));
            let c = parse(Console::Snes, &s.to_lowercase()).unwrap();
            assert_eq!(c.format, CodeFormat::SnesGameGenie);
            assert_eq!(
                rom(c),
                RomOverride {
                    addr,
                    value,
                    compare: None
                }
            );
        }
    }
    // Zero maps to the Genie digit for 0, which is 'D'.
    assert_eq!(encode_snes_game_genie(0, 0), "DDDD-DDDD");
    // Data is the first two digits, unshuffled: 0xFF -> 'E','E'.
    assert_eq!(&encode_snes_game_genie(0, 0xFF)[..2], "EE");
}

#[test]
fn snes_gg_errors() {
    assert!(matches!(
        parse(Console::Snes, "DDDD-DDDZ").unwrap_err(),
        CheatError::BadChar { ch: 'Z', .. }
    ));
    assert!(parse(Console::Snes, "DDDD-DDD").is_err());
    assert!(parse(Console::Snes, "DDDD-DDDD-D").is_err());
    assert!(parse(Console::Snes, "DDDDD-DDD").is_err());
}

#[test]
fn snes_par_regions() {
    let r = |s: &str| parse(Console::Snes, s).unwrap();
    assert_eq!(r("7E0010FF").format, CodeFormat::SnesActionReplay);
    assert_eq!(ram(r("7E0010FF")).addr, 0x7E0010);
    assert!(matches!(r("00100005").effect, CheatEffect::Ram(_)));
    assert!(matches!(r("80000005").effect, CheatEffect::Ram(_)));
    assert!(matches!(r("7F123405").effect, CheatEffect::Ram(_)));
    assert!(matches!(r("70000005").effect, CheatEffect::Ram(_)));
    assert!(matches!(r("00800005").effect, CheatEffect::Rom(_)));
    assert!(matches!(r("C0000005").effect, CheatEffect::Rom(_)));
    assert!(matches!(r("70800005").effect, CheatEffect::Rom(_)));
    assert_eq!(rom(r("C0123405")).value, 5);
    assert_eq!(rom(r("C0123405")).addr, 0xC01234);
}

#[test]
fn snes_raw() {
    let c = parse(Console::Snes, "7E0010?0A:FF").unwrap();
    assert_eq!(c.format, CodeFormat::Raw);
    assert_eq!(
        ram(c),
        RamWrite {
            addr: 0x7E0010,
            value: 0xFF,
            compare: Some(0x0A)
        }
    );
    assert_eq!(
        rom(parse(Console::Snes, "C00001:07").unwrap()).addr,
        0xC00001
    );
    assert!(matches!(
        parse(Console::Snes, "0010:07").unwrap_err(),
        CheatError::BadLength {
            field: "address",
            expected: 6,
            got: 4
        }
    ));
}

#[test]
fn list_parsing() {
    let text = "# comment\n\nGOSSIP\n  \nZEXPYGLA + 0075:01\nnot a code\n";
    let r = parse_list(Console::Nes, text);
    assert_eq!(r.len(), 4);
    assert!(r[0].is_ok() && r[1].is_ok() && r[2].is_ok());
    let e = r[3].as_ref().unwrap_err();
    assert_eq!(e.line, 6);
    assert!(e.to_string().starts_with("line 6:"));
    assert!(parse_list(Console::Snes, "\n# x\n").is_empty());
    let r = parse_list(Console::Nes, "GOSSIP+");
    assert_eq!(r[1].as_ref().unwrap_err().error, CheatError::Empty);
}

#[test]
fn never_panics_on_garbage() {
    let mut inputs: Vec<String> = vec![
        String::new(),
        " ".into(),
        ":".into(),
        "?".into(),
        "-".into(),
        "+".into(),
        "::::".into(),
        "?:".into(),
        "é".repeat(8),
        "日本語コード".into(),
        "🙂🙂🙂🙂-🙂🙂🙂🙂".into(),
        "A".repeat(100_000),
        "\u{0}\u{1}\u{7f}".into(),
        "ZEXPYGLA\n".into(),
    ];
    let alphabet: Vec<char> = "0123456789ABCDEFGHIKLNOPSTUVXYZaz:-?+ #é\u{1F642}"
        .chars()
        .collect();
    for i in 0..4000usize {
        let len = i % 21;
        let mut k = i.wrapping_mul(2_654_435_761);
        let s: String = (0..len)
            .map(|_| {
                let c = alphabet[k % alphabet.len()];
                k = k / alphabet.len() + 7 * i + 1;
                c
            })
            .collect();
        inputs.push(s);
    }
    for s in &inputs {
        for console in [Console::Nes, Console::Snes] {
            let _ = parse(console, s);
            let _ = parse_list(console, s);
        }
    }
    assert!(matches!(
        parse(Console::Nes, &"A".repeat(100_000)).unwrap_err(),
        CheatError::TooLong { .. }
    ));
}
