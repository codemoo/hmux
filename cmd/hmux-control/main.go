package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"github.com/codemoo/hmux/internal/control"
	"github.com/codemoo/hmux/internal/model"
)

var version = "dev"

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "hmux-control:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	root := defaultRoot()
	if len(args) >= 2 && args[0] == "--root" {
		root = args[1]
		args = args[2:]
	}
	if len(args) == 0 {
		return usage()
	}
	store := control.Store{Root: filepath.Clean(root)}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	switch args[0] {
	case "validate":
		inventory, err := store.Validate()
		if err != nil {
			return err
		}
		fmt.Printf("valid schema=%d revision=%s hosts=%d profiles=%d\n", inventory.SchemaVersion, inventory.Revision, len(inventory.Hosts), len(inventory.Profiles))
		return nil
	case "reconcile":
		if err := store.Reconcile(ctx); err != nil {
			return err
		}
		fmt.Println("reconcile complete")
		return nil
	case "rendered":
		if len(args) != 2 {
			return errors.New("usage: hmux-control rendered <ssh|termius|inventory>")
		}
		return store.Rendered(args[1], os.Stdout)
	case "manifest":
		flags := flag.NewFlagSet("manifest", flag.ContinueOnError)
		platform := flags.String("platform", "", "target platform")
		releaseVersion := flags.String("version", "", "release version")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		manifest, err := store.Manifest(*platform, *releaseVersion)
		if err != nil {
			return err
		}
		return printJSON(manifest)
	case "artifact":
		flags := flag.NewFlagSet("artifact", flag.ContinueOnError)
		platform := flags.String("platform", "", "target platform")
		releaseVersion := flags.String("version", "", "release version")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		return store.Artifact(*platform, *releaseVersion, os.Stdout)
	case "publish":
		flags := flag.NewFlagSet("publish", flag.ContinueOnError)
		releaseVersion := flags.String("version", "", "release version")
		signingKey := flags.String("signing-key", "", "Ed25519 signing private key")
		var artifacts control.ArtifactFlag
		flags.Var(&artifacts, "artifact", "platform=path (repeatable)")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if *signingKey == "" {
			return errors.New("publish requires --signing-key; unsigned production releases are forbidden")
		}
		return store.Publish(*releaseVersion, artifacts, *signingKey)
	case "rollback":
		if len(args) != 2 {
			return errors.New("usage: hmux-control rollback <version>")
		}
		return store.Rollback(args[1])
	case "health":
		result := store.Health(ctx)
		if err := printJSON(result); err != nil {
			return err
		}
		if ok, _ := result["ok"].(bool); !ok {
			return errors.New("health check failed")
		}
		return nil
	case "host":
		return host(store, args[1:])
	case "identity":
		if len(args) != 4 || args[1] != "set-path" {
			return errors.New("usage: hmux-control identity set-path <stable-id> <~/.ssh/path>")
		}
		return store.IdentitySetPath(args[2], args[3])
	case "client-key":
		if len(args) != 2 || args[1] != "authorize" {
			return errors.New("usage: hmux-control client-key authorize")
		}
		if err := store.AuthorizeStagedClientKey(); err != nil {
			return err
		}
		fmt.Println("staged client public key authorized")
		return nil
	case "keygen":
		flags := flag.NewFlagSet("keygen", flag.ContinueOnError)
		privatePath := flags.String("private", "", "private key destination")
		publicPath := flags.String("public", "", "public key destination")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if *privatePath == "" || *publicPath == "" {
			return errors.New("keygen requires --private and --public")
		}
		return control.GenerateKeyPair(*privatePath, *publicPath)
	case "init-from-ssh":
		flags := flag.NewFlagSet("init-from-ssh", flag.ContinueOnError)
		sourceAlias := flags.String("source-alias", "", "existing trusted DMZ SSH alias")
		inventoryPath := flags.String("inventory", "", "private inventory destination")
		clientPath := flags.String("client", "", "home client config destination")
		homeIdentity := flags.String("home-identity", "~/.ssh/hmux_home_ed25519", "home authentication identity reference")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if *sourceAlias == "" || *inventoryPath == "" {
			return errors.New("init-from-ssh requires --source-alias and --inventory")
		}
		if err := control.InitFromSSH(ctx, *sourceAlias, *inventoryPath, *clientPath, *homeIdentity); err != nil {
			return err
		}
		fmt.Println("private inventory initialized from trusted SSH config (sensitive values not printed)")
		return nil
	case "version":
		fmt.Printf("hmux-control %s schema=%d\n", version, model.SchemaVersion)
		return nil
	default:
		return usage()
	}
}

func host(store control.Store, args []string) error {
	if len(args) == 0 {
		return errors.New("usage: hmux-control host <list|add|edit|diff>")
	}
	switch args[0] {
	case "list":
		if len(args) > 2 || (len(args) == 2 && args[1] != "--json") {
			return errors.New("usage: hmux-control host list [--json]")
		}
		hosts, err := store.HostList()
		if err != nil {
			return err
		}
		return printJSON(hosts)
	case "diff":
		if len(args) > 2 || (len(args) == 2 && args[1] != "--json") {
			return errors.New("usage: hmux-control host diff [--json]")
		}
		diff, err := store.HostDiff()
		if err != nil {
			return err
		}
		return printJSON(diff)
	case "add", "edit":
		dryRun := false
		for _, arg := range args[1:] {
			switch arg {
			case "--dry-run":
				dryRun = true
			case "--json":
			default:
				return errors.New("usage: hmux-control host <add|edit> [--dry-run] [--json]")
			}
		}
		var input struct {
			Host model.Host `json:"host"`
		}
		decoder := json.NewDecoder(os.Stdin)
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&input); err != nil {
			return fmt.Errorf("decode host JSON from stdin: %w", err)
		}
		var trailing any
		if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
			if err == nil {
				return errors.New("decode host JSON from stdin: trailing JSON value")
			}
			return fmt.Errorf("decode host JSON from stdin: %w", err)
		}
		if err := store.HostUpsert(input.Host, args[0] == "edit", dryRun); err != nil {
			return err
		}
		return printJSON(map[string]any{"ok": true, "operation": args[0], "dry_run": dryRun, "host_id": input.Host.ID})
	default:
		return errors.New("usage: hmux-control host <list|add|edit|diff>")
	}
}

func defaultRoot() string {
	if value := os.Getenv("HMUX_CONTROL_ROOT"); value != "" {
		return value
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".local", "share", "hmux-control")
}

func printJSON(value any) error {
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(value)
}

func usage() error {
	return errors.New("usage: hmux-control [--root path] <validate|reconcile|rendered|manifest|artifact|publish|rollback|health|host|identity|client-key|keygen|init-from-ssh|version>")
}
