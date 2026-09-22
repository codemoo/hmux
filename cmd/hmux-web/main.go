package main

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/webgateway"
	"golang.org/x/term"
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
func run(args []string) error {
	if len(args) == 0 {
		return errors.New("usage: hmux-web <init|serve|connect>")
	}
	fs := flag.NewFlagSet(args[0], flag.ContinueOnError)
	credentials := fs.String("credentials", "", "private credentials file")
	tokenPath := fs.String("token-file", "", "private Home connector token file")
	origin := fs.String("origin", "", "public HTTPS origin")
	listen := fs.String("listen", "127.0.0.1:8088", "loopback listener")
	assets := fs.String("assets", "web/dist", "built web assets")
	endpoint := fs.String("url", "", "wss://public-host/connect")
	configPath := fs.String("config", "", "Home host config")
	if err := fs.Parse(args[1:]); err != nil {
		return err
	}
	if fs.NArg() != 0 {
		return errors.New("unexpected arguments")
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	switch args[0] {
	case "init":
		if *credentials == "" || *tokenPath == "" {
			return errors.New("--credentials and --token-file required")
		}
		for _, path := range []string{*credentials, *tokenPath} {
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
		if err = webgateway.WriteCredentials(*credentials, c); err != nil {
			return err
		}
		if err = config.AtomicWrite(*tokenPath, []byte(webgateway.RandomToken()+"\n"), 0600); err != nil {
			return err
		}
		fmt.Println("Created private credentials and connector token. Never commit or share these files.")
		return nil
	case "serve":
		if err := webgateway.LoopbackAddress(*listen); err != nil {
			return err
		}
		if info, err := os.Stat(*assets + "/index.html"); err != nil || !info.Mode().IsRegular() {
			return errors.New("built web assets missing; run npm ci && npm run build in web/")
		}
		handler, err := webgateway.NewServer(*origin, *credentials, *tokenPath, os.DirFS(*assets))
		if err != nil {
			return err
		}
		defer handler.Close()
		srv := &http.Server{Addr: *listen, Handler: handler, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 15 * time.Second, IdleTimeout: 60 * time.Second, MaxHeaderBytes: 8192}
		go func() {
			<-ctx.Done()
			bounded, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			_ = srv.Shutdown(bounded)
		}()
		fmt.Println("HMux web listening on", *listen, "behind HTTPS")
		err = srv.ListenAndServe()
		if errors.Is(err, http.ErrServerClosed) {
			return nil
		}
		return err
	case "connect":
		token, err := webgateway.LoadToken(*tokenPath)
		if err != nil {
			return err
		}
		cfg, err := config.LoadHome(*configPath)
		if err != nil {
			return err
		}
		fmt.Println("Home connector running; Ctrl-C disconnects web access without ending tmux work.")
		err = webgateway.ConnectHome(ctx, *endpoint, token, cfg)
		if errors.Is(err, context.Canceled) {
			return nil
		}
		return err
	default:
		return errors.New("unknown command")
	}
}
