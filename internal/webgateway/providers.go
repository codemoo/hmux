package webgateway

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filelock"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/providers"
)

// providerEnv is replaced by tests so they never touch the real Home user.
var providerEnv = providers.DefaultEnv

type providerStatus struct {
	providers.Status
	// Profile reports whether the new-session menu already launches this CLI;
	// ProfileID is the profile a "start" action should create.
	Profile   bool   `json:"profile"`
	ProfileID string `json:"profile_id,omitempty"`
}

// providerResult carries user-facing failures as data: generic Home failures
// are masked by the gateway, but these messages contain no secrets.
type providerResult struct {
	Providers []providerStatus     `json:"providers,omitempty"`
	Job       *providers.JobStatus `json:"job,omitempty"`
	Error     string               `json:"error,omitempty"`
}

func providerAction(ctx context.Context, cfg config.HomeConfig, operation string, payload json.RawMessage) (any, error) {
	if cfg.Role != "home" {
		return providerResult{Error: "AI 연결 설정은 Home에서만 지원됩니다."}, nil
	}
	env, err := providerEnv()
	if err != nil {
		return nil, err
	}
	switch operation {
	case "providers":
		if len(payload) > 0 && string(payload) != "null" {
			if err := strictPayload(payload, &struct{}{}); err != nil {
				return nil, errors.New("invalid providers request")
			}
		}
		return providerResult{Providers: providerStatuses(ctx, env, cfg)}, nil
	case "provider-key":
		var q struct {
			Provider string `json:"provider"`
			Key      string `json:"key"`
		}
		if strictPayload(payload, &q) != nil {
			return nil, errors.New("invalid provider key request")
		}
		if err := providers.SetKey(ctx, env, q.Provider, q.Key); err != nil {
			return providerResult{Error: err.Error()}, nil
		}
		if q.Key != "" {
			if err := ensureProviderProfile(ctx, cfg.InventoryPath, env, q.Provider); err != nil {
				return providerResult{Providers: providerStatuses(ctx, env, cfg), Error: err.Error()}, nil
			}
		}
		return providerResult{Providers: providerStatuses(ctx, env, cfg)}, nil
	case "provider-job-start", "provider-job", "provider-job-input", "provider-job-cancel":
		var q struct {
			Provider string `json:"provider"`
			Action   string `json:"action,omitempty"`
			Text     string `json:"text,omitempty"`
		}
		if strictPayload(payload, &q) != nil {
			return nil, errors.New("invalid provider job request")
		}
		if _, ok := providers.Lookup(q.Provider); !ok {
			return nil, errors.New("unknown provider")
		}
		var err error
		switch operation {
		case "provider-job-start":
			err = providers.StartJob(ctx, env, q.Action, q.Provider)
		case "provider-job-input":
			err = providers.JobInput(ctx, env, q.Provider, q.Text)
		case "provider-job-cancel":
			_ = providers.CancelJob(ctx, env, q.Provider)
			return providerResult{Job: &providers.JobStatus{State: providers.JobNone}}, nil
		}
		if err != nil {
			return providerResult{Error: err.Error()}, nil
		}
		job, err := providers.GetJob(ctx, env, q.Provider)
		if err != nil {
			return nil, err
		}
		result := providerResult{Job: &job}
		if job.State == providers.JobConnected || job.State == providers.JobDone || job.State == providers.JobFailed {
			if job.State != providers.JobFailed {
				if err := ensureProviderProfile(ctx, cfg.InventoryPath, env, q.Provider); err != nil {
					result.Error = err.Error()
				}
			}
			result.Providers = providerStatuses(ctx, env, cfg)
		}
		return result, nil
	}
	return nil, errors.New("unknown provider operation")
}

// providerStatuses is read-only. Profiles are added only after an explicit,
// successful install/login/key action.
func providerStatuses(ctx context.Context, env providers.Env, cfg config.HomeConfig) []providerStatus {
	statuses := providers.Statuses(ctx, env)
	registered := registeredProfiles(cfg.InventoryPath)
	var out []providerStatus
	for _, s := range statuses {
		p, _ := providers.Lookup(s.ID)
		id, ok := registered[p.Command]
		out = append(out, providerStatus{Status: s, Profile: ok, ProfileID: id})
	}
	return out
}

// registeredProfiles maps a command name to the first profile launching it.
func registeredProfiles(path string) map[string]string {
	registered := map[string]string{}
	if inventory, err := config.LoadInventory(path); err == nil {
		for _, profile := range inventory.Profiles {
			if len(profile.Command) == 0 {
				continue
			}
			command := filepath.Base(profile.Command[0])
			if _, exists := registered[command]; !exists {
				registered[command] = profile.ID
			}
		}
	}
	return registered
}

func ensureProviderProfile(ctx context.Context, path string, env providers.Env, id string) error {
	p, ok := providers.Lookup(id)
	if !ok {
		return errors.New("unknown provider")
	}
	if _, ok := env.Executable(p.Command); !ok {
		return fmt.Errorf("%s 설치를 확인하지 못했습니다", p.Label)
	}
	lock, err := openProviderInventoryLock(path)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := filelock.Acquire(ctx, lock, 2*time.Second); err != nil {
		return fmt.Errorf("Home inventory 잠금을 얻지 못했습니다: %w", err)
	}
	defer filelock.Unlock(lock)
	return addProviderProfile(path, id)
}

func openProviderInventoryLock(path string) (*os.File, error) {
	lock, err := os.OpenFile(path+".lock", os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, errors.New("Home inventory 잠금 파일을 열지 못했습니다")
	}
	info, err := lock.Stat()
	if err != nil {
		lock.Close()
		return nil, errors.New("Home inventory 잠금 파일이 안전하지 않습니다")
	}
	stat, owner := info.Sys().(*syscall.Stat_t)
	if !info.Mode().IsRegular() || info.Mode().Perm()&0o077 != 0 || !owner || int(stat.Uid) != os.Getuid() {
		lock.Close()
		return nil, errors.New("Home inventory 잠금 파일이 안전하지 않습니다")
	}
	return lock, nil
}

// addProviderProfile appends one launch profile after a timestamped backup. It
// reuses the inventory's established workspace base and never rewrites or
// removes existing profiles. Callers must hold the inventory file lock.
func addProviderProfile(path, id string) error {
	p, ok := providers.Lookup(id)
	if !ok {
		return errors.New("unknown provider")
	}
	inventory, err := config.LoadInventory(path)
	if err != nil {
		return fmt.Errorf("Home inventory를 읽지 못했습니다: %w", err)
	}
	for _, profile := range inventory.Profiles {
		if len(profile.Command) > 0 && filepath.Base(profile.Command[0]) == p.Command {
			return nil
		}
		if profile.ID == p.ID {
			return fmt.Errorf("프로파일 ID %q가 이미 다른 명령에 쓰이고 있습니다", p.ID)
		}
	}
	directory := inventoryWorkspaceBase(inventory)
	inventory.Profiles = append(inventory.Profiles, model.Profile{
		ID: p.ID, Label: p.Label, DefaultDirectory: directory, Command: []string{p.Command}, Tags: []string{"ai", p.ID},
	})
	return config.AppendInventoryProfile(path, inventory.Profiles[len(inventory.Profiles)-1], time.Now())
}

// SetupHome assigns one base to all profiles. For older hand-written
// inventories, use the most common non-empty base (and stable first occurrence
// for ties) rather than inventing ~/work or ~ and escaping the configured root.
func inventoryWorkspaceBase(inventory model.Inventory) string {
	counts := make(map[string]int)
	best, bestCount := "", 0
	for _, profile := range inventory.Profiles {
		directory := profile.DefaultDirectory
		if directory == "" {
			continue
		}
		counts[directory]++
		if counts[directory] > bestCount {
			best, bestCount = directory, counts[directory]
		}
	}
	if best == "" {
		return "~/.hmux"
	}
	return best
}

// changesProviderAuth reports a result after which CLI credentials may have
// changed: a saved or cleared API key, or a connect/update job that finished.
func changesProviderAuth(operation string, data any) bool {
	result, ok := data.(providerResult)
	if !ok || result.Error != "" {
		return false
	}
	if operation == "provider-key" {
		return true
	}
	return result.Job != nil && (result.Job.State == providers.JobConnected || result.Job.State == providers.JobDone)
}
