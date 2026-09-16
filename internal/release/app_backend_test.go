package release

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

func TestValidateAppBackendRequiresProtocolAndFeatures(t *testing.T) {
	requirement := AppBackendRequirement{Protocol: 1, Features: []string{"catalog", "hidden-set"}}
	tests := []struct {
		name    string
		output  string
		wantErr bool
	}{
		{
			name:   "compatible additive set",
			output: `{"app_protocol_version":1,"ok":true,"data":{"backend_protocol_version":1,"features":["catalog","hidden-set","future"]}}`,
		},
		{
			name:    "wrong app protocol",
			output:  `{"app_protocol_version":2,"ok":true,"data":{"backend_protocol_version":1,"features":["catalog","hidden-set"]}}`,
			wantErr: true,
		},
		{
			name:    "missing feature",
			output:  `{"app_protocol_version":1,"ok":true,"data":{"backend_protocol_version":1,"features":["catalog"]}}`,
			wantErr: true,
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "hmux")
			script := "#!/bin/sh\nprintf '%s\\n' '" + test.output + "'\n"
			if err := os.WriteFile(path, []byte(script), 0o700); err != nil {
				t.Fatal(err)
			}
			err := ValidateAppBackend(context.Background(), path, requirement)
			if (err != nil) != test.wantErr {
				t.Fatalf("error=%v wantErr=%t", err, test.wantErr)
			}
		})
	}
}

func TestCurrentAppRequiresSharedWorkspace(t *testing.T) {
	requirement := CurrentAppBackendRequirement()
	found := false
	for _, feature := range requirement.Features {
		if feature == "workspace" {
			found = true
		}
	}
	if !found {
		t.Fatal("native shared tabs must require the workspace bridge")
	}
}
