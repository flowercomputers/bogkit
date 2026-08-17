# tinymo — a toio cube with a bog memory

Every move the cube makes is stored in bog (fold). Two experiments:

1. **record / playback** — press `R` to start recording, drive around, `R` to stop, `P` to
   have the cube replay the moves.
2. **boomerang** — make N moves (default 5); the cube then drives them back in reverse order
   with the wheels negated, returning (approximately — dead reckoning, no mat) to where it
   started. Then it's ready for the next trip.

## Layout

```
src/main.rs        the "brain": owns the fold Stream, listens on 127.0.0.1:7777
toio/              Python BLE driver (from ../toio): tinymo_driver.py talks to the cube
                   and to the brain over newline-delimited JSON
```

**What counts as a move.** Holding a key auto-repeats, sending the same 400 ms drive every
few tens of ms. The brain merges such a burst into ONE move lasting as long as you held the
key (it retracts the stored move and re-inserts the extended one — fold makes that cheap), so
"5 moves" means five distinct presses/holds, not five auto-repeat ticks. `--no-merge` turns
that off. In boomerang mode the brain waits for the last move to finish before reversing.

Moves are stored as `Move { session, seq, at_ms, left, right, duration_ms, label }` in a
`Bag<Move>` plus a `KeyBy(session) → Aggregate → Table<session, count>` — the boomerang limit is
read straight from that incrementally-maintained count. The db lives in
`$TMPDIR/tinymo.db` (override with `TINYMO_DB=path`) and is **kept** across runs (`--fresh` wipes it). Only one brain can hold the db at a time — a second one (including `inspect`) panics with `Locked`. The driver auto-reconnects if the brain is restarted.

## Run

Terminal 1 (brain):

```sh
cargo run -p tinymo -- record                 # experiment 1
cargo run -p tinymo -- boomerang --limit 5    # experiment 2
```

Terminal 2 (driver; cube powered on, LED blinking):

```sh
python3 -m pip install -r examples/tinymo/toio/requirements.txt   # bleak, once
python3 examples/tinymo/toio/tinymo_driver.py                     # or --list / --address
python3 examples/tinymo/toio/tinymo_driver.py --no-cube           # protocol only, no hardware
```

Keys: `W/S A/D` or arrows drive, `Q/E` curve, `1-9` speed, `R` record on/off, `P` play,
`L` LED, `B` beep, `X` quit. LED: green = idle, red = recording, blue = brain is driving
(keys locked), orange = boomerang armed.

## Seeing bog do the work

After every write the brain prints a snapshot read back from fold's persisted views (not
from memory): the `Count`, the per-session `Table`, and the session's rows in the `Bag`.
Extending a held-key move shows up as `-old +new` with the count unchanged. To prove the
state is on disk, stop everything and run:

```sh
cargo run -p tinymo -- inspect     # dumps every view from the db, no driver needed
```

## Tests

```sh
cargo test -p tinymo
python3 examples/tinymo/toio/test_protocol.py
```
