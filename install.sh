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
releases=https://github.com/$repo/releases
issues=https://github.com/$repo/issues
script_url=https://raw.githubusercontent.com/$repo/main/install.sh

data_home=${XDG_DATA_HOME:-$HOME/.local/share}
data_dir=$data_home/agentshub
bin_dir=$HOME/.local/bin
binary=$bin_dir/agentshub
desktop_file=$data_home/applications/agentshub.desktop
icon_file=$data_home/icons/hicolor/scalable/apps/agentshub.svg

bold=
reset=
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != dumb ]; then
    bold=$(printf '\033[1m')
    reset=$(printf '\033[0m')
fi

die() {
    printf 'error: %s\n' "$1" >&2
    shift
    [ $# -eq 0 ] || printf '%s\n' "$@" >&2
    exit 1
}

step() {
    printf '%s==> %s%s\n' "$bold" "$*" "$reset"
}

tilde() {
    case $1 in
        "$HOME"/*) printf '~%s' "${1#"$HOME"}" ;;
        *) printf '%s' "$1" ;;
    esac
}

usage() {
    cat <<EOF
Installs Agents Hub for the current user, or updates it to the latest release.

Usage: install.sh [--uninstall | --help]

  --uninstall   remove Agents Hub (its data is kept)
  --help        show this help

To install a given release, set AGENTSHUB_VERSION, for example:
  curl -fsSL $script_url | AGENTSHUB_VERSION=v0.12.0 sh
EOF
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

# A third argument asks for a progress bar, drawn only on a terminal.
download() {
    if command -v curl >/dev/null 2>&1; then
        if [ $# -gt 2 ] && [ -t 2 ]; then
            curl -fL -# -o "$2" "$1"
        else
            curl -fsSL -o "$2" "$1"
        fi
    elif command -v wget >/dev/null 2>&1; then
        # BusyBox wget has no --show-progress.
        if [ $# -gt 2 ] && [ -t 2 ] && wget --help 2>&1 | grep -q -- --show-progress; then
            wget -q --show-progress -O "$2" "$1"
        else
            wget -q -O "$2" "$1"
        fi
    else
        die "curl or wget is needed to download Agents Hub"
    fi
}

# Matches the running program's path rather than its name, so a copy run from
# elsewhere is not counted. A copy that was updated or removed while running
# shows as "(deleted)". pwd -P resolves a symlinked home, as on Fedora Atomic.
is_running() {
    real=$(cd "$bin_dir" 2>/dev/null && pwd -P)/agentshub || return 1
    for exe in /proc/[0-9]*/exe; do
        case $(readlink "$exe" 2>/dev/null) in
            "$real" | "$real (deleted)") return 0 ;;
        esac
    done
    return 1
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
    installed=
    for f in "$binary" "$desktop_file" "$icon_file"; do
        if [ -e "$f" ] || [ -L "$f" ]; then
            installed=1
        fi
    done
    if [ -n "$installed" ]; then
        rm -f "$binary" "$desktop_file" "$icon_file"
        refresh_menu
        printf '%sAgents Hub is uninstalled.%s\n' "$bold" "$reset"
        if is_running; then
            echo "It is still open; quit it to finish."
        fi
    else
        echo "Agents Hub is not installed in $(tilde "$bin_dir"), so there is nothing to remove."
    fi
    if [ -d "$data_dir" ]; then
        shown=$(tilde "$data_dir")
        printf 'Its data (%s) is still in %s. ' "$(du -sh "$data_dir" | cut -f1)" "$shown"
        # Only offered as a command when it is safe to paste unquoted.
        case $shown in
            *[!A-Za-z0-9._/~-]*) echo "Delete that folder to remove it too." ;;
            *) printf 'To delete it too, run:\n  rm -rf %s\n' "$shown" ;;
        esac
    fi
}

path_note() {
    echo
    echo "Note: $(tilde "$bin_dir") is not on your PATH, so typing 'agentshub' alone will not work in a terminal."
    shell=${SHELL:-}
    case ${shell##*/} in
        fish) echo "To add it, run: fish_add_path ~/.local/bin" ;;
        zsh) echo "To add it, add this line to ~/.zshrc:" ;;
        bash) echo "To add it, add this line to ~/.bashrc:" ;;
        *) echo "To add it, add this line to your shell's startup file:" ;;
    esac
    case ${shell##*/} in
        fish) ;;
        *) echo '  export PATH="$HOME/.local/bin:$PATH"' ;;
    esac
    if [ -n "$new_bin_dir" ]; then
        echo "Logging out and back in may be enough, since many systems add that folder to PATH at login once it exists."
    fi
}

install_app() {
    [ "$(uname -s)" = Linux ] || die "Agents Hub only runs on Linux"
    [ "$(uname -m)" = x86_64 ] || die "there is no Agents Hub build for $(uname -m) yet" \
        "To ask for one, open an issue at $issues"
    case $binary in
        *'
'*) die "the install path $bin_dir has a line break in it, which an app menu entry cannot hold" ;;
    esac

    version=${AGENTSHUB_VERSION:-}
    case $version in
        "" | v*) ;;
        *) version=v$version ;;
    esac
    if [ -n "$version" ]; then
        url=$releases/download/$version/$asset.tar.gz
        retry="Check that $version is listed at $releases and that you are online, then try again."
    else
        url=$releases/latest/download/$asset.tar.gz
        retry="Check your internet connection, then try again."
    fi
    name="Agents Hub${version:+ $version}"

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    # Some shells, dash among them, skip the EXIT trap when a signal ends the
    # script, which would leave $tmp behind on Ctrl-C.
    trap 'exit 130' INT
    trap 'exit 143' TERM

    updating=
    [ ! -e "$binary" ] || updating=1
    new_bin_dir=
    [ -d "$bin_dir" ] || new_bin_dir=1

    step "Downloading $name from github.com/$repo"
    download "$url" "$tmp/$asset.tar.gz" progress || die "could not download $url" "$retry"
    download "$url.sha256" "$tmp/$asset.tar.gz.sha256" || die "could not download $url.sha256" "$retry"
    step "Checking the download"
    (cd "$tmp" && sha256sum -c --status "$asset.tar.gz.sha256") ||
        die "the download does not match its checksum" \
            "Run the installer again. If it keeps failing, do not install it; report it at $issues"
    tar -xzf "$tmp/$asset.tar.gz" -C "$tmp" || die "could not unpack the download"

    current=
    if [ -n "$updating" ] && [ -x "$binary" ] && cmp -s "$tmp/$asset/agentshub" "$binary" 2>/dev/null; then
        current=1
    fi
    was_running=
    if [ -n "$updating" ] && [ -z "$current" ] && is_running; then
        was_running=1
    fi

    mkdir -p "$bin_dir" "$(dirname "$desktop_file")" "$(dirname "$icon_file")"

    if [ -z "$current" ]; then
        if [ -n "$updating" ]; then
            step "Updating $(tilde "$binary")"
        else
            step "Installing to $(tilde "$binary")"
        fi
        # Copied next to the old binary and renamed over it, so an Agents Hub
        # that is running keeps working until it is restarted.
        cp "$tmp/$asset/agentshub" "$binary.new"
        chmod 755 "$binary.new"
        mv -f "$binary.new" "$binary"
    fi

    # The full path, since the desktop session's PATH may not have ~/.local/bin.
    while IFS= read -r line || [ -n "$line" ]; do
        case $line in
            Exec=*) desktop_exec "$binary" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <"$tmp/$asset/agentshub.desktop" >"$desktop_file"
    cp "$tmp/$asset/agentshub.svg" "$icon_file"
    refresh_menu

    echo
    if [ -n "$current" ] && [ -n "$version" ]; then
        done_msg="$name is already installed."
    elif [ -n "$current" ]; then
        done_msg="Agents Hub is already up to date."
    elif [ -n "$updating" ] && [ -z "$version" ]; then
        done_msg="Agents Hub is updated."
    else
        done_msg="$name is installed."
    fi
    printf '%s%s%s\n' "$bold" "$done_msg" "$reset"
    if [ -n "$was_running" ]; then
        echo "It is open now; quit and reopen it to use the new version."
    fi
    if [ -z "$updating" ]; then
        echo "Open it from your app menu, or run: $(tilde "$binary")"
    fi
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) path_note ;;
    esac
    # The entry escapes `%` as the desktop-entry spec says, but GLib checks
    # that the program exists without unescaping it, so it drops the entry.
    case $binary in
        *%*) echo "Note: the install path has a '%' in it, so GNOME and other GLib-based app menus will not list Agents Hub; run it from a terminal instead." ;;
    esac
    if [ -z "$updating" ]; then
        echo
        echo "To update, run the same command again. To uninstall:"
        echo "  curl -fsSL $script_url | sh -s -- --uninstall"
    fi
}

case "${1:-}" in
    "") install_app ;;
    --uninstall) uninstall ;;
    -h | --help) usage ;;
    *) die "unknown option: $1" "Run it with --help to see the options." ;;
esac
