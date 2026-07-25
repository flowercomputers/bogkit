use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey, ssh_key};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, Pty};
use tokio::sync::Mutex;

use crate::commands;
use crate::content::GameContent;
use crate::debug;
use crate::domain::ActorId;
use crate::service::WorldHandle;
use crate::terminal;

#[derive(Clone)]
pub struct MudServer {
    world: WorldHandle,
    content: Arc<GameContent>,
    next_connection_id: Arc<AtomicU64>,
    connection_id: u64,
    user: Option<String>,
    fingerprint: Option<String>,
    actor_id: Option<ActorId>,
    channels: Arc<Mutex<HashMap<ChannelId, Vec<u8>>>>,
    opening_channels: Arc<Mutex<HashSet<ChannelId>>>,
    color_channels: Arc<Mutex<HashSet<ChannelId>>>,
}

impl MudServer {
    fn new(world: WorldHandle, content: Arc<GameContent>) -> Self {
        Self {
            world,
            content,
            next_connection_id: Arc::new(AtomicU64::new(1)),
            connection_id: 0,
            user: None,
            fingerprint: None,
            actor_id: None,
            channels: Arc::new(Mutex::new(HashMap::new())),
            opening_channels: Arc::new(Mutex::new(HashSet::new())),
            color_channels: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    async fn actor(&mut self) -> Result<ActorId> {
        if let Some(actor_id) = self.actor_id {
            return Ok(actor_id);
        }
        let requested_name = self
            .user
            .clone()
            .unwrap_or_else(|| format!("gardener-{}", self.connection_id));
        let actor = self
            .world
            .ensure_human(requested_name, self.fingerprint.clone())
            .await
            .map_err(anyhow::Error::msg)?;
        self.actor_id = Some(actor.id);
        Ok(actor.id)
    }

    async fn run_line(&mut self, line: &str, color: bool) -> (String, bool) {
        let actor_id = match self.actor().await {
            Ok(actor_id) => actor_id,
            Err(error) => {
                return (
                    terminal::error(
                        &format!(
                            "{}\r\n",
                            self.content
                                .render("ui.gate_error", &[("error", error.to_string())])
                        ),
                        color,
                    ),
                    false,
                );
            }
        };
        let output = match commands::parse_with_content(line, &self.content) {
            Ok(command) => self
                .world
                .execute(actor_id, command)
                .await
                .map_err(anyhow::Error::msg),
            Err(error) => Err(anyhow::Error::msg(error)),
        };
        match output {
            Ok(output) => (terminal::output(&output, color), output.quit),
            Err(error) => (
                format!("{}\r\n", terminal::error(&error.to_string(), color)),
                false,
            ),
        }
    }

    async fn welcome(&mut self, color: bool) -> String {
        let actor_id = match self.actor().await {
            Ok(actor_id) => actor_id,
            Err(error) => {
                return terminal::error(
                    &format!(
                        "{}\r\n",
                        self.content
                            .render("ui.gate_error", &[("error", error.to_string())])
                    ),
                    color,
                );
            }
        };
        let mut text = format!(
            "\r\n{}\r\n{}\r\n",
            terminal::accent(&self.content.game.tagline, color),
            terminal::hint(self.content.text("ui.command_hint"), color)
        );
        if let Ok(changes) = self
            .world
            .execute(actor_id, commands::Command::Changes)
            .await
        {
            text.push_str(&terminal::output(&changes, color));
        }
        if let Ok(look) = self
            .world
            .execute(actor_id, commands::Command::Look(None))
            .await
        {
            text.push_str(&terminal::output(&look, color));
        }
        text.push_str("\r\n");
        text.push_str(&terminal::prompt(self.content.text("ui.prompt"), color));
        text
    }
}

impl Server for MudServer {
    type Handler = Self;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        let mut handler = self.clone();
        handler.connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        handler.user = None;
        handler.fingerprint = None;
        handler.actor_id = None;
        handler.channels = Arc::new(Mutex::new(HashMap::new()));
        handler.opening_channels = Arc::new(Mutex::new(HashSet::new()));
        handler.color_channels = Arc::new(Mutex::new(HashSet::new()));
        handler
    }

    fn handle_session_error(&mut self, error: <Self::Handler as Handler>::Error) {
        eprintln!("SSH session {} ended: {error:#}", self.connection_id);
    }
}

impl Handler for MudServer {
    type Error = anyhow::Error;

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let user = user.to_string();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        match self
            .world
            .ensure_human(user.clone(), Some(fingerprint.clone()))
            .await
        {
            Ok(actor) => {
                self.user = Some(user);
                self.fingerprint = Some(fingerprint);
                self.actor_id = Some(actor.id);
                Ok(Auth::Accept)
            }
            Err(_) => Ok(Auth::reject()),
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), Vec::new());
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if std::env::var_os("NO_COLOR").is_none() {
            self.color_channels.lock().await.insert(channel);
        }
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let color = self.color_channels.lock().await.contains(&channel);
        let welcome = self.welcome(color).await;

        let actor_id = self
            .actor_id
            .context("SSH session has no world actor after welcome")?;
        let mut events = self.world.subscribe();
        let handle = session.handle();
        let input_buffers = self.channels.clone();
        let opening_channels = self.opening_channels.clone();
        let prompt = terminal::prompt(self.content.text("ui.prompt"), color);
        let opening_banner = self.content.game.opening_banner.clone();
        let opening_banner_delay = Duration::from_millis(self.content.game.opening_banner_delay_ms);
        let opening_banner_pause = Duration::from_millis(self.content.game.opening_banner_pause_ms);
        opening_channels.lock().await.insert(channel);
        tokio::spawn(async move {
            for (index, line) in opening_banner.iter().enumerate() {
                if handle
                    .data(
                        channel,
                        format!("{}\r\n", terminal::banner(line, color)).into_bytes(),
                    )
                    .await
                    .is_err()
                {
                    opening_channels.lock().await.remove(&channel);
                    return;
                }
                if index + 1 < opening_banner.len() {
                    tokio::time::sleep(opening_banner_delay).await;
                }
            }
            tokio::time::sleep(opening_banner_pause).await;
            if handle.data(channel, welcome.into_bytes()).await.is_err() {
                opening_channels.lock().await.remove(&channel);
                return;
            }
            opening_channels.lock().await.remove(&channel);

            while let Ok(event) = events.recv().await {
                if !should_deliver_live_event(&event, actor_id) {
                    continue;
                }
                let typed = input_buffers
                    .lock()
                    .await
                    .get(&channel)
                    .cloned()
                    .unwrap_or_default();
                let text = render_live_event(
                    &event.message,
                    &prompt,
                    &String::from_utf8_lossy(&typed),
                    color,
                );
                if handle.data(channel, text.into_bytes()).await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let line = String::from_utf8_lossy(data);
        let (text, _) = self.run_line(line.trim(), false).await;
        session.data(channel, text.into_bytes())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.opening_channels.lock().await.contains(&channel) {
            if data.iter().any(|byte| matches!(byte, 3 | 4)) {
                session.close(channel)?;
            }
            return Ok(());
        }

        for byte in data {
            match byte {
                3 | 4 => {
                    session.close(channel)?;
                    return Ok(());
                }
                8 | 127 => {
                    if self
                        .channels
                        .lock()
                        .await
                        .get_mut(&channel)
                        .is_some_and(|buffer| buffer.pop().is_some())
                    {
                        session.data(channel, b"\x08 \x08".to_vec())?;
                    }
                }
                b'\r' | b'\n' => {
                    let line = {
                        let mut channels = self.channels.lock().await;
                        let buffer = channels.entry(channel).or_default();
                        if buffer.is_empty() && *byte == b'\n' {
                            continue;
                        }
                        String::from_utf8_lossy(&std::mem::take(buffer)).to_string()
                    };
                    session.data(channel, b"\r\n".to_vec())?;
                    if line.trim().is_empty() {
                        let color = self.color_channels.lock().await.contains(&channel);
                        session.data(
                            channel,
                            terminal::prompt(self.content.text("ui.prompt"), color).into_bytes(),
                        )?;
                        continue;
                    }
                    let color = self.color_channels.lock().await.contains(&channel);
                    let (text, quit) = self.run_line(line.trim(), color).await;
                    session.data(channel, text.into_bytes())?;
                    if quit {
                        session.close(channel)?;
                        return Ok(());
                    }
                    let prompt = format!(
                        "\r\n{}",
                        terminal::prompt(self.content.text("ui.prompt"), color)
                    );
                    session.data(channel, prompt.into_bytes())?;
                }
                byte if !byte.is_ascii_control() => {
                    self.channels
                        .lock()
                        .await
                        .entry(channel)
                        .or_default()
                        .push(*byte);
                    session.data(channel, vec![*byte])?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub async fn run(
    world_path: PathBuf,
    bind: SocketAddr,
    debug_bind: Option<SocketAddr>,
    host_key_path: PathBuf,
    tick_interval: Duration,
    content: Arc<GameContent>,
    content_path: PathBuf,
) -> Result<()> {
    let world = WorldHandle::start_with_content(world_path, tick_interval, content.clone());
    let key = load_or_create_host_key(&host_key_path)?;
    let config = russh::server::Config {
        inactivity_timeout: Some(Duration::from_secs(60 * 60)),
        auth_rejection_time: Duration::from_millis(200),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![key],
        nodelay: true,
        ..Default::default()
    };
    let mut server = MudServer::new(world.clone(), content.clone());
    println!("MUDGarden is listening on ssh://{bind}");
    println!("Connect with: ssh -p {} <name>@{}", bind.port(), bind.ip());
    if let Some(debug_bind) = debug_bind {
        println!("Backend visualizer: http://{debug_bind}");
    }
    let serve = server.run_on_address(Arc::new(config), bind);
    let debug = debug::serve(world.clone(), debug_bind, content, content_path);
    tokio::pin!(serve);
    tokio::pin!(debug);
    let result = tokio::select! {
        result = &mut serve => result.context("SSH server stopped"),
        result = &mut debug => result.context("backend visualizer stopped"),
        signal = shutdown_signal() => {
            signal?;
            println!("MUDGarden is shutting down.");
            Ok(())
        }
    };
    world.shutdown().await;
    result
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate()).context("could not listen for SIGTERM")?;
    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.context("could not listen for Ctrl-C")
        }
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .context("could not listen for Ctrl-C")
}

fn load_or_create_host_key(path: &Path) -> Result<PrivateKey> {
    if path.exists() {
        return russh::keys::load_secret_key(path, None)
            .with_context(|| format!("could not read host key {}", path.display()));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    key.write_openssh_file(path, ssh_key::LineEnding::LF)?;
    Ok(key)
}

fn render_live_event(message: &str, prompt: &str, typed: &str, color: bool) -> String {
    format!(
        "\r\x1b[2K{}\r\n{prompt}{typed}",
        terminal::event(message, color)
    )
}

pub(crate) fn should_deliver_live_event(
    event: &crate::domain::WorldEvent,
    actor_id: ActorId,
) -> bool {
    event.actor_id != Some(actor_id)
        && (event.room_id.is_none() || event.recipients.contains(&actor_id))
}

#[cfg(test)]
mod tests {
    use crate::domain::{ActorId, EventId, EventKind, RoomId, WorldEvent};

    use super::{render_live_event, should_deliver_live_event};

    fn event(
        actor_id: Option<ActorId>,
        room_id: Option<RoomId>,
        recipients: Vec<ActorId>,
    ) -> WorldEvent {
        WorldEvent {
            id: EventId(1),
            at: 1,
            kind: EventKind::System,
            actor_id,
            room_id,
            plant_id: None,
            recipients,
            message: "Something happens.".to_string(),
        }
    }

    #[test]
    fn live_events_skip_command_echoes_and_unrelated_rooms() {
        let player = ActorId(1);

        assert!(!should_deliver_live_event(
            &event(Some(player), Some(RoomId(1)), vec![player]),
            player,
        ));
        assert!(!should_deliver_live_event(
            &event(Some(ActorId(2)), Some(RoomId(2)), vec![ActorId(2)]),
            player,
        ));
        assert!(should_deliver_live_event(
            &event(Some(ActorId(2)), Some(RoomId(1)), vec![player]),
            player,
        ));
        assert!(should_deliver_live_event(
            &event(None, None, Vec::new()),
            player,
        ));
    }

    #[test]
    fn live_events_replace_the_active_prompt_before_redrawing_it() {
        assert_eq!(
            render_live_event("Mara arrives.", "> ", "say hel", false),
            "\r\x1b[2KMara arrives.\r\n> say hel"
        );
    }
}
