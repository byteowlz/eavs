package control

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
)

// Opt-in integration executes the actual freshly compiled Rust CLI, not a shell
// substitute. All state and credentials are synthetic and machine-local to t.TempDir.
func TestRealRustDeviceEnrollmentAndPiPublication(t *testing.T) {
	binary := os.Getenv("EAVS_DESKTOP_EAVS_BINARY")
	if binary == "" {
		t.Skip("set EAVS_DESKTOP_EAVS_BINARY to a freshly compiled source binary")
	}
	config := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", config)
	t.Setenv("EAVS_DESKTOP_TEST_KEY", "eavs-synthetic000000000000000000000000000000")
	t.Setenv("EAVS_DESKTOP_TEST_LISTENER", "synthetic-listener")
	var denied atomic.Bool
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/eavs/catalog" {
			t.Error("unexpected request path", r.URL.Path)
			http.NotFound(w, r)
			return
		}
		if denied.Load() || r.Header.Get("Authorization") != "Bearer eavs-synthetic000000000000000000000000000000" || r.Header.Get("X-Eavs-Token") != "synthetic-listener" {
			http.Error(w, "private gateway error", http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"schema_version":1,"scope":"key-filtered-chat-candidates","providers":[{"id":"alpha","api":"openai-responses","base_path":"/alpha/v1","request_path":"/alpha/v1/responses","operation":"chat","models":[{"id":"model-a","name":"Model A","reasoning":true,"input":["text"],"context_window":200000,"max_tokens":4096,"pricing":{"status":"unverified_estimate","unit":"USD_per_million_tokens","rates":{"input":3,"output":15,"cache_read":0.3,"cache_write":3.75}}}]}]}`))
	}))
	defer server.Close()
	runner := ExecRunner{Binary: binary}
	ctx := context.Background()
	draft := Enrollment{Name: "fixture", URL: server.URL, KeyEnv: "EAVS_DESKTOP_TEST_KEY", ListenerKeyEnv: "EAVS_DESKTOP_TEST_LISTENER", Trust: true, AllowLoopbackHTTP: true}
	preview, err := runner.Enrollment(ctx, draft, nil)
	if err != nil {
		t.Fatal("Rust enrollment preview", err)
	}
	if _, err := os.Stat(filepath.Join(config, "eavs", "device.json")); !os.IsNotExist(err) {
		t.Fatal("preview mutated registry")
	}
	if _, err := runner.Enrollment(ctx, draft, &preview); err != nil {
		t.Fatal("Rust enrollment apply", err)
	}
	snapshot, err := runner.Status(ctx)
	if err != nil || len(snapshot.Connections) != 1 || snapshot.Connections[0].State != "ready" {
		t.Fatal("Rust status", err)
	}
	target := PiTarget{Name: "fixture", Output: filepath.Join(t.TempDir(), "models.json")}
	plan, err := runner.Pi(ctx, target, nil)
	if err != nil {
		t.Fatal("Rust Pi preview", err)
	}
	if len(plan.ManagedProviders) != 1 {
		t.Fatal("scoped provider projection missing")
	}
	if _, err := runner.Pi(ctx, target, &plan); err != nil {
		t.Fatal("Rust Pi apply", err)
	}
	body, err := os.ReadFile(target.Output)
	if err != nil {
		t.Fatal(err)
	}
	var document map[string]any
	if json.Unmarshal(body, &document) != nil || document["providers"] == nil {
		t.Fatal("invalid published Pi config")
	}
	denied.Store(true)
	if _, err := runner.Pi(ctx, target, nil); err == nil {
		t.Fatal("revocation ignored")
	}
	unchanged, err := os.ReadFile(target.Output)
	if err != nil || string(unchanged) != string(body) {
		t.Fatal("revoked scope changed last-good config")
	}
}
