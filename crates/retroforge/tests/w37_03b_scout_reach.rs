//! **W37-03b: how far the level scout reaches into Super Mario Bros. 1-1 at 32:9** (ROM-gated,
//! law 5: needs the mapper-66 multicart in `RF_SCREENS_ROMS_DIR`).
//!
//! The table is the finder's (the cart's shipped profile has no `[scout]`), with the reach the app
//! gives a 32:9 picture. Before W37-03b the canvas ended about 580 px in: the walker stood at the
//! first pipe (world x about 448) and, its margin measured from where the pass began already
//! filled, gave up. A walker along X now jumps at a wall (`scout::sidestep_pad`), margin filled or
//! not. Measured on this path: 579 px before, 1,274 px after (the walker falls into the first pit).
//!
//! ```text
//! cargo test --release -p retroforge --test w37_03b_scout_reach -- --ignored --nocapture
//! ```

#[allow(dead_code, unused_imports)]
#[path = "w33_07_common/mod.rs"]
mod discovery;
mod w36_06b_common;

use rf_enhance::widescreen::Aspect;

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const PROFILE: &str = "../../profiles/nes/super-mario-bros/profile.toml";
/// The app's widescreen line count (`app::WIDESCREEN_LINES`).
const LINES: usize = 224;
/// The NES picture's width.
const SOURCE: usize = 256;

/// The SMB recipe's object-slot flags (what the app holds at 0 for a mod session, W36-06c).
fn enemy_flags() -> Vec<(u32, u8)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros/recipe.toml");
    let recipe = rf_profiles::load_recipe_file(&path).expect("the SMB recipe parses");
    retroforge::margin_draw::object_flag_addrs(&recipe)
        .into_iter()
        .map(|a| (a, 0))
        .collect()
}

/// The W36-06c path: the live-enemies image, scouted with the enemies held off, entered the way
/// that capture enters 1-1. The walker meets only the level.
#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn smb_1_1_scout_reaches_past_the_first_pipe() {
    reach(true);
}

/// The plain game: the walker also meets the first Goombas (and, by luck of where it stands, got
/// past the first pipe even before W37-03b; measured 826 px then).
#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn smb_1_1_scout_reach_with_enemies() {
    reach(false);
}

fn reach(mod_session: bool) {
    let walk = ["R+B14 R+B+A24"; 12].join(" ");
    let Some((skeleton, ..)) = discovery::discover_game(
        ("nes", FILE, PROFILE),
        "w200 S6 w500",
        &format!("L10 w5 {walk}"),
        false,
    ) else {
        return;
    };
    let mut plan = retroforge::scout_discovery::plan_of(&skeleton);
    println!("finder start_when: {:?}", plan.start_when);
    assert!(
        plan.start_when
            .iter()
            .all(|(addr, _)| !retroforge::scout_find::NES_STACK_PAGE.contains(&(*addr as usize))),
        "the finder named a stack byte as a start condition: {:?}",
        plan.start_when
    );
    plan.reach_px = u32::try_from(
        Aspect::Wide32x9
            .output_width(LINES, 1.0)
            .saturating_sub(SOURCE)
            / 2,
    )
    .expect("fits");
    // As the app sets them (w36_06c_screens): the profile's scene key, a short settle.
    plan.settle_frames = 30;
    plan.scene_key =
        rf_profiles::load_file(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(PROFILE))
            .expect("profile")
            .profile
            .scene
            .as_ref()
            .map(retroforge::scene_key::SceneKeySpec::from_profile);

    let mut rig =
        discovery::Rig::open("nes", FILE, Some(PROFILE)).expect("the ROM was found above");
    if mod_session {
        plan.hold.extend(enemy_flags());
        let image = w36_06b_common::mod_image(&rig.rom);
        rig.st = retroforge::stepper::EmuStepper::open(&image).expect("opens");
        rig.st.set_event_mask(rf_core_api::EventMask::ALL);
        rig.rom = image;
        // Start twice, the intro waited out, the pad idle until the level runs, Mario's injury
        // timer run up (the mod's first Goomba is alive).
        for f in 0..620u32 {
            let pad = if f < 500 {
                w36_06b_common::script_two(f)
            } else {
                0
            };
            rig.frame(u16::from(pad));
        }
    } else {
        rig.script("w200 S6 w500");
    }
    // Right until the gate opens: the finder's level-state rows read as in a level while Mario
    // moves (docs/design/REBUILD_LAB.md, W36-06c). Bounded: 300 frames.
    for _ in 0..300 {
        if retroforge::scout::start_gate_open(&plan.start_when, |a| rig.st.peek_bus(a)) {
            break;
        }
        if mod_session {
            rig.st.poke_bus(0x079E, 0xFF);
        }
        rig.script("R1");
    }
    assert!(
        retroforge::scout::start_gate_open(&plan.start_when, |a| rig.st.peek_bus(a)),
        "the start gate never opened while Mario walked"
    );
    let rom = rig.rom.clone();
    let (width, line) = discovery::scout_width(&mut rig, &rom, plan, 300);
    println!("canvas width {width} px; {line}");
    let scene = rig.live.current_scene().expect("a scene");
    let level = rig
        .live
        .scene_canvases(scene.0)
        .and_then(|set| set.iter().max_by_key(|c| c.width() * c.height()))
        .expect("the level layer");
    // Scenery, not a strip of the sky colour stitched past the end: every 64-px column block
    // below the HUD rows is filled.
    let (x0, y0) = level.origin();
    let right = x0 + level.width() as i64;
    let filled: Vec<usize> = (x0..right)
        .step_by(64)
        .map(|bx| {
            (bx..(bx + 64).min(right))
                .flat_map(|x| (y0 + 40..y0 + level.height() as i64).map(move |y| (x, y)))
                .filter(|&(x, y)| level.get(x, y).is_some())
                .count()
        })
        .collect();
    println!("filled per 64 px: {filled:?}");
    assert!(
        width > 700,
        "the scout reached {width} px; the first pipe is at about 448"
    );
    assert!(
        filled.iter().all(|&n| n > 0),
        "a column block with nothing in it"
    );
}
