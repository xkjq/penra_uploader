# Report Templates — Design and Implementation

## Purpose
Provide a persistent, quickly-accessible system of report templates for radiology reports. Templates should be globally available but usually filtered by the current study using NICIP codes. A study can have multiple NICIP codes; templates can be general or tied to specific modalities/exams.

## Requirements
- Identify current study by NICIP code(s).
- Templates may apply to:
  - All studies (global/general)
  - One or more NICIP codes (study-specific)
  - Modalities / exam types (e.g., CT, MR, XR)
- Quick access in the UI (single-click / keyboard shortcut) to insert templates into the report text.
- Support multiple templates per study; allow favorites and recent lists.
- Template placeholders for patient/study values (e.g., `{{patient_name}}`, `{{study_date}}`).
- Templates editable by users and versioned/backed up.

## Identification: NICIP codes
- Each study is identified by one or more NICIP codes (strings).
- Templates include a list of applicable NICIP codes; an empty list or a special tag (e.g., `"any"`) indicates global applicability.
- When matching, templates that list any of the study's NICIP codes are considered matches.

## Template metadata (schema)
Use YAML or JSON for template files. Example minimal YAML schema:

- id: "ct_head_trauma_v1"
  title: "CT Head — Trauma (short)"
  tags: ["CT","Head","Trauma"]
  applicable_codes: ["CSKUH"]
  modalities: ["CT"]
  scope: "project"            # project | user
  favorites: false
  created_by: "rad_admin"
  created_at: "2026-03-21T12:00:00Z"
  version: 1
  body: |
    Clinical details: {{clinical_history}}

    Findings:
    - No acute intracranial hemorrhage identified.
    - No midline shift.

    Impression:
    1. No acute hemorrhage.
    2. Further correlation with clinical exam recommended.

Notes:
  - `applicable_codes` may be empty to indicate global templates.
  - `modalities` is optional; if present it narrows applicability.

## Storage layout
- Project-wide templates: `templates/project/` (one YAML per template or a single index.json)
- User overrides: `templates/user/` (same schema)
- The app loads both and merges, giving priority to user templates.
- Optionally maintain a lightweight index file `templates/index.json` for fast startup.

Example filesystem:

- templates/
  - project/
    - ct_head_trauma_v1.yaml
    - chest_xr_basic.yaml
  - user/
    - custom_template_1.yaml
  - index.json  # auto-generated cache of metadata for UI

## Matching algorithm (high level)
1. Collect NICIP codes for current study (may be empty).
2. Collect candidate templates:
   - Templates where `applicable_codes` intersects study codes.
   - Templates where `modalities` contains current modality (if known).
   - Global templates (`applicable_codes` empty or contains `any`).
3. Rank candidates:
   - Exact NICIP match with modality match (highest)
   - NICIP match without modality
   - Modality match without NICIP
   - Global templates
   - Within same rank, prefer user scope and higher `version` / `favorites` flag.
4. Present top N in UI quick list and allow searching/filtering.

## UI ideas
- Template side panel: a collapsible panel showing Top Matches, Favorites, and All templates.
- Quick insert: hotkey (e.g., Ctrl+T) opens an inline searchable popup; selecting a template inserts text at cursor.
- Template editor: open a modal to edit metadata and body (with placeholder suggestions).
- Show which NICIP codes caused the match (helpful for verification).

## Placeholders and interpolation
- Use simple Mustache-like placeholders `{{name}}`.
- Provide a small resolver API that can be passed a map of variables (patient name, DOB, study date/time, accession, referring physician, modality, NICIP codes, etc.).
- Allow optional placeholders with defaults: `{{clinical_history|Not provided}}`.

## Versioning and editing workflow
- Each template carries a `version` and `created_at`/`updated_at` timestamps.
- Editing a project template should create a user-scoped override rather than editing project files in-place (unless user has write access and chooses to update project templates).
- Keep auto-backups of edited templates in `templates/backups/` with timestamped filenames.

## Access control and sync
- Project templates can be committed to repo and synced via git/CI.
- Users may have local templates only; consider an export/import flow for sharing templates.

## Implementation notes (Rust)
- Define a `Template` struct mirroring the schema and implement serde (YAML/JSON) deserialization.
- On startup, load `templates/index.json` if present; otherwise scan `templates/project/` and `templates/user/` and build an in-memory index.
- Provide an API:
  - `fn find_templates_for_study(codes: &[String], modality: Option<&str>) -> Vec<Template>`
  - `fn get_template(id: &str) -> Option<Template>`
  - `fn render_template(template: &Template, vars: &HashMap<String,String>) -> String`
- Caching: keep metadata in memory and load bodies on demand for large template sets.

## Examples
- Global template example: `applicable_codes: []` or `applicable_codes: ["any"]`.
- Study-specific template: `applicable_codes: ["NICIP:12345"]`.

## Next steps
- Implement schema files under `templates/project/` and add a few starter templates.
- Add loader code and unit tests for matching logic.
- Add UI quick-access in `src/main.rs` and wire `render_template` to insert into the `TextEdit` buffer.

---
Created: 2026-03-21

