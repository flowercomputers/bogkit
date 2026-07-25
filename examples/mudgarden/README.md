# MUDGarden

MUDGarden is a persistent gardening MUD that runs over SSH. Every person and
server-run resident has one home garden. Public paths connect shared growing
spaces, while visits provide a direct route to another gardener's gate.

The world continues without connected players. Plants dry, recover, grow,
flower, fruit, and become dormant. Weather and seasons are global. Ivo, Wren,
Mosswife, and Almanac use live model planning to pursue persisted goals as a
gardener, helper, spirit, and weather god.

## Current state

MUDGarden is a working prototype rather than a design sketch. The current
implementation includes:

- persistent SSH identities, private home gardens, shared rooms, live room
  events, visits, permissions, trading, and decorations;
- an 8×8 planting board per garden with scheduled growth, care, harvesting,
  weather, and seasons;
- a global 24×24 living-world grid with 2,000 persisted organisms,
  incremental hydrology and habitat updates, aggregate ecology views, surveys,
  and restoration actions;
- four optional model-planned residents whose actions use the same command,
  permission, and mutation paths as human actions;
- a browser field console for live world inspection, clock controls, content
  editing, resident action traces, and multi-player session testing; and
- local Docker packaging plus Railway deployment configuration.

The main deliberate limits are that the ecology is qualitative rather than a
calibrated scientific model, organisms do not yet reproduce or disperse, agent
traces are kept only in memory, and the browser console has no built-in
authentication. Keep that console on loopback or behind platform access
control. See [`FEATURE_MAP.md`](./FEATURE_MAP.md) for the complete implemented
surface and current limits.

## Run

Run these commands from the repository root. The project uses the Rust
toolchain pinned in `examples/mudgarden/rust-toolchain.toml`; the launcher also
expects `curl` and an available browser opener unless `MUDGARDEN_NO_OPEN=1` is
set.

Start the server and open the backend visualizer in your browser:

```sh
./examples/mudgarden/scripts/run-mudgarden.sh
```

The script waits for the world service to become healthy before opening the
page. It keeps the server attached to the terminal; press Ctrl-C to stop it.

To run without opening a browser, use:

```sh
MUDGARDEN_NO_OPEN=1 ./examples/mudgarden/scripts/run-mudgarden.sh
```

Restart the local server after changing content or code:

```bash
./examples/mudgarden/scripts/restart-mudgarden.sh
```

You can also start the server directly:

```sh
cd examples/mudgarden
cargo run
```

The default listener is `127.0.0.1:2222`:

```sh
ssh -p 2222 your-name@127.0.0.1
```

The backend visualizer and field console are available at
`http://127.0.0.1:2223`. It shows the live room graph, individual 8×8 gardens,
global world grid, organisms, plants, actors, materialized schedules, and the
latest event records.
It receives snapshots in real time over a server-sent event stream, reconnects
automatically, and falls back to three-second polling if the stream is
unavailable. The world-clock controls can pause or resume automatic ticks,
advance the simulation by one hour, and change the real-time tick rate without
restarting the backend. Garden-room inspectors include an interactive compact
board, while resident inspectors combine live agent state, configured persona,
home-garden permissions and plots, inventory, and action history. It never
exposes SSH key fingerprints. The **Edit
copy** button provides a structured editor for
game identity, places, home descriptions, resident personalities, interface
strings, and shared dialogue policy. The **Residents** section also edits each
agent's actor kind, strategy, persistent goal, wake interval, action budget,
persona, voice, interests, boundaries, and example lines. Stable agent IDs are
shown but remain read-only.

Choose the **A** lens in the event stream to inspect resident action traces.
You can also select a resident directly in the World Simulation map and open
any run from the **Agent action history** section of their record inspector.
Each trace includes the exact instructions and world context supplied to the
model, every model request, concise model-supplied rationales, read-only
live-world queries and their results, the final command, and whether the world
accepted it. Private hidden model reasoning is not available or represented as
if it were. The backend retains the latest 32 completed traces in memory; they
clear on restart and are not written into the world database.

Open `http://127.0.0.1:2223/sessions` or choose **Sessions** in the field
console to test multiple human players in one browser. Each pane owns a
separate player identity, runs commands through the same parser and serialized
world service as SSH, and receives the same room-scoped live events. **Meet at
path** moves every open pane to the Common Path for quick speech, movement,
trade, and permission testing.

Copy changes are validated against the full content schema and saved to
`mudgarden-content.json` by default. Restart the backend to apply them to the
world and model-planned residents. The launcher automatically loads the saved
file on the next run.

Set `OPENAI_API_KEY` before starting the server to enable model-planned
residents. There is deliberately no deterministic agent fallback: without the
key, or if the model service is unavailable, plants and weather continue
ticking but residents wait for their next model-planned turn.
When a human speaks in a room, enabled residents in that room receive an
immediate speech-triggered turn and must answer with a `say` command. Resident
speech does not trigger more reactive turns, preventing automatic reply loops.
Residents may use up to four read-only observation queries such as `look`,
`look garden`, or `survey` before choosing an action. These queries run
through the serialized world service and cannot mutate the simulation.

The SSH username selects a persistent profile, and the public key is bound to
that username as its credential. Reconnecting with the same username and key
restores the same location, inventory, permissions, and single home. The same
key may back multiple usernames, and each username receives a separate profile.

Environment variables:

- `MUDGARDEN_DB` — persistent Fold database path (`mudgarden.db`, relative to
  the process working directory)
- `MUDGARDEN_BIND` — SSH address (`127.0.0.1:2222`)
- `MUDGARDEN_DEBUG_BIND` — visualizer HTTP address (`127.0.0.1:2223`);
  set to `off` to disable
- `MUDGARDEN_HOST_KEY` — persistent server key (`mudgarden_host_key`, relative
  to the process working directory)
- `MUDGARDEN_TICK_SECONDS` — real seconds per world hour (`20`)
- `MUDGARDEN_GRID_EDGE` — square world-grid width and height (`24`, clamped
  to `4..32`; legacy `MUDGARDEN_BOG_EDGE` is also accepted)
- `MUDGARDEN_WORLD_ORGANISMS` — initially tracked wild organisms (`2000`,
  with legacy `MUDGARDEN_BOG_ORGANISMS` also accepted;
  clamped to `edge²/2..10000`)
- `MUDGARDEN_ECOLOGY_WORK_BUDGET` — maximum ecology records updated per tick
  (`160`, clamped to `16..2000`)
- `MUDGARDEN_CONTENT` — content override and editor save path
  (`mudgarden-content.json`, relative to the process working directory)
- `MUDGARDEN_AGENT_MODEL` — model used by residents (`gpt-5.6-terra`);
  resident requests use medium reasoning effort
- `MUDGARDEN_MODEL_TIMEOUT_SECONDS` — model request timeout (`30`)
- `OPENAI_BASE_URL` — optional compatible Responses API base URL

For a stdin/stdout development session:

```sh
cd examples/mudgarden
cargo run -- local
```

Run the automated test suite with:

```sh
cargo test -p mudgarden
```

## Deploy on Railway

The example includes a production `Dockerfile` and `railway.toml`. Railway
builds the `mudgarden` binary from the repository root, listens for SSH on
container port `2222`, and stores runtime data under `/data`.

After linking the Railway project, deploy the current checkout with:

```sh
./examples/mudgarden/scripts/deploy-mudgarden.sh
```

The script targets the `mudgarden` service in `production` by default. Set
`RAILWAY_SERVICE` or `RAILWAY_ENVIRONMENT` to override either target, and pass
any additional `railway up` options as script arguments.

1. Create a Railway service from the GitHub repository and branch containing
   this project. Keep the build root at `/` because the crate depends on the
   workspace's `fold` package.
2. Set the service's **Config as Code** path to
   `/examples/mudgarden/railway.toml`.
3. Add a Railway volume mounted at `/data`. This persists the world database
   and SSH host key across deploys.
4. Add `OPENAI_API_KEY` as a service variable.
5. In **Settings → Networking**, create a **TCP Proxy** targeting port `2222`.
6. Deploy, then connect using the proxy hostname and port Railway provides:

   ```sh
   ssh -p <railway-port> <name>@<railway-host>
   ```

Optional service variables such as `MUDGARDEN_TICK_SECONDS`,
`MUDGARDEN_AGENT_MODEL`, and `MUDGARDEN_MODEL_TIMEOUT_SECONDS` work the same as
they do locally. `MUDGARDEN_BIND`, `MUDGARDEN_DB`, and `MUDGARDEN_HOST_KEY`
already have container defaults and normally do not need to be set.

The visualizer stays loopback-only by default. To expose it from a hosted
service, set `MUDGARDEN_DEBUG_BIND=0.0.0.0:8080` and route HTTP traffic to
port `8080`. The field console includes clock, content-editing, and test-session
controls and intentionally has no application-level authentication, so put it
behind your platform's access controls rather than exposing it as a public URL.

## Configure the world and residents

[`content.json`](./content.json) is the bundled content package. It owns the
game identity, room and garden descriptions, species, starter inventory,
command help, player-facing text templates, and NPC definitions. Set
`MUDGARDEN_CONTENT` to choose another override file. Objects merge recursively
while arrays replace the bundled array. The HTML editor writes a complete,
validated configuration to that path.

For example:

```json
{
  "game": {
    "tagline": "A damp little world that remembers."
  },
  "text": {
    "output.quit": "You close the gate behind you."
  }
}
```

NPCs are configured by stable `id`, with their simulation role (`kind` and
`strategy`) kept separate from their `dialogue` profile. A dialogue profile has
a persona, voice, interests, boundaries, and example lines. The shared
dialogue policy adds rules that apply to every resident. NPCs are keyed by
stable ID, so an override can target a single field such as
`npcs.mosswife.dialogue.voice`. Existing persisted NPCs keep their world state,
while their stable ID, strategy, and goal are refreshed from the content
package at startup.

The static merchant and her decoration catalog are configured under
`merchant`. Each catalog entry supplies its displayed name and description,
single-character garden-board symbol, and fruit cost. The field console exposes
these settings under **Garden shop**.

## Play

Start with `look`, `look garden`, `inventory`, `help`, and `who`. Use `look
<direction>`—for example, `look east`—to preview the room beyond an exit without moving. Plant a
starter seed in your home, or walk out and use the public Glasshouse, Moon Bed,
Pond, Compost beds, or Wild Edge. Mature plants provide fruit and new seeds:

Use `walk to <place>` to follow the existing room graph to a named destination.
The journey is summarized, but it still crosses each intermediate room and
respects closed private garden gates. `home` and `visit <person>` remain direct
routes for returning home and reaching another gardener's gate.

```text
plant scarlet runner bean at C4 as runner
water C4
harvest C4
```

Sorrel keeps a handcart on the Common Path. Harvested fruit can be traded for
persistent garden decorations, which occupy a plot just like a plant:

```text
out
out
shop
buy mossy stone seat
home
place mossy stone seat at D5
survey garden
take D5
```

More expensive decorations cost multiple fruit of any species. `inventory`
shows carried decorations; `look garden`, `survey garden`, and `inspect
<decoration|coordinate>` show decorations after they are placed. The gardener
who placed a decoration or the garden's owner can pick it back up.

Every garden is an 8x8 board. `look garden` turns the current planting into a
short description of its shape, growth, and condition; `survey garden` draws
ranks 1-8 and files A-H. Plant actions accept coordinates such as `C4` in place
of a plant name. The older `garden` shorthand also draws the board.

Use `gardens` to discover other players' homes. Visiting stops at the owner's
gate, where you can knock or enter if the owner has unlocked the garden or
already allowed you to tend or harvest:

```text
gardens
visit Daniel
knock
enter
out
out
```

The first `out` moves from the garden to its gate; the second reaches the
Common Path. `home` remains a direct route back to your own garden from
anywhere.

Other social commands include `say`, `offer`, `allow`, and `forbid`. The owner
of a home garden can use `unlock` to let anyone enter and `lock` to return to
permission-only entry. When someone is waiting outside, `admit <person>` lets
that person enter once. Entry never grants permission to tend or harvest.

## Work the living world

The persisted ecology is a single global spatial substrate shared by every
room. Its default 24×24 grid holds 576 habitat cells and 2,000 individual
organisms. Each room receives a contiguous region derived deterministically
from the room graph, so existing saved worlds gain spatial regions without a
record migration. Cells track their owning room plus water table, moisture, pH,
nutrients, temperature, light, peat depth, and scrub cover. Organisms track
health, biomass, age, and life stage.

Weather, neighboring water tables, plant uptake, peat builders, seasonal
growth, habitat fit, shade, and scrub competition affect changes over time.
The work is deliberately incremental: each world tick updates at most
`MUDGARDEN_ECOLOGY_WORK_BUDGET` ecology records, even after a long pause.

From anywhere, `ecology` runs an aggregate view of habitat, moisture
percentiles, ecological stress, and species health. Every room supports:

```text
survey
survey 12 7
restore 12 7
```

`survey` without coordinates reads the center of the current room's region.
Explicit coordinates must belong to that room. `restore` raises the local water
table, adds a small nutrient pulse, removes encroaching scrub, and schedules the
cell to respond on the next tick. Model-planned residents receive the same
ecology commands plus a compact list of restoration candidates.

## Shape of the world

All commands—human or autonomous—enter the same serialized world service.
There is no privileged mutation path for agents. Due residents receive a
bounded snapshot containing only relevant world state. Model requests run
concurrently outside the world-owner thread and return one function call whose
command is parsed and validated normally. Names, room speech, and recent events
in that snapshot are sent to the configured model provider. A single keyed
source stores actors, rooms, gardens, plants, events, the clock, and metadata.
Fold derives:

- current actor, room, garden, and plant tables;
- event history ranked by event ID;
- plants ranked by their next transition time;
- agents ranked by their next wake time;
- the live set of plants needing water;
- current world cells and organisms;
- separate due-time rankings for world cells and organisms;
- the live set of stressed organisms;
- organisms grouped by habitat cell;
- per-species count, health, biomass, and flowering aggregates;
- a materialized moisture histogram used for percentile queries.

Updating one plant retracts its old contributions and adds its new ones across
all of those views atomically. Ticks therefore touch only plants and residents
whose scheduled time is due, rather than scanning the world.

The same retract-and-add behavior applies to world cells and organisms. See
[`FEATURE_MAP.md`](./FEATURE_MAP.md) for a status map and architecture diagram.

The SSH layer is deliberately raw: line-oriented text, public-key identity,
one-shot commands for scripts, and a scrolling interactive shell with live
relevant events. `changes` gives a bounded summary after reconnecting.
