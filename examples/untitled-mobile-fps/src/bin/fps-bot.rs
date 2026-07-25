use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;
use untitled_mobile_fps::{
    Account, AppearanceProfile, ClientMessage, MatchInvitation, MatchInvitationStatus,
    MatchSnapshot, MatchStatus, Presence, ServerMessage, VISUAL_DIMENSIONS,
};
use uuid::Uuid;

const FIXTURE_HANDLE: &str = "bog-bot";
const FIXTURE_NAME: &str = "Bog Bot";
const CALIBRATION_MODEL: &str = "vision-hand-pose-2d-v7";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateAccountRequest {
    handle: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountEnvelope {
    account: Account,
    token: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FriendRequestBody {
    handle: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InviteEnvelope {
    snapshot: MatchSnapshot,
}

#[derive(Debug, Serialize)]
struct EmptyBody {}

#[derive(Debug, Deserialize)]
struct RealtimeTicketEnvelope {
    ticket: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureState {
    base_url: String,
    #[serde(default)]
    server_id: String,
    player_id: String,
    token: String,
}

enum Command {
    SeedSocial {
        base: String,
        phone_handle: String,
    },
    FullMatch {
        base: String,
        invite_code: String,
        scripted_hit_return: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = parse_command(std::env::args().skip(1).collect())?;
    let state_path = fixture_state_path();

    match command {
        Command::SeedSocial { base, phone_handle } => {
            let client = reqwest::Client::new();
            let fixture = ensure_fixture(&client, &base, &state_path).await?;
            seed_social(&client, &base, &fixture, &phone_handle).await?;
        }
        Command::FullMatch {
            base,
            invite_code,
            scripted_hit_return,
        } => {
            let client = reqwest::Client::new();
            let fixture = ensure_fixture(&client, &base, &state_path).await?;
            run_full_match(&client, &base, &fixture, &invite_code, scripted_hit_return).await?;
        }
    }
    Ok(())
}

fn parse_command(args: Vec<String>) -> Result<Command, Box<dyn std::error::Error>> {
    let usage = "usage:\n  fps-bot seed-social <server-url> <phone-handle>\n  fps-bot scenario full-match <server-url> <invite-code> [--scripted-hit-return]\n  fps-bot <server-url> <invite-code>  # legacy alias for scenario full-match";
    match args.as_slice() {
        [command, base, phone_handle] if command == "seed-social" => Ok(Command::SeedSocial {
            base: base.clone(),
            phone_handle: phone_handle.clone(),
        }),
        [scenario, full_match, base, invite_code]
            if scenario == "scenario" && full_match == "full-match" =>
        {
            Ok(Command::FullMatch {
                base: base.clone(),
                invite_code: invite_code.clone(),
                scripted_hit_return: false,
            })
        }
        [scenario, full_match, base, invite_code, flag]
            if scenario == "scenario"
                && full_match == "full-match"
                && flag == "--scripted-hit-return" =>
        {
            Ok(Command::FullMatch {
                base: base.clone(),
                invite_code: invite_code.clone(),
                scripted_hit_return: true,
            })
        }
        [base, invite_code] => Ok(Command::FullMatch {
            base: base.clone(),
            invite_code: invite_code.clone(),
            scripted_hit_return: false,
        }),
        _ => Err(usage.into()),
    }
}

fn fixture_state_path() -> PathBuf {
    std::env::var_os("FPS_BOT_STATE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("untitled-mobile-fps-bot.json"))
}

async fn ensure_fixture(
    client: &reqwest::Client,
    base: &str,
    state_path: &PathBuf,
) -> Result<FixtureState, Box<dyn std::error::Error>> {
    let normalized_base = normalize_base(base)?;
    let health = client
        .get(format!("{normalized_base}/health"))
        .send()
        .await?
        .error_for_status()?
        .json::<HealthResponse>()
        .await?;
    if health.server_id.trim().is_empty() {
        return Err("server health did not include a stable serverId".into());
    }
    if let Ok(contents) = std::fs::read(state_path) {
        if let Ok(state) = serde_json::from_slice::<FixtureState>(&contents) {
            if state.base_url == normalized_base
                && state.server_id == health.server_id
                && fetch_me(client, &normalized_base, &state.token)
                    .await
                    .is_ok()
            {
                ensure_appearance(client, &normalized_base, &state).await?;
                println!(
                    "Using persistent fixture @{} ({}) from {}.",
                    FIXTURE_HANDLE,
                    state.player_id,
                    state_path.display()
                );
                return Ok(state);
            }
        }
    }

    let response = client
        .post(format!("{normalized_base}/v1/accounts"))
        .json(&CreateAccountRequest {
            handle: FIXTURE_HANDLE.into(),
            display_name: FIXTURE_NAME.into(),
        })
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "could not create fixture @{} ({status}: {body}). Its credential is intentionally returned only once. Restore FPS_BOT_STATE_PATH={} or reset the temporary FPS_DATA_DIR before retrying.",
            FIXTURE_HANDLE,
            state_path.display()
        )
        .into());
    }
    let envelope: AccountEnvelope = response.json().await?;
    let token = envelope
        .token
        .ok_or("account creation did not return a credential")?;
    let fixture = FixtureState {
        base_url: normalized_base.clone(),
        server_id: health.server_id,
        player_id: envelope.account.player_id,
        token,
    };
    save_fixture_state(state_path, &fixture)?;
    ensure_appearance(client, &normalized_base, &fixture).await?;
    println!(
        "Created persistent fixture @{} ({}) and saved its private credential to {}.",
        FIXTURE_HANDLE,
        fixture.player_id,
        state_path.display()
    );
    Ok(fixture)
}

fn save_fixture_state(
    path: &PathBuf,
    state: &FixtureState,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(state)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

async fn fetch_me(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<Account, reqwest::Error> {
    client
        .get(format!("{base}/v1/me"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
}

async fn ensure_appearance(
    client: &reqwest::Client,
    base: &str,
    fixture: &FixtureState,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut embedding = vec![0.0; VISUAL_DIMENSIONS];
    embedding[0] = 0.8;
    embedding[1] = 0.4;
    embedding[2] = 0.2;
    let profile = AppearanceProfile {
        player_id: fixture.player_id.clone(),
        display_name: FIXTURE_NAME.into(),
        generated_description: "red jacket, dark blue jeans, black cap".into(),
        embedding_model: "fixture-512-v1".into(),
        descriptor_model: "outfit-descriptor-v1".into(),
        whole_body_embedding: embedding.clone(),
        face_embeddings: vec![embedding],
        upper_body_embeddings: Vec::new(),
        lower_body_embeddings: Vec::new(),
        head_accessory_embeddings: Vec::new(),
        silhouette_descriptor: vec![0.0; 64],
        briefing_thumbnail: None,
        // Gives the bot a visibly skinned silhouette, which is the only way to
        // check the rendering without a second phone.
        skin: Some("green_camo".into()),
        updated_at_ms: now_ms(),
    };
    client
        .put(format!("{base}/v1/me/appearance"))
        .bearer_auth(&fixture.token)
        .json(&profile)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn seed_social(
    client: &reqwest::Client,
    base: &str,
    fixture: &FixtureState,
    phone_handle: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .post(format!("{base}/v1/me/friend-requests"))
        .bearer_auth(&fixture.token)
        .json(&FriendRequestBody {
            handle: phone_handle.into(),
        })
        .send()
        .await?
        .error_for_status()?;
    println!(
        "Friend request from @{} sent to @{}. On the iPhone, open Friends, accept it, then create an invite code.",
        FIXTURE_HANDLE, phone_handle
    );
    Ok(())
}

async fn run_full_match(
    client: &reqwest::Client,
    base: &str,
    fixture: &FixtureState,
    invite_code: &str,
    scripted_hit_return: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let joined: InviteEnvelope = if invite_code == "targeted" {
        let invitations: Vec<MatchInvitation> = client
            .get(format!("{base}/v1/me/match-invitations"))
            .bearer_auth(&fixture.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let invitation = invitations
            .into_iter()
            .filter(|invitation| invitation.status == MatchInvitationStatus::Pending)
            .max_by_key(|invitation| invitation.created_at_ms)
            .ok_or("no pending targeted invitation for @bog-bot")?;
        client
            .post(format!(
                "{base}/v1/match-invitations/{}/accept",
                invitation.invitation_id
            ))
            .bearer_auth(&fixture.token)
            .json(&EmptyBody {})
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?
    } else if let Some(invitation_id) = invite_code.strip_prefix("invitation:") {
        client
            .post(format!(
                "{base}/v1/match-invitations/{invitation_id}/accept"
            ))
            .bearer_auth(&fixture.token)
            .json(&EmptyBody {})
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?
    } else {
        client
            .post(format!(
                "{base}/v1/invites/{}/join",
                invite_code.to_uppercase()
            ))
            .bearer_auth(&fixture.token)
            .json(&EmptyBody {})
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?
    };
    let opponent = joined
        .snapshot
        .players
        .iter()
        .find(|player| player.player_id != fixture.player_id)
        .map(|player| player.player_id.clone())
        .ok_or("invite had no opponent")?;
    let ticket: RealtimeTicketEnvelope = client
        .post(format!("{base}/v1/realtime/tickets"))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "matchId": joined.snapshot.match_id }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let websocket_base = base
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let mut url = Url::parse(&format!("{websocket_base}/v1/realtime"))?;
    url.query_pairs_mut()
        .append_pair("ticket", &ticket.ticket)
        .append_pair("matchId", &joined.snapshot.match_id);
    let (stream, _) = tokio_tungstenite::connect_async(url.as_str()).await?;
    let (mut outgoing, mut incoming) = stream.split();

    send_message(
        &mut outgoing,
        &ClientMessage::ReadyWithMetadata {
            command_id: Uuid::new_v4(),
            match_id: joined.snapshot.match_id.clone(),
            calibration_model_version: CALIBRATION_MODEL.into(),
        },
    )
    .await?;
    println!(
        "Joined {} as @{}. On the iPhone: tap Ready, review the briefing, and acknowledge it. This bot will keep reciprocal proximity live for physical three-shot testing.",
        joined.snapshot.invite_code, FIXTURE_HANDLE
    );
    if scripted_hit_return {
        println!(
            "Scripted hit-return is enabled: after ACTIVE, the bot will fire three valid fixture shots at the phone."
        );
    }

    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    let mut briefing_acknowledged = false;
    let mut scripted_hits = 0_u8;
    let mut active_ticks = 0_u8;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let now = now_ms();
                send_message(&mut outgoing, &ClientMessage::Heartbeat { command_id: Uuid::new_v4() }).await?;
                send_message(&mut outgoing, &ClientMessage::Presence {
                    command_id: Uuid::new_v4(),
                    presence: Presence {
                        player_id: fixture.player_id.clone(),
                        latitude: 40.7128,
                        longitude: -74.0060,
                        horizontal_accuracy: 3.0,
                        foreground: true,
                        updated_at_ms: now,
                    },
                }).await?;
                send_message(&mut outgoing, &ClientMessage::Proximity {
                    command_id: Uuid::new_v4(),
                    match_id: joined.snapshot.match_id.clone(),
                    peer_id: opponent.clone(),
                    distance_meters: Some(3.0),
                    direction: Some([0.0, 0.0, 1.0]),
                    sampled_at_ms: now,
                }).await?;
                if scripted_hit_return && active_ticks > 2 && scripted_hits < 3 {
                    scripted_hits += 1;
                    send_message(&mut outgoing, &ClientMessage::Shot {
                        command_id: Uuid::new_v4(),
                        match_id: joined.snapshot.match_id.clone(),
                        target_id: opponent.clone(),
                        reticle: [0.5, 0.5],
                        mask_contains_reticle: true,
                        target_score: 1.0,
                        fired_at_ms: now,
                    }).await?;
                }
            }
            message = incoming.next() => {
                let Some(message) = message else { return Ok(()); };
                let message = message?;
                if let Message::Text(text) = message {
                    let Ok(server) = serde_json::from_str::<ServerMessage>(&text) else {
                        println!("unrecognized server message: {text}");
                        continue;
                    };
                    match &server {
                        ServerMessage::MatchSnapshot { snapshot } => {
                            match snapshot.status {
                                MatchStatus::Briefing if !briefing_acknowledged => {
                                    briefing_acknowledged = true;
                                    send_message(&mut outgoing, &ClientMessage::BriefingAcknowledged {
                                        command_id: Uuid::new_v4(),
                                        match_id: snapshot.match_id.clone(),
                                    }).await?;
                                    println!("Briefing acknowledged. Waiting for the phone acknowledgement.");
                                }
                                MatchStatus::Active => {
                                    active_ticks = active_ticks.saturating_add(1);
                                    if active_ticks == 1 { println!("MATCH ACTIVE. Aim at the physical fixture and land three shots."); }
                                }
                                MatchStatus::Completed => {
                                    println!("MATCH COMPLETED. Verify the result in the iPhone History tab, then restart the server and app to verify persistence.");
                                    return Ok(());
                                }
                                _ => {}
                            }
                        }
                        ServerMessage::ShotResolution { accepted, reason, .. } if scripted_hit_return => {
                            println!("scripted hit result: accepted={accepted}, reason={reason}");
                        }
                        ServerMessage::Error { message } => eprintln!("server error: {message}"),
                        _ => {}
                    }
                }
            }
        }
    }
}

async fn send_message(
    outgoing: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    message: &ClientMessage,
) -> Result<(), Box<dyn std::error::Error>> {
    outgoing
        .send(Message::Text(serde_json::to_string(message)?.into()))
        .await?;
    Ok(())
}

fn normalize_base(base: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut url = Url::parse(base)?;
    if url.path() == "/" {
        url.set_path("");
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{Command, FixtureState, parse_command, save_fixture_state};

    #[test]
    fn parses_social_seed_command() {
        let command = parse_command(vec![
            "seed-social".into(),
            "http://server".into(),
            "phone".into(),
        ])
        .unwrap();
        assert!(matches!(
            command,
            Command::SeedSocial { base, phone_handle } if base == "http://server" && phone_handle == "phone"
        ));
    }

    #[test]
    fn preserves_legacy_invite_spelling() {
        let command = parse_command(vec!["http://server".into(), "ABC123".into()]).unwrap();
        assert!(matches!(
            command,
            Command::FullMatch { base, invite_code, scripted_hit_return: false }
                if base == "http://server" && invite_code == "ABC123"
        ));
    }

    #[test]
    fn parses_scripted_hit_return_opt_in() {
        let command = parse_command(vec![
            "scenario".into(),
            "full-match".into(),
            "http://server".into(),
            "ABC123".into(),
            "--scripted-hit-return".into(),
        ])
        .unwrap();
        assert!(matches!(
            command,
            Command::FullMatch {
                scripted_hit_return: true,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fixture_state_is_owner_only_and_keeps_server_identity() {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("fps-bot-state-{}", uuid::Uuid::new_v4()));
        let path = directory.join("fixture.json");
        let state = FixtureState {
            base_url: "http://server".into(),
            server_id: "stable-server".into(),
            player_id: "player".into(),
            token: "secret".into(),
        };
        save_fixture_state(&path, &state).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let restored: FixtureState =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored.server_id, "stable-server");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
