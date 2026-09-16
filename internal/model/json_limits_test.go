package model

import (
	"strings"
	"testing"
)

func TestValidateCatalogJSONStructureRejectsAmplifyingArrays(t *testing.T) {
	tooManySessions := `{"sessions":[` + strings.Repeat(`{},`, 10_000) + `{}` + `]}`
	if err := ValidateCatalogJSONStructure([]byte(tooManySessions)); err == nil || !strings.Contains(err.Error(), "sessions") {
		t.Fatalf("sessions error=%v", err)
	}
	tooManyTabs := `{"open_tabs":[` + strings.Repeat(`"$1",`, 256) + `"$1"` + `]}`
	if err := ValidateCatalogJSONStructure([]byte(tooManyTabs)); err == nil || !strings.Contains(err.Error(), "open_tabs") {
		t.Fatalf("tabs error=%v", err)
	}
}

func TestValidateCatalogJSONStructureAcceptsCatalogAndRejectsTrailingJSON(t *testing.T) {
	valid := `{"protocol_version":1,"sessions":[{"id":"$1","window_names":[],"tags":[],"workflows":[]}]}`
	if err := ValidateCatalogJSONStructure([]byte(valid)); err != nil {
		t.Fatal(err)
	}
	if err := ValidateCatalogJSONStructure([]byte(valid + `{}`)); err == nil {
		t.Fatal("trailing JSON was accepted")
	}
}
