# Game profiles

A **profile** tells RetroForge about one game: which copies it matches (by
their hashes), where the game keeps its camera, and where it keeps things
like lives and health. Profiles are what unlock Game-Aware features and
[Game info](game-info.md).

## Profiles that come with RetroForge

About 65 games have a profile already, including Super Mario Bros. 1–3,
Super Mario World, The Legend of Zelda, Zelda II, A Link to the Past,
Metroid, Super Metroid, Contra, Super C, Contra III, the Castlevanias,
Mega Man 1–7 and X1–X3, DuckTales and many more.

Each camera in them was found by playing that exact copy of the game: the
byte the game copies into the screen's scroll register on every frame it
moves. Their items (lives, health, coins …) are cited from community RAM
maps such as Data Crystal and TASVideos, each with a link to its source, and
most were checked by watching them change in play. A profile matches the
copy it was made from; a different revision of the same game needs its own.

## When a game has no profile

The Enhancements section says so, and offers two ways to make one.

![No profile yet, with the offer to make one](tour/40-profile-make-offer.png)

### Let RetroForge find the camera while you play

Just play. For a game with no camera, RetroForge watches the screen scroll
in the background (start the game first, and play somewhere the screen
moves). When it is sure, a card says **Found this game's camera**; **Use
it** saves it into a profile for this game, making one if there is none.

![The camera found while playing](tour/43-camera-found.png)

### Make a profile by hand

**Make a profile for this game** creates one and opens the profile
builder beside the running game. It finds an address by asking which way a
value moved:

1. Press **Take the first look**.
2. Make the thing change in the game (walk right until the screen scrolls),
   then press **It went up**; walk back and press **It went down**; stand
   still and press **It didn't change**.
3. When a dozen or fewer addresses are left, press **Use this** on one.

The steps are the camera (left/right and up/down), the player's position,
and lives.

| | |
|---|---|
| ![Narrowing the search](tour/41-profile-builder-finding.png) | ![Saved](tour/42-profile-builder-saved.png) |

Profiles you make are saved in your own folder,
`~/Library/Application Support/retroforge/profiles`, and are used straight
away. For the file format, see [Profiles](profiles.md).
