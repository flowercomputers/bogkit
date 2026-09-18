# Node / TypeScript notebook starter

The dependency-free Node runtime uses the typed Bog Cloud client. It serves the
same notebook page as the Python starter, persists notes in Bog, and supports
semantic and text search when those resources have been added.

```sh
node starters/cloud-notebook-ts/server.js --config /private/path/app.json --port 4318
```

Open http://127.0.0.1:4318. The credential stays in the server process. The server
binds only loopback, validates Host/Origin, limits input, serves only its one page,
and does not expose the configuration directory. Private configuration is mode 600.
Add `semantic_search` and `text_search` resources with the client before searching.
