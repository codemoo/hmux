package control

import (
	"bufio"
	"context"
	"fmt"
	"net"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
)

// InitFromSSH creates a private runtime inventory without printing sensitive
// endpoint values. The source alias must already be trusted in OpenSSH config.
func InitFromSSH(ctx context.Context, sourceAlias, inventoryPath, clientPath, homeIdentity string) error {
	if sourceAlias == "" || strings.ContainsAny(sourceAlias, " \t\r\n;&|`$(){}[]<>") {
		return fmt.Errorf("invalid source SSH alias")
	}
	effective, err := safeexec.Output(exec.CommandContext(ctx, "ssh", "-G", sourceAlias), 1024*1024)
	if err != nil {
		return fmt.Errorf("resolve source SSH alias: %w", err)
	}
	values := parseEffectiveSSH(effective)
	for _, key := range []string{"hostname", "user", "port", "identityfile"} {
		if values[key] == "" {
			return fmt.Errorf("effective SSH config lacks %s", key)
		}
	}
	port, err := strconv.Atoi(values["port"])
	if err != nil {
		return fmt.Errorf("invalid effective SSH port")
	}
	homeAddress, err := reachablePrivateAddress(ctx, sourceAlias)
	if err != nil {
		return err
	}
	current, err := user.Current()
	if err != nil {
		return err
	}
	dmzIdentity := homeRelative(values["identityfile"], current.HomeDir)
	homeIdentity = homeRelative(homeIdentity, current.HomeDir)
	inventory := model.Inventory{
		SchemaVersion: model.SchemaVersion,
		Revision:      "initial",
		Clients: []model.Client{
			{ID: "home-mac", Role: "home"},
			{ID: "office-mac", Role: "remote"},
			{ID: "macbook", Role: "remote"},
		},
		IdentityRefs: []model.IdentityRef{
			{ID: "dmz-client-key", Path: dmzIdentity},
			{ID: "home-client-key", Path: homeIdentity},
		},
		Hosts: []model.Host{
			{
				ID: "dmz", SSHAlias: "hmux-dmz", Address: values["hostname"],
				User: values["user"], Port: port, IdentityRef: "dmz-client-key",
				Tags: []string{"gateway", "dmz"},
			},
			{
				ID: "home", SSHAlias: "hmux-home", Address: homeAddress,
				User: current.Username, Port: 22, ProxyJump: "dmz", IdentityRef: "home-client-key",
				Tags: []string{"home", "tmux-host"},
			},
		},
		Profiles: []model.Profile{
			{ID: "codex", Label: "Codex", DefaultDirectory: "~/Dropbox/dev", Command: []string{"codex"}, Tags: []string{"ai", "codex"}},
			{ID: "claude", Label: "Claude Code", DefaultDirectory: "~/Dropbox/dev", Command: []string{"claude"}, Tags: []string{"ai", "claude"}},
			{ID: "shell", Label: "Shell", DefaultDirectory: "~", Command: []string{"zsh", "-l"}, Tags: []string{"shell"}},
		},
	}
	if err := config.SaveInventory(inventoryPath, inventory); err != nil {
		return err
	}
	if clientPath != "" {
		cfg := config.DefaultClientConfig()
		cfg.ClientID = "home-mac"
		cfg.Role = "home"
		cfg.InventoryPath = inventoryPath
		cfg.UpdateCheck = true
		if err := config.SaveClient(clientPath, cfg); err != nil {
			return err
		}
	}
	return nil
}

func parseEffectiveSSH(data []byte) map[string]string {
	result := map[string]string{}
	scanner := bufio.NewScanner(strings.NewReader(string(data)))
	for scanner.Scan() {
		fields := strings.Fields(scanner.Text())
		if len(fields) < 2 {
			continue
		}
		switch fields[0] {
		case "hostname", "user", "port":
			if result[fields[0]] == "" {
				result[fields[0]] = fields[1]
			}
		case "identityfile":
			if result[fields[0]] == "" {
				result[fields[0]] = fields[1]
			}
		}
	}
	return result
}

func reachablePrivateAddress(ctx context.Context, alias string) (string, error) {
	interfaces, err := net.Interfaces()
	if err != nil {
		return "", err
	}
	for _, iface := range interfaces {
		addresses, err := iface.Addrs()
		if err != nil {
			continue
		}
		for _, address := range addresses {
			ip, _, err := net.ParseCIDR(address.String())
			if err != nil || ip == nil || ip.To4() == nil || !ip.IsPrivate() || ip.IsLoopback() {
				continue
			}
			value := ip.String()
			if strings.ContainsAny(value, " \t\r\n;&|`$(){}[]<>") {
				continue
			}
			probe := exec.CommandContext(ctx, "ssh", "-o", "BatchMode=yes", alias, "--", "nc", "-z", "-w", "3", value, "22")
			if probe.Run() == nil {
				return value, nil
			}
		}
	}
	return "", fmt.Errorf("no private HOME_MAC address is reachable from the selected DMZ alias")
}

func homeRelative(path, home string) string {
	path = filepath.Clean(path)
	if path == home {
		return "~"
	}
	if strings.HasPrefix(path, home+string(os.PathSeparator)) {
		return "~/" + strings.TrimPrefix(path, home+string(os.PathSeparator))
	}
	return path
}
