#!/bin/sh
# Installs Agents Hub from its GitHub releases, for the current user.
#
#   curl -fsSL https://raw.githubusercontent.com/redasalmi/agentshub/main/install.sh | sh
#
# Running it again updates to the latest release. To pick a release, or to
# remove the app:
#
#   curl -fsSL .../install.sh | AGENTSHUB_VERSION=v0.1.0 sh
#   curl -fsSL .../install.sh | sh -s -- --uninstall
set -eu

repo=redasalmi/agentshub
asset=agentshub-x86_64-linux

data_home=${XDG_DATA_HOME:-$HOME/.local/share}
bin_dir=$HOME/.local/bin
binary=$bin_dir/agentshub
desktop_file=$data_home/applications/agentshub.desktop
icon_file=$data_home/icons/hicolor/scalable/apps/agentshub.svg

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# Lets the app menu pick up added or removed entries without a re-login.
refresh_menu() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$data_home/applications" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$data_home/icons/hicolor" >/dev/null 2>&1 || true
    fi
}

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        die "curl or wget is needed to download Agents Hub"
    fi
}

# Prints `Exec=` followed by $1 as a desktop-entry program, so the path works
# whatever it contains. The spec's two layers of escaping apply in turn: the
# program is double-quoted, with `"`, `` ` ``, `$` and `\` backslash-escaped
# inside the quotes and `%` doubled so it is not a field code; then, as a
# string value, every backslash is doubled again and tabs and carriage returns
# become `\t` and `\r`. The path goes through sed as input, not as part of a
# script, so no character in it is special to sed.
desktop_exec() {
    tab=$(printf '\t')
    cr=$(printf '\r')
    printf 'Exec="%s"\n' "$(printf '%s' "$1" | sed \
        -e 's/[\\"`$]/\\&/g' -e 's/%/%%/g' \
        -e 's/\\/\\\\/g' -e "s/$tab/\\\\t/g" -e "s/$cr/\\\\r/g")"
}

uninstall() {
    rm -f "$binary" "$desktop_file" "$icon_file"
    refresh_menu
    echo "Agents Hub is uninstalled."
    echo "Its data is still in $data_home/agentshub; delete that folder to remove it too."
}

install_app() {
    [ "$(uname -s)" = Linux ] || die "Agents Hub only runs on Linux"
    [ "$(uname -m)" = x86_64 ] || die "there is no Agents Hub build for $(uname -m) yet"
    case $binary in
        *'
'*) die "the install path $bin_dir has a line break in it, which an app menu entry cannot hold" ;;
    esac

    if [ -n "${AGENTSHUB_VERSION:-}" ]; then
        url=https://github.com/$repo/releases/download/$AGENTSHUB_VERSION/$asset.tar.gz
    else
        url=https://github.com/$repo/releases/latest/download/$asset.tar.gz
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    echo "Downloading $url"
    download "$url" "$tmp/$asset.tar.gz" || die "could not download $url"
    download "$url.sha256" "$tmp/$asset.tar.gz.sha256" || die "could not download $url.sha256"
    (cd "$tmp" && sha256sum -c --status "$asset.tar.gz.sha256") ||
        die "the download is corrupted (checksum mismatch)"
    tar -xzf "$tmp/$asset.tar.gz" -C "$tmp"

    mkdir -p "$bin_dir" "$(dirname "$desktop_file")" "$(dirname "$icon_file")"

    # Copied next to the old binary and renamed over it, so an Agents Hub
    # that is running keeps working until it is restarted.
    cp "$tmp/$asset/agentshub" "$binary.new"
    chmod 755 "$binary.new"
    mv -f "$binary.new" "$binary"

    # The full path, since the desktop session's PATH may not have ~/.local/bin.
    while IFS= read -r line || [ -n "$line" ]; do
        case $line in
            Exec=*) desktop_exec "$binary" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <"$tmp/$asset/agentshub.desktop" >"$desktop_file"
    cp "$tmp/$asset/agentshub.svg" "$icon_file"
    refresh_menu

    echo "Agents Hub is installed. Open it from your app menu, or run: $binary"
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) echo "Note: $bin_dir is not on your PATH, so typing 'agentshub' alone will not work in a terminal." ;;
    esac
    # The entry escapes `%` as the desktop-entry spec says, but GLib checks
    # that the program exists without unescaping it, so it drops the entry.
    case $binary in
        *%*) echo "Note: the install path has a '%' in it, so GNOME and other GLib-based app menus will not list Agents Hub; run it from a terminal instead." ;;
    esac
}

case "${1:-}" in
    "") install_app ;;
    --uninstall) uninstall ;;
    *) die "unknown option: $1 (the only option is --uninstall)" ;;
esac
