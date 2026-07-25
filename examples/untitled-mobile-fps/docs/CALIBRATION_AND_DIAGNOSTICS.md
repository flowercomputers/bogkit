# Calibration and diagnostics

## Why calibration is required

Apple Vision supplies 2D hand landmarks but no scene-depth ray in this app. Finger proportions, grip, phone position, and foreshortening vary substantially between users and sessions. The app therefore learns how one natural index-finger pose changes when aimed center, left, right, up, and down.

Calibration does not estimate a physical raycast or scene intersection. It creates a user- and pose-relative mapping to a directional screen reticle.

## Calibration flow

Tap **Calibrate** to start a new session. The target order is fixed:

1. `CENTER` at `(0.50, 0.50)`;
2. `LEFT` at `(0.22, 0.50)`;
3. `RIGHT` at `(0.78, 0.50)`;
4. `UP` at `(0.50, 0.76)`;
5. `DOWN` at `(0.50, 0.24)`.

For each target:

- use a natural finger gun with the thumb raised;
- aim in the requested direction rather than translating only the hand;
- wait through the settling period;
- hold steady while the progress bar fills;
- move deliberately when the next target appears.

By default, the collector waits 12 settling frames and then accepts 18 stable frames per target. These values live in `GestureTuning` and are frame-based, so elapsed time depends on achieved Vision FPS.

Starting calibration resets the active aim filters and gesture state. The previously saved calibration is not deleted at the start, but gameplay remains in collecting mode until the new session completes or fails. A successful session replaces the saved value. **Reset** explicitly deletes the stored calibration for the current camera.

## Accepted sample criteria

A calibration sample needs:

- an aim feature derived from sufficiently confident wrist/index/little landmarks;
- a thumb classified as up;
- overall hand confidence at or above the calibration threshold;
- enough movement away from the previous target centroid;
- no feature jump large enough to invalidate the current cluster;
- a stable cluster within the maximum RMS limit.

At completion, the five target centroids must be sufficiently distinct and the fitted mapping must remain below the maximum calibration RMSE. Failure messages distinguish missing targets, similar poses, incomplete landmarks, an unstable fit, and poor fit quality.

The fit must also survive the solver's own axis geometry. Pairwise centroid separation is measured in raw feature space, so a centroid can clear it and still collapse onto center once projected onto the horizontal/vertical axes the solver builds. Because that projection depends only on the calibration, such a fit would be rejected on every subsequent frame — the app would report `CALIBRATED` and `ARMED` while never drawing a reticle or firing. Calibration therefore names the collapsed axis and asks for exaggerated poses instead of saving that mapping, and a previously saved calibration that fails the same check is reported as missing so the UI asks for a recalibration.

## Aim feature

The Vision feature contains seven palm-relative values:

```text
index tip x/y
index PIP x/y
index DIP x/y
projected index path length
```

Positions are measured relative to a palm centroid and divided by palm width. This makes the feature less sensitive to hand translation and scale in the frame.

Although the classifier supports double-barrel poses, calibration and aim intentionally use the index finger for both variations. The resulting calibration is saved once per camera identifier and shared across variations.

## Runtime aim

The solver standardizes the current index direction feature and applies the fitted ridge-regression coefficients saved during calibration to produce the continuous gameplay point. It also constructs horizontal and vertical basis vectors from the five saved target centroids, projects the current feature onto those axes, normalizes each direction against its captured anchor, and rejects large off-axis residuals. That axis projection controls only the nine-zone diagnostic output.

There are two reticle values in diagnostics:

- `RAW XY` is the One Euro-filtered fitted-regression point and the authoritative multiplayer shot point;
- `AIM XY` is the nine-zone position after quantization and stabilization, retained for calibration and diagnostics.

The diagnostic zone position is one of nine zones:

```text
UP-LEFT       UP       UP-RIGHT
LEFT        CENTER       RIGHT
DOWN-LEFT    DOWN     DOWN-RIGHT
```

Horizontal and vertical zone changes use normalized axis thresholds and must remain candidates for several frames before becoming active. Diagonals are synthesized by combining the calibrated axes; diagonal calibration poses are not required.

Brief aim failures can hold the previous solution for `visionAimHoldSeconds`. Tracking loss has a separate short grace interval. Longer loss resets filters and eventually resets gesture state.

A missing aim solution is not the same as a missing hand, and it is reported separately as `AIMREJ` in the HUD. The pose classifier can accept a finger gun and arm the trigger while the solver still declines to place a reticle.

## Sights mode

Sights are entered by drawing the finger gun toward the phone. The detector compares the apparent size of the knuckle line against a running baseline of the player's relaxed hold; entry needs `scopeEnterProximityRatio` (1.40x) held for `scopeEntrySeconds`, release needs `scopeExitProximityRatio` (1.15x) held for `scopeExitSeconds`.

The mode is signalled by a `SIGHTS`/`HIP` badge at the top of the camera view, a vignette with red corner brackets while scoped, an approach ring while closing in, and a haptic tap on each transition. The debug HUD's `MODE` row is not sufficient on its own, because the whole overlay is hidden during an active match.

The baseline is a low percentile (`scopeBaselinePercentile`) of the spans seen over the last `scopeBaselineWindowSeconds`, sampled only while unscoped. Properties that determine what you will see while tuning it:

- It needs a warm-up of both `scopeBaselineMinimumSamples` frames and `scopeBaselineMinimumSeconds` of elapsed time, so the first second of tracking cannot scope. The HUD `PROX` row reads `WARM` until then and `RDY` afterwards.
- It adapts in both directions. A hold at a new distance is re-learned as the new normal within about a window's length; a brief pull-in is too small a minority of the window to be absorbed.
- It stops sampling while scoped, which is the only freeze rule. Holding the gun up therefore keeps you scoped for as long as you hold it; you leave by lowering it.
- A very slow approach is absorbed and will not scope, by design. Sights require a deliberate movement.

The measurement itself uses only the four MCP knuckles, six pairs voting on one scale estimate with outlier pairs discarded. It does *not* use the wrist: on device the wrist was the first landmark lost as the hand approached (median confidence 0.14 on failing frames) and geometrically unstable, and requiring it cost 24% of frames.

Sights need no calibration because the firing point is always camera centre, and a scoped shot runs through the same gameplay targeting as an unscoped one. Starting calibration forces unscoped 1x mode until collection ends.

If sights misbehave, read `PROX` before changing thresholds:

- **Ratio sits well above 1.0 while your hand is relaxed, and sights stick on.** The baseline is below your actual hold. Check that it is still moving; a baseline frozen for many seconds while unscoped is a bug, not tuning.
- **Ratio never rises past ~1.1 during a deliberate pull-in.** Your approach is slow enough that the baseline is tracking it. Move faster or lengthen `scopeBaselineWindowSeconds`.
- **Ratio reads 0 often.** Knuckle landmarks are being lost; fewer than `scopeMinimumProximityPairs` pairs survived. Check hand lighting and whether the hand is leaving the frame.
- **`WARM` never clears.** The hand is not tracked steadily enough for long enough to build a reference.

## Trigger behavior

Calibration gates the trigger in unscoped mode. With no valid calibration, a detected finger-gun pose cannot arm or fire. Sights mode does not require calibration.

Sights entry consumes no thumb signal, so it can never be mistaken for a trigger pull: drawing the hand closer leaves thumb geometry unchanged. Entering or leaving sights resets aim and trigger state, so a thumb transition across a mode change cannot fire.

The normal sequence is:

1. Show a valid pose with thumb up.
2. Keep it stable until the HUD changes from `CANDIDATE` to `ARMED`.
3. Lower the thumb.
4. The state becomes `FIRED` for one update and emits one muzzle flash.
5. The state moves to `REARM`.
6. Raise the thumb and keep it up for the required rearm frames.
7. The state returns to `ARMED`.

A held-down thumb never produces repeated shots. Switching between single and double barrel also does not itself fire.

Physically lowering the thumb can briefly make the ring or little finger appear straight in 2D. After the state has genuinely reached `ARMED`, a 0.18-second latch can accept only that immediate thumb-down edge from the same selected hand while a usable aim remains. It cannot create an armed state, cannot bridge an aiming-mode transition, and cannot supply the stable thumb-up frames required to rearm.

## HUD reference

| Label | Meaning |
| --- | --- |
| `TRACK` | Authoritative tracker. It should read `VISION 2D PRIMARY`. |
| `MODEL` | Vision calibration model version. |
| `MODE` | Aiming mode: `UNSCOPED` or `SIGHTS`. |
| `PROX` | Sights proximity: current ratio against the baseline, the baseline span itself, `WARM`/`RDY` warm-up state, and entry progress toward the threshold. |
| `STATE` | Trigger state: `NO HAND`, `CANDIDATE`, `ARMED`, `FIRED`, or `REARM`. |
| `CAL` | Required, collecting percentage, calibrated, or failure state. |
| `POSE` | Accepted single/double barrel variation, or `—`. |
| `TH` | Vision thumb classification: up, down, ambiguous, or unavailable. |
| `FING` | Vision index/middle/ring/little state: straight (`S`), curled (`C`), ambiguous (`?`), or unavailable (`—`). |
| `REJECT` | First classifier reason the current pose was not accepted. |
| `CONF` | Accepted observation confidence. |
| `M` | Pose margin above the index-straight threshold. |
| `MP Z` | Diagnostic MediaPipe index-ray depth component; positive points away from the rear camera under the repository convention. |
| `2D Δ` | Mean normalized joint distance between fresh Vision and MediaPipe tracks. |
| `RAW XY` | Continuous filtered aim before nine-zone quantization. |
| `AIM XY` | Stable diagnostic reticle and nearest named zone. |
| `AIMREJ` | Why no aim solution was produced this frame, red while the reticle is absent in unscoped mode: `NO POSE` (no usable aim feature), `NO CAL` (no valid calibration), `HOLD` (briefly reusing the previous solution), `CAL AXIS <axis>` (saved calibration cannot separate that axis), `SOLVER` (off-axis residual or reticle velocity limit), or `SIGHTS` (scoped, so the solver is intentionally not run). |
| `VISION` | Primary tracker FPS and inference latency. |
| `VN DROP` | Frames rejected or failed by the serialized Vision runner. |
| `MP DIAG` | MediaPipe diagnostic FPS and latency. |

## Overlay colors and marks

- Vision skeleton and bounds: solid cyan bones with pink joints.
- MediaPipe timing/classification values remain in the HUD but are empty/zero by default. The diagnostic runner must be explicitly enabled, and its skeleton is intentionally hidden.
- Orange ring: continuous raw aim.
- Red dot with red halo: quantized diagnostic reticle.
- Dashed red line: center-to-reticle direction.
- Orange/white burst: one muzzle-flash event.

MediaPipe landmarks are never drawn. When diagnostics are explicitly enabled, temporally fresh results still feed comparison metrics only.

During an active match the full diagnostic overlay is hidden by default. The authoritative continuous crosshair is white outside the person mask, green when its footprint and the identity gate pass, and amber while the target is missing, stale, or below the identity threshold. The **Debug** control restores the skeleton and raw/zone marks beneath that crosshair.

## Common rejection reasons

The exact string is intended for live diagnosis:

- `LOW/MISSING 2D`: a required Vision landmark is absent or below confidence.
- `INDEX CURLED` or `INDEX AMBIG`: the barrel finger is not confidently straight.
- `RING STRAIGHT` / `LITTLE STRAIGHT`: the pose resembles an open hand rather than a finger gun.
- `MIDDLE AMBIG`: the pose cannot be assigned a single- or double-barrel variation.
- `NO BARREL FEATURE`: a required fingertip feature is missing.

MediaPipe diagnostics can additionally report low hand/world confidence, missing palm or barrel fits, divergent double barrels, or a barrel ray that does not point into the scene.

## Diagnostic recordings

Tap **Record data** to begin. While recording, the app stores landmark and derived state only. Tap **Stop data** to write JSON atomically to the app Documents directory, then use **Export data** to share the file. Leaving the camera, lobby, or match while recording automatically completes the file. The debug build exposes the latest saved recording from both **Play** and **Profile**, and restores that export link after an app relaunch.

Top-level recording fields:

| Field | Meaning |
| --- | --- |
| `schemaVersion` | Recording format version; the current writer uses 2. |
| `modelVersion` | Compatibility identifier used by replay. |
| `startedAt` | Recording start date. |
| `frames` | Primary Vision-derived frame records. |
| `trackerSamples` | Independent raw tracker outputs and latencies for Vision and MediaPipe. |

A primary frame can include timestamp, selected hand, Vision analysis, active Vision calibration on its completion frame, aim solution, gesture state, fired flag, flash point, aiming mode, and a `scopeProximity` record carrying that frame's span, baseline, ratio, entry progress, and warm-up state — enough to explain a rejected sights entry from the recording alone. During a multiplayer camera session, the optional `nearbyInteraction` record captures the visible UWB status plus the most recent distance, direction, and sample time. A fired frame can also include an optional `gameplayShot` record with the continuous and zone points, target box/age/identity score, mask coverage, containment result, and local gate status. Older 3D observation/calibration fields remain optional for compatibility.

A tracker sample includes timestamp, source (`VISION` or `MEDIAPIPE`), all detected hands, and inference latency. MediaPipe samples may contain world landmarks; Vision samples contain image landmarks only.

`LandmarkReplay` sorts primary frames by timestamp before calling the replay handler. It currently accepts schema versions 1 and 2 and rejects an incompatible model version.

## Privacy implications

Recordings do not contain camera images, audio, user accounts, or network identifiers. Normalized landmarks can still describe hand movement and should be treated as potentially sensitive diagnostic data. Keep recordings local, share them intentionally, and remove exported copies when no longer needed.

## Tuning safely

Gesture thresholds are centralized in `GestureTuning`; multiplayer target thresholds are centralized in `GameplayTargetingTuning`. The default target evaluation uses a 0.35 foreground cutoff, a reticle radius equal to 1.8% of the image's shorter dimension, 8% minimum footprint coverage, a 0.75-second maximum mask age, and a 0.5 final identity threshold. Person acquisition separately requires a 0.56 lightweight face-structure score against the match briefing thumbnail, a 0.03 lead, and two observations; switching requires a 0.08 identity advantage for three observations. The lightweight acquisition score is distinct from the final multimodal identity score shown by the HUD. Tune one subsystem at a time and pair each change with a regression test or representative landmark recording.

Useful groups include:

- 3D MediaPipe confidence and finger geometry;
- 2D Vision confidence, straight/curled geometry, and thumb geometry;
- calibration settling, sample count, movement, separation, cluster RMS, ridge strength, and maximum fit RMSE;
- reticle residual/velocity rejection, direction thresholds, and stabilization frames;
- gesture stabilization, rearm, tracking grace/reset, and flash duration;
- sights proximity enter/exit ratios, entry dwell and exit delay, baseline window/percentile/warm-up, minimum agreeing pairs and pair disagreement tolerance, and zoom factor;
- One Euro filter cutoff, beta, and derivative cutoff.

Avoid solving a classification issue by broadly lowering confidence and geometry thresholds; that tends to create open-palm false positives. Capture the failing pose, add a narrow synthetic or replay test, then adjust the smallest relevant rule.

## Troubleshooting calibration in order

1. Confirm the HUD shows a fresh solid Vision skeleton.
2. Check `TH` is `UP` and confidence is stable.
3. Check `FING`/`REJECT`; calibration can use an aim feature despite some strict pose rejection, but required index/palm landmarks must exist.
4. Hold still through settling; progress does not advance during those frames.
5. Move the aim meaningfully when prompted. Repeating the prior pose will not advance.
6. Keep the hand within the preview and avoid abrupt jumps that reset the target cluster.
7. If final fit quality fails, use more distinct but still comfortable cardinal poses and retry.
8. Use **Reset** only when you want to remove the saved calibration, not merely restart an in-progress attempt.

## When the pose is accepted but nothing happens

If `STATE` reaches `ARMED` and lowering the thumb still produces no shot and no reticle, the problem is the aim solver, not hand detection. Check `MODE` first: in `SIGHTS` there is no solver reticle by design and `AIMREJ` reads `SIGHTS`, so a scoped frame without a solved aim is expected rather than a fault. Otherwise read `AIMREJ`:

- `NO CAL` with the calibration prompt showing means the saved calibration was rejected as unsolvable. Recalibrate, exaggerating the cardinal poses.
- `CAL AXIS <axis>` means the saved calibration cannot separate that axis. Recalibrate.
- `SOLVER` means the live feature is far off the calibrated axes or the reticle moved too fast. Re-check that the calibrated poses match how the hand is actually held.
- `NO POSE` means no usable aim feature exists this frame; return to the classifier reasons above.

In a diagnostic recording the same condition appears as frames that carry `hand`, `visionAnalysis`, and `visionCalibration` but no `aim`, with `fired` never true.
