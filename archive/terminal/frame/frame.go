package frame

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/tabstate"
)

const (
	envClient      = "HMUX_FRAME_CLIENT"
	envLauncher    = "HMUX_FRAME_LAUNCHER"
	envSession     = "HMUX_FRAME_SESSION"
	envStateDir    = "HMUX_FRAME_STATE_DIR"
	envStatusFile  = "HMUX_FRAME_STATUS_FILE"
	envUIConfig    = "HMUX_FRAME_UI_CONFIG"
	envOuterTMUX   = "HMUX_FRAME_OUTER_TMUX"
	envOuterClient = "HMUX_FRAME_OUTER_CLIENT"

	frontendStatusPoll = 5 * time.Millisecond
	frontendExitGrace  = 100 * time.Millisecond
)

var ErrLauncherExit = errors.New("hmux launcher exit requested")

type Options struct {
	StateDir     string
	ConfigPath   string
	UIConfigPath string
	LauncherID   string
	SessionID    string
	Client       bool
}

func DefaultConfigPaths() (string, string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", "", err
	}
	configDir := filepath.Join(home, ".config", "hmux")
	return filepath.Join(configDir, "frame.tmux.conf"),
		filepath.Join(configDir, "frame-ui.tmux.conf"), nil
}

// ExecAttach runs a disposable outer tmux UI server as the current launcher
// child's foreground process. Its fixed top status is independent from the
// nested body server that owns the resizable LIVE SESSION frame. Neither
// server changes the target tmux.
func ExecAttach(options Options) error {
	if err := validateOptions(options); err != nil {
		return err
	}
	tmuxPath, err := catalog.TmuxPath()
	if err != nil {
		return err
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	if err := validateExecutable(self); err != nil {
		return err
	}
	framesDir := filepath.Join(options.StateDir, "frames")
	if err := ensurePrivateDir(framesDir); err != nil {
		return err
	}
	statusFile := filepath.Join(
		framesDir,
		fmt.Sprintf("%s-%d.status", options.LauncherID, os.Getpid()),
	)
	_ = os.Remove(statusFile)
	_ = os.Remove(failurePath(statusFile))
	defer os.Remove(statusFile)              //nolint:errcheck
	defer os.Remove(failurePath(statusFile)) //nolint:errcheck
	environment := frameCommandEnvironment(os.Environ(), map[string]string{
		envClient:      boolText(options.Client),
		envLauncher:    options.LauncherID,
		envSession:     options.SessionID,
		envStateDir:    options.StateDir,
		envStatusFile:  statusFile,
		envUIConfig:    options.UIConfigPath,
		envOuterTMUX:   "",
		envOuterClient: "",
	})
	socketName := fmt.Sprintf("hmux-frame-%s-%d", options.LauncherID[:12], os.Getpid())
	sessionName := fmt.Sprintf("hmux-frame-%s", options.LauncherID[:12])
	hostCommand := shellQuote(self)
	if options.Client {
		hostCommand += " --no-update-check"
	}
	hostCommand += " frame-host"
	args := []string{
		"-L", socketName, "-f", options.ConfigPath,
		"new-session", "-s", sessionName, hostCommand,
	}
	// #nosec G204 -- tmuxPath and self are validated executable paths, the
	// socket/session names contain only fixed text, hex and digits, and the
	// one shell-command path is single-quoted without user-controlled text.
	command := exec.Command(tmuxPath, args...)
	command.Env = environment
	command.Stdin = os.Stdin
	command.Stdout = os.Stdout
	command.Stderr = os.Stderr
	runErr, stoppedForStatus := runFrontend(command, statusFile)
	switch status := readStatus(statusFile); status {
	case 0:
		if runErr != nil && !stoppedForStatus {
			return fmt.Errorf("run frame frontend: %w", runErr)
		}
		return nil
	case 130:
		return ErrLauncherExit
	default:
		if runErr != nil && !stoppedForStatus {
			return fmt.Errorf("run frame frontend: %w", runErr)
		}
		if detail, ok := readFailure(statusFile); ok {
			return fmt.Errorf("frame frontend failed: %s", detail)
		}
		return errors.New("frame frontend failed")
	}
}

// RunHost runs inside the outer disposable frame server's only pane. It starts
// a second disposable tmux server as the regular pane body. The outer status
// therefore remains physically above every body screen and stays clickable.
func RunHost() (resultErr error) {
	values, err := frameEnvironment()
	if err != nil {
		return err
	}
	defer func() {
		if resultErr != nil {
			resultErr = recordFailure(values.statusFile, resultErr)
		}
	}()
	if err := validateConfigPath(values.uiConfig); err != nil {
		return fmt.Errorf("frame UI config: %w", err)
	}
	tmuxPath, err := catalog.TmuxPath()
	if err != nil {
		return err
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	if err := validateExecutable(self); err != nil {
		return err
	}
	outerClient, _, _, err := waitForOuterClient(tmuxPath)
	if err != nil {
		return err
	}
	outerTMUX := os.Getenv("TMUX")
	if err := validateTMUXRouting(outerTMUX); err != nil {
		return err
	}
	socketName := fmt.Sprintf(
		"hmux-frame-ui-%s-%d", values.launcherID[:12], os.Getpid(),
	)
	sessionName := fmt.Sprintf("hmux-frame-ui-%s", values.launcherID[:12])
	hostCommand := shellQuote(self)
	if values.client {
		hostCommand += " --no-update-check"
	}
	hostCommand += " frame-ui"
	command := exec.Command(
		tmuxPath,
		"-L", socketName,
		"-f", values.uiConfig,
		"new-session",
		"-s", sessionName,
		hostCommand,
	)
	command.Env = frameCommandEnvironment(os.Environ(), map[string]string{
		envOuterTMUX: outerTMUX, envOuterClient: outerClient,
	})
	command.Stdin = os.Stdin
	command.Stdout = os.Stdout
	command.Stderr = os.Stderr
	if err := command.Run(); err != nil {
		return fmt.Errorf("run frame body: %w", err)
	}
	// RunInner records the final status before the nested body exits.
	// Keep the outer pane alive just long enough for ExecAttach to close the
	// disposable tmux client itself. Otherwise tmux can paint a transient
	// "[exited]" pane between this frame and the selector.
	if _, ready := readReadyStatus(values.statusFile); ready {
		time.Sleep(frontendExitGrace)
	}
	// This is the disposable server's only pane. Returning lets tmux close the
	// now-clientless outer session naturally. The parent launcher reads
	// statusFile directly, so no detach command or shell-encoded exit status is
	// needed.
	return nil
}

// RunUI runs inside the nested disposable body server. Three inert spacer
// panes and the body status form the LIVE SESSION frame around the original
// pane. Unlike tmux display-popup, this layout follows every client resize, so
// the frame-inner PTY and the real target client receive SIGWINCH naturally.
func RunUI() (resultErr error) {
	values, err := frameEnvironment()
	if err != nil {
		return err
	}
	defer func() {
		if resultErr != nil {
			resultErr = recordFailure(values.statusFile, resultErr)
		}
	}()
	if err := validateOuterEnvironment(values); err != nil {
		return err
	}
	if err := validateBodyTMUXRouting(os.Getenv("TMUX"), values.launcherID); err != nil {
		return err
	}
	tmuxPath, err := catalog.TmuxPath()
	if err != nil {
		return err
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	if err := validateExecutable(self); err != nil {
		return err
	}
	if _, _, _, err := waitForOuterClient(tmuxPath); err != nil {
		return err
	}
	spacerPanes, err := createFrameLayout(tmuxPath, self, values.client)
	if err != nil {
		return err
	}
	defer removeFrameLayout(tmuxPath, spacerPanes)
	if err := RunInner(values.stateDir); err != nil {
		return err
	}
	if _, ready := readReadyStatus(values.statusFile); ready {
		time.Sleep(frontendExitGrace)
	}
	return nil
}

// RunSpacer keeps an inert pane alive until the disposable body server closes
// it. It does not read input, start a shell or interact with the target server.
func RunSpacer() error {
	stopped := make(chan os.Signal, 1)
	signal.Notify(stopped, syscall.SIGHUP, syscall.SIGINT, syscall.SIGTERM)
	defer signal.Stop(stopped)
	<-stopped
	return nil
}

func createFrameLayout(tmuxPath, self string, client bool) ([]string, error) {
	targetPane := os.Getenv("TMUX_PANE")
	if !validPaneID(targetPane) {
		return nil, errors.New("invalid disposable body pane")
	}
	if err := configureFrameLayout(tmuxPath); err != nil {
		return nil, err
	}
	spacerCommand := shellQuote(self)
	if client {
		spacerCommand += " --no-update-check"
	}
	spacerCommand += " frame-spacer"
	type split struct {
		direction string
		before    bool
	}
	splits := []split{
		{direction: "-v"},
		{direction: "-h", before: true},
		{direction: "-h"},
	}
	spacers := make([]string, 0, len(splits))
	for _, split := range splits {
		args := []string{
			"split-window", "-d", "-P", "-F", "#{pane_id}",
			split.direction, "-l", "1", "-t", targetPane,
		}
		if split.before {
			args = append(args, "-b")
		}
		args = append(args, spacerCommand)
		output, err := exec.Command(tmuxPath, args...).Output()
		if err != nil {
			removeFrameLayout(tmuxPath, spacers)
			return nil, fmt.Errorf("create disposable frame spacer: %w", err)
		}
		paneID := strings.TrimSpace(string(output))
		if !validPaneID(paneID) {
			removeFrameLayout(tmuxPath, spacers)
			return nil, errors.New("tmux returned an invalid frame spacer pane")
		}
		spacers = append(spacers, paneID)
	}
	if err := exec.Command(
		tmuxPath, "select-pane", "-t", targetPane, "-T", "LIVE SESSION",
	).Run(); err != nil {
		removeFrameLayout(tmuxPath, spacers)
		return nil, fmt.Errorf("select disposable target pane: %w", err)
	}
	return spacers, nil
}

func configureFrameLayout(tmuxPath string) error {
	settings := [][2]string{
		{"pane-border-lines", "single"},
		{"pane-border-status", "top"},
		{"pane-border-style", "fg=#4385be,bg=#100f0f"},
		{"pane-active-border-style", "fg=#4385be,bg=#100f0f"},
		{"pane-border-format", "#{?pane_active,#[align=centre]#[bold]#[fg=#cecdc3] LIVE SESSION #[nobold],}"},
		{"status", "on"},
		{"status-position", "bottom"},
		{"status-interval", "2"},
		{"status-style", "bg=#100f0f,fg=#878580"},
		{"status-format[0]", " ⌘` / ⌘L sessions   ⌘R alias   ⌘W close tab   ⌘1–9 switch   ⌘Q exit "},
	}
	args := make([]string, 0, len(settings)*6)
	for index, setting := range settings {
		if index > 0 {
			args = append(args, ";")
		}
		args = append(args, "set-option", "-g", setting[0], setting[1])
	}
	if err := exec.Command(tmuxPath, args...).Run(); err != nil {
		return fmt.Errorf("configure disposable frame layout: %w", err)
	}
	return nil
}

func removeFrameLayout(tmuxPath string, paneIDs []string) {
	for _, paneID := range paneIDs {
		if validPaneID(paneID) {
			_ = exec.Command(tmuxPath, "kill-pane", "-t", paneID).Run()
		}
	}
}

func validPaneID(value string) bool {
	if len(value) < 2 || len(value) > 32 || value[0] != '%' {
		return false
	}
	for _, character := range value[1:] {
		if character < '0' || character > '9' {
			return false
		}
	}
	return true
}

// RunInner runs in the framed body pane and proxies a child PTY containing the real
// tmux client. The proxy consumes only hmux's private Ghostty sequences; every
// ordinary byte, including Ctrl-b, passes through unchanged.
func RunInner(stateDir string) error {
	values, err := frameEnvironment()
	if err != nil {
		return err
	}
	if filepath.Clean(stateDir) != values.stateDir {
		return errors.New("frame state directory does not match client configuration")
	}
	tmuxPath, err := catalog.TmuxPath()
	if err != nil {
		return recordFailure(values.statusFile, err)
	}
	if err := validateOuterEnvironment(values); err != nil {
		return recordFailure(values.statusFile, err)
	}
	controlName := values.outerClient
	// A visual hmux tab must never hand off or detach another tmux client.
	// Closing this disposable frontend only disconnects its own client.
	args, err := catalog.AttachArgs(values.sessionID, true, false)
	if err != nil {
		return recordFailure(values.statusFile, err)
	}
	command := exec.Command(tmuxPath, args...)
	command.Env = innerEnvironment(os.Environ())
	err, userClosed := runTargetProxy(
		command, tmuxPath, values, controlName,
	)
	status := commandStatus(err)
	if userClosed {
		status = 0
	}
	// tmux writes a detach notice after leaving the target session. Clear the
	// framed PTY before it closes so that transient line never flashes between
	// the framed session and the selector.
	_, _ = fmt.Fprint(os.Stdout, "\x1b[2J\x1b[H")
	if status == 1 && err != nil {
		if writeErr := writeFailure(values.statusFile, err); writeErr != nil {
			return writeErr
		}
	}
	if writeErr := writeStatus(values.statusFile, status); writeErr != nil {
		return writeErr
	}
	// Keep the inner disposable pane alive until ExecAttach observes the
	// status and closes the entire frontend. Returning immediately would let
	// this nested UI server paint a transient "[exited]" pane.
	time.Sleep(frontendExitGrace)
	return nil
}

type environment struct {
	client      bool
	launcherID  string
	sessionID   string
	stateDir    string
	statusFile  string
	uiConfig    string
	outerTMUX   string
	outerClient string
}

func frameEnvironment() (environment, error) {
	value := environment{
		client:      os.Getenv(envClient) == "1",
		launcherID:  os.Getenv(envLauncher),
		sessionID:   os.Getenv(envSession),
		stateDir:    filepath.Clean(os.Getenv(envStateDir)),
		statusFile:  filepath.Clean(os.Getenv(envStatusFile)),
		uiConfig:    filepath.Clean(os.Getenv(envUIConfig)),
		outerTMUX:   os.Getenv(envOuterTMUX),
		outerClient: os.Getenv(envOuterClient),
	}
	if err := tabstate.ValidateLauncherID(value.launcherID); err != nil {
		return environment{}, err
	}
	if err := model.ValidateSessionID(value.sessionID); err != nil {
		return environment{}, err
	}
	if value.stateDir == "." || !filepath.IsAbs(value.stateDir) ||
		value.statusFile == "." || !filepath.IsAbs(value.statusFile) ||
		value.uiConfig == "." || !filepath.IsAbs(value.uiConfig) {
		return environment{}, errors.New("invalid frame state paths")
	}
	framesDir := filepath.Join(value.stateDir, "frames")
	if filepath.Dir(value.statusFile) != framesDir {
		return environment{}, errors.New("frame status file is outside the private state directory")
	}
	if os.Getenv(envClient) != "0" && os.Getenv(envClient) != "1" {
		return environment{}, errors.New("invalid frame client mode")
	}
	return value, nil
}

func validateOuterEnvironment(value environment) error {
	if err := validateTMUXRouting(value.outerTMUX); err != nil {
		return err
	}
	if err := tabstate.ValidateClientName(value.outerClient); err != nil {
		return fmt.Errorf("invalid outer frame client: %w", err)
	}
	return nil
}

func validateTMUXRouting(value string) error {
	if value == "" || len(value) > 4096 {
		return errors.New("invalid outer tmux routing")
	}
	last := strings.LastIndexByte(value, ',')
	if last < 1 || last == len(value)-1 {
		return errors.New("invalid outer tmux routing")
	}
	previous := strings.LastIndexByte(value[:last], ',')
	if previous < 1 || previous == last-1 {
		return errors.New("invalid outer tmux routing")
	}
	if !filepath.IsAbs(value[:previous]) {
		return errors.New("invalid outer tmux socket path")
	}
	pid, pidErr := strconv.Atoi(value[previous+1 : last])
	index, indexErr := strconv.Atoi(value[last+1:])
	if pidErr != nil || indexErr != nil || pid < 1 || index < 0 {
		return errors.New("invalid outer tmux routing")
	}
	return nil
}

func validateBodyTMUXRouting(value, launcherID string) error {
	if err := validateTMUXRouting(value); err != nil {
		return err
	}
	if err := tabstate.ValidateLauncherID(launcherID); err != nil {
		return err
	}
	last := strings.LastIndexByte(value, ',')
	previous := strings.LastIndexByte(value[:last], ',')
	socketName := filepath.Base(value[:previous])
	prefix := "hmux-frame-ui-" + launcherID[:12] + "-"
	if !strings.HasPrefix(socketName, prefix) {
		return errors.New("invalid disposable body tmux routing")
	}
	pidText := strings.TrimPrefix(socketName, prefix)
	pid, err := strconv.Atoi(pidText)
	if err != nil || pid < 2 {
		return errors.New("invalid disposable body tmux routing")
	}
	return nil
}

func validateOptions(options Options) error {
	if err := tabstate.ValidateLauncherID(options.LauncherID); err != nil {
		return err
	}
	if err := model.ValidateSessionID(options.SessionID); err != nil {
		return err
	}
	options.StateDir = filepath.Clean(options.StateDir)
	options.ConfigPath = filepath.Clean(options.ConfigPath)
	options.UIConfigPath = filepath.Clean(options.UIConfigPath)
	if !filepath.IsAbs(options.StateDir) || options.StateDir == string(os.PathSeparator) {
		return errors.New("invalid frame state directory")
	}
	if err := validateConfigPath(options.ConfigPath); err != nil {
		return fmt.Errorf("frame config: %w", err)
	}
	if err := validateConfigPath(options.UIConfigPath); err != nil {
		return fmt.Errorf("frame UI config: %w", err)
	}
	return nil
}

func validateConfigPath(path string) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !filepath.IsAbs(path) || info.Mode()&os.ModeSymlink != 0 ||
		!info.Mode().IsRegular() ||
		info.Mode().Perm()&0o022 != 0 {
		return errors.New("config must be a private regular file")
	}
	return nil
}

func validateFrameStatusPath(stateDir, launcherID, statusFile string) error {
	if err := tabstate.ValidateLauncherID(launcherID); err != nil {
		return err
	}
	stateDir = filepath.Clean(stateDir)
	statusFile = filepath.Clean(statusFile)
	if !filepath.IsAbs(stateDir) || stateDir == string(os.PathSeparator) ||
		!filepath.IsAbs(statusFile) {
		return errors.New("invalid frame click paths")
	}
	if filepath.Dir(statusFile) != filepath.Join(stateDir, "frames") {
		return errors.New("frame click status file is outside the private state directory")
	}
	name := filepath.Base(statusFile)
	prefix := launcherID + "-"
	if !strings.HasPrefix(name, prefix) || !strings.HasSuffix(name, ".status") {
		return errors.New("frame click status file has an invalid name")
	}
	pidText := strings.TrimSuffix(strings.TrimPrefix(name, prefix), ".status")
	pid, err := strconv.Atoi(pidText)
	if err != nil || pid < 2 {
		return errors.New("frame click status file has an invalid process ID")
	}
	return nil
}

func validateExecutable(path string) error {
	info, err := os.Stat(path)
	if err != nil {
		return err
	}
	if !filepath.IsAbs(path) || !info.Mode().IsRegular() || info.Mode()&0o111 == 0 {
		return errors.New("frame executable is invalid")
	}
	return nil
}

func ensurePrivateDir(path string) error {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return err
	}
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.IsDir() || info.Mode().Perm()&0o077 != 0 {
		return errors.New("frame state directory must be private")
	}
	return nil
}

func waitForOuterClient(tmuxPath string) (string, int, int, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	ticker := time.NewTicker(20 * time.Millisecond)
	defer ticker.Stop()
	for {
		output, err := exec.CommandContext(
			ctx, tmuxPath, "list-clients",
			"-F", "#{client_name}\t#{client_width}\t#{client_height}",
		).Output()
		if err == nil {
			line := strings.TrimSpace(string(output))
			fields := strings.Split(line, "\t")
			if len(fields) == 3 && tabstate.ValidateClientName(fields[0]) == nil {
				columns, widthErr := strconv.Atoi(fields[1])
				rows, heightErr := strconv.Atoi(fields[2])
				if widthErr == nil && heightErr == nil && columns >= 20 && rows >= 8 {
					return fields[0], columns, rows, nil
				}
			}
		}
		select {
		case <-ctx.Done():
			return "", 0, 0, errors.New("frame client did not become ready")
		case <-ticker.C:
		}
	}
}

func innerEnvironment(values []string) []string {
	result := withoutTmuxEnvironment(values)
	return append(result, "HMUX_FRAMED=1")
}

func frameCommandEnvironment(values []string, replacements map[string]string) []string {
	result := make([]string, 0, len(values)+len(replacements))
	for _, value := range values {
		key := strings.SplitN(value, "=", 2)[0]
		if _, replaced := replacements[key]; replaced {
			continue
		}
		result = append(result, value)
	}
	for _, key := range []string{
		envClient, envLauncher, envSession, envStateDir, envStatusFile,
		envUIConfig, envOuterTMUX, envOuterClient,
	} {
		if replacement, exists := replacements[key]; exists {
			result = append(result, key+"="+replacement)
		}
	}
	return result
}

func withoutTmuxEnvironment(values []string) []string {
	result := make([]string, 0, len(values)+1)
	for _, value := range values {
		key := strings.SplitN(value, "=", 2)[0]
		if key == "TMUX" || key == "TMUX_PANE" {
			continue
		}
		result = append(result, value)
	}
	return result
}

func commandStatus(err error) int {
	if err == nil {
		return 0
	}
	var exit *exec.ExitError
	if errors.As(err, &exit) {
		if code := exit.ExitCode(); code == 130 {
			return 130
		}
	}
	var requested *requestedExitError
	if errors.As(err, &requested) && requested.status == 130 {
		return 130
	}
	return 1
}

type requestedExitError struct {
	status int
}

func (e *requestedExitError) Error() string {
	return fmt.Sprintf("frame frontend requested exit %d", e.status)
}

func runFrontend(command *exec.Cmd, statusFile string) (error, bool) {
	if err := command.Start(); err != nil {
		return err, false
	}
	completed := make(chan error, 1)
	go func() {
		completed <- command.Wait()
	}()
	ticker := time.NewTicker(frontendStatusPoll)
	defer ticker.Stop()
	for {
		select {
		case err := <-completed:
			return err, false
		case <-ticker.C:
			if _, ready := readReadyStatus(statusFile); !ready {
				continue
			}
			killErr := command.Process.Kill()
			runErr := <-completed
			if killErr != nil && !errors.Is(killErr, os.ErrProcessDone) {
				return fmt.Errorf("stop frame frontend: %w", killErr), false
			}
			return runErr, true
		}
	}
}

func writeStatus(path string, status int) error {
	if status != 0 && status != 1 && status != 130 {
		status = 1
	}
	return config.AtomicWrite(path, []byte(strconv.Itoa(status)+"\n"), 0o600)
}

func recordFailure(path string, cause error) error {
	if _, exists := readFailure(path); !exists {
		if err := writeFailure(path, cause); err != nil {
			return fmt.Errorf("%v; record frame failure detail: %w", cause, err)
		}
	}
	if err := writeStatus(path, 1); err != nil {
		return fmt.Errorf("%v; record frame failure: %w", cause, err)
	}
	return cause
}

func failurePath(statusFile string) string {
	return statusFile + ".error"
}

func writeFailure(statusFile string, cause error) error {
	detail := model.SafeText(cause.Error(), 500)
	if detail == "" {
		detail = "unknown frame failure"
	}
	return config.AtomicWrite(
		failurePath(statusFile), []byte(detail+"\n"), 0o600,
	)
}

func readFailure(statusFile string) (string, bool) {
	data, err := os.ReadFile(failurePath(statusFile))
	if err != nil || len(data) == 0 || len(data) > 2048 {
		return "", false
	}
	detail := model.SafeText(strings.TrimSpace(string(data)), 500)
	return detail, detail != ""
}

func readStatus(path string) int {
	status, ready := readReadyStatus(path)
	if !ready {
		return 1
	}
	return status
}

func readReadyStatus(path string) (int, bool) {
	data, err := os.ReadFile(path)
	if err != nil || len(data) > 8 {
		return 0, false
	}
	status, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil || (status != 0 && status != 1 && status != 130) {
		return 0, false
	}
	return status, true
}

func boolText(value bool) string {
	if value {
		return "1"
	}
	return "0"
}

func shellQuote(value string) string {
	return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'"
}

func truncateBytes(value string, maximum int) string {
	runes := []rune(value)
	if len(runes) <= maximum {
		return value
	}
	if maximum < 1 {
		return ""
	}
	return string(runes[:maximum])
}
