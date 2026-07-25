use std::io::{self, BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use mudgarden::{commands, content::GameContent, server, store::World, terminal};

#[tokio::main]
async fn main() -> Result<()> {
    let configured_content_path = std::env::var_os("MUDGARDEN_CONTENT").map(PathBuf::from);
    let content_path = configured_content_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("mudgarden-content.json"));
    let content = if configured_content_path.is_some() || content_path.exists() {
        GameContent::load(&content_path)?
    } else {
        GameContent::bundled()
    };
    let db_path = std::env::var_os("MUDGARDEN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("mudgarden.db"));
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "serve".to_string());
    if mode == "local" {
        return run_local(db_path, content);
    }
    if mode != "serve" {
        anyhow::bail!("usage: mudgarden [serve|local]");
    }
    let bind: SocketAddr = std::env::var("MUDGARDEN_BIND")
        .unwrap_or_else(|_| "127.0.0.1:2222".to_string())
        .parse()?;
    let debug_bind = match std::env::var("MUDGARDEN_DEBUG_BIND")
        .unwrap_or_else(|_| "127.0.0.1:2223".to_string())
        .trim()
    {
        "off" | "disabled" | "" => None,
        value => Some(value.parse()?),
    };
    let host_key = std::env::var_os("MUDGARDEN_HOST_KEY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("mudgarden_host_key"));
    let tick_seconds = std::env::var("MUDGARDEN_TICK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(20);
    server::run(
        db_path,
        bind,
        debug_bind,
        host_key,
        Duration::from_secs(tick_seconds.max(1)),
        content,
        content_path,
    )
    .await
}

fn run_local(db_path: PathBuf, content: std::sync::Arc<GameContent>) -> Result<()> {
    let name = std::env::var("MUDGARDEN_NAME").unwrap_or_else(|_| "gardener".to_string());

    let mut world = World::open_with_content(&db_path, content.clone());
    world.ensure_world_agents()?;
    let actor = world.ensure_human(&name, None)?;
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    play_opening_banner(&content, color)?;
    println!(
        "{}",
        terminal::accent(
            &content
                .game
                .local_intro
                .replace("{{path}}", &db_path.display().to_string()),
            color,
        )
    );
    println!(
        "{}\n",
        terminal::hint(content.text("ui.command_hint"), color)
    );

    print_output(world.execute(actor.id, commands::Command::Changes)?, color);
    print_output(
        world.execute(actor.id, commands::Command::Look(None))?,
        color,
    );

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("\n{}", terminal::prompt(content.text("ui.prompt"), color));
        io::stdout().flush()?;
        let Some(line) = lines.next() else {
            println!();
            break;
        };
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if run_local_command(&mut world, actor.id, &line, &content, color)? {
            break;
        }
    }
    world.checkpoint();
    Ok(())
}

fn play_opening_banner(content: &GameContent, color: bool) -> Result<()> {
    for (index, line) in content.game.opening_banner.iter().enumerate() {
        println!("{}", terminal::banner(line, color));
        io::stdout().flush()?;
        if index + 1 < content.game.opening_banner.len() {
            thread::sleep(Duration::from_millis(content.game.opening_banner_delay_ms));
        }
    }
    thread::sleep(Duration::from_millis(content.game.opening_banner_pause_ms));
    Ok(())
}

fn run_local_command(
    world: &mut World,
    actor_id: mudgarden::domain::ActorId,
    line: &str,
    content: &GameContent,
    color: bool,
) -> Result<bool> {
    let command = match commands::parse_with_content(line, content) {
        Ok(command) => command,
        Err(message) => {
            println!("{}", terminal::error(&message, color));
            return Ok(false);
        }
    };
    let output = match world.execute(actor_id, command) {
        Ok(output) => output,
        Err(error) => {
            println!("{}", terminal::error(&error.to_string(), color));
            return Ok(false);
        }
    };
    let quit = output.quit;
    print_output(output, color);
    if quit {
        return Ok(true);
    }
    for event in world.tick()? {
        println!("{}", terminal::event(&event.message, color));
    }
    Ok(false)
}

fn print_output(output: mudgarden::domain::WorldOutput, color: bool) {
    print!("{}", terminal::output(&output, color).replace("\r\n", "\n"));
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn local_command_errors_do_not_end_the_session() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let actor = world.ensure_human("gardener", None).unwrap();
        let content = GameContent::bundled();

        assert!(!run_local_command(&mut world, actor.id, "inspect C4", &content, false).unwrap());
        assert!(!run_local_command(&mut world, actor.id, "look", &content, false).unwrap());
    }
}
