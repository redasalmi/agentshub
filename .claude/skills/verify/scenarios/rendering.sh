# Draws what agent CLIs draw that a font alone gets wrong: Claude's block
# logo, check marks, boxes, a usage bar, braille spinners, shades, Powerline
# separators, emoji and text attributes, at the default size and zoomed in.
# Covers: the terminal's sprites, font fallbacks and pixel grid.

project="$AHV_WORK/project"
seed_project "$project"

export AHV_SCREEN="$AHV_WORK/screen.txt"
logo='\e[38;2;215;119;87m'
# The last line ends back at its start, so the CLI's prompt puts the cursor
# on its check mark, which spreads into the blank cell after it.
printf '%b' \
	"$logo ▐▛███▜▌\e[0m   \e[1mClaude Code\e[0m v2.1\r\n" \
	"$logo▝▜█████▛▘\e[0m  Opus 5.5 · Claude Pro\r\n" \
	"$logo  ▘▘ ▝▝\e[0m    ~/project\r\n" \
	"\r\n" \
	"╭──────────────────────────╮\r\n" \
	"│ \e[32m✔\e[0m atlassian    26 tools  │\r\n" \
	"│ \e[32m✔\e[0m browse        3 tools  │\r\n" \
	"│ ○ blender                │\r\n" \
	"╰──────────────────────────╯\r\n" \
	"Current session \e[38;2;177;185;249m██████████▌\e[0m\e[2m░░░░░░░░░\e[0m 52% used\r\n" \
	"╔══╦══╗ ┏━━┳━━┓ ┌┄┄┬╌╌┐ ╱╲╳ ├─┼─┤\r\n" \
	"║  ║  ║ ┃  ┃  ┃ ┆  ┊  ╎ ╲╱  ╞═╪═╡\r\n" \
	"╚══╩══╝ ┗━━┻━━┛ └──┴──┘     ╘═╧═╛\r\n" \
	"▁▂▃▄▅▆▇█ ▏▎▍▌▋▊▉ ░▒▓ ▖▗▘▙▚▛▜▝▞▟ 🬀🬁🬂🬃🬋\r\n" \
	"⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ⣿⡇ ✻ ✽ ✢ ⚠ ✓ ✗ ● ★\r\n" \
	"⏺ Read 2 files\r\n" \
	"  ⎿  Done\r\n" \
	"\e[30;44m dir \e[0;34m\e[0m \e[30;42m git \e[0;32m\e[0m     emoji: 🎉 ✅ 🚀\r\n" \
	"\e[2mfaint\e[0m \e[4:3mcurly\e[0m \e[4mline\e[0m \e[9mstrike\e[0m \e[1mbold\e[0m \e[3mitalic\e[0m\r\n" \
	"\e[4mA long underlined run, which must end where its cells do,\e[0mplain\r\n" \
	"\e[9mA long struck-out run, which must end where its cells do,\e[0mplain\r\n" \
	"\e[4:3mA long curly underlined run, ending where its cells do,\e[0mplain\r\n" \
	"Symbols fit their cell before text: A\e[32m✔\e[0mB [✔]done ★✔→ and spread before a space: ✔ ok\r\n" \
	"  \e[32m✔\e[0m \r" \
	>"$AHV_SCREEN"

app_start
key ctrl+1
key ctrl+shift+n
key Return
wait_until 15 "fake claude to start" cli_started claude
wait_until 5 "fake claude to print the screen" grep -q "^screen claude " "$CLI_LOG"
shot screen

# Ctrl+scroll over the terminal zooms, to a size with thicker lines.
zoomed() { [ "$(sql "SELECT value > 14 FROM settings WHERE key = 'terminal_font_size'")" = 1 ]; }
xdotool mousemove 640 450 keydown ctrl click --repeat 8 --delay 100 4 keyup ctrl
wait_until 5 "the terminal text to grow" zoomed
shot zoomed
