#!/bin/sh

set -eu

usage() {
    cat <<'EOF'
Build and install an optimized tbnf executable.

Usage: ./install.sh [OPTIONS]

Options:
  --bin-dir DIR  Install directly into DIR
  --prefix DIR   Install into DIR/bin
  --portable     Do not optimize for the current CPU
  -h, --help     Show this help

The default install directory is $TBNF_INSTALL_DIR, $XDG_BIN_HOME, or
$HOME/.local/bin, in that order.
EOF
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install_dir=${TBNF_INSTALL_DIR-}
optimize_for_host=1

while [ "$#" -gt 0 ]; do
    case "$1" in
        --bin-dir)
            [ "$#" -ge 2 ] || {
                echo "error: --bin-dir requires a directory" >&2
                exit 2
            }
            install_dir=$2
            shift 2
            ;;
        --prefix)
            [ "$#" -ge 2 ] || {
                echo "error: --prefix requires a directory" >&2
                exit 2
            }
            install_dir=$2/bin
            shift 2
            ;;
        --portable)
            optimize_for_host=0
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ -z "$install_dir" ]; then
    if [ -n "${XDG_BIN_HOME-}" ]; then
        install_dir=$XDG_BIN_HOME
    elif [ -n "${HOME-}" ]; then
        install_dir=$HOME/.local/bin
    else
        echo "error: cannot choose an install directory; use --bin-dir" >&2
        exit 1
    fi
fi

command -v cargo >/dev/null 2>&1 || {
    echo "error: Cargo is required to build tbnf" >&2
    exit 1
}

build_rustflags=${RUSTFLAGS-}
if [ "$optimize_for_host" -eq 1 ]; then
    if [ -n "$build_rustflags" ]; then
        build_rustflags="$build_rustflags -C target-cpu=native"
    else
        build_rustflags="-C target-cpu=native"
    fi
fi

build_target_dir=${CARGO_TARGET_DIR-}
if [ -z "$build_target_dir" ]; then
    build_target_dir=$script_dir/target
elif [ "${build_target_dir#/}" = "$build_target_dir" ]; then
    build_target_dir=$script_dir/$build_target_dir
fi

echo "Building tbnf with the optimized release profile..."
(
    cd "$script_dir"
    CARGO_TARGET_DIR="$build_target_dir" RUSTFLAGS="$build_rustflags" \
        cargo build --release --locked
)

mkdir -p "$install_dir"
install -m 755 "$build_target_dir/release/tbnf" "$install_dir/tbnf"

echo "Installed tbnf to $install_dir/tbnf"
case ":${PATH-}:" in
    *:"$install_dir":*) ;;
    *) echo "Add $install_dir to PATH to run tbnf from your shell." ;;
esac
