# tbnf

A fast fuzzy terminal file navigator inspired by
[horse](https://github.com/if-not-nil/horse). Type part of a filename, move to
the result you want, and either enter its directory or open it in your editor.

## Install

Build with maximum release optimization, tune for the current CPU, and install
the executable to `~/.local/bin`:

```sh
./install.sh
```

Choose another location with `--bin-dir DIR` or `--prefix DIR`. Use
`--portable` to build an executable that is not tuned to the current CPU.
Alternatively, install directly with Cargo:

```sh
cargo install --path .
```

Set your preferred editor if needed:

```sh
export EDITOR=nvim
```

## Shell setup

A child program cannot change its parent shell's directory. For that reason,
`tbnf` prints a safely quoted `cd` or `$EDITOR` command after you make a
selection. Add this wrapper to `~/.zshrc` or `~/.bashrc` so the shell performs
that command automatically:

```sh
tbnf() {
  local result
  result="$(command tbnf "$@")" || return
  [ -n "$result" ] && eval "$result"
}
```

Restart the shell or reload its configuration:

```sh
source ~/.zshrc       # zsh
# source ~/.bashrc    # bash
```

## Quick guide

Start in the current directory, or pass another directory:

```sh
tbnf
tbnf ~/projects
```

Then:

1. Start typing to fuzzy-search every file and directory below the current
   directory. Results are shown as relative paths such as `src/main.rs`.
2. Use the arrow keys to choose a result.
3. Press `Tab` on a directory to browse into it.
4. Press `Enter` or `Tab` on a file to leave `tbnf` and open it in `$EDITOR`.
   The editor starts with the nearest Git project root as its working directory,
   while the selected file opens directly. Outside a Git project, the directory
   where you launched `tbnf` is used.
5. Press `Enter` on a directory to leave `tbnf` and `cd` there.

Backspace edits the search. When the search is already empty, Backspace moves
to the parent directory. `Ctrl-W` clears the entire search.

Recursive indexing runs in the background and is reused for every keystroke.
It respects `.gitignore`, `.ignore`, Git's exclude files, and the global Git
ignore file, and it does not follow symlinked directories. With an empty query,
the list remains a simple view of the current directory. Recursive indexing is
disabled at the filesystem root to avoid accidentally walking an entire disk.
Changing directories cancels the obsolete background scan immediately. Search
counts every match but retains and sorts only the best 500 results, keeping
large-tree searches responsive; the header shows both counts when results are
limited, for example `[1/500 of 2314]`.

Hidden files are shown by default. Press `Alt-H` or `F2` to hide or show them;
the header displays the current state. VCS internals such as `.git/`, `.hg/`,
and `.svn/` are never recursively indexed.

The right pane previews the selected file with syntax highlighting. Directories
show a preview of their contents. Files over 50 KB and binary files are not
rendered. The normal layout gives 40% of the terminal to results and 60% to the
preview. The preview disappears automatically in a narrow terminal, or it can be
disabled explicitly:

```sh
tbnf --no-preview
```

## File operations

- Press `Ctrl-A`, type `notes.md`, and press Enter to create a file.
- End the name with `/`, such as `drafts/`, to create and enter a directory.
- Press `Ctrl-D` and enter `y` to delete the selected item. Directory deletion
  is recursive, so check the confirmation prompt carefully.
- Press `Ctrl-O` to open the selection in the operating system's default app
  without leaving `tbnf`.

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
