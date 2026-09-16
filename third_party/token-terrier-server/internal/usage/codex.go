package usage

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"net/http"
	"strings"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
)

// codexUsageResponse mirrors CodexUsageResponse — accepts both snake_case
// and camelCase shapes plus the rate_limit nested envelope, matching what
// codex CLI's `/wham/usage` endpoint may return across rollouts.
type codexUsageResponse struct {
	Primary      *codexWindow  `json:"primary"`
	Secondary    *codexWindow  `json:"secondary"`
	Tertiary     *codexWindow  `json:"tertiary"`
	Credits      *codexCredits `json:"credits"`
	LoginMethod  string        `json:"loginMethod"`
	PlanType     string        `json:"plan_type"`
	AccountEmail string        `json:"accountEmail"`
	RateLimit    *struct {
		PrimaryWindow   *codexWindow `json:"primary_window"`
		SecondaryWindow *codexWindow `json:"secondary_window"`
		TertiaryWindow  *codexWindow `json:"tertiary_window"`
	} `json:"rate_limit"`
}

// effectiveLoginMethod picks the loginMethod field, falling back to plan_type
// (older rollouts that returned plan in plan_type only).
func (r *codexUsageResponse) effectiveLoginMethod() string {
	if v := strings.TrimSpace(r.LoginMethod); v != "" {
		return v
	}
	return strings.TrimSpace(r.PlanType)
}

func (r *codexUsageResponse) primaryWindow() *codexWindow {
	if r.Primary != nil {
		return r.Primary
	}
	if r.RateLimit != nil {
		return r.RateLimit.PrimaryWindow
	}
	return nil
}

func (r *codexUsageResponse) secondaryWindow() *codexWindow {
	if r.Secondary != nil {
		return r.Secondary
	}
	if r.RateLimit != nil {
		return r.RateLimit.SecondaryWindow
	}
	return nil
}

func (r *codexUsageResponse) tertiaryWindow() *codexWindow {
	if r.Tertiary != nil {
		return r.Tertiary
	}
	if r.RateLimit != nil {
		return r.RateLimit.TertiaryWindow
	}
	return nil
}

// codexWindow captures the various shapes a single quota window has been
// observed in. We accept all of them and pick the first that decoded to
// non-zero/non-empty.
type codexWindow struct {
	UsedPercentCamel   flexFloat   `json:"usedPercent"`
	UsedPercentSnake   flexFloat   `json:"used_percent"`
	ResetsAtRaw        *string     `json:"resetsAt"`
	ResetAtRaw         json.Number `json:"reset_at"`
	WindowMinutes      flexInt     `json:"windowMinutes"`
	LimitWindowSeconds flexInt     `json:"limit_window_seconds"`
}

type codexCredits struct {
	Remaining flexFloat `json:"remaining"`
	Balance   flexFloat `json:"balance"`
	UpdatedAt string    `json:"updatedAt"`
}

func (c *codexCredits) effectiveRemaining() *float64 {
	if c == nil {
		return nil
	}
	if v := c.Remaining.Ptr(); v != nil {
		return v
	}
	return c.Balance.Ptr()
}

func (c *Client) fetchCodex(ctx context.Context, credential auth.OAuthCredential) (*codexUsageResponse, error) {
	req, err := http.NewRequestWithContext(ctx, "GET", "https://chatgpt.com/backend-api/wham/usage", nil)
	if err != nil {
		return nil, &APIError{Kind: KindInvalidResponse, Message: err.Error()}
	}
	req.Header.Set("Authorization", "Bearer "+credential.AccessToken)
	req.Header.Set("Accept", "application/json")
	if credential.AccountID != "" {
		req.Header.Set("ChatGPT-Account-Id", credential.AccountID)
	}
	body, err := c.execute(req)
	if err != nil {
		return nil, err
	}
	var resp codexUsageResponse
	if err := json.Unmarshal(body, &resp); err != nil {
		return nil, &APIError{Kind: KindInvalidResponse, Message: err.Error()}
	}
	if err := validateCodexUsage(&resp); err != nil {
		return nil, &APIError{Kind: KindInvalidResponse, Message: err.Error()}
	}
	return &resp, nil
}

func validateCodexUsage(resp *codexUsageResponse) error {
	if resp.primaryWindow() == nil && resp.secondaryWindow() == nil {
		return fmt.Errorf("missing rolling quota window")
	}
	for name, window := range map[string]*codexWindow{
		"primary": resp.primaryWindow(), "secondary": resp.secondaryWindow(), "tertiary": resp.tertiaryWindow(),
	} {
		if window == nil {
			continue
		}
		pct := window.UsedPercentCamel.Ptr()
		if pct == nil {
			pct = window.UsedPercentSnake.Ptr()
		}
		if pct == nil || math.IsNaN(*pct) || math.IsInf(*pct, 0) || *pct < 0 || *pct > 100 {
			return fmt.Errorf("%s has invalid used percentage", name)
		}
		if raw := strings.TrimSpace(stringValue(window.ResetsAtRaw)); raw != "" && parseISO(raw).IsZero() {
			return fmt.Errorf("%s has invalid reset time", name)
		}
		if window.ResetAtRaw != "" {
			seconds, err := window.ResetAtRaw.Int64()
			if err != nil || seconds <= 0 {
				return fmt.Errorf("%s has invalid reset epoch", name)
			}
		}
	}
	return nil
}

func stringValue(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}
