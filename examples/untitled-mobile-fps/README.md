# Untitled Mobile FPS

Untitled Mobile FPS is an iOS 17+ rear-camera finger-gun tech demo. Point a natural finger-gun pose, raise the thumb to arm it, and lower the thumb to fire one procedural muzzle flash. The app supports single-barrel (index extended) and double-barrel (index and middle extended) poses.

Apple Vision 2D hand landmarks drive gameplay. The MediaPipe 3D comparison path is diagnostics-only, disabled by default, and never determines the reticle, gesture state, or shot.

## Current scope

This BogKit submission implements phase one and a hackathon-sized phase-two 1v1 loop:

- live portrait camera preview from the rear wide-angle camera;
- Vision hand skeleton, pose classification, and rejection diagnostics;
- five-target, per-camera aim calibration;
- a stabilized nine-zone reticle: center, four cardinal directions, and four diagonals;
- single-shot thumb-trigger state with explicit rearming;
- a proximity-activated sights mode: draw the finger gun toward the phone to scope, with a fixed centre reticle and 1.25x zoom;
- a procedural muzzle flash at the solved reticle position;
- landmark-only diagnostic recording and export;
- opt-in MediaPipe timing/classification diagnostics without a second hand outline;
- default/custom server selection, device-bound accounts, and server-scoped Keychain credentials;
- guided body-and-face appearance enrollment with no self-reported outfit field;
- a readiness-gated Play hub plus Friends, History, and Profile navigation;
- an in-repository Rust backend built on Fold, ESE, and ANNy;
- exact-handle friend requests, friend challenges, and share-code matches;
- lobby/briefing/active/result flow, UWB token relay, and server-authoritative three-hit matches;
- an honest UWB radar that distinguishes token exchange, ranging, live distance, stale readings, suspension, and failure instead of drawing a placeholder contact;
- newest-first completed-match history and event details;
- match-scoped multimodal target comparison and a live Vision person mask, filled opaquely with the opponent's chosen silhouette skin (Red Tartan, Green Tartan, Pink Camo, Green Camo — patterns generated procedurally, so no art assets ship);
- an outfit-anchored target score: whole-body/garment signals decide the lock while face and silhouette can only confirm it within a bounded band, so no single modality carries a shot;
- a monochrome, e-fit-style stylized opponent briefing image (a real photo of a player never reaches the peer);
- random nearby matchmaking that pairs waiting players through the presence HNSW, with a one-shot Quick Match location upload on top of the otherwise location-free heartbeat;
- a browser inspector and deterministic bot fallback for one-person testing, now also surfacing live ANNy/BM25 index sizes, per-query search latency, and re-enrollment counts.

3v3, cross-device account recovery, push notifications, scene-depth hit reconstruction, background location, and killcam recording remain post-hackathon work. The reticle/mask intersection is a 2D hit gate, not a reconstructed 3D hit point. Nearby matchmaking is complete and server-tested; its on-device GPS/permission path still needs validation on two physical phones.

## Requirements

- macOS with a recent Xcode capable of building iOS 17 apps;
- an iPhone running iOS 17 or later for meaningful camera testing;
- CocoaPods for the MediaPipe Tasks Vision dependency;
- Rust/Cargo for the backend included in this BogKit workspace;
- a development team/signing identity for device deployment.

The app target is iPhone-only and portrait-only. The portable core package supports macOS 13+ and iOS 17+.

## Quick start

1. From the BogKit repository root, start the backend:

   ```sh
   cargo run -p untitled-mobile-fps
   ```

   The server listens on `0.0.0.0:3000`, stores state under
   `examples/untitled-mobile-fps/data`, and exposes a redacted inspector at
   <http://127.0.0.1:3000/inspector>.

2. In another terminal, install the iOS dependency from the submission directory:

   ```sh
   cd examples/untitled-mobile-fps
   pod install
   open UntitledMobileFPS.xcworkspace
   ```

3. Select the `UntitledMobileFPS` scheme and a physical iPhone.
4. Select a development team if Xcode requests one, then build and run.
5. Connect to the build-configured default server or choose **Custom Server**. On a physical phone, use the Mac's LAN URL rather than `localhost`.
6. Register a handle and display name, then complete the full-body and face captures. The app generates the outfit description from the body photo.
7. Open the calibration checklist item. Keep the thumb raised and demonstrate center, left, right, up, and down until each target advances automatically.
8. Challenge a friend, create or join a code, or use **Solo test** in a debug build. Both players ready, inspect and acknowledge the opponent briefing, then enter camera gameplay.
9. To use sights, hold the finger gun at a comfortable distance for about a second so the baseline settles, then draw it back toward the phone until the ring fills.

The app connects directly to private LAN addresses instead of sending them through a configured Wi-Fi proxy. The complete phone-plus-bot and two-phone scripts are in [`docs/PHASE_2_TESTING.md`](docs/PHASE_2_TESTING.md).

Calibration is saved in `UserDefaults` for the physical camera identifier. It is shared by single- and double-barrel poses. The debug Profile screen can reset it for the active camera.

## Sights mode

Sights engage from a gesture, with nothing to touch on screen: hold a finger gun, then **draw it back toward the phone**, the way a real weapon comes up to the eye. A ring near the bottom of the screen fills as the hand approaches; when it completes, the HUD reads `SIGHTS`, the camera zooms to 1.25x, and every shot resolves to the fixed centre crosshair. Lower the gun again and the mode releases on its own.

Sights need no saved calibration, because the firing point is always camera centre. Shots taken while scoped run through the same gameplay targeting as unscoped shots, so a scoped hit registers against the opponent mask normally. Starting calibration forces unscoped 1x mode until collection ends.

You can always tell which mode you are in: a `SIGHTS`/`HIP` badge sits at the top of the camera view, the scoped state darkens the frame edges and closes red brackets in around the centre, and each transition fires a haptic tap so you do not have to be looking at the edges of the screen to notice.

What the detector measures is the apparent size of the knuckle line compared against a running baseline of the player's own relaxed hold. A few properties follow from that choice and are worth knowing when tuning it:

- **Only the four knuckles are used — not the wrist, not the finger.** The index barrel foreshortens badly when it points away from the lens, which is exactly the pose a player scopes from. The wrist is worse: it is the first landmark lost as the hand approaches, because it leaves the bottom of the frame, and its distance to the knuckles changes as the wrist flexes. Requiring it cost a quarter of all frames.
- **Six knuckle pairs each estimate the same thing and vote.** Any pair that disagrees with the others is discarded, so one bad landmark cannot move the result, and losing a knuckle degrades the measurement rather than killing the frame. If too much of the hand is corrupted the frame is skipped instead of guessed at.
- **The measurement is aspect-corrected and zoom-normalised.** Vision's normalised coordinates stretch x relative to y, so without correction merely rolling the hand would change the reading; and the scoped 1.25x ramp would otherwise feed back into its own retention test.
- **The baseline is a low percentile of recent unscoped frames.** It adapts in both directions, so a hold at a new distance is re-learned as normal, but a brief pull-in is too small a minority of the window to be absorbed.
- **Holding the gun up keeps you scoped**, for as long as you hold it — the baseline stops sampling while scoped. You leave sights by lowering the gun.
- **A very slow drift will not scope.** That is intended: sights require a deliberate movement, not a gradual creep.
- **Entry consumes no thumb signal.** Pulling the hand closer does not change thumb geometry, so entering sights can never be confused with a trigger pull. The thumb stays exclusively the trigger.
- **A pose gate still applies.** An open palm pushed at the lens is rejected, so proximity alone cannot scope.

## Using calibration well

Use a comfortable, repeatable pose rather than trying to place the fingertip directly over each target. Aim the finger gun naturally in the requested direction, keep the thumb up, hold steady, and make a deliberate movement between targets. Calibration rejects low-confidence, unstable, insufficiently separated, or poorly fitted samples.

The calibrated input is translation- and scale-normalized around the palm. Horizontal and vertical axes come from the center/left/right/up/down examples; diagonal directions are synthesized by combining those axes. Runtime aim is classified into one of nine stable zones.

See [`docs/CALIBRATION_AND_DIAGNOSTICS.md`](docs/CALIBRATION_AND_DIAGNOSTICS.md) for failure modes, HUD labels, recording fields, and tuning guidance.

## App navigation

| Destination | Behavior |
| --- | --- |
| **Play** | Shows server/account/appearance/calibration readiness, pending challenges, friend challenges, share codes, and the debug-only solo path. |
| **Friends** | Finds an exact handle, manages requests, and removes existing friends. |
| **History** | Lists completed matches and their participant/hit/event details. |
| **Profile** | Shows the server-scoped identity and appearance, switches servers, and exposes developer resets plus the latest debug-data export. |

During gameplay, the debug build can record/export landmark diagnostics. Leaving automatically finalizes an active recording, and the newest saved JSON remains exportable from Play or Profile after the match and after relaunch. Person targeting and shot submission start only after the server reports an active two-player match.

The radar reads `NO RANGE` until Nearby Interaction produces a real peer update. Its status line then explains whether tokens are still exchanging, ranging is waiting for distance, a reading went stale, or the session failed. A red direction marker and numeric distance are evidence of a live UWB reading; the warning icon is not.

## Privacy and data

Camera frames and live Vision processing stay on-device. The source enrollment photos are discarded after analysis; the app uploads a generated briefing thumbnail, an automatically generated description, and numeric descriptors to the configured match server. Global discovery indices contain outfit text and non-face whole-body features only; face and body-region signals are available only to briefing/active match peers. Device account credentials are scoped per stable server ID in the iOS Keychain, and the backend persists only SHA-256 token hashes. Foreground friend availability uses a location-free heartbeat; a coarse location is uploaded only when the player explicitly taps Quick Match, and solely to pair them with a nearby opponent. The public inspector omits invite codes, identities, appearance payloads, and coordinates. Diagnostic recordings remain landmark-only and leave the device only when explicitly exported.

## Development and tests

Run framework-independent classifier, calibration/aim, gesture-state, replay, and preview-coordinate tests with:

```sh
swift test
```

Run the authenticated backend, social/history, and bot tests with:

```sh
cargo test -p untitled-mobile-fps
```

The Swift package intentionally excludes AVFoundation/SwiftUI/MediaPipe integration files so core behavior can be tested without an iOS runtime. App integration should additionally be built from the workspace and camera behavior should be exercised on an iPhone.

Thresholds for classification, calibration, tracking loss, filtering, sights proximity/zoom, and gesture timing are centralized in `GestureTuning` in `UntitledMobileFPS/Models.swift`.

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): runtime pipeline, ownership, threading, data models, and persistence.
- [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md): setup, build/test workflows, and safe change recipes.
- [`docs/CALIBRATION_AND_DIAGNOSTICS.md`](docs/CALIBRATION_AND_DIAGNOSTICS.md): calibration model, trigger behavior, HUD, recordings, and troubleshooting.
- [`docs/PHASE_2_TESTING.md`](docs/PHASE_2_TESTING.md): one-person bot workflow, two-phone UWB workflow, and layer-by-layer debugging.
- [`PLAN.MD`](PLAN.MD): product direction beyond the current prototype.
- [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md): MediaPipe and model attribution.

## Repository layout

```text
UntitledMobileFPS/          App and core Swift sources
UntitledMobileFPSTests/     Portable XCTest suite
src/                        Fold/ESE/ANNy service, shared protocol, and test bot
docs/                       Architecture and operating guides
Cargo.toml                  BogKit workspace package; defaults to the server
Package.swift               Core-only Swift package used by swift test
Podfile                     MediaPipe Tasks Vision dependency
UntitledMobileFPS.xcworkspace  CocoaPods-enabled Xcode workspace
UntitledMobileFPS.xcodeproj    Native app and test targets
```

The standalone development history is also available at
<https://github.com/rcrdlbl/untitled-mobile-fps>.

## Known boundaries

- Physical-device validation is essential; simulator camera behavior is not representative.
- Vision provides 2D landmarks and unknown handedness in this pipeline. The calibration model is therefore camera-specific, not user- or hand-specific.
- MediaPipe is disabled in the normal `CameraService()` configuration. Developers can opt into it for timing/classification comparisons; failure remains non-fatal.
- The bundled `hand_landmarker.task` supports that optional diagnostic path, while the Vision gameplay path neither loads nor runs the model by default.
