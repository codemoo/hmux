package ui

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
	"github.com/codemoo/hmux/internal/workflow"
	"golang.org/x/term"
)

var ErrCancelled = errors.New("selection cancelled")

type Selector struct {
	FZFPath       string
	Query         string
	Mobile        bool
	Resume        bool
	OpenTabs      []model.Session
	CurrentTabID  string
	Executable    string
	RefreshArgs   []string
	FooterArgs    []string
	NewArgs       []string
	TerminateArgs []string
	AliasArgs     []string
}

// SelectableSessions returns a fresh slice without presentation-hidden
// sessions. Structured catalogs remain complete so open tabs and management
// surfaces can continue to observe hidden session health.
func SelectableSessions(sessions []model.Session) []model.Session {
	visible := make([]model.Session, 0, len(sessions))
	for _, session := range sessions {
		if !session.Hidden {
			visible = append(visible, session)
		}
	}
	return visible
}

func (s Selector) Select(ctx context.Context, sessions []model.Session) (string, error) {
	sessions, err := SortSessions(sessions, "session", "asc")
	if err != nil {
		return "", err
	}
	fzf := s.FZFPath
	if fzf == "" {
		fzf = fzfPath()
	}
	if fzf == "" {
		if len(sessions) == 0 {
			return "", errors.New("no tmux sessions are available; install fzf to create one interactively")
		}
		return numericFallback(sessions)
	}
	var input strings.Builder
	rowWidth := selectorRowWidth(terminalWidth(), s.Mobile)
	initialSessions := sessions
	if s.Query != "" && s.Executable != "" && len(s.RefreshArgs) > 0 {
		initialSessions = FilterSessions(sessions, s.Query)
	}
	for _, session := range initialSessions {
		input.WriteString(selectorLine(session, s.Mobile, rowWidth))
	}
	height := "100%"
	keyHelp := "↵ attach   / search"
	if len(s.NewArgs) > 0 {
		keyHelp += "   ^N new"
	}
	if len(s.TerminateArgs) > 0 {
		keyHelp += "   ^X terminate"
	}
	if len(s.AliasArgs) > 0 {
		keyHelp += "   ^R alias"
	}
	if s.Executable != "" && len(s.RefreshArgs) > 0 {
		// Refresh remains an internal reload primitive for sorting and actions;
		// it is intentionally not exposed as a user-facing key.
	}
	keyHelp += "   ^Q exit"
	header := formatSelectorHeaderWithTabs(
		sessions, s.OpenTabs, s.CurrentTabID, s.Mobile, rowWidth, true,
	)
	footerWidth := max(32, terminalWidth()-8)
	footer := formatSelectorFooter(keyHelp, footerWidth, readSystemUsage(), time.Now(), true)
	selectorHelper := ""
	if s.Executable != "" && len(s.RefreshArgs) > 0 {
		var cleanup func()
		selectorHelper, cleanup, err = createSelectorHelper(
			s.Executable, s.RefreshArgs, s.FooterArgs, s.NewArgs, s.AliasArgs, s.TerminateArgs,
			input.String(), s.Query,
		)
		if err != nil {
			return "", err
		}
		defer cleanup()
	}
	args := []string{
		"--delimiter=\t", "--with-nth=2", "--ansi", "--layout=reverse",
		"--height=" + height, "--no-multi", "--no-hscroll", "--ellipsis=", "--exact",
		"--style=minimal",
		"--input-border=rounded", "--input-label= FILTER ",
		"--list-border=rounded", "--list-label= LIVE SESSIONS ",
		"--header-border=inline",
		"--header=" + header,
		"--footer=" + footer, "--footer-border=inline",
		"--prompt=›  ", "--ghost=session name · runtime · model · process · workspace",
		"--info=inline-right: ", "--pointer=›", "--gutter= ", "--highlight-line", "--cycle",
		"--no-scrollbar", "--no-separator", "--margin=1,2", "--padding=0,1",
		"--id-nth=1", "--track",
		"--bind=ctrl-q:abort",
	}
	launcher := os.Getenv("HMUX_LAUNCHER") == "1"
	if launcher {
		// The selector uses fzf's inline height mode, so preserving its rendered
		// interface leaves a ready-made backing screen beneath the disposable
		// tmux frame. When that frame closes, Ghostty reveals the list directly
		// instead of briefly showing a cleared shell screen.
		args = append(args, "--no-clear")
	}
	if os.Getenv("NO_COLOR") != "" {
		args = append(args, "--no-color")
	} else {
		args = append(args, "--color=fg:#cecdc3,bg:#100f0f,fg+:#cecdc3,bg+:#282726,hl:#da702c,hl+:#d0a215,info:#878580,prompt:#4385be,pointer:#4385be,marker:#879a39,spinner:#879a39,header:#878580,border:#343331,label:#4385be,query:#cecdc3")
	}
	if s.Query != "" {
		args = append(args, "--query="+s.Query)
	}
	if s.Executable != "" && len(s.RefreshArgs) > 0 {
		helper := shellQuote(selectorHelper)
		args = append(args, "--disabled", "--no-input")
		// Catalog and system-stat refreshes must never block fzf's input loop.
		// Any query or focus change cancels an in-flight background catalog
		// fetch; the helper starts another only after fzf reports two seconds
		// of user inactivity, suppresses overlapping fetches, and reloads only
		// when the rendered catalog actually changed. Stable-ID tracking then
		// spans only the fast local snapshot replacement, not the slow fetch.
		args = append(
			args,
			"--bind=/:bg-cancel+transform("+helper+" begin-search)",
			"--bind=change:bg-cancel+transform("+helper+" change {q})",
			"--bind=focus:bg-cancel",
			"--bind=click-header:bg-cancel+reload-sync("+helper+" toggle {q})",
			"--bind=enter:bg-cancel+transform("+helper+" enter {1} {q})",
			"--bind=esc:bg-cancel+transform("+helper+" escape)",
			"--bind=every(3):bg-transform("+helper+" auto-refresh)+bg-transform-footer("+helper+" footer)",
		)
		if len(s.NewArgs) > 0 {
			args = append(args, "--bind=ctrl-n:bg-cancel+transform("+helper+" begin-new)")
		}
		if len(s.TerminateArgs) > 0 {
			args = append(args, "--bind=ctrl-x:bg-cancel+transform("+helper+" begin-terminate {1})")
		}
		if len(s.AliasArgs) > 0 {
			args = append(args, "--bind=ctrl-r:bg-cancel+transform("+helper+" begin-alias {1})")
		}
	}
	// Network/catalog timeouts must not limit how long a person may spend in
	// the interactive selector. fzf still receives terminal signals directly.
	cmd := exec.CommandContext(context.WithoutCancel(ctx), fzf, args...)
	cmd.Stdin = strings.NewReader(input.String())
	cmd.Stderr = os.Stderr
	prepareSelectorScreen(s.Resume)
	output, err := safeexec.Output(cmd, 64*1024)
	if err != nil {
		RestoreLauncherScreen()
		var exit *exec.ExitError
		if errors.As(err, &exit) && (exit.ExitCode() == 1 || exit.ExitCode() == 130) {
			return "", ErrCancelled
		}
		return "", fmt.Errorf("fzf: %w", err)
	}
	id := strings.SplitN(strings.TrimSpace(string(output)), "\t", 2)[0]
	if err := model.ValidateSessionID(id); err != nil {
		RestoreLauncherScreen()
		return "", err
	}
	return id, nil
}

func prepareSelectorScreen(resume bool) {
	if os.Getenv("HMUX_LAUNCHER") != "1" || !term.IsTerminal(int(os.Stderr.Fd())) {
		return
	}
	// A resumed selector overlays the preserved fzf interface from its top-left
	// corner. Clearing here would introduce the exact blank frame that
	// --no-clear is intended to avoid.
	_, _ = fmt.Fprint(os.Stderr, selectorScreenEscape(resume))
}

func selectorScreenEscape(resume bool) string {
	if resume {
		return "\x1b[H"
	}
	return "\x1b[2J\x1b[H"
}

// RestoreLauncherScreen removes a selector intentionally preserved with
// --no-clear before the launcher returns control to the user's shell.
func RestoreLauncherScreen() {
	if os.Getenv("HMUX_LAUNCHER") != "1" || !term.IsTerminal(int(os.Stderr.Fd())) {
		return
	}
	_, _ = fmt.Fprint(os.Stderr, "\x1b[2J\x1b[H")
}

func createSelectorHelper(
	executable string,
	refreshArgs, footerArgs, newArgs, aliasArgs, terminateArgs []string,
	initialCatalog, initialQuery string,
) (string, func(), error) {
	dir, err := os.MkdirTemp("", "hmux-sort-*")
	if err != nil {
		return "", nil, err
	}
	cleanup := func() { _ = os.RemoveAll(dir) }
	if err := os.Chmod(dir, 0o700); err != nil {
		cleanup()
		return "", nil, err
	}
	sortPath := filepath.Join(dir, "sort-state")
	if err := os.WriteFile(sortPath, []byte("session asc\n"), 0o600); err != nil {
		cleanup()
		return "", nil, err
	}
	modePath := filepath.Join(dir, "mode-state")
	if err := os.WriteFile(modePath, []byte("normal -\n"), 0o600); err != nil {
		cleanup()
		return "", nil, err
	}
	queryPath := filepath.Join(dir, "query-state")
	safeInitialQuery := model.SafeText(initialQuery, 4096)
	if err := os.WriteFile(queryPath, []byte(safeInitialQuery+"\n"), 0o600); err != nil {
		cleanup()
		return "", nil, err
	}
	refreshLock := filepath.Join(dir, "refresh-lock")
	refreshResult := filepath.Join(dir, "refresh-result")
	if err := os.WriteFile(refreshResult, []byte(initialCatalog), 0o600); err != nil {
		cleanup()
		return "", nil, err
	}
	command := shellQuote(executable)
	for _, arg := range refreshArgs {
		command += " " + shellQuote(arg)
	}
	footerCommand := shellQuote(executable)
	for _, arg := range footerArgs {
		footerCommand += " " + shellQuote(arg)
	}
	newCommand := shellQuote(executable)
	for _, arg := range newArgs {
		newCommand += " " + shellQuote(arg)
	}
	aliasCommand := shellQuote(executable)
	for _, arg := range aliasArgs {
		aliasCommand += " " + shellQuote(arg)
	}
	terminateCommand := shellQuote(executable)
	for _, arg := range terminateArgs {
		terminateCommand += " " + shellQuote(arg)
	}
	helperPath := filepath.Join(dir, "selector")
	refreshAction := "reload(cat " + shellQuote(refreshResult) + ")"
	restoreAction := "rebind(/)+hide-input+disable-search+clear-query+change-prompt(›  )+" +
		"change-ghost(session name · runtime · model · process · workspace)+" +
		"change-list-label( LIVE SESSIONS )"
	script := fmt.Sprintf(`#!/bin/sh
set -eu
sort_state=%s
mode_state=%s
query_state=%s
refresh_lock=%s
refresh_result=%s
refresh_action=%s
restore_action=%s
mode=${1:-refresh}
read -r column direction < "$sort_state"
refresh() {
  exec %s --selector-sort "$column" --selector-direction "$direction" --selector-width "${FZF_COLUMNS:-100}"
}
search() {
  query=${1:-}
  read -r current_mode current_id < "$mode_state"
  if [ "$current_mode" != normal ]; then
    refresh
  fi
  exec %s --selector-sort "$column" --selector-direction "$direction" --selector-query "$query" --selector-width "${FZF_COLUMNS:-100}"
}
search_state() {
  query=
  IFS= read -r query < "$query_state" || :
  search "$query"
}
valid_id() {
  number=${1#'$'}
  [ "$number" != "$1" ] && [ -n "$number" ] &&
    case "$number" in *[!0-9]*) false ;; *) true ;; esac
}
normal_state() {
  printf 'normal -\n' > "$mode_state"
  chmod 600 "$mode_state"
}
cleanup_refresh() {
  rm -f "$refresh_result.next.$$" "$refresh_lock/pid"
  rmdir "$refresh_lock" 2>/dev/null || :
}
acquire_refresh() {
  if mkdir "$refresh_lock" 2>/dev/null; then
    printf '%%s\n' "$$" > "$refresh_lock/pid"
    chmod 600 "$refresh_lock/pid"
    return 0
  fi
  refresh_pid=
  IFS= read -r refresh_pid < "$refresh_lock/pid" 2>/dev/null || :
  case "$refresh_pid" in
  ''|*[!0-9]*) ;;
  *)
    if kill -0 "$refresh_pid" 2>/dev/null; then
      return 1
    fi
    ;;
  esac
  rm -f "$refresh_lock/pid"
  rmdir "$refresh_lock" 2>/dev/null || return 1
  mkdir "$refresh_lock" 2>/dev/null || return 1
  printf '%%s\n' "$$" > "$refresh_lock/pid"
  chmod 600 "$refresh_lock/pid"
}
auto_refresh() {
  read -r current_mode current_id < "$mode_state"
  [ "$current_mode" = normal ] || return 0
  idle_ms=${FZF_IDLE_TIME_MS:-0}
  case "$idle_ms" in ''|*[!0-9]*) return 0 ;; esac
  [ "$idle_ms" -ge 2000 ] || return 0
  acquire_refresh || return 0
  trap cleanup_refresh 0
  trap 'cleanup_refresh; exit 130' HUP INT TERM
  query=
  IFS= read -r query < "$query_state" || :
  next="$refresh_result.next.$$"
  if %s --selector-sort "$column" --selector-direction "$direction" --selector-query "$query" --selector-width "${FZF_COLUMNS:-100}" > "$next"; then
    chmod 600 "$next"
    if cmp -s "$next" "$refresh_result"; then
      rm -f "$next"
      return 0
    fi
    mv "$next" "$refresh_result"
    printf '%%s' "$refresh_action"
  fi
}
restore_actions() {
  printf '%%s' "$restore_action"
}
case "$mode" in
refresh)
  refresh
  ;;
footer)
  exec %s --selector-width "${FZF_COLUMNS:-100}"
  ;;
auto-refresh)
  auto_refresh
  ;;
toggle)
  query=${2:-}
  case "${FZF_CLICK_HEADER_WORD:-}" in
    SESSION) selected=session ;;
    RUNTIME) selected=runtime ;;
    MODEL) selected=model ;;
    STATE) selected=state ;;
    PROCESS) selected=process ;;
    WORKSPACE) selected=workspace ;;
    WORK) selected=work ;;
    *) selected= ;;
  esac
  if [ -n "$selected" ]; then
    if [ "$column" = "$selected" ] && [ "$direction" = asc ]; then
      direction=desc
    else
      direction=asc
    fi
    column=$selected
    next="$sort_state.next"
    printf '%%s %%s\n' "$column" "$direction" > "$next"
    chmod 600 "$next"
    mv "$next" "$sort_state"
  fi
  exec %s --selector-sort "$column" --selector-direction "$direction" --selector-query "$query" --selector-width "${FZF_COLUMNS:-100}"
  ;;
search)
  search "${2:-}"
  ;;
search-state)
  search_state
  ;;
begin-search)
  normal_state
  printf 'unbind(/)+show-input+disable-search+change-prompt(SEARCH › )+change-ghost(session name · runtime · model · process · workspace)+change-list-label( LIVE SESSIONS )'
  ;;
change)
  read -r current_mode current_id < "$mode_state"
  if [ "$current_mode" != normal ]; then
    exit 0
  fi
  query=${2:-}
  if [ "${#query}" -gt 4096 ]; then
    printf 'change-footer( hmux · query is too long )'
    exit 0
  fi
  next="$query_state.next"
  printf '%%s\n' "$query" > "$next"
  chmod 600 "$next"
  mv "$next" "$query_state"
  printf 'reload(%s search-state)'
  ;;
begin-new)
  printf 'new -\n' > "$mode_state"
  chmod 600 "$mode_state"
  printf 'unbind(/)+show-input+disable-search+clear-query+change-prompt(NEW · profile [name] › )+change-ghost(codex | claude | shell [session name])+change-list-label( INLINE CREATE · ENTER APPLY · ESC CANCEL )'
  ;;
begin-alias|begin-terminate)
  id=${2:-}
  valid_id "$id" || {
    printf 'change-footer( hmux · invalid selection )'
    exit 0
  }
  action=${mode#begin-}
  printf '%%s %%s\n' "$action" "$id" > "$mode_state"
  chmod 600 "$mode_state"
  if [ "$action" = alias ]; then
    printf 'unbind(/)+show-input+disable-search+clear-query+change-prompt(ALIAS %%s › )+change-ghost(new alias · empty restores original name)+change-list-label( INLINE RENAME · ENTER APPLY · ESC CANCEL )' "$id"
  else
    printf 'unbind(/)+show-input+disable-search+clear-query+change-prompt(TERMINATE %%s › )+change-ghost(type yes to permanently terminate)+change-list-label( INLINE CONFIRMATION · ENTER APPLY · ESC CANCEL )' "$id"
  fi
  ;;
escape)
  normal_state
  restore_actions
  ;;
enter)
  read -r action id < "$mode_state"
  if [ "$action" = normal ]; then
    printf 'accept'
    exit 0
  fi
  value=${3:-}
  if [ "$action" = alias ]; then
    if %s --inline "$id" "$value" >/dev/null 2>&1; then
      normal_state
      printf 'clear-screen+'
      restore_actions
      printf '+reload(%s refresh)'
    else
      printf 'change-prompt(ALIAS ERROR · retry › )'
    fi
    exit 0
  fi
  if [ "$action" = new ]; then
    if [ -z "$value" ]; then
      printf 'change-prompt(NEW · enter profile [name] › )'
    elif %s --inline "$value" >/dev/null 2>&1; then
      normal_state
      printf 'clear-screen+'
      restore_actions
      printf '+reload(%s refresh)'
    else
      printf 'change-prompt(NEW ERROR · profile [name] › )'
    fi
    exit 0
  fi
  case "$value" in
  y|Y|yes|YES|Yes)
    if %s --inline "$id" "$value" >/dev/null 2>&1; then
      normal_state
      printf 'clear-screen+'
      restore_actions
      printf '+reload(%s refresh)'
    else
      printf 'change-prompt(TERMINATE ERROR · retry yes › )'
    fi
    ;;
  *)
    printf 'change-prompt(TYPE yes TO TERMINATE %%s › )' "$id"
    ;;
  esac
  ;;
*)
  printf 'change-footer( hmux · invalid inline action )'
  ;;
esac
`,
		shellQuote(sortPath),
		shellQuote(modePath),
		shellQuote(queryPath),
		shellQuote(refreshLock),
		shellQuote(refreshResult),
		shellQuote(refreshAction),
		shellQuote(restoreAction),
		command,
		command,
		command,
		footerCommand,
		command,
		shellQuote(helperPath),
		aliasCommand,
		shellQuote(helperPath),
		newCommand,
		shellQuote(helperPath),
		terminateCommand,
		shellQuote(helperPath),
	)
	// #nosec G306 -- this short-lived helper is intentionally executable and
	// resides in a freshly created, mode-0700 directory owned by this process.
	if err := os.WriteFile(helperPath, []byte(script), 0o700); err != nil {
		cleanup()
		return "", nil, err
	}
	return helperPath, cleanup, nil
}

func SelectProfile(ctx context.Context, profiles []model.Profile) (string, error) {
	if len(profiles) == 0 {
		return "", errors.New("no profiles are configured")
	}
	SortProfiles(profiles)
	fzf := fzfPath()
	if fzf == "" {
		return numericProfileFallback(profiles)
	}
	var input strings.Builder
	for _, profile := range profiles {
		display := fmt.Sprintf("%-16s %s", profile.ID, profile.Label)
		search := strings.Join([]string{profile.ID, profile.Label, strings.Join(profile.Tags, " ")}, " ")
		fmt.Fprintf(&input, "%s\t%s\x1b[8m %s\x1b[0m\n", profile.ID, display, model.SafeText(search, 4096))
	}
	args := []string{
		"--delimiter=\t", "--with-nth=2", "--ansi", "--layout=reverse",
		"--style=minimal", "--input-border=rounded",
		"--input-label= hmux · new session ", "--list-border=rounded", "--list-label= Profiles ",
		"--prompt=›  ", "--ghost=profile · label · tag",
		"--pointer=›", "--gutter= ", "--highlight-line", "--cycle", "--no-hscroll",
		"--bind=ctrl-q:abort",
	}
	if os.Getenv("NO_COLOR") != "" {
		args = append(args, "--no-color")
	} else {
		args = append(args, "--color=fg:#cecdc3,bg:#100f0f,fg+:#cecdc3,bg+:#282726,hl:#da702c,hl+:#d0a215,prompt:#4385be,pointer:#4385be,border:#343331,label:#4385be,query:#cecdc3")
	}
	cmd := exec.CommandContext(context.WithoutCancel(ctx), fzf, args...)
	cmd.Stdin = strings.NewReader(input.String())
	cmd.Stderr = os.Stderr
	output, err := safeexec.Output(cmd, 64*1024)
	if err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) && (exit.ExitCode() == 1 || exit.ExitCode() == 130) {
			return "", ErrCancelled
		}
		return "", err
	}
	id := strings.SplitN(strings.TrimSpace(string(output)), "\t", 2)[0]
	for _, profile := range profiles {
		if profile.ID == id {
			return id, nil
		}
	}
	return "", errors.New("selector returned an unknown profile")
}

func numericProfileFallback(profiles []model.Profile) (string, error) {
	if len(profiles) == 1 {
		return profiles[0].ID, nil
	}
	info, err := os.Stdin.Stat()
	if err != nil || info.Mode()&os.ModeCharDevice == 0 {
		return "", errors.New("fzf is unavailable and stdin is not interactive; pass a profile explicitly")
	}
	for index, profile := range profiles {
		fmt.Fprintf(os.Stderr, "%3d  %-16s %s\n", index+1, profile.ID, profile.Label)
	}
	fmt.Fprint(os.Stderr, "Select profile number (empty to cancel): ")
	line, err := bufio.NewReader(os.Stdin).ReadString('\n')
	if err != nil {
		return "", err
	}
	line = strings.TrimSpace(line)
	if line == "" {
		return "", ErrCancelled
	}
	n, err := strconv.Atoi(line)
	if err != nil || n < 1 || n > len(profiles) {
		return "", errors.New("invalid profile selection")
	}
	return profiles[n-1].ID, nil
}

func fzfPath() string {
	if path, err := exec.LookPath("fzf"); err == nil {
		return path
	}
	for _, path := range []string{"/opt/homebrew/bin/fzf", "/usr/local/bin/fzf", "/usr/bin/fzf"} {
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Mode()&0o111 != 0 {
			return path
		}
	}
	return ""
}

func SelectorLines(sessions []model.Session, mobile bool) string {
	return SelectorLinesAtWidth(sessions, mobile, terminalWidth())
}

func SelectorLinesAtWidth(sessions []model.Session, mobile bool, screenWidth int) string {
	var out strings.Builder
	rowWidth := selectorRowWidth(screenWidth, mobile)
	for _, session := range sessions {
		out.WriteString(selectorLine(session, mobile, rowWidth))
	}
	return out.String()
}

func FormatRow(s model.Session, mobile bool, width int) string {
	return formatRow(s, mobile, width, false)
}

func formatRow(s model.Session, mobile bool, width int, styled bool) string {
	status := statusGlyph(s)
	workTime := "—"
	if (normalizedRuntime(s) == "codex" || normalizedRuntime(s) == "claude") &&
		stateLabel(s) == "working" {
		workTime = "?"
		if s.WorkingSince > 0 {
			workTime = workingDuration(s.WorkingSince)
		}
	}
	name := displayName(s)
	runtimeName := runtimeLabel(s)
	modelName := emptyDash(model.SafeText(s.Model, 128))
	state := stateLabel(s)
	process := processLabel(s)
	if badge := workflow.SummaryBadge(s.Workflow); badge != "" {
		process = badge + " " + process
	}
	path := homeRelative(s.CurrentPath)
	if path == "" {
		path = "—"
	}
	if width < 28 {
		width = 28
	}
	if width < 40 {
		runtimeWidth := 7
		stateWidth := 7
		nameWidth := width - displayWidth(status) - runtimeWidth - stateWidth - 3
		if nameWidth < 4 {
			nameWidth = 4
		}
		return strings.Join([]string{
			styleStatus(status, s, styled),
			styleField(padRight(truncate(name, nameWidth), nameWidth), "name", styled),
			styleField(padRight(truncate(runtimeName, runtimeWidth), runtimeWidth), "runtime-"+strings.ToLower(runtimeName), styled),
			styleField(padRight(truncate(state, stateWidth), stateWidth), "state-"+s.State, styled),
		}, " ")
	}
	if width < 56 {
		runtimeWidth := 7
		modelWidth := 10
		stateWidth := 7
		nameWidth := width - displayWidth(status) - runtimeWidth - modelWidth - stateWidth - 4
		if nameWidth < 4 {
			nameWidth = 4
		}
		return strings.Join([]string{
			styleStatus(status, s, styled),
			styleField(padRight(truncate(name, nameWidth), nameWidth), "name", styled),
			styleField(padRight(truncate(runtimeName, runtimeWidth), runtimeWidth), "runtime-"+strings.ToLower(runtimeName), styled),
			styleField(padRight(truncate(modelName, modelWidth), modelWidth), "model", styled),
			styleField(padRight(truncate(state, stateWidth), stateWidth), "state-"+s.State, styled),
		}, " ")
	}
	if width < 68 || mobile {
		runtimeWidth := 7
		modelWidth := 14
		stateWidth := 8
		workWidth := 7
		remaining := width - displayWidth(status) - runtimeWidth - modelWidth -
			stateWidth - workWidth - 6
		nameWidth := remaining * 3 / 5
		processWidth := remaining - nameWidth
		if nameWidth < 8 {
			nameWidth = 8
		}
		if processWidth < 5 {
			processWidth = 5
		}
		return strings.Join([]string{
			styleStatus(status, s, styled),
			styleField(padRight(truncate(name, nameWidth), nameWidth), "name", styled),
			styleField(padRight(truncate(runtimeName, runtimeWidth), runtimeWidth), "runtime-"+strings.ToLower(runtimeName), styled),
			styleField(padRight(truncate(modelName, modelWidth), modelWidth), "model", styled),
			styleField(padRight(truncate(state, stateWidth), stateWidth), "state-"+s.State, styled),
			styleField(padRight(truncate(workTime, workWidth), workWidth), "work", styled),
			styleField(padRight(truncate(process, processWidth), processWidth), "process", styled),
		}, " ")
	}
	nameWidth, runtimeWidth, modelWidth, stateWidth, workWidth, processWidth := fullColumnWidths(width)
	pathWidth := width - displayWidth(status) - nameWidth - runtimeWidth - modelWidth -
		stateWidth - workWidth - processWidth - 7
	return strings.Join([]string{
		styleStatus(status, s, styled),
		styleField(padRight(truncate(name, nameWidth), nameWidth), "name", styled),
		styleField(padRight(truncate(runtimeName, runtimeWidth), runtimeWidth), "runtime-"+strings.ToLower(runtimeName), styled),
		styleField(padRight(truncate(modelName, modelWidth), modelWidth), "model", styled),
		styleField(padRight(truncate(state, stateWidth), stateWidth), "state-"+s.State, styled),
		styleField(padRight(truncate(workTime, workWidth), workWidth), "work", styled),
		styleField(padRight(truncate(process, processWidth), processWidth), "process", styled),
		styleField(padRight(truncateMiddle(path, pathWidth), pathWidth), "path", styled),
	}, " ")
}

func selectorLine(session model.Session, mobile bool, width int) string {
	row := formatRow(session, mobile, width, true)
	return fmt.Sprintf("%s\t%s\n", session.ID, row)
}

func selectorSearchText(session model.Session) string {
	values := []string{
		session.Name,
		session.Alias,
		session.Label,
		session.Profile,
		strings.Join(session.Tags, " "),
		strings.Join(session.WindowNames, " "),
		session.ActiveWindow,
		session.CurrentCommand,
		session.CurrentPath,
		session.HostAlias,
		normalizedRuntime(session),
		session.Model,
		stateLabel(session),
		processLabel(session),
		workflow.SummaryBadge(session.Workflow),
		strconv.FormatInt(session.ActivityAt, 10),
	}
	for index := range values {
		values[index] = model.SafeText(values[index], 4096)
	}
	return strings.Join(values, " ")
}

func FilterSessions(sessions []model.Session, query string) []model.Session {
	terms := strings.Fields(strings.ToLower(model.SafeText(query, 4096)))
	if len(terms) == 0 {
		return append([]model.Session(nil), sessions...)
	}
	result := make([]model.Session, 0, len(sessions))
	for _, session := range sessions {
		haystack := strings.ToLower(selectorSearchText(session))
		matches := true
		for _, term := range terms {
			if !strings.Contains(haystack, term) {
				matches = false
				break
			}
		}
		if matches {
			result = append(result, session)
		}
	}
	return result
}

func SortSessions(sessions []model.Session, column, direction string) ([]model.Session, error) {
	switch column {
	case "", "session", "runtime", "model", "state", "work", "process", "workspace":
	default:
		return nil, fmt.Errorf("invalid selector sort column %q", column)
	}
	if column == "" {
		column = "session"
	}
	if direction == "" {
		direction = "asc"
	}
	if direction != "asc" && direction != "desc" {
		return nil, fmt.Errorf("invalid selector sort direction %q", direction)
	}
	result := append([]model.Session(nil), sessions...)
	value := func(session model.Session) string {
		switch column {
		case "runtime":
			return normalizedRuntime(session)
		case "model":
			return session.Model
		case "state":
			return stateLabel(session)
		case "process":
			return processLabel(session)
		case "workspace":
			return session.CurrentPath
		default:
			return displayName(session)
		}
	}
	sort.SliceStable(result, func(i, j int) bool {
		comparison := 0
		switch column {
		case "work":
			left := sortableWorkingSince(result[i])
			right := sortableWorkingSince(result[j])
			switch {
			case left == 0 && right != 0:
				comparison = 1
			case left != 0 && right == 0:
				comparison = -1
			case left > right:
				// A later start means a shorter working duration.
				comparison = -1
			case left < right:
				comparison = 1
			}
		default:
			left := strings.ToLower(value(result[i]))
			right := strings.ToLower(value(result[j]))
			comparison = strings.Compare(left, right)
			if comparison == 0 {
				comparison = strings.Compare(strings.ToLower(displayName(result[i])), strings.ToLower(displayName(result[j])))
			}
		}
		if comparison == 0 {
			comparison = strings.Compare(result[i].ID, result[j].ID)
		}
		if direction == "desc" {
			return comparison > 0
		}
		return comparison < 0
	})
	return result, nil
}

func sortableWorkingSince(session model.Session) int64 {
	if (normalizedRuntime(session) != "codex" && normalizedRuntime(session) != "claude") ||
		stateLabel(session) != "working" || session.WorkingSince <= 0 {
		return 0
	}
	return session.WorkingSince
}

func selectorRowWidth(screenWidth int, mobile bool) int {
	if screenWidth < 40 {
		screenWidth = 40
	}
	if mobile {
		return screenWidth - 6
	}
	width := screenWidth - 10
	if width < 32 {
		return 32
	}
	if width > 180 {
		return 180
	}
	return width
}

func formatSelectorHeader(sessions []model.Session, mobile bool, width int, styled bool) string {
	return formatSelectorHeaderWithTabs(sessions, nil, "", mobile, width, styled)
}

func formatSelectorHeaderWithTabs(
	sessions, openTabs []model.Session,
	currentTabID string,
	mobile bool,
	width int,
	styled bool,
) string {
	counts := map[string]int{"codex": 0, "claude": 0, "process": 0}
	workflowCounts := model.WorkflowSummary{}
	for _, session := range sessions {
		runtimeName := normalizedRuntime(session)
		counts[runtimeName]++
		if session.Workflow != nil {
			workflowCounts.Running += session.Workflow.Running
			workflowCounts.WaitingApproval += session.Workflow.WaitingApproval
			workflowCounts.WaitingInput += session.Workflow.WaitingInput
			workflowCounts.Completed += session.Workflow.Completed
			workflowCounts.Failed += session.Workflow.Failed
			workflowCounts.Interrupted += session.Workflow.Interrupted
			workflowCounts.Stale += session.Workflow.Stale
		}
	}
	title := "  HMUX  /  SESSION CONTROL"
	tabs := formatSelectorTabs(openTabs, currentTabID, max(0, width-displayWidth(title)-2), styled)
	if tabs != "" {
		tabs = "  " + tabs
	} else {
		tabs = "  TABS"
	}
	summary := ""
	if width < 64 {
		summary = fmt.Sprintf(
			"  %d total   CX %d   CL %d   PR %d",
			len(sessions), counts["codex"], counts["claude"], counts["process"],
		)
	} else {
		summary = fmt.Sprintf(
			"  %d sessions   %d Codex   %d Claude   %d Processes",
			len(sessions), counts["codex"], counts["claude"], counts["process"],
		)
		if badge := workflow.SummaryBadge(&workflowCounts); badge != "" && width >= 88 {
			summary += "   WF " + badge
		}
	}
	columns := formatColumnHeader(mobile, width)
	if styled {
		if os.Getenv("NO_COLOR") == "" {
			title = "\x1b[1;38;2;67;133;190m" + title + "\x1b[0m"
			summary = "\x1b[38;2;135;133;128m" + summary + "\x1b[0m"
			columns = "\x1b[1;38;2;206;205;195m" + columns + "\x1b[0m"
		} else {
			title = "\x1b[1m" + title + "\x1b[0m"
			columns = "\x1b[1m" + columns + "\x1b[0m"
		}
	}
	return title + tabs + "\n" + summary + "\n\n" + columns
}

func formatSelectorTabs(
	sessions []model.Session,
	currentID string,
	maximumWidth int,
	styled bool,
) string {
	label := "TABS"
	if maximumWidth < displayWidth(label) {
		return ""
	}
	type tab struct {
		index  int
		name   string
		active bool
	}
	used := displayWidth(label)
	visible := make([]tab, 0, min(len(sessions), 9))
	for index, session := range sessions {
		if index >= 9 {
			break
		}
		name := workflowTabLabel(session, 16)
		plain := fmt.Sprintf("   %d %s ", index+1, name)
		if used+displayWidth(plain) > maximumWidth {
			break
		}
		used += displayWidth(plain)
		visible = append(visible, tab{
			index: index + 1, name: name, active: session.ID == currentID,
		})
	}
	hidden := len(sessions) - len(visible)
	hiddenText := ""
	if hidden > 0 {
		candidate := fmt.Sprintf("  +%d", hidden)
		if used+displayWidth(candidate) <= maximumWidth {
			hiddenText = candidate
		}
	}
	if !styled || os.Getenv("NO_COLOR") != "" {
		var result strings.Builder
		result.WriteString(label)
		for _, item := range visible {
			fmt.Fprintf(&result, "   %d %s ", item.index, item.name)
		}
		result.WriteString(hiddenText)
		return result.String()
	}
	var result strings.Builder
	result.WriteString("\x1b[1;38;2;135;133;128m")
	result.WriteString(label)
	result.WriteString("\x1b[0m")
	for _, item := range visible {
		capColor := "40;39;38"
		bodyForeground := "206;205;195"
		if item.active {
			capColor = "67;133;190"
			bodyForeground = "16;15;15"
		}
		fmt.Fprintf(
			&result,
			"  \x1b[38;2;%sm\x1b[1;38;2;%s;48;2;%sm %d %s \x1b[0m\x1b[38;2;%sm\x1b[0m",
			capColor, bodyForeground, capColor, item.index, item.name, capColor,
		)
	}
	if hiddenText != "" {
		result.WriteString("\x1b[38;2;135;133;128m")
		result.WriteString(hiddenText)
		result.WriteString("\x1b[0m")
	}
	return result.String()
}

func formatColumnHeader(mobile bool, width int) string {
	if width < 28 {
		width = 28
	}
	if width < 40 {
		runtimeWidth := 7
		stateWidth := 7
		nameWidth := width - 1 - runtimeWidth - stateWidth - 3
		if nameWidth < 4 {
			nameWidth = 4
		}
		return strings.Join([]string{
			" ",
			padRight(truncate("SESSION", nameWidth), nameWidth),
			padRight("RUNTIME", runtimeWidth),
			padRight("STATE", stateWidth),
		}, " ")
	}
	if width < 56 {
		runtimeWidth := 7
		modelWidth := 10
		stateWidth := 7
		nameWidth := width - 1 - runtimeWidth - modelWidth - stateWidth - 4
		if nameWidth < 4 {
			nameWidth = 4
		}
		return strings.Join([]string{
			" ",
			padRight(truncate("SESSION", nameWidth), nameWidth),
			padRight("RUNTIME", runtimeWidth),
			padRight("MODEL", modelWidth),
			padRight("STATE", stateWidth),
		}, " ")
	}
	if width < 68 || mobile {
		runtimeWidth := 7
		modelWidth := 14
		stateWidth := 8
		workWidth := 7
		remaining := width - 1 - runtimeWidth - modelWidth - stateWidth - workWidth - 6
		nameWidth := remaining * 3 / 5
		processWidth := remaining - nameWidth
		if nameWidth < 8 {
			nameWidth = 8
		}
		if processWidth < 5 {
			processWidth = 5
		}
		return strings.Join([]string{
			" ",
			padRight(truncate("SESSION", nameWidth), nameWidth),
			padRight("RUNTIME", runtimeWidth),
			padRight("MODEL", modelWidth),
			padRight("STATE", stateWidth),
			padRight("WORK", workWidth),
			padRight(truncate("PROCESS", processWidth), processWidth),
		}, " ")
	}
	nameWidth, runtimeWidth, modelWidth, stateWidth, workWidth, processWidth := fullColumnWidths(width)
	pathWidth := width - 1 - nameWidth - runtimeWidth - modelWidth -
		stateWidth - workWidth - processWidth - 7
	return strings.Join([]string{
		" ",
		padRight("SESSION", nameWidth),
		padRight("RUNTIME", runtimeWidth),
		padRight("MODEL", modelWidth),
		padRight("STATE", stateWidth),
		padRight("WORK", workWidth),
		padRight("PROCESS", processWidth),
		padRight(truncate("WORKSPACE", pathWidth), pathWidth),
	}, " ")
}

func fullColumnWidths(width int) (name, runtimeName, modelName, state, work, process int) {
	if width < 76 {
		return 12, 7, 10, 7, 6, 7
	}
	if width < 100 {
		return 15, 7, 11, 8, 7, 8
	}
	return 18, 8, 14, 9, 8, 10
}

type systemUsage struct {
	CPU      int
	RAM      int
	CPUValid bool
	RAMValid bool
}

func SelectorFooter(help string, width int) string {
	if width < 32 {
		width = 32
	}
	return formatSelectorFooter(help, width, readSystemUsage(), time.Now(), true)
}

func formatSelectorFooter(help string, width int, usage systemUsage, now time.Time, styled bool) string {
	cpu := "CPU —"
	if usage.CPUValid {
		cpu = fmt.Sprintf("CPU %d%%", usage.CPU)
	}
	ram := "RAM —"
	if usage.RAMValid {
		ram = fmt.Sprintf("RAM %d%%", usage.RAM)
	}
	left := "  " + help
	right := cpu + "   " + ram + "   " + now.Format("Jan 02 03:04 PM") + "  "
	var footer string
	if displayWidth(left)+displayWidth(right)+2 <= width {
		footer = left + strings.Repeat(" ", width-displayWidth(left)-displayWidth(right)) + right
	} else {
		footer = truncate(left, width) + "\n" + strings.Repeat(" ", max(0, width-displayWidth(right))) + truncate(right, width)
	}
	if !styled {
		return footer
	}
	if os.Getenv("NO_COLOR") == "" {
		return "\x1b[38;2;135;133;128m" + footer + "\x1b[0m"
	}
	return footer
}

func readSystemUsage() systemUsage {
	var result systemUsage
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	psPath := executablePath("ps", "/bin/ps", "/usr/bin/ps")
	if psPath != "" {
		if output, err := safeexec.Output(
			exec.CommandContext(ctx, psPath, "-A", "-o", "%cpu="),
			4*1024*1024,
		); err == nil {
			total := 0.0
			for _, field := range strings.Fields(string(output)) {
				value, parseErr := strconv.ParseFloat(strings.ReplaceAll(field, ",", "."), 64)
				if parseErr == nil && value >= 0 {
					total += value
				}
			}
			cores := runtime.NumCPU()
			if cores < 1 {
				cores = 1
			}
			result.CPU = clampPercent(int(total/float64(cores) + 0.5))
			result.CPUValid = true
		}
	}
	memoryPath := executablePath("memory_pressure", "/usr/bin/memory_pressure")
	if memoryPath != "" {
		if output, err := safeexec.Output(
			exec.CommandContext(ctx, memoryPath, "-Q"),
			64*1024,
		); err == nil {
			for _, field := range strings.Fields(string(output)) {
				if !strings.HasSuffix(field, "%") {
					continue
				}
				free, parseErr := strconv.Atoi(strings.TrimSuffix(field, "%"))
				if parseErr == nil {
					result.RAM = clampPercent(100 - free)
					result.RAMValid = true
				}
			}
		}
	}
	return result
}

func clampPercent(value int) int {
	if value < 0 {
		return 0
	}
	if value > 100 {
		return 100
	}
	return value
}

func statusGlyph(s model.Session) string {
	switch stateLabel(s) {
	case "working":
		return "●"
	case "running":
		return "◆"
	case "idle":
		return "○"
	default:
		return "◌"
	}
}

func runtimeLabel(s model.Session) string {
	switch normalizedRuntime(s) {
	case "codex":
		return "Codex"
	case "claude":
		return "Claude"
	default:
		return "Process"
	}
}

func normalizedRuntime(s model.Session) string {
	switch s.Runtime {
	case "codex", "claude":
		return s.Runtime
	}
	switch s.Kind {
	case "codex", "claude":
		return s.Kind
	default:
		return "process"
	}
}

func stateLabel(s model.Session) string {
	switch s.State {
	case "working", "idle", "running", "unknown":
		return s.State
	default:
		if normalizedRuntime(s) == "process" {
			return "running"
		}
		return "unknown"
	}
}

func processLabel(s model.Session) string {
	if s.Process != "" {
		if value := model.SafeText(s.Process, 128); value != "" {
			return value
		}
	}
	if s.CurrentCommand != "" {
		if value := model.SafeText(s.CurrentCommand, 128); value != "" {
			return value
		}
	}
	if runtimeName := normalizedRuntime(s); runtimeName == "codex" || runtimeName == "claude" {
		return runtimeName
	}
	return "shell"
}

func displayName(s model.Session) string {
	if alias := model.SafeText(s.Alias, 128); alias != "" {
		return alias
	}
	return model.SafeText(s.Name, 512)
}

func workingDuration(timestamp int64) string {
	if timestamp <= 0 {
		return "—"
	}
	delta := time.Since(time.Unix(timestamp, 0))
	if delta < 0 {
		delta = 0
	}
	switch {
	case delta < time.Minute:
		return "<1m"
	case delta < time.Hour:
		return fmt.Sprintf("%dm", int(delta.Minutes()))
	case delta < 24*time.Hour:
		return fmt.Sprintf("%dh", int(delta.Hours()))
	default:
		return fmt.Sprintf("%dd", int(delta.Hours()/24))
	}
}

func emptyDash(value string) string {
	if value == "" {
		return "—"
	}
	return value
}

func styleStatus(value string, session model.Session, enabled bool) string {
	if !enabled {
		return value
	}
	if os.Getenv("NO_COLOR") != "" {
		if session.State == "working" {
			return "\x1b[1m" + value + "\x1b[0m"
		}
		return value
	}
	switch stateLabel(session) {
	case "working":
		return "\x1b[1;38;2;135;154;57m" + value + "\x1b[0m"
	case "running":
		return "\x1b[38;2;58;169;159m" + value + "\x1b[0m"
	case "idle":
		return "\x1b[38;2;135;133;128m" + value + "\x1b[0m"
	default:
		return "\x1b[38;2;208;162;21m" + value + "\x1b[0m"
	}
}

func styleField(value, role string, enabled bool) string {
	if !enabled {
		return value
	}
	code := "2"
	switch role {
	case "name":
		code = "1"
	case "runtime-codex":
		if os.Getenv("NO_COLOR") == "" {
			code = "1;38;2;58;169;159"
		}
	case "runtime-claude":
		if os.Getenv("NO_COLOR") == "" {
			code = "1;38;2;206;93;151"
		}
	case "runtime-process":
		if os.Getenv("NO_COLOR") == "" {
			code = "38;2;67;133;190"
		}
	case "model":
		if os.Getenv("NO_COLOR") == "" {
			code = "38;2;218;112;44"
		}
	case "state-working":
		if os.Getenv("NO_COLOR") == "" {
			code = "1;38;2;135;154;57"
		}
	case "state-running":
		if os.Getenv("NO_COLOR") == "" {
			code = "38;2;58;169;159"
		}
	case "state-unknown":
		if os.Getenv("NO_COLOR") == "" {
			code = "38;2;208;162;21"
		}
	case "process", "path":
		code = "2"
	case "work":
		if os.Getenv("NO_COLOR") == "" {
			code = "38;2;208;162;21"
		}
	case "activity":
		if os.Getenv("NO_COLOR") == "" {
			code = "38;2;135;133;128"
		}
	}
	return "\x1b[" + code + "m" + value + "\x1b[0m"
}

func padRight(value string, width int) string {
	padding := width - displayWidth(value)
	if padding <= 0 {
		return value
	}
	return value + strings.Repeat(" ", padding)
}

func numericFallback(sessions []model.Session) (string, error) {
	info, err := os.Stdin.Stat()
	if err != nil || info.Mode()&os.ModeCharDevice == 0 {
		return "", errors.New("fzf is unavailable and stdin is not interactive; install fzf")
	}
	for index, session := range sessions {
		fmt.Fprintf(os.Stderr, "%3d  %s\n", index+1, FormatRow(session, false, terminalWidth()))
	}
	fmt.Fprint(os.Stderr, "Select session number (empty to cancel): ")
	line, err := bufio.NewReader(os.Stdin).ReadString('\n')
	if err != nil {
		return "", err
	}
	line = strings.TrimSpace(line)
	if line == "" {
		return "", ErrCancelled
	}
	n, err := strconv.Atoi(line)
	if err != nil || n < 1 || n > len(sessions) {
		return "", errors.New("invalid selection")
	}
	return sessions[n-1].ID, nil
}

func homeRelative(path string) string {
	home, err := os.UserHomeDir()
	if err == nil && (path == home || strings.HasPrefix(path, home+string(os.PathSeparator))) {
		return "~" + strings.TrimPrefix(path, home)
	}
	return path
}

func truncate(value string, width int) string {
	if displayWidth(value) <= width {
		return value
	}
	var out strings.Builder
	used := 0
	for _, r := range value {
		w := runeWidth(r)
		if used+w > width-1 {
			break
		}
		out.WriteRune(r)
		used += w
	}
	return out.String() + "…"
}

func truncateMiddle(value string, width int) string {
	if displayWidth(value) <= width {
		return value
	}
	leftWidth := (width - 1) / 2
	rightWidth := width - 1 - leftWidth
	left := truncate(value, leftWidth)
	left = strings.TrimSuffix(left, "…")
	runes := []rune(value)
	var right []rune
	used := 0
	for index := len(runes) - 1; index >= 0; index-- {
		w := runeWidth(runes[index])
		if used+w > rightWidth {
			break
		}
		right = append(right, runes[index])
		used += w
	}
	for i, j := 0, len(right)-1; i < j; i, j = i+1, j-1 {
		right[i], right[j] = right[j], right[i]
	}
	return left + "…" + string(right)
}

func displayWidth(value string) int {
	width := 0
	for _, r := range value {
		width += runeWidth(r)
	}
	return width
}

func runeWidth(r rune) int {
	if r == 0 {
		return 0
	}
	if r < utf8.RuneSelf {
		return 1
	}
	switch {
	case r >= 0x1100 && r <= 0x115f,
		r >= 0x2329 && r <= 0x232a,
		r >= 0x2e80 && r <= 0xa4cf,
		r >= 0xac00 && r <= 0xd7a3,
		r >= 0xf900 && r <= 0xfaff,
		r >= 0xfe10 && r <= 0xfe6f,
		r >= 0xff00 && r <= 0xff60,
		r >= 0x1f300 && r <= 0x1faff:
		return 2
	default:
		return 1
	}
}

func terminalWidth() int {
	for _, file := range []*os.File{os.Stderr, os.Stdout, os.Stdin} {
		width, _, err := term.GetSize(int(file.Fd()))
		if err == nil && width >= 20 && width <= 1000 {
			return width
		}
	}
	if value, err := strconv.Atoi(os.Getenv("COLUMNS")); err == nil && value >= 20 && value <= 1000 {
		return value
	}
	return 100
}

func shellQuote(value string) string {
	return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'"
}

func FormatTabs(sessions []model.Session, currentID string, limit int) string {
	if len(sessions) == 0 || limit < 1 {
		return ""
	}
	visible := len(sessions)
	if visible > limit {
		visible = limit
	}
	var parts []string
	for index, session := range sessions[:visible] {
		parts = append(parts, fmt.Sprintf(
			"#[range=user|tab%d]%s#[norange]",
			index+1, FormatTab(session, index+1, currentID),
		))
	}
	if hidden := len(sessions) - visible; hidden > 0 {
		parts = append(parts, fmt.Sprintf(
			"#[fg=#282726,bg=#100f0f]#[fg=#cecdc3,bg=#282726] +%d #[fg=#282726,bg=#100f0f]",
			hidden,
		))
	}
	return strings.Join(parts, "#[bg=#100f0f] ")
}

// FormatTab returns one visual tab without a tmux mouse range. The outer
// disposable server keeps all nine ranges literal in its status format and
// expands these option values inside them, so dynamic label updates cannot
// invalidate tmux's click map.
func FormatTab(session model.Session, number int, currentID string) string {
	if number < 1 || number > 9 {
		return ""
	}
	capColor := "#282726"
	bodyStyle := "#[fg=#cecdc3,bg=#282726]"
	if session.ID == currentID {
		capColor = "#4385be"
		bodyStyle = "#[bold,fg=#100f0f,bg=#4385be]"
	}
	name := strings.ReplaceAll(workflowTabLabel(session, 16), "#", "")
	name = padRight(name, 16)
	return fmt.Sprintf(
		"#[fg=%s,bg=#100f0f]%s %d %s #[nobold,fg=%s,bg=#100f0f]",
		capColor, bodyStyle, number, name, capColor,
	)
}

func workflowTabLabel(session model.Session, width int) string {
	name := displayName(session)
	badge := workflow.SummaryBadge(session.Workflow)
	if badge == "" {
		return truncate(name, width)
	}
	badge = truncate(badge, min(10, width-1))
	nameWidth := width - displayWidth(badge) - 1
	if nameWidth < 3 {
		return truncate(badge, width)
	}
	return truncate(name, nameWidth) + " " + badge
}

func SortProfiles(profiles []model.Profile) {
	sort.Slice(profiles, func(i, j int) bool { return profiles[i].ID < profiles[j].ID })
}

func executablePath(name string, fallbacks ...string) string {
	if path, err := exec.LookPath(name); err == nil {
		return path
	}
	for _, path := range fallbacks {
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Mode()&0o111 != 0 {
			return path
		}
	}
	return ""
}
