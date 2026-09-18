# Chat/history sync example

Create a Bog using `definition.json` in this directory. Install a private app
configuration, then run either equivalent reader:

```sh
python3 starters/cloud-chat/sync.py --config /private/path/app.json
node starters/cloud-chat/sync.mjs /private/path/app.json
```

Write messages with `put(key, {role, content, created_at})`, where `created_at` is
a numeric timestamp. Each reader obtains the latest 100 messages in **one ranked
request**, including those three fields. Results are newest first; reverse this
bounded view for an oldest-to-newest chat display. Keys need not encode time.

Both readers capture a change cursor **before** fetching the initial view.
A write between reading the view and waiting is therefore not lost. They refetch
the relevant ranked view whenever `changed` or `reset` is true. This covers edits,
deletions, earlier-sorting inserts and bursts without mirroring the whole Bog.
A worker restart resets the cursor; it does not turn notifications into a durable
event log. Multiple changes can be collapsed into one invalidation.

For older ranked history, call `top('recent', limit, offset, include_fields)` with
the next offset (Python uses named `include_fields`). Refresh the visible window
on invalidation because concurrent changes can shift offset pages. Table listing
also supports exclusive `after`/`before` key bounds; those bounds order keys, not
message timestamps, and cannot be mixed with offset pagination.

Restarting a reader obtains a new cursor and view. Renew expired app credentials
through a fresh private handoff and restart with the new file; never embed the
management credential in the application. These examples print record contents,
so run them only where the application history may be displayed.
