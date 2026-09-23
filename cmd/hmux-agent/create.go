package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/config"
)

func parseCreate(args []string) (profile, name, inventory string, dryRun, nameStdin, jsonOutput bool, err error) {
	inventory = defaultInventoryPath()
	nameSet := false
	for index := 0; index < len(args); index++ {
		switch args[index] {
		case "--name":
			if nameSet || nameStdin {
				return "", "", "", false, false, false, errors.New("--name and --name-stdin are mutually exclusive")
			}
			if index+1 >= len(args) {
				return "", "", "", false, false, false, errors.New("--name requires a value")
			}
			name = args[index+1]
			nameSet = true
			index++
		case "--name-stdin":
			if nameSet || nameStdin {
				return "", "", "", false, false, false, errors.New("--name and --name-stdin are mutually exclusive")
			}
			nameStdin = true
		case "--inventory":
			if index+1 >= len(args) {
				return "", "", "", false, false, false, errors.New("--inventory requires a value")
			}
			inventory = args[index+1]
			index++
		case "--dry-run":
			dryRun = true
		case "--json":
			jsonOutput = true
		default:
			if profile != "" {
				return "", "", "", false, false, false, errors.New("too many create arguments")
			}
			profile = args[index]
		}
	}
	if dryRun && jsonOutput {
		return "", "", "", false, false, false, errors.New("--json cannot be combined with --dry-run")
	}
	return profile, name, inventory, dryRun, nameStdin, jsonOutput, nil
}

func defaultInventoryPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".config", "hmux", "inventory.toml")
}

func runCreate(ctx context.Context, args []string) error {
	profile, name, inventoryPath, dryRun, nameStdin, jsonOutput, err := parseCreate(args)
	if err != nil {
		return err
	}
	if nameStdin {
		name, err = readBoundedSingleLine(os.Stdin, 512)
		if err != nil {
			return fmt.Errorf("read session name: %w", err)
		}
	}
	if profile == "" {
		return errors.New("usage: hmux-agent create <profile-id> [--name name|--name-stdin] [--json]")
	}
	inventory, err := config.LoadInventory(inventoryPath)
	if err != nil {
		return err
	}
	if dryRun {
		planned, err := agent.ValidateCreate(inventory, profile, name)
		if err != nil {
			return err
		}
		fmt.Println(planned)
		return nil
	}
	cfg, err := config.LoadHome("")
	if err != nil {
		return err
	}
	if jsonOutput {
		result, err := agent.CreateSession(ctx, inventory, profile, name, cfg.StateDir)
		if err != nil {
			return err
		}
		return json.NewEncoder(os.Stdout).Encode(result)
	}
	id, err := agent.Create(ctx, inventory, profile, name, cfg.StateDir)
	if err != nil {
		return err
	}
	fmt.Println(id)
	return nil
}
