# Life Zone

A locally-run desktop simulation in which a community of creatures — each driven by a local
LLM — forages, farms, forms families, and dies. Creatures live about four in-game weeks, so
the unit that matters is the **lineage**, not the individual.

The player does not command creatures. They build the world, set the rules, watch, and read
the record.

- **Spec:** [`docs/PRD.md`](docs/PRD.md) — source of truth
- **Plan:** [`docs/BUILD.md`](docs/BUILD.md) — milestones, invariants, testing
- **Design:** [`design/mockups/`](design/mockups/) — visual source of truth

## Running

```
npm install
npm run tauri dev
```

Tests:

```
cargo test --manifest-path src-tauri/Cargo.toml
```

To view the design mockups (they must be served over HTTP, not opened as `file://`):

```
cd design/mockups && python -m http.server 8731
```

## Platform notes

`rust-toolchain.toml` pins `stable` for the **host** triple. The primary dev machine is ARM64
(Snapdragon X Elite); an x86_64 build there would run under Prism emulation, which the M2 tick
budget cannot afford.

Ollama on that machine is **CPU-only**, which makes deliberation far more expensive than
`docs/PRD.md` §5.1 assumes. Measured with a ~760-token prompt:

| model | median call | calls/sec |
|---|---|---|
| qwen3:8b (the PRD's target) | 16.2s | 0.06 |
| qwen3:4b | 11.0s | 0.09 |
| qwen3:1.7b (**current default**) | 6.8s | 0.15 |

Two consequences are baked into the config defaults:

- **Concurrency buys nothing** on a CPU-only host — throughput is flat from 1 to 6 concurrent
  calls, because it is compute-bound rather than I/O-bound.
- **Prompt ingestion dominates**, and Ollama's prefix cache is exploitable: ordering the prompt
  static-first and creature-specific-last cut prompt-eval from 3.82s to 0.58s. This is
  `llm.static_prefix_ordering` and it is worth more than model choice.

Fonts are bundled in `src/assets/fonts/`. The app makes no network requests except to Ollama
on localhost, and the webview CSP enforces that.
