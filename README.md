# tbnf

A fuzzy terminal file navigator with syntax-highlighted previews. Search files,
browse directories, and open a selection in your editor or change your shell's
working directory.

## Install

With a current stable Rust toolchain, build and install from this checkout:

```sh
cargo install --locked --path .
```

Alternatively, `./install.sh` installs a CPU-tuned release build to
`~/.local/bin`. Use `--portable` to disable CPU tuning, or `--bin-dir DIR` to
choose the install location.

## Shell setup

Add this wrapper to `~/.zshrc` or `~/.bashrc`, then reload your shell:

```sh
export EDITOR=nvim

tbnf() {
  local result
  result="$(command tbnf "$@")" || return
  if [ -n "$result" ]; then
    eval "$result"
  fi
}
```

The wrapper executes the quoted shell command emitted by `tbnf`, allowing it to
change the parent shell's directory. The interface uses stderr, leaving stdout
for the selected command.

## Usage

```sh
tbnf                  # Browse the current directory
tbnf ~/projects       # Start elsewhere
tbnf --no-preview     # Hide the preview pane
```

Type to search recursively, use the arrow keys to select, and press `Tab` to
browse into a directory. `Enter` changes your shell's directory or opens a file
in `$EDITOR`. Files open from the nearest Git project root, falling back to the
starting directory.

An empty query lists the current directory. Recursive search respects ignore
files, skips VCS internals and common generated directories (such as `target`
and `node_modules`), and does not follow directory symlinks. Hidden files are
shown by default. Search retains the best 500 matches from an index capped at
500,000 nested entries; the header indicates limits. Recursive indexing is
disabled at the filesystem root.

The preview shows directory contents or highlighted text. Binary files and
files over 50 KB are not rendered; text previews read at most 10 KB. The preview
pane hides automatically in narrow terminals.

## Key reference

| Key | Action |
| --- | --- |
| `↑` / `Ctrl-K` / `Ctrl-P` | Select previous |
| `↓` / `Ctrl-J` / `Ctrl-N` | Select next |
| `Tab` / `Ctrl-L` / `Ctrl-F` | Enter directory or select file |
| `Enter` | Open selected file or `cd` to selected directory |
| `Backspace` / `Ctrl-B` | Erase query or go to parent |
| `Ctrl-W` | Clear query |
| `Ctrl-E` | Toggle home/root |
| `Alt-H` / `F2` | Show or hide hidden files |
| `Ctrl-A` | Create file; append `/` to create a directory |
| `Ctrl-D` | Delete selected item after confirmation |
| `Ctrl-O` | Open with the desktop default application |
| `Esc` / `Ctrl-C` | Quit |

Directory deletion is recursive and requires typing `y` followed by `Enter`.

## Development

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo package --locked
```

CI runs these checks on Linux and macOS. Run `cargo fmt` to format changes.

## License

[MIT](LICENSE)
