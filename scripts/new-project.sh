#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: ./scripts/new-project.sh <project-name>" >&2
  exit 1
fi

name="$1"

if ! [[ "$name" =~ ^[a-zA-Z0-9][a-zA-Z0-9_-]*$ ]]; then
  echo "project name must start with a letter or number and contain only letters, numbers, '-' or '_'" >&2
  exit 1
fi

project_dir="examples/$name"

if [ -e "$project_dir" ]; then
  echo "$project_dir already exists" >&2
  exit 1
fi

mkdir -p "$project_dir/src"

cat > "$project_dir/Cargo.toml" <<EOF
[package]
name = "$name"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
anny = { path = "../../anny" }
ese = { path = "../../ese", features = ["dim-512", "quant-8"] }
fold = { path = "../../fold" }
serde = { version = "1", features = ["derive"] }
EOF

cat > "$project_dir/src/main.rs" <<EOF
fn main() {
    println!("welcome to bog kit. start hacking in $project_dir/src/main.rs");
}
EOF

echo "created $project_dir"
echo "run it with: cargo run -p $name"
