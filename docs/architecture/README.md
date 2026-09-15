# Git DrillDown architecture guide

Open `index.html` directly, or serve this directory for automatic JSON loading:

```bash
python3 -m http.server 8765 --directory docs/architecture
```

Then open `http://127.0.0.1:8765/#overview/system`.

## Information model

`architecture.json` is the canonical model. The viewer progressively projects the same data as:

```text
System → layer → responsibility → file → function → source
                     ↘ user flow ↗
```

The first screen intentionally shows only the three primary application layers. Files, functions, call relationships, findings and source locations remain available through **Deep dive**.

Function importance is a deterministic static-analysis heuristic based on approximate fan-in/fan-out, modeled flow participation, entry points and architectural boundary crossings. It is a study-order aid, not an objective quality score.

## Refresh after source changes

```bash
node docs/architecture/analyze-source.mjs
node docs/architecture/generate-static-data.mjs
node --test docs/architecture/viewer.test.mjs
```

The first command refreshes source-derived files, functions, ranks and traceability while preserving the hand-authored architecture, flows and findings. The second creates the generated fallback required when `index.html` is opened through `file://`.
