# Banderole

Create cross-platform single-executables for node.js projects.

Unlike [Node.js SEA](https://nodejs.org/api/single-executable-applications.html) or [pkg](https://github.com/yao-pkg/pkg), it bundles compiled node.js app, all node modules, and a portable node binary into a single executable, and on the first launch it will unpack everything into a cache directory. Every subsequent execution of the binary will point to the extract data.

While it results in the same performance as executing `/path/to/portable/node my/app/index.js` (except for the first execution), it also means that binaries are a lot larger than, say, pkg, which traverses your project and dependencies to include only relevant files.

You should stick to pkg (or Node.js SEA once it is stable enough) unless you have to deal with an app that has a nested dependency that has dynamic imports or imports non-javascript files, which makes it difficult to patch.

## Installation

```sh
cargo install banderole
```

## Usage

```sh
banderole project-dir output-dir
```

## Feature List

- [x] Support Linux, MacOS, and Windows for both x64 and arm64 architectures.
- [x] Support custom node.js version based on project's `.nvmrc` and `.node-version`
- [ ] Support workspaces (currently you need to install dependencies directly)

## License

MIT
