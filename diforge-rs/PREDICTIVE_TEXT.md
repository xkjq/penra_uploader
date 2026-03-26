**Predictive Text — Design Summary for diforge**

Overview:
- **Goal:** Provide inline predictive text completions in the diforge editor based on the user's past report history and the current exam context. Prioritise suggestions from the same exam type and recent reports.

Requirements:
- **Context-aware:** use exam metadata (exam type, body part, modality) to bias suggestions.
- **History-based:** learn from user's previous reports (local per-user corpus).
- **Weighted:** same-exam reports and recent reports get higher weight.
- **Low-latency:** completions must appear interactively in the editor.
- **Privacy:** store and process history locally by default; opt-in for any server sync.
- **Configurable:** allow enabling/disabling, clamp memory size, and purge history.

Data collection & storage:
- **Corpus source:** past reports (project-level and user-level) and in-session typing events (if user opts-in).
- **Storage format:** local SQLite or LMDB DB in the user's config dir; store tokenized n-grams, metadata (exam type, timestamp, file ID), and aggregated counts.
- **Retention:** configurable window (e.g., 1M tokens or X months). Keep a compact index for fast lookup.

Suggestion model & scoring:
- **Tiered approach (recommended):**
  - Stage 1 (fast, on-device): weighted n-gram model (3- to 5-gram) with exam-type conditioning and recency decay. Extremely fast for interactive suggestions and easy to implement.
  - Stage 2 (optional advanced): lightweight transformer or LSTM fine-tuned on local history for richer long-range suggestions. Use only if Stage 1 quality is insufficient.
- **Scoring function:** combine:
  - base n-gram probability
  - exam match multiplier (e.g., same-exam ×2, same-modality ×1.2)
  - recency decay (exponential, half-life configurable)
  - personal frequency (user-specific counts > global/project counts)
  - safety/blacklist penalty for PHI or disallowed phrases

Indexing & retrieval:
- Keep an in-memory hot index for most-recent/relevant n-grams for interactive latency.
- Query by current token prefix and exam context; return top-K completions with scores.

Editor integration:
- **Hooks:** instrument the editor component to request completions on: token boundary, explicit shortcut (e.g., Tab/Ctrl+Space), and as-you-type (debounced ~100ms).
- **UI:** inline ghost text for single best completion, dropdown for multiple suggestions. Provide accept/reject shortcuts.
- **Telemetry:** locally record accepted suggestions for reinforcement learning of ranking.

Privacy & settings:
- Default: local-only storage, opt-in for sharing or server-backed personalization.
- Provide UI to view/delete stored history and to export/import corpus.
- Encrypt on-disk DB if user requests (password/OS keyring integration).

Performance & resource constraints:
- Keep n-gram tables bounded (prune low-frequency entries). Use compact integer encodings for tokens.
- Run heavy model operations off the UI thread; use a background worker and a simple RPC (channel) to the UI.

Implementation plan (concrete steps):
1. Identify editor component and data paths in the repository (where to hook completions).
2. Implement local corpus extractor that ingests past reports and emits tokenized n-grams with metadata.
3. Implement an on-device n-gram suggestion service with exam-aware scoring and recency weighting.
4. Integrate a suggestions API (request/response) and wire it into the editor UI with inline ghost text and dropdown.
5. Add settings UI (enable/disable, retention, weighting parameters) and privacy controls.
6. Add tests (unit for scoring, integration for latency and correctness) and performance benchmarks.

Tradeoffs & recommendations:
- Start with the n-gram approach: fastest to implement, explainable scores, low resource usage.
- Reserve transformer/LSTM options for a second phase only if n-gram quality is insufficient.
- Keep everything local by default to avoid PHI/exposure risks; if server syncing is desired, require explicit encryption and consent.

Files to add or update (suggested):
- /home/ross/penra_uploader/diforge-rs/src/ — suggestion service and background worker module.
- /home/ross/penra_uploader/diforge-rs/src/editor_integration.rs — small adapter to call suggestion API from the editor.
- /home/ross/penra_uploader/diforge-rs/PREDICTIVE_TEXT.md — this design summary.

Next steps I can take:
- Locate the editor code and open the exact integration points, then implement a prototype n-gram suggestion service and basic UI integration.

— End of summary
