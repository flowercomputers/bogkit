# Development guide

## Toolchain and dependencies

The application target requires iOS 17+, Xcode, SwiftUI, AVFoundation, Vision, Nearby Interaction, UIKit, and CocoaPods. The Rust service is the `untitled-mobile-fps` package in the enclosing BogKit workspace and requires Cargo. MediaPipe is declared as:

```ruby
pod 'MediaPipeTasksVision', '~> 0.10.0'
```

The repository also defines a Swift 6 package manifest for portable core tests. The Xcode project currently compiles sources in Swift 5 language mode. Keep both build surfaces working when changing shared files.

## Initial setup

From `examples/untitled-mobile-fps`:

```sh
pod install
open UntitledMobileFPS.xcworkspace
```

Use the workspace for app development because CocoaPods supplies MediaPipe. Select the `UntitledMobileFPS` scheme, choose a development team if required, and run on an iPhone. The configured bundle identifiers are example identifiers and may need to be changed for signing in your environment.

The LAN match client intentionally bypasses configured HTTP proxies and times out requests after eight seconds. Opening server selection starts the declared `_untitledfps._tcp` Bonjour browser to trigger Local Network authorization while the app is foregrounded. Choose **Custom Server** or open `http://<mac-lan-address>:3000/health` on the phone before debugging enrollment. The built app must contain `NSLocalNetworkUsageDescription` and `NSBonjourServices`; if access was previously denied, re-enable the app under **Settings › Privacy & Security › Local Network**.

`FPS_DEFAULT_SERVER_URL` and `FPS_DEFAULT_SERVER_NAME` are build settings expanded into `Info.plist`. Debug defaults to the local simulator service; release builds deliberately require a deployed HTTPS URL or custom server selection. Public HTTP endpoints are rejected, while localhost and RFC1918 LAN addresses remain available for development. Server URLs must be origins with no path component.

The bundled `UntitledMobileFPS/hand_landmarker.task` model must be present when opting into MediaPipe timing/classification diagnostics. `CameraService()` leaves that path disabled to avoid its camera-frame cost during normal play; use `CameraService(mediaPipeDiagnosticsEnabled: true)` for comparison work. Its skeleton is not drawn. If the model is absent or incompatible, Vision gameplay continues.

## Build surfaces

### Portable core

`Package.swift` includes:

- `Models.swift`;
- `FingerGunClassifier.swift`;
- `GestureStateMachine.swift`;
- `Aiming.swift`;
- `LandmarkRecording.swift`;
- `PreviewGeometry.swift`;
- `MultiplayerModels.swift`;
- `AppearanceMatching.swift`;
- `AppModels.swift`.

It excludes camera, SwiftUI, Vision integration, MediaPipe integration, the app entry point, and the model asset. Keep new core logic independent of UIKit-only or third-party APIs where possible and add its source file to the package target when needed.

### iOS application

The Xcode workspace builds the app and `UntitledMobileFPSTests`. Use a physical device to validate camera orientation, latency, heat, gesture ergonomics, permission recovery, and calibration stability.

## Commands

Install or update pods after a `Podfile` change:

```sh
pod install
```

Run all portable tests:

```sh
swift test
```

Build and test the Bogkit service:

```sh
cargo test -p untitled-mobile-fps
cargo run -p untitled-mobile-fps
```

Build for a generic simulator without signing:

```sh
xcodebuild -workspace UntitledMobileFPS.xcworkspace \
  -scheme UntitledMobileFPS \
  -destination 'generic/platform=iOS Simulator' \
  build
```

Run Xcode tests on an installed simulator by replacing the destination with a simulator available from `xcrun simctl list devices available`:

```sh
xcodebuild -workspace UntitledMobileFPS.xcworkspace \
  -scheme UntitledMobileFPS \
  -destination 'platform=iOS Simulator,name=<available iPhone>' \
  test
```

Simulator builds validate compilation and portable behavior, not rear-camera interaction.

## Test organization

- `FingerGunClassifierTests.swift`: 3D MediaPipe-style and 2D Vision pose classification, handedness, barrel variation, confidence, and invalid poses.
- `AimingTests.swift`: 3D calibration/aim compatibility, Vision five-target calibration, target movement, bad fits, nine-zone aim, recording compatibility, and deterministic replay.
- `GestureStateMachineTests.swift`: stable arming, one-shot firing, rearm, variation changes, and tracking-loss timing.
- `PreviewGeometryTests.swift`: Vision Y-axis flip, aspect-fill crop, and rectangle conversion.
- `MultiplayerTests.swift`: tagged JSON compatibility, automatic descriptions, embeddings, and scoped score fusion.
- `AppModelsTests.swift`: canonical server URLs, safe development transports, and match-readiness gates.
- `SightsAimingTests.swift`: proximity measurement invariance to hand roll and camera zoom, wrist independence, knuckle-dropout and outlier-pair tolerance, baseline warm-up, two-directional re-learning and no-latch regression, entry dwell and dropout grace, pose-gate rejection, hysteresis, held-scope retention, exit and retention grace, mode-chatter absence, non-monotonic timestamps, and centred shot resolution.

Prefer deterministic synthetic landmarks and explicit timestamps. Tests should not need camera hardware, model inference, wall-clock sleeps, or network access.

## Common change workflows

### Adjust pose recognition

1. Add a failing synthetic case to the relevant classifier test class.
2. Change thresholds in `GestureTuning` rather than embedding new classifier constants.
3. Check both valid variations and false-positive poses.
4. Verify calibration can still receive an aim feature when strict pose classification rejects an end-on index; this fallback is intentional.
5. Exercise representative recordings or device poses before shipping.

### Change aim features or calibration

1. Update calibration collector and solver together.
2. Add tests for all five calibration anchors and all nine output zones.
3. Preserve movement, stability, fit-quality, velocity, and residual rejection.
4. Bump `VisionAimCalibration.modelVersion` if saved feature semantics or solver requirements changed.
5. Update calibration and architecture documentation.

A model-version bump intentionally invalidates old `UserDefaults` entries because the storage key includes the version.

### Change trigger timing

Modify `GestureStateMachine` and the related `GestureTuning` values. Assert that a held-down thumb emits one shot only, a thumb-up sequence is required to rearm, and short/long tracking losses remain distinct. `armedPoseLatchSeconds` may bridge only an immediate thumb-down edge after a validated armed pose; it must never arm from fallback input or accept fallback thumb-up frames for rearming.

### Change person targeting

Keep identity scoring ahead of person selection. Add multi-person tests showing that the enrolled opponent wins over the old geometry favorite, an ambiguous identity tie does not acquire, and a transient challenger cannot switch the tracked lock. If changing cadence, update confirmation-frame values with the effective elapsed dwell in mind.

### Change sights activation

Modify `ScopeModeDetector`, `ScopeProximityBaseline`, or `HandProximityMeasure` in `SightsAiming.swift` plus the `scope*` values in `GestureTuning`. Keep entry a single monotonic scalar with hysteresis and a visible progress value; the earlier alignment-based gate was a conjunction of absolute in-frame positions, and it failed whenever any one of them marginally missed while telling the player nothing about which. Several invariants are load-bearing and have tests: the measurement must not change when the hand rolls (aspect correction) or when the camera zooms; it must not depend on the wrist; it must survive losing a knuckle; and the baseline must adapt in both directions without absorbing a deliberate approach. A one-directional baseline latched sights on for 81% of a device session, so any change that can only move the reference one way is a regression. Entry must not read the thumb, or scoping becomes indistinguishable from firing.

### Change camera processing

Keep session configuration off the main thread and UI publication on the main thread. Maintain portrait orientation and the preview's aspect-fill mapping. If parallelizing tracker work, do not share a locked `CVPixelBuffer` unsafely; copy or explicitly transfer ownership first. Validate interruption recovery and sustained runtime on a device.

### Change recording schema

Add optional fields when backward compatibility is possible. For a breaking change:

1. increment the writer schema version;
2. update `LandmarkReplay.load`'s accepted range;
3. add old/new decode tests and deterministic replay tests;
4. document the exact compatibility boundary.

Never add raw frames to diagnostic recording without an explicit product/privacy decision and a corresponding permission, storage, and retention design.

### Add a source file

Add it to the Xcode app target. If it is portable core logic, also add it to `Package.swift` and ensure it does not import app-only or MediaPipe modules. Confirm both `swift test` and the workspace build can see it.

## Runtime verification checklist

For camera, classification, calibration, or UI changes, verify on an iPhone:

- permission grant and denied-permission recovery;
- portrait preview orientation and skeleton alignment near every screen edge;
- center/left/right/up/down calibration advancement;
- rejection of holding the same pose for multiple targets;
- single- and double-barrel recognition;
- thumb-up arming and exactly one flash per thumb-down transition;
- deliberate thumb-up rearm;
- stable center/cardinal/diagonal reticle transitions;
- brief occlusion grace and longer-loss reset;
- Vision FPS/latency and dropped frames under sustained use;
- MediaPipe diagnostic failure not affecting gameplay;
- recording stop, export, JSON decode, and absence of image data;
- camera interruption/background and foreground resume;
- photo-derived description and appearance upload;
- body-plus-face capture and source-photo disposal;
- Keychain credential restoration and cross-server isolation;
- exact-handle requests, friend challenge acceptance, and server switching;
- foreground availability, background clearing, and invitation polling;
- invite/join/ready transitions and exactly three accepted hits;
- lobby/briefing acknowledgements, results, and completed-match history;
- red person-mask alignment with the preview crop;
- real UWB distance/direction on two capable phones, including token retry, `NO RANGE`, stale-reading, suspension, and failure HUD states;
- bot fallback behavior with UWB disabled;
- inspector-safe aggregate revisions and appearance search.

## Debugging guide

### App builds only from the project, then fails on MediaPipe import

Open `UntitledMobileFPS.xcworkspace`, not `UntitledMobileFPS.xcodeproj`, and run `pod install` if the workspace or Pods integration is stale.

### MediaPipe diagnostics are empty

Empty/zero MediaPipe values are expected in the default configuration. For explicit comparison work, construct `CameraService(mediaPipeDiagnosticsEnabled: true)`, confirm the model asset is bundled, and confirm CocoaPods resolved `MediaPipeTasksVision`. Only the solid Vision skeleton is expected; MediaPipe has no visible hand outline.

### Skeleton or reticle does not align with preview

Check all of these together:

- capture orientation is `.right`;
- `orientedImageSize` swaps pixel-buffer width and height;
- preview rotation is 90 degrees;
- preview and `PreviewGeometry` both use aspect fill;
- Vision `y` is flipped only when mapping to the view.

Add a `PreviewGeometryTests` case for the failing aspect ratio before changing the math.

### Calibration stalls

Inspect `CAL`, `FING`, `TH`, `REJECT`, and confidence in the HUD. Samples require thumb up, adequate hand confidence, settling time, cluster stability, and a deliberate feature change after each completed target.

### Calibration completes but aim disappears

The solver can reject mismatched model data, degenerate axes, excessive template residual, or an implausibly fast reticle jump. Record landmarks and inspect calibration plus Vision analysis rather than loosening all thresholds at once.

### Repeated shots while thumb stays down

Treat this as a state-machine regression. Add a deterministic test showing the observed thumb sequence; do not debounce only at the UI or flash layer.

## Generated and local files

Do not commit:

- `.build/`;
- `DerivedData/`;
- `Pods/`;
- `xcuserdata/`;
- `*.xcuserstate`;
- `.swiftpm/` workspace metadata.

`Podfile.lock`, the shared Xcode scheme, project/workspace metadata, model asset, notices, sources, tests, and documentation are intentional repository files.
