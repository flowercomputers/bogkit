# Phase 2 testing and debugging

This is the repeatable one-person verification path for server registration, appearance enrollment, calibration, friends, lobby/briefing, physical shots, history, and persistence. It uses `fps-bot` as the second player; it does not bypass the phone's camera, gesture, target-mask, or server shot checks.

## Automated checks

From the BogKit repository root:

```sh
cargo test -p untitled-mobile-fps
swift test --package-path examples/untitled-mobile-fps
xcodebuild -workspace examples/untitled-mobile-fps/UntitledMobileFPS.xcworkspace \
  -scheme UntitledMobileFPS \
  -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO \
  build
```

Run the camera, Local Network, Keychain, calibration, and proximity portions on a physical iPhone. A simulator is only a compilation and portable-logic check.

## One person: fresh server plus iPhone plus bot

1. Put the Mac and iPhone on the same Wi-Fi network. Obtain the Mac's LAN address (for example, `ipconfig getifaddr en0`). In one terminal, create an isolated, disposable server state and start the service:

   ```sh
   TEST_DATA_DIR="$(mktemp -d)"
   export FPS_DATA_DIR="$TEST_DATA_DIR"
   cargo run -p untitled-mobile-fps
   ```

   Open `http://127.0.0.1:3000/inspector` on the Mac. The inspector shows the server identity, redacted materialization counts, aggregate match readiness, command counts, and stale-presence-safe diagnostics. It must never show invite codes, match/player identifiers, credentials, appearance payloads, raw images, or locations.

2. On the iPhone, choose **Custom Server** and enter `http://<mac-lan-address>:3000`; do not use `localhost`. Test the connection and allow iOS Local Network access. If it fails, open `http://<mac-lan-address>:3000/health` in Safari on that phone before debugging the app.

3. Register a unique handle and display name. Complete both appearance captures: a full-body outfit photo and a face/briefing photo. Pick a silhouette skin in step 03; it is uploaded with the profile. Confirm the outfit description is generated from the body photo, rather than typed. Calibrate all five finger-gun points until the Play checklist is fully ready.

4. Seed the deterministic fixture friendship from a second terminal. Persist its one-time credential alongside the temporary server state so repeated bot commands reuse the same account:

   ```sh
   FPS_BOT_STATE_PATH="$TEST_DATA_DIR/fps-bot.json" \
   cargo run -p untitled-mobile-fps --bin fps-bot -- \
     seed-social http://127.0.0.1:3000 YOUR_HANDLE
   ```

   Accept the `@bog-bot` request in **Friends**. Re-running this command is safe while the request is pending or the friendship exists. If the state file was lost but `@bog-bot` remains on the server, the bot reports the recovery path: restore that file or restart with a fresh temporary `FPS_DATA_DIR`.

5. In **Play**, create a share-code match. Start the bot with the displayed code and leave it running:

   ```sh
   FPS_BOT_STATE_PATH="$TEST_DATA_DIR/fps-bot.json" \
   cargo run -p untitled-mobile-fps --bin fps-bot -- \
     scenario full-match http://127.0.0.1:3000 INVITE_CODE
   ```

   The legacy `fps-bot SERVER_URL INVITE_CODE` spelling remains an alias. To exercise the friend challenge path instead, challenge `@bog-bot` from the iPhone and replace `INVITE_CODE` with `targeted`; the bot accepts its newest pending targeted invitation. `invitation:INVITATION_ID` is also accepted for an explicitly selected invitation. The normal solo path uses a share code because it is visible and easy to repeat.

6. When the bot reports that it joined, enable **Simulate phone proximity for fps-bot** on the phone. Tap **Ready**, inspect the opponent briefing, and acknowledge it. The bot sends its `ready_with_metadata`, acknowledgement, heartbeat, foreground presence, and reciprocal 3 m proximity continuously. Both acknowledgements transition the match to **Active**.

7. Aim at a clearly person-shaped fixture—a cooperative person or a large full-body photo on a monitor makes a repeatable target. Only the person should receive the opaque silhouette fill, patterned with the opponent's skin — the bot fixture enrolls `green_camo`, so the bot target should read as pixel camo rather than flat colour. The bot fixture reads `BOT TEST TARGET`; a real opponent reads `IDENTITY` with measured confidence. A dashed rectangle means segmentation has not produced a usable mask yet. Move the authoritative reticle inside the silhouette, arm with thumb-up, fire by lowering the thumb, and rearm between shots.

   Verify the crosshair turns green only when its footprint overlaps the silhouette. Fire once outside it and confirm health does not change and `MISS · AIM OUTSIDE TARGET` remains visible. Move the foreground finger-gun hand across the frame and confirm the target rectangle does not jump away from the person. Toggle **Debug** and confirm the continuous and nine-zone points appear separately beneath the authoritative reticle without changing gameplay. Record and export the run; fired frames should contain `gameplayShot` targeting diagnostics but no image or mask pixels.

   Land three accepted physical shots. The phone should route to a completed result, and **History** should show the opponent, timestamps, duration, hit totals, and event timeline. The bot's fixed passing appearance score does not bypass person segmentation, reticle containment, command deduplication, reciprocal proximity, or the three-hit state machine.

8. Change the silhouette skin from **Profile → Silhouette skin**. It re-uploads the cached profile rather than re-running enrollment, so the outfit description and briefing thumbnail must be unchanged afterward. Restart the app and confirm the selection survives.

9. Stop and restart the server with the same `FPS_DATA_DIR`, then relaunch the app. Verify the server ID, device account, fixture friendship, registered appearance, silhouette skin, and completed history restore. The bot state file must still allow `seed-social` or the next match without creating another fixture.

`--scripted-hit-return` is available only for a protocol-v2 server that accepts `shot` messages. It has the bot fire three fixture shots at the phone after activation, which is useful for server state debugging but deliberately not part of the physical three-shot acceptance path.

## Inspector and failure diagnosis

| Symptom | Check | Expected result |
| --- | --- | --- |
| Server cannot connect | `/health` in iPhone Safari, Local Network permission, same Wi-Fi | `status: ok`, protocol version 2, and a stable server ID. |
| Bot cannot register | `FPS_BOT_STATE_PATH`, server account records | `@bog-bot` is reused with its saved credential; a lost credential requires restoring the state file or using a fresh temp directory. |
| Friend request is absent | Friends refresh, inspector materialization counts | A pending social record then an accepted friendship; no address-book matching is involved. |
| Match stays in lobby or briefing | Bot stdout and inspector aggregate match state | Two players, two current-model readiness records, then two briefing acknowledgements before `active`. |
| Physical shots never count | Reticle color/status, bot process, inspector command count | The phone sends the reverse proximity report, bot sends its report, and failures name the target/proximity gate. |
| Result is missing from history | Inspector completion/materialization counts and History refresh | Exactly three accepted hits, one completed match record, then a newest-first history row. |
| Presence looks odd | Inspector only | Presence has no player ID or coordinates; background or older-than-30-second records are unavailable, and location-free hub heartbeats never enter nearby search. |

For a clean run, stop the server and create a new `TEST_DATA_DIR` with `mktemp -d`; never delete an unresolved production-like data directory. The temporary bot state file contains a private development credential, so do not commit or share it.

## Two phones: real UWB

Install on two direction-capable iPhones, register distinct accounts and both appearance captures, then join one phone's share code from the other. Leave the bot proximity simulation off. Ready and acknowledge both briefings, permit Nearby Interaction, and move the phones within about 15 m.

On both phones, watch the radar status progress from token exchange to ranging and then to a numeric distance. `NO RANGE` plus a warning icon means no UWB reading exists; the app no longer draws a placeholder contact. If one token message is missed, one WebSocket subscribes late, or iOS invalidates one local `NISession`, the one-second relay retry, client session recreation, and server's match-scoped token cache must recover without remaining at `peer token buffered`. Cover or separate the phones long enough to stop updates and verify the last distance clears to `UWB reading stale`, then restores when ranging resumes. Suspension, unsupported hardware, invalid tokens, and WebSocket relay failures must appear as explicit status text.

The server accepts shots only with recent, mutually consistent reports from both phones. UWB does not work in the simulator, and direction can temporarily be absent while distance remains usable.
