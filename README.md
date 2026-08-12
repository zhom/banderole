# Banderole

Create cross-platform single-executables for Node.js projects. Windows is not supported.

Banderole bundles your Node.js app, all dependencies, and a portable Node binary into a single native executable. On first launch, it unpacks to a cache directory for fast subsequent executions.

Unlike [Node.js SEA](https://nodejs.org/api/single-executable-applications.html) or [pkg](https://github.com/yao-pkg/pkg), banderole handles complex projects with dynamic imports and non-JavaScript files without requiring patches — it ships a stock Node binary and your real dependency tree rather than a patched runtime and a virtual filesystem.

## Performance

Measured on linux-x64, Node 22.17.1, against [`@yao-pkg/pkg`](https://github.com/yao-pkg/pkg) 6.22.0 building the same app. Startup is the median of 60 interleaved runs (targets rotate every round, so machine drift affects both equally).

| | trivial app | | app with deps<br>(express, lodash, chalk, dayjs) | |
|---|---|---|---|---|
| | **banderole** | pkg | **banderole** | pkg |
| Executable size | **33,416,408 B** | 74,306,703 B | **35,366,424 B** | 77,346,956 B |
| Startup (median) | **16.1 ms** | 25.7 ms | **53.1 ms** | 78.8 ms |
| Resident processes | **1** | 1 | **1** | 1 |
| Resident memory (RSS) | **43.8 MiB** | 49.4 MiB | **66.4 MiB** | 76.2 MiB |

pkg's size floor is the uncompressed Node binary it embeds, which its `--compress` option does not touch. banderole compresses the Node binary with zstd and ships only the executable itself, so a bundle is roughly half the size.

The trade-off is the **first** launch, which unpacks the payload into the cache directory: 117 ms versus pkg's 30 ms for the trivial app. Every launch after that takes the warm path above.

### Scaling with project size

First-launch extraction grows with the dependency tree; steady-state launch does not, because it is one file read followed by `exec`.

| `node_modules` | files | bundle time | first launch | **every later launch** |
|---|---|---|---|---|
| — | 0 | 16 s | 159 ms | **19.4 ms** |
| 14 MB | 5,500 | 18 s | 224 ms | **22.4 ms** |
| 68 MB (real npm tree) | 9,725 | 19 s | 316 ms | **71.2 ms** |
| 143 MB | 27,500 | 20 s | 348 ms | **23.3 ms** |
| 763 MB | 110,000 | 25 s | 862 ms | **22.9 ms** |

A 763 MB dependency tree still unpacks in under a second, and warm launch is flat across the whole range. (The 68 MB row is a real npm install; its higher warm figure is Node resolving express/lodash/dayjs at runtime, not launcher overhead. The other rows are generated trees, which compress better than real ones.)

## Requirements

Banderole requires a Rust toolchain (`cargo`, `rustc`, `rustup`) **and a working C linker** — `cc`/`gcc`/`clang` on Unix, MSVC on Windows — because it compiles a small launcher for each bundle.

## Installation

```sh
cargo install banderole
```

## Usage

```sh
# Bundle a project using the project name
banderole bundle /path/to/project

# Bundle with custom output path
banderole bundle /path/to/project --output /path/to/output/executable

# Bundle with custom name
banderole bundle /path/to/project --name my-app

# Bundle with both custom output and name
banderole bundle /path/to/project --output /path/to/my-app --name my-app
```

## Feature List

- [x] Support Linux, MacOS, and Windows for both x64 and arm64 architectures.
- [x] Support custom node.js version based on project's `.nvmrc` and `.node-version`
- [x] Support TypeScript projects with automatic detection of compiled output directories
- [x] Support workspaces (only pnpm workspaces tested)
- [ ] Only the executable has permissions to read and execute bundled files

## License

MIT
