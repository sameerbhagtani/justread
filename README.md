<p align="center">
    <img src="assets/icon-128.png" width="96" alt="JustRead icon">
</p>

<h1 align="center">JustRead</h1>

<p align="center">A super fast, minimal PDF reader, built in Rust.</p>

Most PDF readers grow into editors, sidebars, toolbars and cloud accounts. JustRead goes the other way. It opens your document in about a tenth of a second, shows the pages, and gets out of the way. There are no panels to manage and nothing to sign in to. It just lets you read.

## Download

Get the latest version from the [releases page](https://github.com/sameerbhagtani/justread/releases/latest).

| Platform | File | How to install |
|---|---|---|
| Windows (x64) | `justread_<version>_x64-setup.exe` | Run the installer. |
| Debian, Ubuntu and derivatives | `justread_<version>_amd64.deb` | `sudo apt install ./justread_<version>_amd64.deb` |
| Any Linux (x86_64) | `justread_<version>_x86_64.AppImage` | `chmod +x` the file, then run it. |

Notes:

- The Windows installer is not code-signed yet, so Windows may show a SmartScreen warning. Click "More info", then "Run anyway".
- The AppImage needs FUSE 2. If it doesn't start, install `libfuse2` (Debian/Ubuntu) or `fuse2` (Arch), or run it with `--appimage-extract-and-run`.
- The Linux builds run on Ubuntu 22.04 and newer, and on other distributions of a similar age.

## Features

- **Fast:** the first page shows in about 0.1 seconds, even for large PDFs.
- **Reading:** smooth scrolling, zoom, go to page, find with highlighted matches, per-page rotation and dark mode.
- **Annotating:** freehand pen and eraser with undo and redo. Your PDF is never modified, and annotations follow the file even if you rename or move it.
- **Out of the way:** no toolbars or sidebars. Everything is a shortcut, listed in the command palette (Ctrl+K).
- **Private:** works fully offline. No accounts, no telemetry, no update checks.

## Keyboard shortcuts

| Action | Shortcut |
|---|---|
| Command palette | Ctrl+K or Ctrl+Shift+P |
| Open a PDF | Ctrl+O |
| Next / previous page | Page Down or Right / Page Up or Left |
| First / last page | Home / End |
| Go to page | Ctrl+G |
| Find | Ctrl+F, then Enter or Down for next, Shift+Enter or Up for previous |
| Pen / eraser | P / E (press again, or Esc, to stop) |
| Undo / redo | Ctrl+Z / Ctrl+Shift+Z or Ctrl+Y |
| Zoom in / out / reset | Ctrl+= / Ctrl+- / Ctrl+0 |
| Rotate page clockwise / counter-clockwise | Ctrl+R / Ctrl+Shift+R |
| Toggle dark mode | Ctrl+D |
| Close find, or put the pen down | Esc |

## Tech stack

| Part | Used for |
|---|---|
| [Rust](https://rust-lang.org) | The whole app |
| [MuPDF](https://mupdf.com) via the [`mupdf`](https://crates.io/crates/mupdf) crate | Parsing, rendering and searching PDFs |
| [egui / eframe](https://github.com/emilk/egui) with OpenGL | The window and interface |
| [rfd](https://crates.io/crates/rfd) | Native file dialogs |
| [BLAKE3](https://crates.io/crates/blake3) and [serde_json](https://crates.io/crates/serde_json) | Identifying documents and storing annotations |
| [cargo-packager](https://github.com/crabnebula-dev/cargo-packager) | Building the installers |
| [GitHub Actions](https://docs.github.com/actions) | Testing and releasing on Linux and Windows |

## Building from source

The toolchain is pinned with [mise](https://mise.jdx.dev/):

```sh
mise install                         # Rust and cargo-packager at the pinned versions
cargo run --release -- some.pdf      # build and open a PDF
cargo test --release                 # run the tests
mise run package                     # build the .deb and AppImage into dist/
```

On Linux you'll also need a C toolchain (MuPDF is compiled from source), libclang and the fontconfig headers. On Ubuntu:

```sh
sudo apt install build-essential pkg-config libfontconfig1-dev libclang-dev
```

To measure startup time yourself on Linux, run `justread --bench-startup some.pdf`. It prints how long the first page took to appear.

## License

JustRead is licensed under the [GNU Affero General Public License v3.0](LICENSE).

Made by [Sameer Bhagtani](https://sameerbhagtani.dev).
