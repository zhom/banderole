# Banderole

Create cross-platform single-executables for Node.js projects. Windows is not supported.

Banderole bundles your Node.js app, all dependencies, and a portable Node binary into a single native executable. On first launch, it unpacks to a cache directory for fast subsequent executions.

Unlike [Node.js SEA](https://nodejs.org/api/single-executable-applications.html) or [pkg](https://github.com/yao-pkg/pkg), banderole handles complex projects with dynamic imports and non-JavaScript files without requiring patches, but since it includes all dependencies by default, it has significantly larger filesize.

## Requirements

Banderole requires the Rust toolchain to be installed on your system to build portable executables.

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
