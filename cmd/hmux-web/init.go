package main

import (
	"bufio"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/webgateway"
	"golang.org/x/term"
)

func initializeCredentials(opts webOptions) error {
	if opts.credentials == "" || opts.tokenPath == "" {
		return errors.New("--credentials and --token-file required")
	}
	for _, path := range []string{opts.credentials, opts.tokenPath} {
		if _, err := os.Lstat(path); !errors.Is(err, os.ErrNotExist) {
			return errors.New("refusing to overwrite existing secret files")
		}
	}
	if !term.IsTerminal(int(os.Stdin.Fd())) {
		return errors.New("init requires an interactive terminal; secrets are not accepted in argv")
	}
	reader := bufio.NewReader(os.Stdin)
	fmt.Print("Username: ")
	name, _ := reader.ReadString('\n')
	fmt.Print("Password (at least 8 bytes): ")
	password, err := term.ReadPassword(int(os.Stdin.Fd()))
	fmt.Println()
	if err != nil {
		return err
	}
	fmt.Print("Confirm password: ")
	confirm, err := term.ReadPassword(int(os.Stdin.Fd()))
	fmt.Println()
	if err != nil || string(confirm) != string(password) {
		return errors.New("passwords do not match")
	}
	c, err := webgateway.NewCredentials(strings.TrimSpace(name), string(password))
	if err != nil {
		return err
	}
	fmt.Println("Add this secret to Google Authenticator (time-based). Keep it private:")
	fmt.Println(c.TOTPSecret)
	fmt.Println("Enrollment URI:", c.EnrollmentURI())
	fmt.Print("Current 6-digit code: ")
	code, _ := reader.ReadString('\n')
	step := c.MatchCode(strings.TrimSpace(code), time.Now())
	if step < 0 {
		return errors.New("authenticator code did not match")
	}
	c.LastStep = step
	if err = webgateway.WriteCredentials(opts.credentials, c); err != nil {
		return err
	}
	if err = config.AtomicWrite(opts.tokenPath, []byte(webgateway.RandomToken()+"\n"), 0600); err != nil {
		return err
	}
	fmt.Println("Created private credentials and connector token. Never commit or share these files.")
	return nil
}
