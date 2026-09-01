#!/bin/sh
set -eu

repo_url=${SERAPH_REPO_URL:-https://github.com/Matari-Audio/SERAPH.git}
data_root=${XDG_DATA_HOME:-"$HOME/.local/share"}
install_dir=${SERAPH_INSTALL_DIR:-"$data_root/seraph"}
bin_dir=${SERAPH_BIN_DIR:-"$HOME/.local/bin"}

fail() {
    printf 'SERAPH install: %s\n' "$1" >&2
    exit 1
}

for command in git cargo node npm python3; do
    command -v "$command" >/dev/null 2>&1 || fail "missing $command"
done

node -e 'const [a,b]=process.versions.node.split(".").map(Number);process.exit(a>22||a===22&&b>=19?0:1)' \
    || fail 'Node.js 22.19 or newer is required'
python3 -c 'import sys; raise SystemExit(sys.version_info < (3, 11))' \
    || fail 'Python 3.11 or newer is required'

if [ -d "$install_dir/.git" ]; then
    [ -z "$(git -C "$install_dir" status --porcelain)" ] \
        || fail "$install_dir has local changes; refusing to overwrite them"
    [ "$(git -C "$install_dir" branch --show-current)" = main ] \
        || fail "$install_dir is not on main"
    git -C "$install_dir" pull --ff-only origin main
elif [ -e "$install_dir" ]; then
    fail "$install_dir exists and is not a SERAPH checkout"
else
    mkdir -p "$(dirname "$install_dir")"
    git clone --depth 1 "$repo_url" "$install_dir"
fi

npm --prefix "$install_dir" ci --omit=dev
cargo build --manifest-path "$install_dir/Cargo.toml" --release --locked

mkdir -p "$bin_dir"
staged="$bin_dir/.seraph.$$"
trap 'rm -f "$staged"' EXIT HUP INT TERM
install -m 755 "$install_dir/target/release/seraph" "$staged"
mv -f "$staged" "$bin_dir/seraph"
trap - EXIT HUP INT TERM

printf '\nInstalled SERAPH at %s\n' "$bin_dir/seraph"
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) printf 'Add %s to PATH, then run: seraph\n' "$bin_dir" ;;
esac
