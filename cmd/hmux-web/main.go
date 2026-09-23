package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"github.com/codemoo/hmux/internal/homeservice"
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) == 0 {
		return errors.New("usage: hmux-web <init|serve|connect|service>")
	}
	if args[0] == "service" {
		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
		defer stop()
		return homeservice.Run(ctx, args[1:], os.Stdout)
	}
	fs := flag.NewFlagSet(args[0], flag.ContinueOnError)
	credentials := fs.String("credentials", "", "private credentials file")
	tokenPath := fs.String("token-file", "", "private Home connector token file")
	origin := fs.String("origin", "", "public HTTPS origin")
	listen := fs.String("listen", "127.0.0.1:8088", "loopback listener")
	assets := fs.String("assets", "web/dist", "built web assets")
	endpoint := fs.String("url", "", "wss://public-host/connect")
	configPath := fs.String("config", "", "Home host config")
	logPath := fs.String("log-file", "", "private bounded Home lifecycle log (service mode)")
	if err := fs.Parse(args[1:]); err != nil {
		return err
	}
	if fs.NArg() != 0 {
		return errors.New("unexpected arguments")
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	opts := webOptions{credentials: *credentials, tokenPath: *tokenPath, origin: *origin, listen: *listen, assets: *assets, endpoint: *endpoint, configPath: *configPath, logPath: *logPath}
	switch args[0] {
	case "init":
		return initializeCredentials(opts)
	case "serve":
		return serveGateway(ctx, opts)
	case "connect":
		return connectHome(ctx, opts)
	default:
		return errors.New("unknown command")
	}
}

type webOptions struct {
	credentials string
	tokenPath   string
	origin      string
	listen      string
	assets      string
	endpoint    string
	configPath  string
	logPath     string
}
