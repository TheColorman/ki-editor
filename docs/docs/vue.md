# Vue

Ki recognizes `.vue` single-file components and highlights templates, JavaScript,
TypeScript, JSX/TSX, CSS, and SCSS. Syntax-node, syntax-token, and top-node
selections use the embedded grammar inside script and style blocks.

## Language servers

The default Vue configuration uses:

- **vtsls** as the primary server, with `@vue/typescript-plugin` for Vue support.
- **vscode-eslint-language-server** as a secondary diagnostics server.

Install TypeScript, vtsls, and the Vue TypeScript plugin in your project:

```sh
npm install --save-dev typescript @vtsls/language-server @vue/typescript-plugin
```

For ESLint diagnostics, make `vscode-eslint-language-server` available on `PATH`
or in the project's `node_modules/.bin`. The project also needs ESLint and a
working Vue ESLint configuration. A missing secondary server is logged without
preventing the primary server from starting.

Ki prefers executable commands in the selected root's `node_modules/.bin`, then
searches `PATH`. Package-manager workspace or lock files take precedence over
the nearest `package.json`, within the editor's working directory. Different
roots receive separate server instances.

## Overrides

Use `languages.vue.lsp_servers` in your [configuration](configuration.mdx) to
replace the default server list. Each entry has a unique `id`, a `command`, and
a `primary` flag. Interactive requests go to primary servers; lifecycle
notifications go to every configured server.

Server settings can include:

- `initialization_options`: options sent during initialization.
- `settings`: values returned to that server's `workspace/configuration` requests;
  dotted sections such as `typescript.tsdk` are resolved through nested objects.
- `diagnostics`: whether to accept diagnostic reports, defaulting to `true`.
- `diagnostic_mode`: `push` (default), `pull`, or `both`.
- `root_markers`: ordered groups of marker filenames. The nearest match in the
  first matching group wins. An empty list uses the editor's working directory.
- `environment`: environment variables passed to the server process.

`${workspace}` in initialization options and settings expands to the selected
server root. The Vue defaults use that root's `node_modules/typescript/lib` and
`node_modules/@vue/typescript-plugin`. For globally installed or Nix-managed
packages, override these paths explicitly in **both** the initialization options
and settings, as well as the server command if needed. Ki does not infer package
locations from a globally installed Vue language-server wrapper.

Defining `lsp_servers` replaces the whole default list. Set `primary: true` on the
server that should handle completion, navigation, and other interactive requests.
Existing languages using a single `lsp_command` continue to work.
