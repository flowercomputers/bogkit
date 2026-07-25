# Design plan: tactical reticles and silhouette skins

Status: **implemented**, pending device verification. The behavior is documented
in `docs/ARCHITECTURE.md`, which is authoritative; this file records the design
reasoning and the decisions taken along the way.

Deviations from the original proposal, all found by looking at rendered output:

- The digital camo first shipped at a 64-cell grid with a 5×5 blur, which read
  as television static rather than pixel camo. It is now a 24-cell grid with
  three 3×3 passes and a 12% dither into neighbouring bands.
- Palette bands are weighted (`bandShares`) rather than even quantiles. Even
  quantiles gave every colour 25% of the tile, which made the camo far darker
  and more contrasty than the reference; real pixel camo is mostly its two
  lighter tones with the darkest as an accent.
- The calibration target moved to `Canvas` as well. A 1.5pt plain white ring
  disappears against a bright frame; it needed the same contrast pass as the
  laser reticles, even though it stays white rather than red.

Two user-visible changes:

1. **Thinner, "laser sights" reticles.** Every crosshair in the app is
   currently drawn with 2.5–6pt strokes and heavy black halos. Restyle them to
   hairline weights with a glow-based legibility strategy.
2. **Player-selectable silhouette skins.** A player picks a skin for their own
   in-game silhouette; their opponent's phone renders the target with that
   pattern, fully opaque, instead of today's 28%-alpha red wash. Launch set:
   Green Tartan, Red Tartan, Pink Camo, Green Camo.

---

## Part 1 — Reticle restyle

### What exists today

| Surface | File | Current weights |
| --- | --- | --- |
| Sights crosshair (non-match) | `UntitledMobileFPS/SightsReticle.swift` | 46pt ring at 2.5pt over a 7pt glow ring, 2.5pt arms, 1.8pt ticks, 4pt centre dot |
| Match reticle (drawn at the aim point) | `UntitledMobileFPS/MultiplayerViews.swift` → `GameplayReticleOverlay` | 6pt black halo ring + 3pt colour ring, 5pt black bars + 2pt colour bars, 7pt centre dot |
| Scope entry ring | `UntitledMobileFPS/SightsReticle.swift` → `ScopeEntryIndicator` | 4pt track and progress |
| Calibration target | `UntitledMobileFPS/ContentView.swift` → `calibrationTargetOverlay` | 3pt ring, 2pt bars, 8pt dot |
| Debug aim dot | `UntitledMobileFPS/DebugOverlay.swift` → `drawAim` | 1.5pt, 8pt dot |

The thick strokes exist for one real reason: the reticle sits over a live
camera image and has to stay readable against a white wall as well as a dark
room. The current answer is a fat black halo. Going thinner means replacing
that answer, not just lowering the numbers.

### Legibility strategy for hairline strokes

Draw every reticle element in three passes, from back to front:

1. **Bloom** — the same path, `lineWidth + 3`, in the accent colour at ~0.30
   alpha, inside a `context.drawLayer` with `.blur(radius: 3)` and
   `.blendMode(.plusLighter)`. This reads as emitted light and is what makes it
   feel like a laser sight rather than a printed crosshair.
2. **Contrast** — the same path, `lineWidth + 1.2`, in `.black.opacity(0.55)`.
   That leaves a 0.6pt dark edge on each side of the stroke: enough to hold the
   line against a bright background, thin enough that the reticle still reads
   as a hairline.
3. **Core** — the path at its nominal `lineWidth` in the accent colour at full
   opacity.

`SightsReticle` already uses `Canvas`, which supports `drawLayer`, `addFilter`,
and `blendMode` directly. `GameplayReticleOverlay` is built from stacked
`Shape` views; port it to `Canvas` so both share one drawing routine.

### Shared style tokens

New file `UntitledMobileFPS/ReticleStyle.swift`:

```swift
struct ReticleStyle {
    var hairline: CGFloat = 1.0      // ticks, secondary marks
    var primary: CGFloat = 1.4       // rings and crosshair arms
    var contrastPadding: CGFloat = 1.2
    var bloomPadding: CGFloat = 3.0
    var bloomRadius: CGFloat = 3.0
    var bloomOpacity: Double = 0.30
    var contrastOpacity: Double = 0.55
    var laser = Color(red: 1.0, green: 0.17, blue: 0.21)
    static let `default` = ReticleStyle()
}
```

Plus a `GraphicsContext.strokeTactical(_ path:, color:, width:, style:)`
helper that runs the three passes. The project guidance allows local display-only
geometry constants, but the point of the token struct is that the four
surfaces stay visually consistent when we tune them, so it is worth the file.

`ReticleStyle.swift` is SwiftUI-only, so add it to the `exclude` list in
`Package.swift` alongside `SightsReticle.swift` (the package `sources` list is
explicit, so it will not be compiled by `swift test` either way; the exclude
entry just keeps SwiftPM quiet).

### Per-surface targets

**Sights reticle** — the "aiming down the barrel" view; it should feel like
glass optics, not a HUD.

- Ring: 46pt → 34pt diameter, 2.5pt → 1.4pt. Drop the separate 7pt glow ring;
  the bloom pass replaces it.
- Crosshair arms: 2.5pt → 1.4pt, gap from centre 31pt → 24pt, outer reach
  84pt → 92pt. Longer and thinner reads more tactical than short and thick.
- Ticks: 1.8pt → 1.0pt, 20pt wide → 9pt wide, keep the two rings at 48/66pt
  (re-space to 44/60/76 for a three-step ladder — mil-dot cadence).
- Centre: 4pt dot → 2.5pt core with a 7pt bloom. This is the "laser dot".
- Add a thin 1.0pt vertical post from the bottom arm to the ring edge for the
  tactical silhouette. Optional; cut it if it reads as clutter on device.

**Match reticle** — sits over the opponent, so it must occlude as little as
possible.

- Halo/ring: 6pt black + 3pt colour → a single 1.4pt ring with the three-pass
  treatment. Radius stays derived from `reticleRadiusFraction`.
- Bars: 5pt black + 2pt colour → 1.0pt, and **clipped to outside the ring** so
  the centre of the target is never covered. Today the bars run edge to edge
  through the middle.
- Centre dot: 7pt → 3pt core plus bloom.
- Status colours stay as they are (`green` / `orange` / `white` per
  `GameplayTargetingStatus`) so hit-readiness is unchanged; only weight
  changes. The bloom uses the status colour, not the fixed laser red.
- Status label typography is unchanged.

**Scope entry ring** — 4pt → 2.5pt track and progress, same copy and geometry.

**Calibration target** — 3pt → 1.5pt ring, 2pt → 1.0pt bars, dot 8pt → 5pt.
Stays white, not laser red: it is an instruction marker, not a sight.
The project guidance requires calibration targets stay unobscured, and thinner strictly
helps.

**Debug aim dot** — leave alone. The debug overlay is a diagnostic surface and
its weights are already thin.

### Risks

- Hairlines over a noisy camera image can shimmer. The contrast pass should
  prevent it; verify on device against a white wall, a window, and a dark room
  before calling it done.
- `plusLighter` over a bright background saturates to white. If the bloom
  washes out in daylight, drop the blend mode and keep the plain blurred
  layer — the contrast pass is doing the real legibility work.

### Verification

No unit-testable logic here. Verify by running the app on device (the `/run`
skill) and comparing before/after screenshots in the four states: sights
reticle, match reticle on and off target, scope-entry partial fill, and each
calibration target.

---

## Part 2 — Silhouette skins

### Reference

`docs/` intentionally does not carry the reference art. The visual target is:
red tartan (crimson / black / bone, with a 45° hatch on the dark bands), pink
blob camo, and sage digital/pixel camo, each filling an opaque human
silhouette.

### The four launch skins

| Skin | Family | Palette |
| --- | --- | --- |
| Red Tartan | tartan | crimson `#B4232A`, black `#1A1512`, bone `#F2EAD8` |
| Green Tartan | tartan | forest `#1E5B34`, black `#12180F`, bone `#EDE7D2` |
| Pink Camo | blob camo | hot pink `#E8628C`, mid pink `#D98CA6`, pale pink `#F6D3DC`, dark brown `#2B1E1A` |
| Green Camo | digital camo | light `#C9CDBD`, sage `#9FAE95`, olive `#5E6B4F`, charcoal `#3A3F35` |

Mirroring the reference, the two camos deliberately use different families —
pink is organic blob camo, green is pixel/digital camo — so the four skins are
distinguishable at a glance across a room, which is the whole job of a target
marker.

### Procedural tiles, not bundled art

**Recommendation: generate the pattern tiles procedurally with Core Graphics
at first use and cache them.**

- No asset pipeline, no licensing question, no binary blobs in git.
- A seeded LCG makes the output byte-identical on every device, so both
  players see the same pattern and it does not shimmer between frames.
- The palettes and geometry live in the portable core, so they are testable.

The alternative — four seamless PNG tiles in the asset catalog — gives more
art control and less code. It is the right call if the procedural camo looks
cheap on device; the rendering seam is one function (`tile(for:)`), so
swapping the source later is contained. Decide after seeing the first
procedural pass on hardware.

Generation, all into a 256×256 tile:

- **Tartan** — base fill, then horizontal and vertical bands at fixed offsets
  and widths, multiplied where they cross, plus paired 1px bone lines and a
  45° hatch over the dark bands.
- **Blob camo** — ~14 seeded blobs, each a closed path of 10 points at
  jittered radii, painted in palette order. Each blob is drawn nine times at
  ±tile offsets so the tile wraps seamlessly.
- **Digital camo** — a 4px cell grid filled from seeded value noise thresholded
  into the four palette bands, with the noise lattice computed modulo the grid
  size so the tile wraps.

### File layout

- `UntitledMobileFPS/SilhouetteSkin.swift` — **added to `Package.swift`
  `sources`**. Contains the enum, display names, palettes as plain RGB
  triples, and the family tag. Pure data, no UIKit, unit-testable.
- `UntitledMobileFPS/SilhouetteSkinRenderer.swift` — **added to `exclude`**.
  Core Graphics tile generation, `UIImage` cache, and the SwiftUI swatch view.

```swift
enum SilhouetteSkin: String, Codable, CaseIterable, Sendable {
    case redTartan = "red_tartan"
    case greenTartan = "green_tartan"
    case pinkCamo = "pink_camo"
    case greenCamo = "green_camo"

    static let fallback: SilhouetteSkin = .redTartan
}
```

Raw values are wire and persistence format; they must not change once shipped.

### Rendering the target

`TargetSilhouetteOverlay` (`UntitledMobileFPS/MultiplayerViews.swift`) today
draws the mask as a `.template` image tinted red at `opacity(0.28)`. Replace
the tint with a tiled pattern masked by the same alpha image:

```swift
Image(uiImage: SilhouetteSkinRenderer.tile(for: skin))
    .resizable(resizingMode: .tile)
    .frame(width: proxy.size.width, height: proxy.size.height)
    .mask { maskLayer }                      // existing mask image + rect crop
    .saturation(eliminated ? 0 : 1)
    .opacity(eliminated ? 0.6 : 1)
```

- **Fully opaque** (`opacity(1)`) for a live target, as requested.
- **Eliminated targets** desaturate to grey and drop to 0.6, satisfying
  `PLAN.MD`'s "silhouette fades to grey" without losing the shape.
- Tile scale is **fixed in screen points** (~110pt), not scaled by target
  distance. The pattern reads as a decal on the target rather than as fabric
  at true scale, which keeps it identifiable at any range.
- The bounding-box stroke stays, restyled to the Part 1 hairline treatment and
  tinted with the skin's accent colour instead of always red. It remains the
  dashed fallback when no mask is available.

### The real risk: opacity exposes the mask

At 28% alpha, segmentation error is invisible. At 100% it is the main thing
you see. Two specific problems, both worth measuring before locking opacity at
1.0:

1. **Edge quality.** `PersonTargetingRunner.personMask` runs
   `VNGeneratePersonSegmentationRequest` at `qualityLevel = .fast`. If the
   edges look chewed, raise the display path to `.balanced` while keeping
   `.fast` for the collision mask, or feather the mask alpha by one pixel.
   Note that quality level costs frame time on the targeting queue.
2. **Staleness.** `submit` throttles to one mask every 0.30s (≈3.3 Hz), so an
   opaque body visibly lags a moving target — it will "swim" behind the real
   person. Mitigations, in order of cost: crossfade between consecutive masks
   (~0.15s), reuse the bounding box as the live element it already is, or
   raise the submission rate. Do not raise the rate without re-measuring
   `metrics` on device.

Ship with the fill opacity as a single named constant in the renderer so it
can be dialled back after the first device test without touching layout.

### Choosing and transporting a skin

The skin is chosen by a player and rendered on their **opponent's** phone, so
it has to travel with the appearance profile.

**Swift** — `AppearanceProfile` (`UntitledMobileFPS/MultiplayerModels.swift`)
gains `let skin: SilhouetteSkin?`. Decode leniently: a missing key **and** an
unrecognised string both decode to `nil`, so an older server or a newer
client's skin can never fail the whole profile. Render `skin ?? .fallback`.

**Rust** — `AppearanceProfile` (`src/lib.rs`) gains
`#[serde(default)] pub skin: Option<String>`. The struct is already
`rename_all = "camelCase"`, so the wire key is `skin` either way. The server
stores and returns it verbatim; it never interprets the value. Also set a skin
on the fixture profile in `src/bin/fps-bot.rs` (suggest
`green_camo`) so the bot target is visibly skinned — that is the only way to
test this without a second phone.

**Changing a skin must not require re-enrolling photos.** `EnrollmentCache`
already persists the full profile locally, so no new endpoint is needed: clone
the cached profile with the new skin and re-`uploadAppearance`. Add to
`AppSession`:

```swift
func setSilhouetteSkin(_ skin: SilhouetteSkin) async
```

which uploads the amended profile, updates `appearanceProfile`, rewrites the
enrollment cache, and calls `game.setAppearanceProfile`. If no profile exists
yet, store the choice in `UserDefaults` and apply it when
`enrollAppearance` builds its candidate profile.

### Picker UI

- **`ProfileView`** (`UntitledMobileFPS/AppViews.swift`, APPEARANCE section) —
  a horizontal row of four swatches, each a small silhouette glyph filled with
  the live-generated tile, with a selection ring and the display name beneath.
  Tapping runs `setSilhouetteSkin`; show the existing `busy` state during the
  upload and surface failures through `session.message`.
- **`AppearanceEnrollmentView`** — after a successful enrollment, present the
  same picker as the final step so first-run players leave with a skin chosen
  rather than silently defaulting.
- Non-target people in frame are unaffected: they are not rendered as
  silhouettes at all today, and that does not change.

### Tests

`swift test` (portable core, `UntitledMobileFPSTests/MultiplayerTests.swift`
or a new `SilhouetteSkinTests.swift`):

- Every `SilhouetteSkin` raw value matches its expected string — a
  regression here silently breaks persisted profiles.
- `AppearanceProfile` JSON without `skin` decodes with `skin == nil`.
- `AppearanceProfile` round-trips a skin through encode/decode.
- An unknown skin string decodes to `nil` rather than throwing.
- Palette invariant: every skin has at least three distinct colours (cheap
  guard against a copy-paste palette).

Rust (`src/lib.rs`): serde round-trip covering a profile with
and without `skin`, asserting the field survives an upsert.

Tile generation itself is visual; verify on device.

### Documentation to update when this lands

- `docs/ARCHITECTURE.md` — the new profile field and the overlay's rendering
  path.
- `docs/PHASE_2_TESTING.md` — how to verify skins against the bot fixture.
- `README.md` — only if it describes the HUD or profile fields.

---

## Suggested sequencing

1. **Reticle restyle.** Self-contained, no protocol change, immediately
   demoable. Ship first.
2. **Skin data plumbing.** Enum, `AppearanceProfile` field, Rust field, bot
   fixture, tests. No visible change yet, so it can land while the renderer is
   still being tuned.
3. **Renderer, opaque silhouette, and picker.** The part that needs device
   time for edge quality and staleness.

## Open questions

- Should the local player ever see their own skin (lobby preview, killcam
  replay)? Assumed no for now beyond the profile swatch.
- Are skins a per-profile cosmetic (assumed) or a per-account unlockable that
  should survive re-enrollment independently? Per-profile is simpler and
  matches the current storage; revisit if unlocks are planned.
- Should an eliminated target keep its skin desaturated (assumed) or switch to
  a flat grey silhouette? `PLAN.MD` says "fades to grey"; desaturation is the
  closer reading and keeps the shape legible.
