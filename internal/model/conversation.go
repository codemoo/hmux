package model

const (
	ConversationReady       = "ready"
	ConversationUnavailable = "unavailable"
	ConversationAmbiguous   = "ambiguous"
)

// Conversation is a bounded, presentation-safe view of the public messages
// in the provider transcript bound to one exact tmux session instance.
type Conversation struct {
	Provider  string                `json:"provider,omitempty"`
	SessionID string                `json:"session_id"`
	CreatedAt int64                 `json:"created_at"`
	Status    string                `json:"status"`
	Messages  []ConversationMessage `json:"messages"`
	Truncated bool                  `json:"truncated"`
}

type ConversationMessage struct {
	ID   string `json:"id"`
	Role string `json:"role"`
	Text string `json:"text"`
}
