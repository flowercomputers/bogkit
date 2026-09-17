# Bog Cloud / Flower design alignment

The company site is the primary visual source. `cloud/static/flower-site.css` is a byte-for-byte copy of its `src/styles/site.css`. `flower-header.css` and `flower-footer.css` copy the corresponding Astro component style blocks, with only Astro's `:global(...)` wrappers removed. Arizona Text and Sans font files are exact copies; the Flower mark and copy icon reuse the source paths.

The shared HTML shell applies the same design to the native and legacy homepages, documentation, about/contact/privacy pages, console, and device approval page. Product-specific forms and controls live in `guide.css`, `console.css`, and `cloud.css`. Source tokens, header dimensions, and breakpoints remain authoritative: 640px mobile navigation and 1600px large-screen scaling.

The console preserves existing field IDs, requests, permissions, and credential handling. It adds section navigation and moves organization creation into its own section. Signed-out users receive a clear sign-in prompt. Public machine-readable contracts are unchanged.

Verification: desktop side-by-side reference comparison at 1280px, mobile navigation and approval/console pages at 390px, large-screen scaling at 1700px, copy-prompt action, service test suites, JavaScript syntax checks, and strict cloud/MCP Clippy. The shared-shell test guards against duplicated navigation/targets across the four page templates.
