## Much faster text embedding preview

This repo contains a rust crate containing the static embedding model deployment pipeline described [here](https://www.flowercomputer.com/news/fast-static-embedding/). 

The crate exposes two functions, `encode` and `encode_single`, take a look in `lib.rs` for more info.

### `encode`
This accepts an array of strings, encoding them in parallel

### `encode_single`
This accepts a single string

## To use

Clone the repo, and reference locally as a crate in your rust project. If you want to use a preliminary python wheel version [see the documentation at `./api-py/readme.md`](./api-py/readme.md). You might need to `cargo install maturin`.

### Crate features

#### Quantization and Truncation
By default this crate provides an unquantized model truncated to 512 dimensions. To change this, change the feature setting in your cargo.toml for your `ese` dependency ([see `Cargo.toml` for all crate features](./Cargo.toml)).

## Reproducible model artifacts

The build pins the model and tokenizer to Hugging Face revision
`f60985c706f192d45d218078e49e5a8b6f15283a` and verifies both files with
SHA-256 before using them. Verification applies to newly downloaded files and
files already present in the Cargo target directory's `ese-cache` subdirectory
(for example, `target/ese-cache`, or `$CARGO_TARGET_DIR/ese-cache` when a custom
target directory is configured). If a cached file fails verification, remove
that named file and rebuild so ESE can fetch the pinned copy.

Consumers that persist embeddings or indexes should store `ese::ENCODER_ID` and
require an exact match when reopening them. `ese::ENCODER_IDENTITY` exposes the
same compatibility inputs as fields: model revision, artifact hashes,
preprocessing version, output dimensions, and scalar representation.
