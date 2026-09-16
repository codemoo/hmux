package main

import (
	"errors"
	"testing"
)

func TestShouldExecuteSelectedAppBackendAvoidsFreshClientAgentSkew(t *testing.T) {
	if shouldExecuteSelectedAppBackend(true, errors.New("agent update failed")) {
		t.Fatal("fresh client was activated after its paired agent update failed")
	}
	if !shouldExecuteSelectedAppBackend(false, errors.New("agent check failed")) {
		t.Fatal("previously validated cached client was unnecessarily disabled")
	}
	if !shouldExecuteSelectedAppBackend(true, nil) {
		t.Fatal("coherent client/agent update was not activated")
	}
}
