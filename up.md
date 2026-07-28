# Tasks

Common development commands. Run any block with `upmd` (auto-opens this file)
and press Enter on it, or use `upmd --cli up.md --block <name> --yes`.

## Build

```sh [name:build]
cargo build
```

```sh [name:build-release]
cargo build --release
```

## Test

```sh [name:test, deps:"build"]
cargo test
```

```sh [name:test-one]
cargo test -- "$TEST_NAME"
```

## Lint and format

```sh [name:clippy]
cargo clippy -- -D warnings
```

```sh [name:fmt-check]
cargo fmt -- --check
```

```sh [name:fmt]
cargo fmt
```

## Run

```sh [name:run-cli, deps:build]
cargo run -- --cli README.md --all --yes
```

```sh [name:run-tui, deps:build]
cargo run -- DEMO.md
```

```sh [name:run-demo, deps:build]
cargo run -- --cli DEMO.md --all --yes
```

```sh [name:run-file]
cargo run -- "$FILE"
```

## Verification

```sh [name:check-config, deps:build]
cargo run --quiet -- --dump-default-config | diff - src/apps/config.toml
```

```sh [name:verify-all, deps:"clippy | fmt-check, test"]
echo all checks passed
```

## Maintenance

```sh [name:clean]
cargo clean
```

```sh [name:outdated]
cargo outdated -R
```

## Demo recordings

```sh [name:demos, deps:build-release]
for tape in .github/vhs/*.tape; do
  vhs "$tape"
done
```
