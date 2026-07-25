# Architecture

## System overview

Untitled Mobile FPS is a SwiftUI camera application with three orchestration boundaries. `AppSession` owns server-scoped identity, onboarding readiness, social/history loading, and root navigation. `GameplayCoordinator` owns the selected live match, opponent data, Nearby Interaction, and match commands. `CameraService` owns the latency-sensitive camera, gesture, calibration, and targeting pipeline. The Rust package under `src/` provides authenticated, persistent BogKit materializations and server-authoritative match state.

The runtime pipeline is:

```text
Rear camera (AVCaptureSession)
  -> BGRA CVPixelBuffer in portrait orientation
  -> Apple Vision hand-pose request
  -> hand selection and 2D landmark smoothing
  -> Vision finger-gun classification
  -> hand-proximity sights detection
  -> unscoped: five-target calibration lookup/collection
  -> unscoped: fitted continuous aim and nine-zone diagnostic aim
  -> sights: fixed centre aim and 1.25x camera zoom
  -> thumb gesture state machine
  -> reticle, HUD, and muzzle flash
  -> landmark-only diagnostic recording

During an active match, after Vision releases the frame:
  -> stable Vision person selection
  -> segmentation of the selected person crop
  -> match-scoped multimodal target score
  -> opaque skinned silhouette and reticle containment

When MediaPipe diagnostics are explicitly enabled:
  -> MediaPipe Hand Landmarker
  -> independent hand selection and 3D classification
  -> comparison metrics only; no second hand outline
```

Vision is authoritative. MediaPipe is disabled by default, and its results never feed calibration, aim, arming, firing, or the visible gameplay state.

## Component responsibilities

### Application and UI

- `UntitledMobileFPSApp.swift` creates the single `ContentView` scene.
- `AppViews.swift` supplies server selection, registration, Play/Friends/History/Profile tabs, appearance enrollment, calibration, lobby, briefing, and result routes.
- `AppSession.swift` restores one Keychain credential and enrollment cache per stable server ID, computes match readiness, and coordinates social/history stores with the live match.
- `ContentView.swift` supplies the full-screen gameplay camera, active-match overlays, diagnostic controls, and lifecycle handling.
- `GameplayCoordinator.swift` owns match snapshots, WebSocket state, opponent profile, UWB, shot submission, and bot fallback.
- `MultiplayerViews.swift` supplies the grayscale opponent briefing, radar/health HUD, and the skinned target/reticle overlays.
- `CameraPreview.swift` wraps `AVCaptureVideoPreviewLayer`, uses `.resizeAspectFill`, and rotates preview output 90 degrees for portrait.
- `DebugOverlay.swift` renders the authoritative Vision skeleton, HUD metrics, unscoped reticle, and muzzle flash, including a `PROX` row carrying the live proximity ratio, baseline, warmth, and entry progress. Note that this overlay is hidden during an active match unless match diagnostics are toggled on, so it cannot be the only thing that communicates aiming mode.
- `SightsReticle.swift` holds the sights chrome. `SightsFrameOverlay` — a vignette plus corner brackets — is the primary "you are scoped" signal and shows in *both* match and non-match, because a match is where telling the modes apart matters. It deliberately draws no crosshair, so it layers over `GameplayReticleOverlay` without stacking two reticles; the plain `SightsReticle` crosshair is drawn only outside a match, where nothing else supplies an aim point. `ScopeEntryIndicator` is the approach ring, and `AimingModeBadge` is an always-visible `SIGHTS`/`HIP` pill. `GameplayCameraView` also fires a distinct haptic on each mode transition, since a player watching the target will not necessarily notice a change at the edges of the screen. The first device test reported the mode was "really hard to tell", which it was: the only cues were a debug row hidden during matches, a crosshair suppressed during matches, and a 1.25x zoom far too subtle to read as a state change.
- `PreviewGeometry.swift` applies the same centered aspect-fill crop as the preview layer and converts Vision's lower-left normalized coordinates into UIKit's upper-left view coordinates.

### Camera orchestration

`CameraService.swift` is the integration boundary. It:

- requests camera permission and reports denial or unavailability;
- configures a high-preset rear wide-angle capture session;
- discards late capture frames and emits BGRA buffers;
- submits at most one Vision inference at a time;
- enables person segmentation only while a match is active and throttles it to roughly three submissions per second;
- when MediaPipe diagnostics are enabled, schedules them only after Vision and any accepted person-segmentation job have released the same pixel buffer;
- selects one stable hand from up to two candidates;
- smooths Vision image landmarks;
- runs sights detection, calibration, aim routing, and trigger state;
- ramps the rear camera between 1x unscoped and a clamped 1.25x sights zoom, and normalizes proximity by the zoom actually in effect;
- publishes UI state on the main queue;
- handles camera interruptions and media-service resets;
- records primary and diagnostic tracker data.

### Trackers

`VisionHandPoseDetector.swift` wraps `VNDetectHumanHandPoseRequest` with a maximum of two hands. It maps Vision's 21 recognized joints into `TrackedHand.imagePoints`. Vision does not populate `worldPoints`, `palmFrame`, or physical handedness; handedness is `.unknown`.

`MediaPipeHandTracker.swift` wraps the MediaPipe Tasks Vision live-stream hand landmarker. It maps image landmarks into the same lower-left coordinate convention and world landmarks into the repository camera-space convention. MediaPipe's handedness labels are inverted because its labels assume mirrored/selfie input while this app uses an unmirrored rear camera.

Both trackers implement `HandTracking`, allowing injection in tests or alternate orchestration. `CameraService()` does not instantiate MediaPipe. Passing `mediaPipeDiagnosticsEnabled: true` loads the bundled tracker, while injecting a tracker also enables the diagnostic path. An unavailable tracker is substituted if opt-in initialization fails, keeping Vision operational.

### Hand identity and smoothing

`HandSelector` scores candidates by confidence, visible bounds area, and proximity to the previous wrist. It locks handedness where available and resets after discontinuity. This reduces switching when two hands are visible or MediaPipe handedness flickers for end-on poses.

`VisionLandmarkSmoother` applies movement-sensitive interpolation to each 2D joint. Small jitter is damped; larger deliberate movements catch up faster. It resets after a tracking gap.

### Classification

`VisionFingerGunClassifier` is the gameplay classifier. It uses 2D joint angles, path-to-chord ratios, and thumb separation to classify each finger. A valid pose requires:

- index straight;
- ring curled;
- little curled;
- middle curled for a single barrel or straight for a double barrel;
- sufficient landmark confidence.

Thumb state is reported independently so both thumb-up and thumb-down finger guns remain valid observations. This lets the state machine see the trigger transition.

The Vision aim feature is built from palm-relative, palm-width-normalized index tip/PIP/DIP positions plus projected index length. Aim stays index-based for both barrel variations so middle-finger classification flicker does not change calibration feature space.

`FingerGunClassifier` is the MediaPipe/3D diagnostic classifier. It creates a palm-local coordinate frame, classifies finger extension, fits index and middle barrel rays, and rejects directions that do not point sufficiently away from the rear camera. Its output appears in diagnostics and portable tests but does not drive gameplay.

### Calibration and aim

The active path uses `VisionAimCalibrationCollector`, `VisionAimCalibrationStore`, and `VisionAimSolver` in `Aiming.swift`.

The collector gathers targets in this fixed order:

1. center;
2. left;
3. right;
4. up;
5. down.

Only thumb-up, sufficiently confident samples are accepted. Each target includes settling frames and a stable sample cluster. The collector also requires meaningful movement between targets, adequate target separation, and an acceptable regression fit.

The saved model includes feature normalization, regression coefficients, target centroids, standardized direction templates, cluster errors, fit RMSE, camera identifier, and model version. The runtime solver applies the fitted ridge-regression coefficients to the standardized feature for the continuous gameplay point. It separately derives horizontal and vertical axes from the five direction centroids, rejects excessive off-axis residual or velocity, and quantizes that diagnostic projection into one of nine `AimDirectionZone` values.

The nine-zone output deliberately stabilizes direction changes for several frames. `AimSolution.rawScreenPoint` contains the One-Euro-filtered continuous point and is exposed as `gameplayScreenPoint` for multiplayer. `AimSolution.screenPoint` remains the quantized calibration/diagnostic position.

The older 3D `AimCalibration`, `AimCalibrationCollector`, and `AngularAimSolver` types remain in the portable core for MediaPipe-era tests and replay compatibility. They are not wired into `CameraService`'s primary runtime path.

### Sights detection and aim routing

`SightsAiming.swift` holds the whole sights contract and consumes only authoritative Vision output.

`HandProximityMeasure` reduces a frame to one scalar: apparent hand scale in knuckle-width units, aspect-corrected and divided by the live camera zoom. The aspect correction matters because Vision's normalised coordinates scale x and y differently, so without it hand roll alone would move the measurement by the frame's aspect ratio. Dividing by the live zoom stops the scoped 1.25x ramp feeding back into the mode's own retention test.

It measures only the four MCP knuckles. Two landmark groups were rejected on device evidence. The index barrel foreshortens badly in the very pose sights are entered from. The wrist is worse: measured against knuckle width over confident frames it varied by an IQR of 0.29–0.34 (wrist flexion changes the distance and the landmark wanders) versus 0.02–0.09 for knuckle-to-knuckle spans, and it is the first landmark lost as the hand approaches because it leaves the bottom of the frame — median confidence 0.14 on frames where measurement failed. An earlier version required two wrist spans and lost 24% of frames, biased toward exactly the close poses sights depends on.

Each of the six knuckle pairs independently estimates the same quantity by dividing its measured length by its nominal proportion of knuckle width (constants taken as medians over high-confidence device frames). Estimates are combined by median, then any disagreeing with that median by more than `scopeProximityPairDisagreement` is dropped and the median retaken, so a single bad landmark cannot move the result. Because every pair estimates the same quantity, a partial pair set stays comparable with the baseline rather than silently measuring something else — which is what lets the measurement survive dropout at all. `scopeMinimumProximityPairs` frames with too few surviving pairs are reported as unmeasured, and a grossly corrupted hand is refused rather than guessed at: a missing frame cannot trigger anything, a confidently wrong scale can.

`ScopeProximityBaseline` is a low percentile (`scopeBaselinePercentile`) of the spans seen over a sliding `scopeBaselineWindowSeconds` window, sampled only while unscoped, and warm only once it holds both enough samples and enough elapsed time.

This replaced an exponential average that stopped updating whenever the current sample was elevated, and that freeze was a one-way latch: because every elevated sample was ignored, the reference could only ever move *down*, so one low reading — a distant or half-tracked hand — pinned it there permanently and every later frame read as "close". On a device recording the baseline sat unchanged for the first 16 seconds and sights were engaged for 81% of the session, flipping mode 9 times in 25 seconds. A percentile over a sliding window cannot latch by construction, because it is recomputed from scratch each frame and can move in both directions. It still resists absorbing the gesture, since a fraction of a second of approach is a small minority of a multi-second window and a low percentile ignores it, and it is inherently robust to outlier spans. Sampling only while unscoped is now the *only* freeze rule, and it is what keeps a held scoped pose from drifting back under the exit threshold — holding the gun up keeps you scoped for as long as you hold it.

`ScopeModeDetector` then applies dwell and hysteresis to that one ratio: entry at `scopeEnterProximityRatio` sustained for `scopeEntrySeconds` with a short loss grace for dropped frames, release at `scopeExitProximityRatio` sustained for `scopeExitSeconds`, and a `scopeRetentionLossSeconds` grace for measurement gaps. Ratios above `scopeMaximumProximityRatio` are treated as broken landmarks rather than a very close hand. A single monotonic scalar with hysteresis replaced an earlier conjunction of absolute in-frame position gates, which failed whenever any one gate marginally missed and gave the player no indication of which. `diagnostic` exposes span, baseline, ratio, progress, and warmth per frame; `entryProgress` drives the on-screen ring in `ScopeEntryIndicator` so the threshold is visible rather than guessed at.

Entry deliberately consumes no thumb signal. Drawing the hand closer leaves thumb geometry unchanged, so entering sights cannot be mistaken for a trigger pull, and the thumb remains exclusively the trigger. `ScopePosePolicy` supplies the pose half of the gate: it normally uses the strict finger-gun observation but tolerates an index classified curled when aim landmarks stay confident, ring/little are not straight, and the thumb is unambiguous — so an open palm pushed at the lens still cannot scope.

`AimingModePolicy` keeps the two aim contracts explicit. Unscoped observations remain calibration- and valid-aim-gated and publish the solver's `AimSolution`. Sights observations bypass calibration, publish no directional solution, and resolve every shot to normalized Vision point `(0.5, 0.5)` via `gameplayPoint`, which `CameraService` feeds into the same `GameplayTargetEvaluator` path as an unscoped shot — a scoped hit is evaluated against the opponent mask exactly like any other. Entering or leaving sights resets aim and trigger state so a cross-mode thumb transition cannot fire.

Sights detection is suppressed during calibration. Calibration start, lifecycle stop, interruption, and reset restore unscoped mode and request 1x zoom. Zoom failure is non-fatal; the fixed reticle and centred firing contract remain available.

### Trigger state

`GestureStateMachine.swift` implements:

```text
NO HAND -> CANDIDATE -> ARMED -> FIRED -> REARM -> ARMED
```

- Stable thumb-up observations move from candidate to armed.
- Thumb-down while armed emits exactly one `fired` event.
- Once armed, a 0.18-second pose latch can consume a thumb-down edge from the same selected hand when ring/little labels flicker during the physical press. The latch cannot arm, accept thumb-up, or rearm, and it still requires a usable aim feature and gameplay aim.
- Holding the thumb down cannot fire again.
- Stable thumb-up frames are required to rearm.
- Short tracking loss preserves state; a longer loss resets it.
- `CameraService` supplies no observation to the state machine until calibration exists.

### Diagnostics and recording

`DebugOverlay` shows primary Vision landmarks as solid cyan/pink. When explicitly enabled, MediaPipe contributes timing and comparison values to the HUD but does not draw another hand outline. Its values remain empty/zero in the default configuration. The overlay also displays classification state, calibration state, rejection reasons, confidence, continuous/quantized aim, zone, tracker performance, and Vision dropped-frame count. Active matches hide this diagnostic layer by default and place one authoritative continuous crosshair above it; the in-match Debug control restores diagnostics without changing the shot point.

`DiagnosticRecorder` collects `LandmarkRecordingFrame` entries from the Vision path and `TrackerLandmarkSample` entries from both trackers. Fired frames optionally include continuous/zone points, target box and age, identity score, mask coverage, and the frozen local gate result; multiplayer frames also preserve the current Nearby Interaction status and reading. Leaving a camera view finalizes an active recording, and `CameraService` discovers the newest file in Documents at startup so Play/Profile can export it after match exit or relaunch. `LandmarkReplay` validates schema/model compatibility, sorts frames by timestamp, and replays them deterministically. The JSON contains no image or mask pixels.

## Multiplayer and appearance

`AppearanceAnalyzer` accepts a full-body image and a dedicated face image. It explicitly requests a full-body Vision rectangle, divides the person into upper/lower/head regions, and derives the outfit description automatically from the body image. (`VNDetectHumanRectanglesRequest` otherwise defaults to upper-body-only detection.) The top and bottom are named by MobileCLIP zero-shot classification (`OutfitZeroShotClassifier`): each garment crop is embedded with the bundled image encoder and cosine-matched against precomputed text-label embeddings (`OutfitLabels.json`, generated by `scripts/generate_label_embeddings.py` with the same model's text tower); color and garment type are marginalized independently. This classifies the garment semantically rather than by pixel color, so exposed skin no longer forces every top to read as "orange". When the label set or image encoder is unavailable, it falls back to a deterministic dominant-color sampler that skips skin-like pixels, plus Vision garment classification. The face image must contain a detected face and supplies the match-scoped descriptor and briefing thumbnail. Source images are released after analysis, and the user supplies a display name but never an outfit description.

Visual crops are embedded by a bundled MobileCLIP2-S0 Core ML image encoder (`MobileCLIPEmbedder`), producing 512-dimension unit-length vectors identified by `mobileclip2-s0-image-512-v1`. When the model is absent or inference fails, `AppearanceFeatureExtractor.embedding` falls back to the deterministic `bogshot-color-grid-512-v2` color-grid hash; the profile's `embeddingModel` reflects whichever encoder actually ran, so ANNy never compares vectors across encoders. The descriptor space is `vision-full-body-descriptor-v2`. The app discards cached profiles with older model identifiers so upper-body-derived vectors and descriptions are not silently reused. `MobileCLIPImageEncoder.mlpackage` is produced by `scripts/convert_mobileclip.py` (see `scripts/README_MOBILECLIP.md`) and is bundled in the app target only; the SwiftPM core library stays Core ML-free.

`RealtimeMatchClient` uses a short-lived direct-LAN URL session with proxying disabled, an eight-second request timeout, and actionable transport errors. Server selection starts a declared `_untitledfps._tcp` Bonjour browse in the foreground so iOS can present and register Local Network authorization before the first HTTP request. `/health` must return a nonempty stable server ID plus protocol and capabilities. Custom URLs are origin-only; paths are rejected so API prefixes cannot be silently discarded. `AppSession` keys credentials and cached enrollment by server and player ID, preventing a custom server or replacement account from receiving another identity or profile.

`NearbyInteractionService` starts only on devices that report precise-distance support. It buffers a peer discovery token that arrives before the local `NISession`, relays the local token once per second until the first actual nearby-object update, and ignores duplicate configured tokens. Receiving a peer token idempotently ensures that the local session exists, while an iOS-invalidated session is recreated after a short delay; current match snapshots provide an additional lifecycle checkpoint. The server keeps the latest discovery token for each player in an in-memory, match-scoped cache: after forwarding a new token to its peer, it also returns the peer's cached token to the sender. This store-and-forward handshake lets either phone's retry recover when the other phone sent its token before the receiving WebSocket subscribed. A reading expires from the HUD after 1.2 seconds, ahead of the server's 1.5-second reciprocal-proximity limit. The radar renders direction and distance only from a live `NINearbyObject`; otherwise it shows `NO RANGE` plus the actual exchange, ranging, stale, suspended, unsupported, or error status. WebSocket token/report failures are surfaced rather than discarded.

Account registration returns an opaque device credential stored in Keychain. Private HTTP calls use it as a bearer token. Before realtime connection, the client exchanges it for a one-use, 60-second ticket bound to the selected match; the WebSocket URL carries the ticket, never the bearer credential or a client-claimed player ID. The server emits only the selected match and suppresses unchanged match revisions when presence or social state changes elsewhere.

Match entry is gated on a reachable server, restored account, body-and-face enrollment, and current camera calibration. Share-code and friend-targeted invitations converge on one server-authoritative state machine:

```text
lobby -> briefing -> active -> completed
```

Both players submit the exact current calibration model before briefing; protocol-v2 legacy readiness is rejected. Both acknowledge the opponent briefing before camera gameplay becomes active. Completion uses the server receipt time and materializes one per-player history entry. `AppSession` also publishes a location-free foreground availability heartbeat and polls social invitations while the hub is active; sentinel availability records never enter nearby search.

`PersonTargetingRunner` requests full-body person rectangles after the hand request releases the frame. It runs only during an active match and accepts at most one submission every 0.30 seconds. Every plausible person with a visible face first receives a lightweight grayscale structure score against the opponent's match-scoped briefing thumbnail. Mean/contrast normalization makes the live crop comparable with the posterized thumbnail, and checking both horizontal orientations handles capture mirroring. This acquisition stage invokes no Core ML model, so a crowded frame cannot starve the authoritative hand request while still distinguishing similarly dressed people. Initial acquisition requires a score of at least 0.56, a 0.03 lead over the next candidate, and two spatially continuous observations. A tracked lock changes to another person only when the challenger leads by at least 0.08 for three observations. Continuity is capped at 30% of the frame between submissions, preventing a distant bystander from being treated as the same tall person. Person shape remains a geometric filter, and a large foreground hand or arm is rejected. Segmentation and the full multimodal score run only on the selected crop. The local grayscale result is composited back into normalized full-camera coordinates and feeds two independent consumers: a full bitmap for collision/alpha rendering and an 8×8 occupancy descriptor used only for appearance comparison. The final score compares available whole-body, upper, lower, head, silhouette, and face observations to the enrolled opponent. `AppearanceScoreFusion` renormalizes weights across the modalities that are actually present. Face and silhouette/body-shape evidence can improve an active-match score, while global search excludes them.

The target image uses segmentation luminance as alpha, leaving background pixels transparent. That alpha masks an opaque tiled pattern — the opponent's silhouette skin — rather than a translucent red tint. `SilhouetteSkin` (portable core) holds the four launch skins, their palettes, and a fixed per-skin seed; `SilhouetteSkinRenderer` generates each pattern tile procedurally with Core Graphics on first use and caches it, so both players see an identical pattern and no art assets are shipped. A player's own choice rides on `AppearanceProfile.skin` as a raw string, which the server stores and returns without interpreting; an absent or unrecognised value renders as `SilhouetteSkin.fallback`, and an unrecognised value survives a re-upload rather than being dropped. Changing a skin re-uploads the cached enrollment profile, so it never requires retaking the source photos. Eliminated opponents keep the pattern but desaturate, which is the "fades to grey" behavior. If segmentation produces no foreground, the HUD shows only a dashed acquisition rectangle and does not invent a silhouette.

All aiming overlays share `ReticleStyle` and `GraphicsContext.strokeTactical`, which draw each element as a blurred additive bloom, a narrow dark contrast edge, and a hairline core — the sights crosshair, the match reticle, the scoped frame's corner brackets, the scope-entry ring, and the calibration target. The dark pass is what keeps ~1.4pt strokes readable over a bright camera frame; it replaces the heavy black halos the overlays previously used. The match reticle's bars stop short of its ring so nothing crosses the middle of the target. `CameraService` evaluates a circular continuous reticle footprint against the same mask and freezes point, coverage, score, and target age into the shot event. A shot command carries that continuous reticle, mask-containment boolean, fused target score, and a unique command ID. The server also requires recent reciprocal proximity and an active match before applying damage. Bot fixtures intentionally use a fixed passing appearance score, which is labeled `BOT TEST TARGET` rather than presented as measured identity confidence; mask contact and every other gameplay gate still apply.

## Backend

The `untitled-mobile-fps` Cargo package contains the shared protocol/domain library, Axum HTTP/WebSocket server, and deterministic account/bot binary. Protocol v2 uses distinct appearance, presence, match, event, and command stores so v1 Postcard records cannot be decoded or surfaced as the new schema. Appearance storage has an additional v3 generation because adding the optional silhouette skin changed the positional Postcard struct layout: startup reads either v2 layout from the authoritative Fold `keyed_root`, imports missing profiles into `appearances-v3.db`, rebuilds its derived search indexes, then leaves v2 untouched as a backup. The migration is retry-safe through a completion marker and never overwrites a profile already present in v3. Future persisted Postcard shape changes must likewise use an explicit store generation or versioned storage envelope; JSON `serde` defaults do not provide binary compatibility. Startup reconciliation also repairs accepted friendships/invitations, processed accepted shots, and completed history if a process stopped between related Fold writes. It uses:

- Fold tables and secondary materializations for accounts, normalized handles, token hashes, appearances, presences, match snapshots, friend requests, friendships, invitations, and idempotency keys;
- Fold BM25 plus ESE semantic description encoding;
- ANNy-backed HNSW terminals for ESE, non-face visual, and location vectors;
- a Fold event stream and aggregate for damage diagnostics;
- a completed-match table plus `FlatMap -> KeyedRanked` per-player timestamp view for newest-first history.

Only generated outfit text and the whole-body non-face embedding fan out to global indices. A bearer-authenticated endpoint verifies that requester and target share a briefing or active match before returning the full multimodal appearance record. Tokens are persisted only as SHA-256 hashes. Nearby and friend availability exclude background or older-than-30-second presence; location-free availability heartbeats are excluded from the ANNy nearby query. The public inspector serializes a separate aggregate DTO with no invite codes, match/player identifiers, appearance payloads, or coordinates. Match snapshots are authoritative, revisioned, use server receipt times for accepted hits, and complete when one player's third health point is removed.

## Data contracts

### Coordinates

- Vision image landmarks: normalized `[0, 1]`, origin lower-left, `+y` up.
- SwiftUI/UIKit view: origin upper-left, `+y` down. `PreviewGeometry` performs this conversion.
- Camera space: `+x` screen-right, `+y` screen-up, `+z` away from the rear camera.
- Viewport aim: normalized `[0, 1]`, using the Vision-style lower-left convention until display mapping.

Do not change one side of a coordinate conversion without updating tests and every tracker/overlay consumer.

### Calibration persistence

Vision calibration is JSON-encoded into `UserDefaults` under:

```text
vision-aim-calibration.<model-version>.<camera-identifier>
```

It is shared across barrel variations. Decoding rejects calibrations whose embedded model version differs from `VisionAimCalibration.modelVersion`. Bump that model version whenever the meaning or dimensionality of aim features, target templates, or solver expectations changes.

### Recording persistence

Recordings are written atomically to the app Documents directory as:

```text
finger-gun-<ISO-8601 timestamp>-landmarks.json
```

The current writer emits schema version 2. Replay accepts schema versions 1 and 2 and requires `AimCalibration.modelVersion` to match the recording model version. Optional fields allow version 1 recordings to decode while version 2 adds independent tracker samples, newer Vision diagnostics, and shot-time gameplay targeting diagnostics.

## Threading model

| Context | Responsibility |
| --- | --- |
| Main queue | Published SwiftUI state, visible flash lifetime, permission/status UI. |
| `camera.session.queue` | Capture session configuration, start, and stop. |
| `camera.capture.queue` | `AVCaptureVideoDataOutput` callbacks. |
| Vision diagnostic runner queue | Serialized Vision request execution with a busy guard. |
| `camera.analysis.queue` | Selection, classification, calibration, aim, gesture state, metrics, and recorder mutation. |
| Person targeting queue | Serialized human detection, mask generation, and multimodal score extraction. |
| MediaPipe internal live-stream callback | MediaPipe result delivery, forwarded to the analysis queue. |
| Main actor | App session, gameplay coordinator, WebSocket messages, Nearby Interaction UI state, and match commands. |
| Backend store actor thread | Fold read/write transactions and mutable reciprocal-proximity state. |

`CVPixelBuffer` is deliberately processed by the primary Vision hand request, then active-match person targeting, then opt-in MediaPipe diagnostics. Disabled or throttled consumers are skipped. Running enabled consumers concurrently against the same buffer previously caused pixel-buffer unlock failures; preserve this ordering unless buffers are copied or ownership is redesigned.

## Failure behavior

- Camera permission denial presents an Open Settings action.
- Missing rear camera or capture configuration failure becomes an unavailable status card.
- Capture interruption presents a temporary status and resumes automatically.
- Media-services reset triggers capture reconnection.
- A busy Vision runner drops the incoming analysis frame rather than building a backlog.
- Vision inference failure increments a dropped-frame diagnostic and leaves the camera session alive.
- MediaPipe is not loaded by default; opt-in initialization/submission/inference failure is ignored by gameplay.
- Brief lost tracking holds the last observation/aim; longer loss resets filters and eventually gesture state.

## Testing seams

Portable tests cover:

- 2D and 3D pose classification, including invalid poses;
- single/double variation behavior;
- five-target calibration, degeneracy rejection, and variation sharing;
- nine-zone reticle output and stabilization;
- continuous multiplayer aim and full-mask reticle-footprint collision;
- 3D angular calibration/aim compatibility;
- trigger arming, single-fire, rearm, and tracking-loss timing;
- sights proximity measurement (aspect and zoom invariance), baseline warm-up and freeze, entry dwell, hysteresis, exit, and centred shot resolution;
- recording encode/decode and deterministic timestamp ordering;
- aspect-fill and Y-axis coordinate conversion;
- multiplayer protocol encoding/decoding and three-hit snapshots;
- server canonicalization and readiness gates;
- automatic appearance descriptions and scoped multimodal fusion;
- Bogkit account/social/history persistence, token hashing, ranked history, and reciprocal proximity validation.

`CameraService` supports tracker and classifier injection, but full camera scheduling, permission, interruption, and overlay behavior still requires Xcode and preferably a physical iPhone.
