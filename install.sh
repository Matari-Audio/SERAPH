#!/bin/sh
set -eu

data_root=${XDG_DATA_HOME:-"$HOME/.local/share"}
install_dir=${SERAPH_INSTALL_DIR:-"$data_root/seraph"}
bin_dir=${SERAPH_BIN_DIR:-"$HOME/.local/bin"}
release_url=${SERAPH_RELEASE_URL:-https://github.com/Matari-Audio/SERAPH/releases/latest/download}

fail() {
    printf 'SERAPH install: %s\n' "$1" >&2
    exit 1
}

for command in curl tar node python3; do
    command -v "$command" >/dev/null 2>&1 || fail "missing $command"
done

node -e 'const [a,b]=process.versions.node.split(".").map(Number);process.exit(a>22||a===22&&b>=19?0:1)' \
    || fail 'Node.js 22.19 or newer is required'
python3 -c 'import sys; raise SystemExit(sys.version_info < (3, 11))' \
    || fail 'Python 3.11 or newer is required'

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=darwin-arm64 ;;
    Darwin-x86_64) target=darwin-x64 ;;
    Linux-aarch64|Linux-arm64) target=linux-arm64 ;;
    Linux-x86_64) target=linux-x64 ;;
    *) fail "unsupported platform: $(uname -s) $(uname -m)" ;;
esac

asset="seraph-$target.tar.gz"
mkdir -p "$install_dir/releases" "$bin_dir"
stage=$(mktemp -d "$install_dir/.stage.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
curl -fL "$release_url/$asset" -o "$stage/$asset"
curl -fL "$release_url/$asset.sha256" -o "$stage/$asset.sha256"
(cd "$stage" && if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "$asset.sha256"; else shasum -a 256 -c "$asset.sha256"; fi) \
    || fail "release checksum mismatch"
tar -xzf "$stage/$asset" -C "$stage"
[ -x "$stage/seraph" ] && [ -f "$stage/VERSION" ] \
    || fail "release archive is incomplete"
version=$(sed -n '1p' "$stage/VERSION")
case "$version" in *[!A-Za-z0-9._-]*|'') fail "invalid release version" ;; esac
release_dir="$install_dir/releases/$version"
if [ ! -d "$release_dir" ]; then
    rm -f "$stage/$asset" "$stage/$asset.sha256"
    mv "$stage" "$release_dir"
    stage=
fi

ln -s "releases/$version" "$install_dir/.current.$$"
mv -f "$install_dir/.current.$$" "$install_dir/current"
ln -s "$install_dir/current/seraph" "$bin_dir/.seraph.$$"
mv -f "$bin_dir/.seraph.$$" "$bin_dir/seraph"
trap - EXIT HUP INT TERM

printf '\nInstalled prebuilt SERAPH %s at %s\n' "$version" "$bin_dir/seraph"
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) printf 'Add %s to PATH, then run: seraph\n' "$bin_dir" ;;
esac
