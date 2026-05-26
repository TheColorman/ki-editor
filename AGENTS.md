# AGENTS.md

## Repo Map

- Rust workspace for the Ki editor binary; `src/main.rs` only calls `ki::main()`, with CLI/app wiring in `src/lib.rs` and `src/cli.rs`.
- Workspace crates are `event`, `my_proc_macros`, `grammar`, `shared`, `tree_sitter_quickfix`, `zed_theme`, `ki-protocol-types`, and `nvim-treesitter-highlight-queries`; `mock_repos/rust1` is intentionally excluded.
- `ki-vscode` is the VS Code extension and starts the backend as `ki @ embed <cwd>` from `ki-vscode/src/extension.ts`.
- `docs` is Docusaurus, but root `just doc` first regenerates Rust-produced doc assets.
- `ki-protocol-types` generates protocol bindings for `ki-vscode/src/protocol/types.ts` and `ki-jetbrains/src/kotlin/protocol/Types.kt`.

## Tooling

- Run commands that use project-specific tools through `nix develop -c <command>` so they use the pinned development environment instead of relying on tools installed in the host shell.
- Rust is pinned to `1.89.0`; the development shell also provides `just`, `cargo-nextest`, `typeshare`, `cargo-machete`, `alejandra`, Bun, and Node tooling.
- Use npm/package-locks, not yarn; `docs/README.md` still has stale yarn commands.
- Root JS/MD formatting is `npm run check` / `npm run fix`; Nix formatting uses `alejandra --exclude ./nvim-treesitter-highlight-queries/nvim-treesitter/ --check ./`.

## Verification

- Full local gate: `just` runs install, check, all builds, lint, tests, and docs; it is broad and expensive.
- Rust build: `cargo build --workspace --tests --locked` or `just build`.
- Rust tests: `just test [testname]` runs `cargo nextest run --workspace --no-fail-fast -- --skip 'doc_assets_' [testname]`; retries are configured in `.config/nextest.toml`.
- Focused Rust test without doc-asset generators: `cargo nextest run --workspace -- --skip 'doc_assets_' <filter>`.
- Lint: `just lint` runs clippy for workspace and tests with `-D warnings`, `cargo machete`, then VS Code unused-export checks.
- Docs site from repo root: `just -f docs/justfile build`; inside `docs`, `just build` installs, typechecks, runs `tsx validate.ts`, runs `npm test`, then builds Docusaurus.
- VS Code extension: `cd ki-vscode && npm run compile` for TS compile, `npm run bundle` for Bun bundling, and root `just vscode-package` for Nix-built binaries plus VSIX packaging.
- Tree-sitter quickfix grammar: root `just tree-sitter-quickfix` delegates to `tree_sitter_quickfix/justfile`, which runs `npm run build` then `npm run test`.

## Generated Files

- After changing `ki-protocol-types`, run `just update-typeshare`; `just check-typeshare` verifies generated TypeScript and Kotlin bindings.
- Doc-asset tests are excluded from `just test`; run `just doc-assets [testname]` when touching `docs/static` generation, recipes, keymaps, schemas, or default config output.
- `cargo test -- doc_assets_export_json_schemas` updates `docs/static/app_config_json_schema.json`, `script_input_json_schema.json`, and `script_output_json_schema.json`; `just check-config-schema` verifies only the app config schema after formatting.
- `cargo test -- doc_assets_export_keymaps_json` updates `docs/static/keymaps/*.json`; `cargo test -- doc_assets_export_keyboard_layouts` updates `docs/static/keyboard-layouts.json`; `cargo test -- doc_assets_default_config_json` updates `docs/static/config_default.json`.

## Workflow Notes

- `just test` may set global git `user.name` and `user.email` if missing because test setup creates commits; run `cargo nextest` directly to avoid that side effect.
- CLI subcommands live under `ki @`; common ones are `ki @ grammar fetch`, `ki @ grammar build`, `ki @ log [default|lsp]`, `ki @ keymap table`, and `ki @ keymap keymap-drawer`.
- PR instructions from `CLAUDE.md`: create a new branch before committing/opening a PR, and PR test plans should list only manual testing steps because CI covers automated tests.
