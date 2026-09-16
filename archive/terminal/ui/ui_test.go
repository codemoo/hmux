package ui

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

func TestKoreanDisplayWidthAndTruncation(t *testing.T) {
	if got := displayWidth("ab한글"); got != 6 {
		t.Fatalf("width=%d", got)
	}
	got := truncate("아주긴한글프로젝트", 8)
	if displayWidth(got) > 8 {
		t.Fatalf("truncated width=%d value=%q", displayWidth(got), got)
	}
}

func TestRowAndSearchSeparateRawMetadata(t *testing.T) {
	session := model.Session{
		ID: "$1", Name: "main", CurrentPath: "/work/private-api", CurrentCommand: "codex",
		WindowCount: 2, Tags: []string{"hidden-tag"}, WindowNames: []string{"editor"},
		HostAlias: "hmux-home", Runtime: "codex", Model: "gpt-5.6-sol",
		State: "working", Process: "codex",
	}
	row := FormatRow(session, false, 100)
	if strings.Contains(row, "hidden-tag") {
		t.Fatal("hidden search tag leaked into display")
	}
	search := selectorSearchText(session)
	for _, value := range []string{"hidden-tag", "editor", "private-api", "hmux-home", "gpt-5.6-sol", "working"} {
		if !strings.Contains(search, value) {
			t.Errorf("search missing %q", value)
		}
	}
}

func TestRowsFitFortyEightyAndOneSixtyColumnsWithoutANSI(t *testing.T) {
	session := model.Session{
		ID: "$9", Name: "한글과-emoji-🚀-아주-긴-session-name",
		CurrentPath:    "/Users/example/Dropbox/dev/매우-긴-프로젝트/하위/경로",
		CurrentCommand: "claude", Kind: "claude", Runtime: "claude",
		Model: "claude-opus-5", State: "idle", Process: "claude", WindowCount: 123, Attached: 2,
	}
	for _, width := range []int{40, 80, 160} {
		row := FormatRow(session, false, width)
		if strings.Contains(row, "\x1b") {
			t.Fatalf("width %d row contains ANSI: %q", width, row)
		}
		if displayWidth(row) > width {
			t.Fatalf("width %d row is unexpectedly wide (%d): %q", width, displayWidth(row), row)
		}
	}
}

func TestSelectorLinesHandlesFiveHundredSessions(t *testing.T) {
	sessions := make([]model.Session, 500)
	for index := range sessions {
		sessions[index] = model.Session{
			ID: "$" + strconv.Itoa(index+1), Name: "세션 " + strconv.Itoa(index+1),
			CurrentPath: "/work/project", CurrentCommand: "codex", Kind: "codex",
			Runtime: "codex", Model: "gpt-5.6-sol", State: "idle", Process: "codex",
		}
	}
	lines := SelectorLines(sessions, false)
	if count := strings.Count(lines, "\n"); count != 500 {
		t.Fatalf("selector lines=%d", count)
	}
}

func TestSelectorLineNeverLeaksHiddenSearchMetadata(t *testing.T) {
	fzf := fzfPath()
	if fzf == "" {
		t.Skip("fzf is not installed")
	}
	session := model.Session{
		ID: "$17", Name: "short-visible-name-with-unique-suffix",
		CurrentPath: "/work/project", CurrentCommand: "codex",
		Tags: []string{"metadata-only-tag"},
	}
	other := model.Session{
		ID: "$18", Name: "qqqq", Label: "qqqq", Profile: "qqqq",
		CurrentPath: "/qqqq", CurrentCommand: "qqqq", Kind: "qqqq",
		Tags: []string{"qqqq"}, WindowNames: []string{"qqqq"}, HostAlias: "qqqq",
	}
	line := selectorLine(session, false, 32) + selectorLine(other, false, 32)
	for _, forbidden := range []string{"\x1b[8m", "metadata-only-tag", "unique-suffix"} {
		if strings.Contains(line, forbidden) {
			t.Fatalf("selector line leaked %q: %q", forbidden, line)
		}
	}
	if strings.Contains(line, "/work/project") {
		t.Fatalf("narrow selector line exposed clipped workspace metadata: %q", line)
	}
}

func TestExactSearchDoesNotMatchScatteredCharacters(t *testing.T) {
	fzf := fzfPath()
	if fzf == "" {
		t.Skip("fzf is not installed")
	}
	scattered := model.Session{
		ID: "$1", Name: "a-only", CurrentPath: "/work/i-project",
		Tags: []string{"f-tag"}, Runtime: "process", State: "running", Process: "node",
	}
	exact := model.Session{
		ID: "$2", Name: "aif-service", Runtime: "process", State: "running", Process: "node",
	}
	command := exec.Command(
		fzf, "--filter=aif", "--delimiter=\t", "--with-nth=2", "--ansi", "--exact",
	)
	command.Stdin = strings.NewReader(
		selectorLine(scattered, false, 80) + selectorLine(exact, false, 80),
	)
	output, err := command.Output()
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(string(output), "$2\t") || strings.Contains(string(output), "\n$1\t") {
		t.Fatalf("exact query returned scattered match: %q", output)
	}
}

func TestSelectorRowWidthUsesFullListAfterInspectorRemoval(t *testing.T) {
	for screen, want := range map[int]int{80: 70, 160: 150, 240: 180} {
		if got := selectorRowWidth(screen, false); got != want {
			t.Fatalf("screen=%d width=%d want=%d", screen, got, want)
		}
	}
}

func TestNarrowDesktopRowsKeepCommandContext(t *testing.T) {
	session := model.Session{
		Name: "api-server", CurrentCommand: "codex", ActivityAt: time.Now().Unix(),
		Runtime: "codex", Model: "gpt-5.6-sol", State: "working", Process: "codex",
	}
	row := FormatRow(session, false, 40)
	if !strings.Contains(row, "api-") || !strings.Contains(row, "Codex") ||
		!strings.Contains(row, "gpt-5.6") || !strings.Contains(row, "working") {
		t.Fatalf("narrow row lost useful context: %q", row)
	}
	if displayWidth(row) > 40 {
		t.Fatalf("narrow row width=%d: %q", displayWidth(row), row)
	}
}

func TestStyledSelectorRowsPreservePlainDisplayWidth(t *testing.T) {
	session := model.Session{
		Name: "dev-console", CurrentCommand: "node", ActivityAt: time.Now().Unix(),
		Runtime: "process", State: "running", Process: "node", Attached: 1,
	}
	plain := FormatRow(session, false, 36)
	styled := formatRow(session, false, 36, true)
	if !strings.Contains(styled, "\x1b[") {
		t.Fatal("styled selector row has no ANSI hierarchy")
	}
	plainStyled := regexp.MustCompile(`\x1b\[[0-9;]*m`).ReplaceAllString(styled, "")
	if got := displayWidth(plainStyled); got != displayWidth(plain) {
		t.Fatalf("styled width=%d plain width=%d", got, displayWidth(plain))
	}
}

func TestHeaderCountsRuntimesAndMatchesResponsiveColumns(t *testing.T) {
	sessions := []model.Session{
		{Runtime: "codex"},
		{Runtime: "codex"},
		{Runtime: "claude"},
		{Runtime: "process"},
	}
	header := formatSelectorHeader(sessions, false, 150, false)
	for _, value := range []string{
		"HMUX  /  SESSION CONTROL", "4 sessions", "2 Codex", "1 Claude", "1 Processes",
		"SESSION", "RUNTIME", "MODEL", "STATE", "WORK", "PROCESS", "WORKSPACE",
	} {
		if !strings.Contains(header, value) {
			t.Errorf("header missing %q: %q", value, header)
		}
	}
	for _, width := range []int{40, 80, 160} {
		columns := formatColumnHeader(false, width)
		if got := displayWidth(columns); got > width {
			t.Fatalf("width=%d header width=%d: %q", width, got, columns)
		}
		for _, required := range []string{"SESSION", "RUNTIME", "MODEL", "STATE"} {
			if !strings.Contains(columns, required) {
				t.Fatalf("width=%d missing %q: %q", width, required, columns)
			}
		}
		lines := strings.Split(formatSelectorHeader(sessions, false, width, false), "\n")
		if len(lines) < 2 || displayWidth(lines[1]) > width {
			t.Fatalf("width=%d summary does not fit: %q", width, lines)
		}
	}
}

func TestSelectorHasNoInspectorOrPreviewBinding(t *testing.T) {
	dir := t.TempDir()
	fzf := filepath.Join(dir, "fzf")
	argsPath := filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_TEST_FZF_ARGS\"\nprintf '$1\\tselected\\n'\n"
	if err := os.WriteFile(fzf, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_TEST_FZF_ARGS", argsPath)
	_, err := (Selector{FZFPath: fzf}).Select(context.Background(), []model.Session{{
		ID: "$1", Name: "main", Runtime: "codex", Model: "gpt-5.6-sol",
		State: "idle", Process: "codex",
	}})
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	args := string(data)
	for _, forbidden := range []string{"--preview=", "Inspector", "ctrl-p", "--scrollbar="} {
		if strings.Contains(args, forbidden) {
			t.Fatalf("selector still contains %q: %s", forbidden, args)
		}
	}
	for _, required := range []string{
		"HMUX  /  SESSION CONTROL", "--list-label= LIVE SESSIONS ", "--exact", "--no-scrollbar",
		"--header-border=inline", "--footer-border=inline", "--no-separator",
		"CPU ", "RAM ",
	} {
		if !strings.Contains(args, required) {
			t.Fatalf("selector missing %q: %s", required, args)
		}
	}
}

func TestHiddenMetadataRemainsSearchableWithoutBeingDisplayed(t *testing.T) {
	sessions := []model.Session{
		{
			ID: "$7", Name: "main", Runtime: "process", State: "running",
			Tags: []string{"backend", "한국어"}, WindowNames: []string{"deploy-log"},
			Profile: "shell", CurrentPath: "/work/service",
		},
		{ID: "$8", Name: "other", Runtime: "process", State: "running"},
	}
	filtered := FilterSessions(sessions, "backend deploy-log")
	if len(filtered) != 1 || filtered[0].ID != "$7" {
		t.Fatalf("hidden metadata filter=%v", filtered)
	}
	line := selectorLine(sessions[0], false, 100)
	if strings.Contains(line, "deploy-log") || strings.Contains(line, "backend") {
		t.Fatalf("hidden metadata leaked into the rendered row: %q", line)
	}
	if got := FilterSessions(sessions, "aif"); len(got) != 0 {
		t.Fatalf("scattered exact term unexpectedly matched: %v", got)
	}
}

func TestSelectorCanOpenInlineCreateWithNoExistingSessions(t *testing.T) {
	dir := t.TempDir()
	fzf := filepath.Join(dir, "fzf")
	argsPath := filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_TEST_FZF_ARGS\"\nexit 1\n"
	if err := os.WriteFile(fzf, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_TEST_FZF_ARGS", argsPath)
	_, err := (Selector{
		FZFPath: fzf, Executable: "/usr/bin/true",
		RefreshArgs: []string{"selector-lines"}, NewArgs: []string{"new"},
	}).Select(context.Background(), nil)
	if !errors.Is(err, ErrCancelled) {
		t.Fatalf("empty selector did not remain available for inline create: %v", err)
	}
	data, readErr := os.ReadFile(argsPath)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if args := string(data); !strings.Contains(args, "ctrl-n:bg-cancel+transform(") ||
		!strings.Contains(args, "begin-new") {
		t.Fatalf("empty selector has no inline create action: %s", args)
	}
}

func TestFooterRightAlignsUsageAndClock(t *testing.T) {
	now := time.Date(2026, time.July, 29, 5, 31, 0, 0, time.Local)
	footer := formatSelectorFooter(
		"↵ attach   ^N new",
		100,
		systemUsage{CPU: 12, RAM: 34, CPUValid: true, RAMValid: true},
		now,
		false,
	)
	if strings.Contains(footer, "\n") {
		t.Fatalf("wide footer unexpectedly wrapped: %q", footer)
	}
	if displayWidth(footer) != 100 {
		t.Fatalf("footer width=%d: %q", displayWidth(footer), footer)
	}
	if !strings.HasSuffix(footer, "CPU 12%   RAM 34%   Jul 29 05:31 AM  ") {
		t.Fatalf("footer right block is not right-aligned: %q", footer)
	}
}

func TestSelectorOptionsAreAcceptedByInstalledFZF(t *testing.T) {
	fzf := fzfPath()
	if fzf == "" {
		t.Skip("fzf is not installed")
	}
	t.Setenv("HMUX_LAUNCHER", "1")
	dir := t.TempDir()
	wrapper := filepath.Join(dir, "fzf")
	script := "#!/bin/sh\nexec \"$HMUX_REAL_FZF\" --filter=hmux-no-such-session \"$@\"\n"
	if err := os.WriteFile(wrapper, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_REAL_FZF", fzf)
	_, err := (Selector{
		FZFPath: wrapper, Executable: "/usr/bin/true", RefreshArgs: []string{"selector-lines"},
		FooterArgs: []string{"selector-footer"}, NewArgs: []string{"new"},
		AliasArgs: []string{"alias"}, TerminateArgs: []string{"terminate"},
	}).Select(context.Background(), []model.Session{{
		ID: "$1", Name: "main", Runtime: "codex", Model: "gpt-5.6-sol",
		State: "idle", Process: "codex",
	}})
	if !errors.Is(err, ErrCancelled) {
		t.Fatalf("installed fzf rejected selector options: %v", err)
	}
}

func TestLauncherSelectorKeepsItsBackingScreenBetweenFrames(t *testing.T) {
	dir := t.TempDir()
	fzf := filepath.Join(dir, "fzf")
	argsPath := filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_TEST_FZF_ARGS\"\nprintf '$1\\tselected\\n'\n"
	if err := os.WriteFile(fzf, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_LAUNCHER", "1")
	t.Setenv("HMUX_TEST_FZF_ARGS", argsPath)
	id, err := (Selector{FZFPath: fzf, Resume: true}).Select(
		context.Background(),
		[]model.Session{{ID: "$1", Name: "main"}},
	)
	if err != nil || id != "$1" {
		t.Fatalf("id=%q err=%v", id, err)
	}
	args, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(args), "--no-clear\n") {
		t.Fatalf("launcher selector did not preserve its backing screen: %s", args)
	}
	if got := selectorScreenEscape(false); got != "\x1b[2J\x1b[H" {
		t.Fatalf("first selector escape=%q", got)
	}
	if got := selectorScreenEscape(true); got != "\x1b[H" {
		t.Fatalf("resumed selector cleared the preserved screen: %q", got)
	}
}

func TestInteractiveSelectorOutlivesNetworkTimeout(t *testing.T) {
	dir := t.TempDir()
	fzf := filepath.Join(dir, "fzf")
	script := "#!/bin/sh\nsleep 0.05\nprintf '$1\\tselected\\n'\n"
	if err := os.WriteFile(fzf, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer cancel()
	id, err := (Selector{FZFPath: fzf}).Select(ctx, []model.Session{{
		ID: "$1", Name: "long-running selection", ActivityAt: time.Now().Unix(),
	}})
	if err != nil || id != "$1" {
		t.Fatalf("selector was killed by network timeout: id=%q err=%v", id, err)
	}
}

func TestAliasWorkingDurationAndVisibleRowBoundary(t *testing.T) {
	now := time.Now()
	session := model.Session{
		ID: "$3", Name: "tmux-original", Alias: "friendly-tab",
		Runtime: "codex", Model: "gpt-5.6-sol", State: "working", Process: "codex",
		WorkingSince: now.Add(-95 * time.Minute).Unix(),
		CurrentPath:  "/Users/example/private/path",
		ActivityAt:   now.Add(-2 * time.Minute).Unix(),
	}
	row := FormatRow(session, false, 160)
	for _, expected := range []string{"friendly-tab", "Codex", "gpt-5.6-sol", "working", "1h"} {
		if !strings.Contains(row, expected) {
			t.Fatalf("row missing %q: %q", expected, row)
		}
	}
	if strings.Contains(row, "tmux-original") {
		t.Fatalf("original name should be replaced by alias in the visible row: %q", row)
	}
	line := selectorLine(session, false, 160)
	fields := strings.SplitN(strings.TrimSuffix(line, "\n"), "\t", 3)
	if len(fields) != 2 || fields[0] != "$3" {
		t.Fatalf("selector row exposed hidden metadata beyond ID and display: %q", line)
	}
	idle := session
	idle.State = "idle"
	if row := FormatRow(idle, false, 160); strings.Contains(row, "1h") {
		t.Fatalf("idle agent retained a working duration: %q", row)
	}
	unknown := session
	unknown.WorkingSince = 0
	if row := FormatRow(unknown, false, 160); !strings.Contains(row, "?") {
		t.Fatalf("working agent without a lifecycle timestamp is not marked unknown: %q", row)
	}
}

func TestSortSessionsSupportsEveryClickableColumn(t *testing.T) {
	now := time.Now().Unix()
	sessions := []model.Session{
		{ID: "$3", Name: "zeta", Runtime: "process", State: "running", Process: "ruby", CurrentPath: "/z", ActivityAt: now - 600},
		{ID: "$1", Name: "alpha", Alias: "beta", Runtime: "codex", Model: "gpt-5.6-sol", State: "working", Process: "codex", CurrentPath: "/b", ActivityAt: now - 60, WorkingSince: now - 30},
		{ID: "$2", Name: "gamma", Alias: "alpha", Runtime: "claude", Model: "claude-opus-5", State: "working", Process: "claude", CurrentPath: "/a", ActivityAt: now - 120, WorkingSince: now - 300},
	}
	for _, column := range []string{"session", "runtime", "model", "state", "work", "process", "workspace"} {
		ascending, err := SortSessions(sessions, column, "asc")
		if err != nil || len(ascending) != len(sessions) {
			t.Fatalf("column=%s err=%v result=%v", column, err, ascending)
		}
		descending, err := SortSessions(sessions, column, "desc")
		if err != nil || len(descending) != len(sessions) {
			t.Fatalf("column=%s desc err=%v result=%v", column, err, descending)
		}
		if ascending[0].ID != descending[len(descending)-1].ID {
			t.Fatalf("column=%s directions are inconsistent: asc=%v desc=%v", column, ascending, descending)
		}
	}
	defaultOrder, err := SortSessions(sessions, "", "")
	if err != nil || defaultOrder[0].ID != "$2" || defaultOrder[1].ID != "$1" {
		t.Fatalf("default session alias order=%v err=%v", defaultOrder, err)
	}
	workOrder, err := SortSessions(sessions, "work", "asc")
	if err != nil || workOrder[0].ID != "$1" || workOrder[1].ID != "$2" || workOrder[2].ID != "$3" {
		t.Fatalf("work duration order=%v err=%v", workOrder, err)
	}
	for _, invalid := range [][2]string{{"bad", "asc"}, {"session", "sideways"}} {
		if _, err := SortSessions(sessions, invalid[0], invalid[1]); err == nil {
			t.Fatalf("invalid sort accepted: %v", invalid)
		}
	}
}

func TestSelectorActionsExposeSortAliasAndConfirmedTermination(t *testing.T) {
	dir := t.TempDir()
	fzf := filepath.Join(dir, "fzf")
	argsPath := filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_TEST_FZF_ARGS\"\nprintf '$1\\tselected\\n'\n"
	if err := os.WriteFile(fzf, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_TEST_FZF_ARGS", argsPath)
	_, err := (Selector{
		FZFPath: fzf, Executable: "/usr/bin/true",
		RefreshArgs: []string{"selector-lines"},
		NewArgs:     []string{"new"}, AliasArgs: []string{"alias"}, TerminateArgs: []string{"terminate"},
	}).Select(context.Background(), []model.Session{{ID: "$1", Name: "main"}})
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	args := string(data)
	for _, expected := range []string{
		"click-header:bg-cancel+reload-sync(", "every(3):bg-transform(", "auto-refresh",
		"bg-transform-footer(", "change:bg-cancel+transform(", "focus:bg-cancel",
		"--disabled", "--no-input", "/:bg-cancel+transform(", "begin-search",
		"ctrl-n:bg-cancel+transform(", "begin-new", "ctrl-r:bg-cancel+transform(", "begin-alias",
		"ctrl-x:bg-cancel+transform(", "begin-terminate", "--track", "--id-nth=1",
	} {
		if !strings.Contains(args, expected) {
			t.Fatalf("selector action missing %q: %s", expected, args)
		}
	}
	for _, forbidden := range []string{
		"f5:", "every(3):reload(", "every(3):transform(", "+transform-footer(",
		"ctrl-r:reload", "ctrl-r:execute", "ctrl-x:execute", "AGE",
	} {
		if strings.Contains(args, forbidden) {
			t.Fatalf("obsolete refresh binding present %q: %s", forbidden, args)
		}
	}
}

func TestSelectorAutoRefreshWaitsForIdleAndSkipsInlineOrOverlappingWork(t *testing.T) {
	dir := t.TempDir()
	executable := filepath.Join(dir, "selector-lines")
	started := filepath.Join(dir, "started")
	release := filepath.Join(dir, "release")
	argsPath := filepath.Join(dir, "args")
	script := `#!/bin/sh
set -eu
: > "$HMUX_REFRESH_STARTED"
printf '%s\n' "$@" > "$HMUX_REFRESH_ARGS"
while [ ! -f "$HMUX_REFRESH_RELEASE" ]; do sleep 0.01; done
printf '$1\trow\n'
`
	if err := os.WriteFile(executable, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_REFRESH_STARTED", started)
	t.Setenv("HMUX_REFRESH_RELEASE", release)
	t.Setenv("HMUX_REFRESH_ARGS", argsPath)
	helper, cleanup, err := createSelectorHelper(
		executable,
		[]string{"selector-lines"},
		[]string{"selector-footer"},
		[]string{"new"},
		[]string{"alias"},
		[]string{"terminate"},
		"$1\trow\n",
		"row",
	)
	if err != nil {
		t.Fatal(err)
	}
	defer cleanup()
	run := func(idle string, args ...string) string {
		command := exec.Command(helper, args...)
		command.Env = append(os.Environ(), "FZF_IDLE_TIME_MS="+idle)
		output, runErr := command.CombinedOutput()
		if runErr != nil {
			t.Fatalf("helper %v idle=%s: %v: %s", args, idle, runErr, output)
		}
		return string(output)
	}
	if action := run("1999", "auto-refresh"); action != "" {
		t.Fatalf("active selector refreshed: %q", action)
	}
	if err := os.WriteFile(release, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if action := run("2000", "auto-refresh"); action != "" {
		t.Fatalf("unchanged idle selector reloaded: %q", action)
	}
	if got, err := os.ReadFile(argsPath); err != nil ||
		!strings.Contains(string(got), "--selector-query\nrow\n") {
		t.Fatalf("initial query was not preserved across refresh: %q err=%v", got, err)
	}
	if err := os.WriteFile(executable, []byte("#!/bin/sh\nset -eu\n: > \"$HMUX_REFRESH_STARTED\"\nprintf '%s\\n' \"$@\" > \"$HMUX_REFRESH_ARGS\"\nwhile [ ! -f \"$HMUX_REFRESH_RELEASE\" ]; do sleep 0.01; done\nprintf '$1\\tchanged\\n'\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	if action := run("2000", "auto-refresh"); !strings.HasPrefix(action, "reload(cat ") {
		t.Fatalf("changed idle selector did not refresh: %q", action)
	}
	if action := run("2000", "auto-refresh"); action != "" {
		t.Fatalf("already-applied catalog reloaded again: %q", action)
	}
	if err := os.Remove(release); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(started); err != nil {
		t.Fatal(err)
	}
	if action := run("0", "begin-alias", "$1"); !strings.Contains(action, "INLINE RENAME") {
		t.Fatal(action)
	}
	if action := run("5000", "auto-refresh"); action != "" {
		t.Fatalf("inline selector refreshed: %q", action)
	}
	_ = run("0", "escape")

	first := exec.Command(helper, "auto-refresh")
	first.Env = append(os.Environ(), "FZF_IDLE_TIME_MS=5000")
	if err := first.Start(); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(2 * time.Second)
	for {
		if _, err := os.Stat(started); err == nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("refresh helper did not start")
		}
		time.Sleep(5 * time.Millisecond)
	}
	if action := run("5000", "auto-refresh"); action != "" {
		t.Fatalf("overlapping refresh was started: %q", action)
	}
	if err := os.WriteFile(release, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := first.Wait(); err != nil {
		t.Fatal(err)
	}
}

func TestSelectorHelperMapsClickedHeaderAndTogglesDirection(t *testing.T) {
	dir := t.TempDir()
	executable := filepath.Join(dir, "selector-lines")
	logPath := filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_SORT_ARGS\"\n"
	if err := os.WriteFile(executable, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_SORT_ARGS", logPath)
	helper, cleanup, err := createSelectorHelper(
		executable,
		[]string{"selector-lines"},
		[]string{"selector-footer"},
		[]string{"new"},
		[]string{"alias"},
		[]string{"terminate"},
		"",
		"",
	)
	if err != nil {
		t.Fatal(err)
	}
	defer cleanup()
	run := func(word string) string {
		command := exec.Command(helper, "toggle")
		command.Env = append(os.Environ(), "FZF_CLICK_HEADER_WORD="+word)
		if output, err := command.CombinedOutput(); err != nil {
			t.Fatalf("word=%q err=%v output=%s", word, err, output)
		}
		data, err := os.ReadFile(logPath)
		if err != nil {
			t.Fatal(err)
		}
		return string(data)
	}
	if args := run("WORK"); !strings.Contains(args, "--selector-sort\nwork\n") ||
		!strings.Contains(args, "--selector-direction\nasc\n") ||
		!strings.Contains(args, "--selector-width\n100\n") {
		t.Fatalf("first WORK click args=%q", args)
	}
	if args := run("WORK"); !strings.Contains(args, "--selector-direction\ndesc\n") {
		t.Fatalf("second WORK click did not toggle direction: %q", args)
	}
	if args := run("AGE"); strings.Contains(args, "--selector-sort\nage\n") {
		t.Fatalf("removed AGE column is still sortable: %q", args)
	}
}

func TestSelectorHelperUsesInlineModesWithoutExecuteActions(t *testing.T) {
	dir := t.TempDir()
	executable := filepath.Join(dir, "action")
	logPath := filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_ACTION_ARGS\"\n"
	if err := os.WriteFile(executable, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_ACTION_ARGS", logPath)
	helper, cleanup, err := createSelectorHelper(
		executable,
		[]string{"selector-lines"},
		[]string{"selector-footer"},
		[]string{"new"},
		[]string{"alias"},
		[]string{"terminate"},
		"",
		"",
	)
	if err != nil {
		t.Fatal(err)
	}
	defer cleanup()
	run := func(args ...string) string {
		output, err := exec.Command(helper, args...).CombinedOutput()
		if err != nil {
			t.Fatalf("helper %v: %v: %s", args, err, output)
		}
		return string(output)
	}
	if action := run("begin-search"); !strings.Contains(action, "show-input") ||
		!strings.Contains(action, "unbind(/)") || !strings.Contains(action, "SEARCH") ||
		strings.Contains(action, "clear-query") {
		t.Fatalf("search did not explicitly reveal the preserved query input: %q", action)
	}
	if action := run("begin-alias", "$7"); !strings.Contains(action, "disable-search") ||
		!strings.Contains(action, "show-input") || !strings.Contains(action, "INLINE RENAME") ||
		strings.Contains(action, "execute(") {
		t.Fatalf("alias did not enter inline mode: %q", action)
	}
	if action := run("change", "friendly name"); action != "" {
		t.Fatalf("inline typing triggered a catalog reload: %q", action)
	}
	if action := run("enter", "$7", "friendly name"); !strings.Contains(action, "reload(") ||
		strings.Contains(action, "reload-sync(") || !strings.Contains(action, "clear-screen") ||
		!strings.Contains(action, "change-ghost(session name") || strings.Contains(action, "execute(") {
		t.Fatalf("alias did not restore selector inline: %q", action)
	}
	if got, err := os.ReadFile(logPath); err != nil ||
		string(got) != "alias\n--inline\n$7\nfriendly name\n" {
		t.Fatalf("alias argv=%q err=%v", got, err)
	}

	if action := run("begin-terminate", "$7"); !strings.Contains(action, "INLINE CONFIRMATION") {
		t.Fatalf("terminate did not enter inline mode: %q", action)
	}
	if action := run("enter", "$7", "no"); !strings.Contains(action, "TYPE yes") {
		t.Fatalf("negative confirmation was accepted: %q", action)
	}
	if got, _ := os.ReadFile(logPath); string(got) != "alias\n--inline\n$7\nfriendly name\n" {
		t.Fatalf("negative confirmation executed a command: %q", got)
	}
	if action := run("enter", "$7", "yes"); !strings.Contains(action, "reload(") ||
		strings.Contains(action, "reload-sync(") {
		t.Fatalf("positive confirmation did not refresh: %q", action)
	}
	if got, err := os.ReadFile(logPath); err != nil ||
		string(got) != "terminate\n--inline\n$7\nyes\n" {
		t.Fatalf("terminate argv=%q err=%v", got, err)
	}

	if action := run("begin-new"); !strings.Contains(action, "INLINE CREATE") {
		t.Fatalf("new did not enter inline mode: %q", action)
	}
	if action := run("enter", "$7", "codex new project"); !strings.Contains(action, "reload(") ||
		strings.Contains(action, "reload-sync(") {
		t.Fatalf("new did not refresh: %q", action)
	}
	if got, err := os.ReadFile(logPath); err != nil ||
		string(got) != "new\n--inline\ncodex new project\n" {
		t.Fatalf("new argv=%q err=%v", got, err)
	}

	if action := run("begin-alias", "$7"); !strings.Contains(action, "INLINE RENAME") {
		t.Fatal(action)
	}
	if action := run("escape"); !strings.Contains(action, "disable-search") ||
		!strings.Contains(action, "rebind(/)") || !strings.Contains(action, "hide-input") ||
		!strings.Contains(action, "clear-query") ||
		!strings.Contains(action, "LIVE SESSIONS") {
		t.Fatalf("escape did not cancel only inline mode: %q", action)
	}
	if action := run("escape"); !strings.Contains(action, "disable-search") ||
		strings.Contains(action, "abort") {
		t.Fatalf("normal escape must keep the selector open: %q", action)
	}

	untrustedQuery := `api)+abort+change-prompt(injected)`
	if action := run("change", untrustedQuery); !strings.Contains(action, "reload(") ||
		strings.Contains(action, untrustedQuery) {
		t.Fatalf("normal search query was embedded in an fzf action: %q", action)
	}
	_ = run("search-state")
	if got, err := os.ReadFile(logPath); err != nil ||
		!strings.Contains(string(got), "--selector-query\n"+untrustedQuery+"\n") {
		t.Fatalf("saved query argv=%q err=%v", got, err)
	}
}

func TestTabsPreserveOpenOrderAndRenderAsStyledBlocks(t *testing.T) {
	sessions := []model.Session{
		{ID: "$9", Name: "third", CreatedAt: 30},
		{ID: "$2", Name: "first", CreatedAt: 10},
		{ID: "$4", Name: "second#[bad]", CreatedAt: 20},
	}
	got := FormatTabs(sessions, "$4", 9)
	first := strings.Index(got, " 1 third")
	second := strings.Index(got, " 2 first")
	third := strings.Index(got, " 3 second[bad]")
	if first < 0 || second <= first || third <= second ||
		strings.Contains(got, "second#[bad]") ||
		strings.Count(got, "#[range=user|tab") != 3 ||
		!strings.Contains(got, "#[bold,fg=#100f0f,bg=#4385be] 3 ") {
		t.Fatalf("tabs=%q", got)
	}
	if got := FormatTabs(nil, "", 9); got != "" {
		t.Fatalf("empty tabs=%q", got)
	}
}

func TestSelectorHeaderShowsLauncherLocalTabsOnTheStableTopRow(t *testing.T) {
	sessions := []model.Session{
		{ID: "$1", Name: "alpha"},
		{ID: "$2", Name: "beta"},
	}
	header := formatSelectorHeaderWithTabs(
		sessions, []model.Session{sessions[1], sessions[0]}, "$2",
		false, 140, false,
	)
	lines := strings.Split(header, "\n")
	if len(lines) != 4 {
		t.Fatalf("header lines=%d: %q", len(lines), header)
	}
	if !strings.Contains(lines[0], "HMUX  /  SESSION CONTROL") ||
		!strings.Contains(lines[0], "TABS") ||
		!strings.Contains(lines[0], "1 beta") ||
		!strings.Contains(lines[0], "2 alpha") {
		t.Fatalf("launcher tabs are not on the fixed top row: %q", lines[0])
	}
	if !strings.Contains(lines[3], "SESSION") {
		t.Fatalf("column header moved from its stable row: %q", header)
	}
}

func TestDesktopWorkspaceColumnUsesRemainingWidth(t *testing.T) {
	session := model.Session{
		Name: "workspace-width", Runtime: "codex", Model: "gpt-5.6-sol",
		State: "working", Process: "codex",
		CurrentPath: "/Users/example/Dropbox/dev/very-long-workspace/service/backend",
	}
	for _, width := range []int{70, 80, 100, 160} {
		row := FormatRow(session, false, width)
		header := formatColumnHeader(false, width)
		if !strings.Contains(header, "WORKSPACE") {
			t.Fatalf("width=%d omitted WORKSPACE: %q", width, header)
		}
		if got := displayWidth(row); got > width {
			t.Fatalf("width=%d row width=%d: %q", width, got, row)
		}
		if got := displayWidth(header); got > width {
			t.Fatalf("width=%d header width=%d: %q", width, got, header)
		}
	}
	wide := FormatRow(session, false, 160)
	if !strings.Contains(wide, "very-long-workspace") || !strings.Contains(wide, "service/backend") {
		t.Fatalf("wide WORKSPACE lost useful path context: %q", wide)
	}
	mobile := formatColumnHeader(true, 100)
	if strings.Contains(mobile, "WORKSPACE") {
		t.Fatalf("mobile layout unexpectedly expanded: %q", mobile)
	}
}

func TestReloadRowsUseTheFZFScreenWidth(t *testing.T) {
	session := model.Session{
		ID: "$7", Name: "width-stable", Runtime: "codex", Model: "gpt-5.6-sol",
		State: "idle", Process: "codex", CurrentPath: "/work/aif/backend",
	}
	line := SelectorLinesAtWidth([]model.Session{session}, false, 80)
	fields := strings.SplitN(strings.TrimSuffix(line, "\n"), "\t", 2)
	if len(fields) != 2 || !strings.Contains(fields[1], "ackend") {
		t.Fatalf("80-column reload row is malformed: %q", line)
	}
	plain := regexp.MustCompile(`\x1b\[[0-9;]*m`).ReplaceAllString(fields[1], "")
	if got := displayWidth(plain); got != selectorRowWidth(80, false) {
		t.Fatalf("reload width=%d want=%d row=%q", got, selectorRowWidth(80, false), plain)
	}
}

func TestWorkflowSummaryAppearsInRowsAndTabs(t *testing.T) {
	session := model.Session{
		ID: "$7", Name: "codex-main", Runtime: "codex", Process: "codex",
		Workflow: &model.WorkflowSummary{Running: 2, Completed: 1, WaitingApproval: 1},
	}
	line := selectorLine(session, false, 120)
	if !strings.Contains(line, "2▶ 1✓ 1!") {
		t.Fatalf("selector row=%q", line)
	}
	tab := FormatTab(session, 1, session.ID)
	if !strings.Contains(tab, "2▶") {
		t.Fatalf("tab=%q", tab)
	}
}

func TestSelectableSessionsExcludesHiddenWithoutMutatingInput(t *testing.T) {
	sessions := []model.Session{
		{ID: "$1", Name: "visible-a"},
		{ID: "$2", Name: "hidden", Hidden: true},
		{ID: "$3", Name: "visible-b"},
	}
	visible := SelectableSessions(sessions)
	if len(visible) != 2 || visible[0].ID != "$1" || visible[1].ID != "$3" {
		t.Fatalf("visible=%#v", visible)
	}
	visible[0].Name = "changed"
	if sessions[0].Name != "visible-a" || !sessions[1].Hidden {
		t.Fatalf("input was mutated: %#v", sessions)
	}
}
