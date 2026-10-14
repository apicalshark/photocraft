# Stroke Lab for PhotoCraft

WebAssembly filter plug-ins for PhotoCraft's plug-in ABI v1 (`photocraft/docs/plugins.md`),
built like the upstream example `examples/plugins/invert-rs`: `no_std`, no allocator, no imports.
Each cleans up shaky rasterized line art on the active layer, after the fact.

An ABI v1 module is one manifest and one menu entry, so each plug-in is its own module:

| Module | Menu entry (Filter › Plug-ins) | Id | Dialog |
|---|---|---|---|
| `stroke_lab_smooth.wasm` | Stroke Lab: Smooth Lines… | `io.github.storytold.stroke-lab.smooth` | a strength slider and a Gentle/Standard/Strong preset dropdown |
| `stroke_lab_smooth_advanced.wasm` | Stroke Lab: Smooth Lines (Advanced)… | `io.github.storytold.stroke-lab.smooth-advanced` | radius, edge preservation, alpha handling, blend amount |

Both descriptions state the honesty clause: a post-process filter, not real-time
stabilization while drawing.

## Layout

This directory is **its own cargo workspace** (not a member of the repository workspace): it
only builds for `wasm32-unknown-unknown`, and `.cargo/config.toml` makes that the default target
here. The modules share one version (`[workspace.package]`), which every manifest reports.
Plain `cargo test` does not work here (the `panic_handler` conflicts with the test harness);
behaviour is covered by `crates/plugins/tests/smooth_lines.rs` in the main workspace.

```text
abi/        stroke-lab-photocraft: the four exports (`plugin!`), the band and the smoothing loop,
            shared by every plug-in
smooth/     stroke-lab-smooth → stroke_lab_smooth.wasm: its manifest and its parameters
advanced/   stroke-lab-smooth-advanced → stroke_lab_smooth_advanced.wasm: the same, with every control
```

A new plug-in is a crate next to `smooth/`: a manifest, and a function from its parameters to
pixel-kernel settings, handed to `stroke_lab_photocraft::plugin!`.

## Build

```sh
rustup target add wasm32-unknown-unknown
./build.sh            # cargo build --release --target wasm32-unknown-unknown
cargo clippy -- -D warnings   # no --all-targets: the test harness conflicts with no_std
```
