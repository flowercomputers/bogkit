# Localization catalog compiler

This is a dependency-free prototype for validating structured localization
catalogs and emitting deterministic runtime tables. It is a partial fit for
the narrow validation boundary only; it is not a production ICU/CLDR compiler
and it does not use a BogKit component.

Run these commands from `developer-simulation/`:

```console
cargo test --offline --locked --manifest-path Cargo.toml -p catalog-compiler-prototype
cargo fmt --manifest-path Cargo.toml -p catalog-compiler-prototype -- --check
cargo clippy --offline --locked --manifest-path Cargo.toml -p catalog-compiler-prototype --all-targets -- -D warnings
cargo build --offline --locked --release --manifest-path Cargo.toml -p catalog-compiler-prototype
mkdir -p scratch/catalog-stress
cargo run --offline --locked --release --manifest-path Cargo.toml -p catalog-compiler-prototype -- compile runs/2026-08-07--localization-catalog-compiler/fixtures/valid scratch/catalog-runtime.table
cargo run --offline --locked --release --manifest-path Cargo.toml -p catalog-compiler-prototype -- lookup runs/2026-08-07--localization-catalog-compiler/fixtures/valid de nested
cargo run --offline --locked --release --manifest-path Cargo.toml -p catalog-compiler-prototype -- lookup-table scratch/catalog-runtime.table de nested
cargo run --offline --locked --release --manifest-path Cargo.toml -p catalog-compiler-prototype -- generate-stress scratch/catalog-stress 100000 18
```

The exact trial evidence, limitations, reviewer correction, and decision audit
are in [`evidence/REPORT.md`](evidence/REPORT.md). Generated stress fixtures
and output tables are intentionally not archived.
